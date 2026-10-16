//! The web side of Crisol, put together (F5.4): sites, the PHP each one runs with, the PHP
//! pools and Nginx.
//!
//! Two pieces:
//!
//! - [`plan_web`] turns the sites on disk into a [`WebPlan`]: every site with the PHP version
//!   that [`resolve`](crate::resolve) picks for its folder. Nothing runs and nothing is written.
//! - [`WebStack`] runs a plan. It owns Nginx and one pool per PHP version in use as
//!   [`Service`]s, writes the Nginx configuration, and **changes it while running**:
//!   [`WebStack::apply`] with a new plan starts the pools new sites need, regenerates the files
//!   and reloads Nginx, then stops the pools nobody uses. The other sites keep answering
//!   throughout (`nginx -s reload` swaps workers without dropping connections).
//!
//! A new configuration never replaces the one on disk until Nginx has accepted it. It is
//! written in a staging folder (`etc/nginx/.next/`, with its own paths), tested there with
//! `nginx -t`, and only if the test passes is it written to `etc/nginx/` and `nginx -s reload`
//! sent. A configuration Nginx rejects (a mistake in a user override, typically) is reported
//! ([`WebEvent::ReloadFailed`], with where the output is) and neither the running server nor
//! the files its next start would read change. The first start follows the same road.
//!
//! A reload is a small sequence advanced by [`WebStack::tick`]: wait for the pools the change
//! created, test, promote, reload. Changes that arrive meanwhile are merged into one more
//! round. A pool no site uses any more is stopped a few seconds after the reload, because the
//! old Nginx workers keep serving for a moment after the signal.
//!
//! Ports of a running pool never move: a version added later gets a free block
//! ([`PhpPools::ensure`]), whatever its position in version order.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::ca::ensure_ca;
use crate::catalog::Catalog;
use crate::config::{Config, PhpConfig};
use crate::manage::InstalledVersion;
use crate::nginx::{
    config_differs, render, stage_nginx_config, write_nginx_config, NginxError, NginxInput,
    NginxSite, NginxTls, PhpPools,
};
use crate::paths::{CrisolPaths, PathsError, PrivateDirs};
use crate::phppool::{pool_spec, service_name};
use crate::ports::PortProbe;
use crate::process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
};
use crate::project::FileOwners;
use crate::resolve::{resolve, ResolveContext, Tool};
use crate::service::{
    HealthCheck, LifecyclePolicy, Service, ServiceEvent, ServiceSpec, ServiceState,
};
use crate::sites::{discover, Site, SitesFile};
use crate::tls::{ensure_site_cert, Renewal};

/// How long each step of a reload may take.
const RELOAD_STEP_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a pool that no site uses keeps running after the reload that stops sending it
/// requests: the old Nginx workers finish what they were doing.
const RETIRE_GRACE: Duration = Duration::from_secs(5);

// ---- planning -------------------------------------------------------------------------

/// A site and the PHP it runs with (`None`: none installed fits, so it is static only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSite {
    /// The site.
    pub site: Site,
    /// The installed PHP that serves it.
    pub php: Option<InstalledVersion>,
}

/// What the web side should be serving.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebPlan {
    /// The sites, by name.
    pub sites: Vec<PlannedSite>,
    /// Problems worth showing: sites left out, a PHP that is not installed.
    pub warnings: Vec<String>,
}

impl WebPlan {
    /// The PHP versions the sites use.
    #[must_use]
    pub fn php_versions(&self) -> BTreeMap<String, InstalledVersion> {
        self.sites
            .iter()
            .filter_map(|s| s.php.as_ref())
            .map(|v| (v.version.to_string(), v.clone()))
            .collect()
    }
}

/// Builds a plan from `sites`, asking `php_for` which PHP each one uses.
#[must_use]
pub fn plan_from(
    sites: Vec<Site>,
    mut warnings: Vec<String>,
    php_for: &mut dyn FnMut(&Site) -> Result<Option<InstalledVersion>, String>,
) -> WebPlan {
    let mut planned = Vec::with_capacity(sites.len());
    for site in sites {
        let php = php_for(&site).unwrap_or_else(|why| {
            warnings.push(format!("{} is served without PHP: {why}", site.domain));
            None
        });
        planned.push(PlannedSite { site, php });
    }
    WebPlan {
        sites: planned,
        warnings,
    }
}

/// The plan for what is on disk now: parked folders and linked sites, each with the PHP that
/// the shims would run in its folder (`.crisol.toml`, `.php-version`, `composer.json`, the
/// default…).
///
/// `CRISOL_PHP` and `CRISOL_NODE` are hidden from the resolution: they override one terminal,
/// not the web server. The warnings of the resolution (a `.crisol.toml` ignored because of its
/// owner…) are added to the plan's, with the site's domain in front.
///
/// A site whose requested PHP is not installed is still served, as static files, and the plan
/// says why.
#[must_use]
pub fn plan_web(
    paths: &CrisolPaths,
    config: &Config,
    linked: &SitesFile,
    catalog: &Catalog,
    owners: &dyn FileOwners,
    env: &dyn Fn(&str) -> Option<String>,
) -> WebPlan {
    let found = discover(paths, config, linked);
    let terminal_only = |name: &str| match name {
        "CRISOL_PHP" | "CRISOL_NODE" => None,
        other => env(other),
    };
    let mut resolution_warnings: Vec<String> = Vec::new();
    let mut php_for = |site: &Site| -> Result<Option<InstalledVersion>, String> {
        let context = ResolveContext {
            paths,
            cwd: &site.root,
            ceiling: None,
            owners,
            env: &terminal_only,
            catalog,
        };
        match resolve(Tool::Php, &context) {
            Ok(resolution) => {
                resolution_warnings.extend(
                    resolution
                        .warnings
                        .iter()
                        .map(|w| format!("{}: {w}", site.domain)),
                );
                Ok(Some(resolution.resolved.version))
            }
            Err(error) => Err(error.to_string()),
        }
    };
    let mut plan = plan_from(found.sites, found.warnings, &mut php_for);
    for warning in resolution_warnings {
        if !plan.warnings.contains(&warning) {
            plan.warnings.push(warning);
        }
    }
    plan
}

// ---- running --------------------------------------------------------------------------

/// Why the stack could not be configured.
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    /// A path is not allowed.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// The Nginx configuration could not be written.
    #[error(transparent)]
    Nginx(#[from] NginxError),
}

/// Something that happened in the web stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebEvent {
    /// A service (`nginx`, `php-8.4.26`) did something.
    Service {
        /// Its name.
        name: String,
        /// What.
        event: ServiceEvent,
    },
    /// The configuration changed and is going to be tested and, if it passes, loaded.
    ReloadStarted,
    /// Nginx accepted the new configuration and was told to reload. Its new workers take over
    /// a moment later (`nginx -s reload` returns once the signal is sent).
    Reloaded,
    /// The new configuration was not put in place: Nginx rejected it, or could not be asked
    /// to reload. The files in `etc/nginx/` are the ones that worked before.
    ReloadFailed {
        /// What went wrong.
        reason: String,
    },
}

/// What [`WebStack::apply`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The configuration for this plan is not what is in `etc/nginx/` yet.
    pub config_changed: bool,
    /// Pools created for PHP versions that were not in use.
    pub new_pools: Vec<String>,
    /// Pools no site uses any more; they stop shortly after Nginx stops sending them requests.
    pub retired_pools: Vec<String>,
    /// A reload was requested (or one under way will run again).
    pub reload_requested: bool,
    /// The local CA was made now: it is not trusted by anything yet, so browsers will warn
    /// until the user installs it (`TrustStore`, F6.3).
    pub ca_created: bool,
    /// The certificate for the secured sites was made again, and why; `None` if it was kept
    /// (or there are no secured sites).
    pub certificate: Option<Renewal>,
    /// Sites left out of the configuration and other things worth saying.
    pub warnings: Vec<String>,
}

/// The configuration the stack wants: what [`WebStack::apply`] was last given, owned so that
/// it can be written later, when Nginx has accepted it.
#[derive(Debug, Clone)]
struct Desired {
    sites: Vec<(Site, Option<String>)>,
    pools: PhpPools,
    /// The certificate and port of the secured sites, if there is a certificate for them.
    tls: Option<(PathBuf, PathBuf, u16)>,
    /// The certificate changed but the text of the configuration did not: Nginx still has to
    /// reload to pick the new one up.
    force_reload: bool,
}

impl Desired {
    fn entries(&self) -> Vec<NginxSite<'_>> {
        self.sites
            .iter()
            .map(|(site, php)| NginxSite {
                site,
                php: php.as_deref(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for the new pools to come up.
    Pools,
    /// `nginx -t` is running on the staged configuration.
    Test,
    /// `nginx -s reload` is running.
    Reload,
}

struct Reload {
    phase: Phase,
    since: Instant,
    helper: Option<(GroupId, ProcessId)>,
    /// What is being tested (and, if it passes, written).
    tested: Option<Desired>,
    /// This round is the first start of Nginx, not a reload.
    then_start: bool,
}

struct Pool {
    service: Service,
}

/// Nginx and the PHP pools, running (or ready to).
pub struct WebStack {
    paths: CrisolPaths,
    http_port: u16,
    https_port: u16,
    tld: String,
    php: PhpConfig,
    reserved: BTreeSet<u16>,
    nginx_dir: PathBuf,
    supervisor: Arc<dyn ProcessSupervisor>,
    probe: Arc<dyn PortProbe>,
    dirs: Arc<dyn PrivateDirs>,
    nginx: Option<Service>,
    pools: BTreeMap<String, Pool>,
    /// Pools started by `apply` that have not finished starting; a reload waits for these.
    awaiting: BTreeSet<String>,
    /// Pools that no site uses; stopped once the reload that stops sending them requests ends.
    unused: BTreeSet<String>,
    retire_at: Option<Instant>,
    stopping: Vec<Service>,
    ports: PhpPools,
    desired: Option<Desired>,
    running: bool,
    reload: Option<Reload>,
    reload_again: bool,
}

impl std::fmt::Debug for WebStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebStack")
            .field("running", &self.running)
            .field("pools", &self.pools.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl WebStack {
    /// A stack that is stopped. `nginx_dir` is the Nginx installation (`bin/nginx/<version>`).
    /// `dirs` makes `certs/` private and tells whether it is open to other accounts: the local
    /// CA is never used otherwise (ADR-0008, ADR-0013).
    #[must_use]
    pub fn new(
        paths: CrisolPaths,
        config: &Config,
        nginx_dir: PathBuf,
        supervisor: Arc<dyn ProcessSupervisor>,
        probe: Arc<dyn PortProbe>,
        dirs: Arc<dyn PrivateDirs>,
    ) -> Self {
        Self {
            paths,
            http_port: config.services.nginx.http_port,
            https_port: config.services.nginx.https_port,
            tld: config.sites.tld.clone(),
            php: config.php.clone(),
            reserved: config.reserved_ports(),
            nginx_dir,
            supervisor,
            probe,
            dirs,
            nginx: None,
            pools: BTreeMap::new(),
            awaiting: BTreeSet::new(),
            unused: BTreeSet::new(),
            retire_at: None,
            stopping: Vec::new(),
            ports: PhpPools::default(),
            desired: None,
            running: false,
            reload: None,
            reload_again: false,
        }
    }

    /// Whether the stack was started and not stopped.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The state of Nginx, if it has been configured.
    #[must_use]
    pub fn nginx_state(&self) -> Option<&ServiceState> {
        self.nginx.as_ref().map(Service::state)
    }

    /// The state of the pool of a PHP version.
    #[must_use]
    pub fn pool_state(&self, version: &str) -> Option<&ServiceState> {
        self.pools.get(version).map(|p| p.service.state())
    }

    /// The ports the pool of a version listens on.
    #[must_use]
    pub fn pool_ports(&self, version: &str) -> Option<&[u16]> {
        self.ports.ports(version)
    }

    fn main_conf(&self) -> Result<PathBuf, PathsError> {
        Ok(self.paths.etc_for("nginx")?.join("nginx.conf"))
    }

    /// Takes `plan` as what should be served. Pools for new PHP versions are created (and
    /// started if the stack runs), and if the stack runs and the configuration differs from the
    /// one on disk a reload is requested: it will be tested in the staging folder and only
    /// then written. Nothing under `etc/nginx/` changes here.
    ///
    /// When the stack is stopped it only prepares: [`start`](Self::start) tests and runs it.
    pub fn apply(&mut self, plan: &WebPlan, now: Instant) -> Result<Applied, WebError> {
        let versions = plan.php_versions();
        for version in versions.keys() {
            self.ports.ensure(
                version,
                self.php.base_port,
                self.php.workers,
                &self.reserved,
            );
        }
        let secured: Vec<String> = plan
            .sites
            .iter()
            .filter(|s| s.site.secure)
            .map(|s| s.site.domain.clone())
            .collect();
        let mut tls_warnings = Vec::new();
        let (tls, ca_created, certificate) = if secured.is_empty() {
            (None, false, None)
        } else {
            match self.prepare_tls(&secured, SystemTime::now()) {
                Ok((files, created, certificate)) => (Some(files), created, certificate),
                Err(why) => {
                    tls_warnings.push(format!("the secured sites are served over HTTP: {why}"));
                    (None, false, None)
                }
            }
        };
        let desired = Desired {
            sites: plan
                .sites
                .iter()
                .map(|planned| {
                    let php = planned
                        .php
                        .as_ref()
                        .map(|v| v.version.to_string())
                        .filter(|v| self.ports.ports(v).is_some());
                    (planned.site.clone(), php)
                })
                .collect(),
            pools: self.ports.clone(),
            tls,
            force_reload: certificate.is_some(),
        };
        let (config_changed, mut warnings) = {
            let entries = desired.entries();
            let input = self.input(&desired, &entries);
            (config_differs(&input)?, render(&input).warnings)
        };
        warnings.extend(tls_warnings);
        self.desired = Some(desired);

        let (new_pools, retired_pools) = self.sync_pools(&versions)?;

        if self.nginx.is_none() {
            self.nginx = Some(self.nginx_service()?);
        }

        let mut reload_requested = false;
        if self.running {
            for version in &new_pools {
                if let Some(pool) = self.pools.get_mut(version) {
                    if pool.service.start(now).is_ok() {
                        self.awaiting.insert(version.clone());
                    }
                }
            }
            if config_changed || certificate.is_some() || self.reload.is_some() {
                self.request_reload(now);
                reload_requested = true;
            } else if !retired_pools.is_empty() {
                // Nothing to reload, and nobody sends requests to these pools any more.
                self.retire_unused(now);
            }
        } else {
            self.retire_unused(now);
        }

        Ok(Applied {
            config_changed,
            new_pools,
            retired_pools,
            reload_requested,
            ca_created,
            certificate,
            warnings,
        })
    }

    /// Creates the pools for PHP versions that have none (not started) and marks as unused the
    /// ones no site needs. Returns the new and the retired versions.
    fn sync_pools(
        &mut self,
        versions: &BTreeMap<String, InstalledVersion>,
    ) -> Result<(Vec<String>, Vec<String>), WebError> {
        let mut new_pools = Vec::new();
        for (version, installed) in versions {
            self.unused.remove(version);
            if self.pools.contains_key(version) {
                continue;
            }
            let Some(ports) = self.ports.ports(version) else {
                continue; // no free ports: its sites get 503 from the generated file
            };
            let spec = pool_spec(&self.paths, &self.php, version, &installed.path, ports)?;
            let service = Service::new(spec, self.supervisor.clone(), self.probe.clone());
            self.pools.insert(version.clone(), Pool { service });
            new_pools.push(version.clone());
        }
        let retired_pools: Vec<String> = self
            .pools
            .keys()
            .filter(|v| !versions.contains_key(*v))
            .cloned()
            .collect();
        self.unused.extend(retired_pools.iter().cloned());

        Ok((new_pools, retired_pools))
    }

    /// The local CA (made if there is none) and the certificate for `hosts`. Returns the files
    /// and port for Nginx, whether the CA is new, and why the certificate was made again.
    #[allow(clippy::type_complexity)]
    fn prepare_tls(
        &self,
        hosts: &[String],
        now: SystemTime,
    ) -> Result<((PathBuf, PathBuf, u16), bool, Option<Renewal>), crate::ca::CaError> {
        let ensured = ensure_ca(&self.paths, self.dirs.as_ref(), &self.tld, now)?;
        let cert = ensure_site_cert(&self.paths, &ensured.ca, hosts, now)?;
        Ok((
            (cert.cert, cert.key, self.https_port),
            ensured.created,
            cert.changed,
        ))
    }

    fn input<'a>(&'a self, desired: &'a Desired, entries: &'a [NginxSite<'a>]) -> NginxInput<'a> {
        NginxInput {
            paths: &self.paths,
            nginx_dir: &self.nginx_dir,
            http_port: self.http_port,
            sites: entries,
            pools: &desired.pools,
            tls: desired
                .tls
                .as_ref()
                .map(|(cert, key, https_port)| NginxTls {
                    cert,
                    key,
                    https_port: *https_port,
                }),
        }
    }

    fn nginx_service(&self) -> Result<Service, WebError> {
        let program = self
            .nginx_dir
            .join(format!("nginx{}", std::env::consts::EXE_SUFFIX));
        let main = self.main_conf()?;
        let control = |extra: &[&str]| {
            ProcessSpec::new(&program)
                .arg("-c")
                .arg(&main)
                .arg("-p")
                .arg(&self.nginx_dir)
                .args(extra.iter().copied())
        };
        let log = self.paths.runtime_log("nginx")?;
        let spec = ServiceSpec::new("nginx")
            .process(control(&[]).cwd(&self.nginx_dir).log_file(log))
            .port(self.http_port)
            .health(HealthCheck::Ports(vec![self.http_port]))
            .graceful_stop(control(&["-s", "quit"]))
            .policy(LifecyclePolicy::default());
        Ok(Service::new(
            spec,
            self.supervisor.clone(),
            self.probe.clone(),
        ))
    }

    /// Starts the pools and, once the configuration has passed `nginx -t` and been written,
    /// Nginx. Call [`apply`](Self::apply) first. If the new configuration is rejected and an
    /// earlier one is on disk, Nginx starts with that one and [`WebEvent::ReloadFailed`] says
    /// why.
    pub fn start(&mut self, now: Instant) -> Vec<WebEvent> {
        let mut events = Vec::new();
        self.running = true;
        for (name, pool) in &mut self.pools {
            if let Ok(list) = pool.service.start(now) {
                events.extend(wrap(&service_name(name), list));
            }
        }
        let on_disk = self.main_conf().is_ok_and(|main| main.is_file());
        if self.desired.is_some() {
            self.begin_test(true, now, &mut events);
        } else if on_disk {
            self.start_nginx(now, &mut events);
        }
        events
    }

    fn start_nginx(&mut self, now: Instant, events: &mut Vec<WebEvent>) {
        if let Some(nginx) = &mut self.nginx {
            if let Ok(list) = nginx.start(now) {
                events.extend(wrap("nginx", list));
            }
        }
    }

    /// Stops Nginx and the pools, and cancels a reload under way.
    pub fn stop(&mut self, now: Instant) -> Vec<WebEvent> {
        let mut events = Vec::new();
        self.running = false;
        self.cancel_reload();
        self.reload_again = false;
        self.retire_at = None;
        self.awaiting.clear();
        if let Some(nginx) = &mut self.nginx {
            events.extend(wrap("nginx", nginx.stop(now)));
        }
        for (name, pool) in &mut self.pools {
            events.extend(wrap(&service_name(name), pool.service.stop(now)));
        }
        events
    }

    /// Whether nothing is running any more (after [`stop`](Self::stop)).
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        let down =
            |s: &Service| matches!(s.state(), ServiceState::Stopped | ServiceState::Failed(_));
        self.nginx.as_ref().is_none_or(down)
            && self.pools.values().all(|p| down(&p.service))
            && self.stopping.iter().all(down)
    }

    /// Advances everything. Call it every 150 to 500 ms.
    pub fn tick(&mut self, now: Instant) -> Vec<WebEvent> {
        let mut events = Vec::new();
        for (name, pool) in &mut self.pools {
            events.extend(wrap(&service_name(name), pool.service.tick(now)));
        }
        if let Some(nginx) = &mut self.nginx {
            events.extend(wrap("nginx", nginx.tick(now)));
        }
        for service in &mut self.stopping {
            let name = service.name().to_owned();
            events.extend(wrap(&name, service.tick(now)));
        }
        self.stopping
            .retain(|s| !matches!(s.state(), ServiceState::Stopped | ServiceState::Failed(_)));
        let pools = &self.pools;
        self.awaiting.retain(|version| {
            pools
                .get(version)
                .is_some_and(|p| *p.service.state() == ServiceState::Starting)
        });
        self.tick_reload(now, &mut events);
        if self.retire_at.is_some_and(|at| now >= at) {
            self.retire_unused(now);
        }
        events
    }

    // ---- reload -----------------------------------------------------------------------

    fn request_reload(&mut self, now: Instant) {
        if self.reload.is_some() {
            self.reload_again = true;
        } else {
            self.reload = Some(Reload {
                phase: Phase::Pools,
                since: now,
                helper: None,
                tested: None,
                then_start: false,
            });
        }
    }

    fn cancel_reload(&mut self) {
        if let Some(reload) = self.reload.take() {
            if let Some((group, _)) = reload.helper {
                self.close_helper(group);
            }
        }
    }

    fn close_helper(&self, group: GroupId) {
        let _ = self.supervisor.kill_group(group);
        let _ = self.supervisor.close_group(group);
    }

    /// Runs `nginx -c <conf> -p <dir> <args>` once, in a group of its own.
    fn run_nginx(&self, conf: &Path, args: &[&str]) -> Result<(GroupId, ProcessId), String> {
        let program = self
            .nginx_dir
            .join(format!("nginx{}", std::env::consts::EXE_SUFFIX));
        let log = self
            .paths
            .logs_for("nginx")
            .map_err(|e| e.to_string())?
            .join("reload.log");
        let spec = ProcessSpec::new(program)
            .arg("-c")
            .arg(conf)
            .arg("-p")
            .arg(&self.nginx_dir)
            .args(args.iter().copied())
            .cwd(&self.nginx_dir)
            .log_file(log);
        let group = self
            .supervisor
            .create_group("nginx-control", GroupOptions::default())
            .map_err(|e| e.to_string())?;
        match self.supervisor.spawn(group, &spec) {
            Ok(process) => Ok((group, process)),
            Err(e) => {
                self.close_helper(group);
                Err(e.to_string())
            }
        }
    }

    fn reload_log(&self) -> PathBuf {
        self.paths
            .logs_for("nginx")
            .map(|d| d.join("reload.log"))
            .unwrap_or_default()
    }

    /// The round failed: the files in `etc/nginx/` are untouched. If this was the first start
    /// and an earlier configuration exists, Nginx starts with it.
    fn fail_reload(&mut self, reason: String, now: Instant, events: &mut Vec<WebEvent>) {
        let then_start = self.reload.as_ref().is_some_and(|r| r.then_start);
        self.cancel_reload();
        events.push(WebEvent::ReloadFailed { reason });
        if then_start && self.running && self.main_conf().is_ok_and(|main| main.is_file()) {
            self.start_nginx(now, events);
        }
        self.finish_reload_round(now, false);
    }

    /// After a round, successful or not: stop pools nobody uses (after a grace period) and run
    /// the round that was asked for in the meantime.
    fn finish_reload_round(&mut self, now: Instant, ok: bool) {
        if ok && !self.unused.is_empty() {
            self.retire_at = Some(now + RETIRE_GRACE);
        }
        if std::mem::take(&mut self.reload_again) && self.running {
            self.request_reload(now);
        }
    }

    fn retire_unused(&mut self, now: Instant) {
        self.retire_at = None;
        for version in std::mem::take(&mut self.unused) {
            if let Some(mut pool) = self.pools.remove(&version) {
                let _ = pool.service.stop(now);
                self.stopping.push(pool.service);
            }
        }
        let mut keep: BTreeSet<String> = self.pools.keys().cloned().collect();
        keep.extend(
            self.stopping
                .iter()
                .filter_map(|s| s.name().strip_prefix("php-").map(str::to_owned)),
        );
        self.ports.retain(&keep);
    }

    fn tick_reload(&mut self, now: Instant, events: &mut Vec<WebEvent>) {
        let Some(reload) = self.reload.as_ref() else {
            return;
        };
        let (phase, since, helper) = (reload.phase, reload.since, reload.helper);
        if now.saturating_duration_since(since) >= RELOAD_STEP_TIMEOUT {
            let reason = match phase {
                Phase::Pools => "the new PHP pools did not come up in time".to_owned(),
                Phase::Test => "`nginx -t` did not finish in time".to_owned(),
                Phase::Reload => "`nginx -s reload` did not finish in time".to_owned(),
            };
            self.fail_reload(reason, now, events);
            return;
        }
        match phase {
            Phase::Pools => {
                if !self.running {
                    self.cancel_reload();
                } else if self.awaiting.is_empty() {
                    self.begin_test(false, now, events);
                }
            }
            Phase::Test | Phase::Reload => {
                let Some((group, process)) = helper else {
                    return;
                };
                match self.supervisor.status(process) {
                    Ok(ProcessStatus::Running) => {}
                    Ok(ProcessStatus::Exited(ExitReason::Code(0))) => {
                        self.close_helper(group);
                        if phase == Phase::Test {
                            self.promote(now, events);
                        } else {
                            self.reload = None;
                            events.push(WebEvent::Reloaded);
                            self.finish_reload_round(now, true);
                        }
                    }
                    _ => {
                        let what = if phase == Phase::Test {
                            "nginx rejected the new configuration (the previous one stays in place)"
                        } else {
                            "nginx could not be asked to reload"
                        };
                        let reason = format!("{what}; see {}", self.reload_log().display());
                        self.fail_reload(reason, now, events);
                    }
                }
            }
        }
    }

    /// Stages what is wanted now and runs `nginx -t` on it.
    fn begin_test(&mut self, then_start: bool, now: Instant, events: &mut Vec<WebEvent>) {
        self.cancel_reload();
        let Some(desired) = self.desired.clone() else {
            return;
        };
        let staged = {
            let entries = desired.entries();
            let input = self.input(&desired, &entries);
            match config_differs(&input) {
                Ok(false)
                    if !desired.force_reload
                        && self.main_conf().is_ok_and(|main| main.is_file()) =>
                {
                    None
                }
                Ok(_) => Some(stage_nginx_config(&input)),
                Err(e) => Some(Err(e)),
            }
        };
        let staged = match staged {
            // The disk already has exactly this: nothing to test or reload.
            None => {
                if then_start {
                    self.start_nginx(now, events);
                }
                self.finish_reload_round(now, true);
                return;
            }
            Some(Ok(staged)) => staged,
            Some(Err(e)) => {
                self.reload = Some(Reload {
                    phase: Phase::Test,
                    since: now,
                    helper: None,
                    tested: None,
                    then_start,
                });
                self.fail_reload(
                    format!("could not prepare the configuration: {e}"),
                    now,
                    events,
                );
                return;
            }
        };
        if !then_start {
            events.push(WebEvent::ReloadStarted);
        }
        match self.run_nginx(&staged.main, &["-t"]) {
            Ok(helper) => {
                self.reload = Some(Reload {
                    phase: Phase::Test,
                    since: now,
                    helper: Some(helper),
                    tested: Some(desired),
                    then_start,
                });
            }
            Err(reason) => {
                self.reload = Some(Reload {
                    phase: Phase::Test,
                    since: now,
                    helper: None,
                    tested: None,
                    then_start,
                });
                self.fail_reload(reason, now, events);
            }
        }
    }

    /// `nginx -t` passed: write the tested configuration to `etc/nginx/`, then reload (or, for
    /// the first start, start Nginx).
    fn promote(&mut self, now: Instant, events: &mut Vec<WebEvent>) {
        let Some(reload) = self.reload.take() else {
            return;
        };
        let Some(tested) = reload.tested else {
            return;
        };
        let written = {
            let entries = tested.entries();
            let input = self.input(&tested, &entries);
            write_nginx_config(&input)
        };
        if let Some(stage) = crate::nginx::staging_dir(&self.paths) {
            let _ = std::fs::remove_dir_all(stage);
        }
        if let Some(desired) = &mut self.desired {
            desired.force_reload = false;
        }
        if let Err(e) = written {
            self.reload = Some(Reload {
                tested: None,
                helper: None,
                phase: Phase::Test,
                since: now,
                then_start: reload.then_start,
            });
            self.fail_reload(
                format!("could not write the configuration: {e}"),
                now,
                events,
            );
            return;
        }
        if reload.then_start {
            self.start_nginx(now, events);
            self.finish_reload_round(now, true);
            return;
        }
        let Ok(main) = self.main_conf() else {
            return;
        };
        match self.run_nginx(&main, &["-s", "reload"]) {
            Ok(helper) => {
                self.reload = Some(Reload {
                    phase: Phase::Reload,
                    since: now,
                    helper: Some(helper),
                    tested: None,
                    then_start: false,
                });
            }
            Err(reason) => {
                self.reload = Some(Reload {
                    phase: Phase::Reload,
                    since: now,
                    helper: None,
                    tested: None,
                    then_start: false,
                });
                self.fail_reload(reason, now, events);
            }
        }
    }
}

fn wrap(name: &str, events: Vec<ServiceEvent>) -> impl Iterator<Item = WebEvent> + '_ {
    events.into_iter().map(move |event| WebEvent::Service {
        name: name.to_owned(),
        event,
    })
}

#[cfg(test)]
mod tests;
