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
//! A reload is a small sequence, advanced by [`WebStack::tick`]: wait until the new pools are
//! up, check the configuration with `nginx -t`, and only if it passes send `nginx -s reload`.
//! A configuration Nginx rejects is reported ([`WebEvent::ReloadFailed`], with where its
//! output is) and the running server is left as it was. Requests that arrive while a reload is
//! under way are merged into one more reload afterwards.
//!
//! Ports of a running pool never move: a version added later gets a free block
//! ([`PhpPools::ensure`]), whatever its position in version order.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::catalog::Catalog;
use crate::config::{Config, PhpConfig};
use crate::manage::InstalledVersion;
use crate::nginx::{write_nginx_config, NginxError, NginxInput, NginxSite, PhpPools, WrittenNginx};
use crate::paths::{CrisolPaths, PathsError};
use crate::phppool::{pool_spec, service_name};
use crate::ports::PortProbe;
use crate::process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
};
use crate::project::FileOwners;
use crate::resolve::{resolve, ResolveContext, ResolveError, Tool};
use crate::service::{
    HealthCheck, LifecyclePolicy, Service, ServiceEvent, ServiceSpec, ServiceState,
};
use crate::sites::{discover, Site, SitesFile};

/// How long each step of a reload may take.
const RELOAD_STEP_TIMEOUT: Duration = Duration::from_secs(20);

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
    let mut php_for = |site: &Site| -> Result<Option<InstalledVersion>, String> {
        let context = ResolveContext {
            paths,
            cwd: &site.root,
            ceiling: None,
            owners,
            env,
            catalog,
        };
        match resolve(Tool::Php, &context) {
            Ok(resolution) => Ok(Some(resolution.resolved.version)),
            Err(
                error @ (ResolveError::NotInstalled { .. } | ResolveError::NothingInstalled(_)),
            ) => Err(error.to_string()),
            Err(other) => Err(other.to_string()),
        }
    };
    plan_from(found.sites, found.warnings, &mut php_for)
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
    /// The configuration changed and Nginx is going to be reloaded.
    ReloadStarted,
    /// Nginx accepted the new configuration and was told to reload. Its new workers take over
    /// a moment later (`nginx -s reload` returns once the signal is sent).
    Reloaded,
    /// Nginx did not accept the new configuration, or could not be asked to reload. The
    /// running server keeps the old one.
    ReloadFailed {
        /// What went wrong.
        reason: String,
    },
}

/// What [`WebStack::apply`] changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The configuration files.
    pub nginx: WrittenNginx,
    /// Pools created for PHP versions that were not in use.
    pub new_pools: Vec<String>,
    /// Pools no site uses any more; they stop once Nginx stops sending them requests.
    pub retired_pools: Vec<String>,
    /// A reload was requested.
    pub reload_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for the new pools to come up.
    Pools,
    /// `nginx -t` is running.
    Test,
    /// `nginx -s reload` is running.
    Reload,
}

struct Reload {
    phase: Phase,
    since: Instant,
    helper: Option<(GroupId, ProcessId)>,
}

struct Pool {
    service: Service,
}

/// Nginx and the PHP pools, running (or ready to).
pub struct WebStack {
    paths: CrisolPaths,
    http_port: u16,
    php: PhpConfig,
    nginx_dir: PathBuf,
    supervisor: Arc<dyn ProcessSupervisor>,
    probe: Arc<dyn PortProbe>,
    nginx: Option<Service>,
    main_conf: Option<PathBuf>,
    pools: BTreeMap<String, Pool>,
    /// Pools that no site uses; stopped once the reload that stops sending them requests ends.
    unused: BTreeSet<String>,
    stopping: Vec<Service>,
    ports: PhpPools,
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
    #[must_use]
    pub fn new(
        paths: CrisolPaths,
        config: &Config,
        nginx_dir: PathBuf,
        supervisor: Arc<dyn ProcessSupervisor>,
        probe: Arc<dyn PortProbe>,
    ) -> Self {
        Self {
            paths,
            http_port: config.services.nginx.http_port,
            php: config.php.clone(),
            nginx_dir,
            supervisor,
            probe,
            nginx: None,
            main_conf: None,
            pools: BTreeMap::new(),
            unused: BTreeSet::new(),
            stopping: Vec::new(),
            ports: PhpPools::default(),
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

    /// Writes the configuration for `plan` and makes the running stack follow it: pools for
    /// new PHP versions are started, Nginx is reloaded, and pools nobody uses are retired.
    /// When the stack is stopped it only prepares: the next [`start`](Self::start) runs it.
    pub fn apply(&mut self, plan: &WebPlan, now: Instant) -> Result<Applied, WebError> {
        let versions = plan.php_versions();
        for version in versions.keys() {
            self.ports
                .ensure(version, self.php.base_port, self.php.workers);
        }

        let entries: Vec<NginxSite<'_>> = plan
            .sites
            .iter()
            .map(|s| NginxSite {
                site: &s.site,
                php: s
                    .php
                    .as_ref()
                    .and_then(|v| versions.get_key_value(&v.version.to_string()))
                    .map(|(version, _)| version.as_str())
                    .filter(|version| self.ports.ports(version).is_some()),
            })
            .collect();
        let nginx = write_nginx_config(&NginxInput {
            paths: &self.paths,
            nginx_dir: &self.nginx_dir,
            http_port: self.http_port,
            sites: &entries,
            pools: &self.ports,
        })?;
        self.main_conf = Some(nginx.main.clone());

        let mut new_pools = Vec::new();
        for (version, installed) in &versions {
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

        if self.nginx.is_none() {
            self.nginx = Some(self.nginx_service(&nginx.main)?);
        }

        let mut reload_requested = false;
        if self.running {
            for version in &new_pools {
                if let Some(pool) = self.pools.get_mut(version) {
                    let _ = pool.service.start(now);
                }
            }
            if !nginx.changed.is_empty() {
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
            nginx,
            new_pools,
            retired_pools,
            reload_requested,
        })
    }

    fn nginx_service(&self, main: &Path) -> Result<Service, WebError> {
        let program = self
            .nginx_dir
            .join(format!("nginx{}", std::env::consts::EXE_SUFFIX));
        let control = |extra: &[&str]| {
            ProcessSpec::new(&program)
                .arg("-c")
                .arg(main)
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

    /// Starts Nginx and every pool. Call [`apply`](Self::apply) first.
    pub fn start(&mut self, now: Instant) -> Vec<WebEvent> {
        let mut events = Vec::new();
        self.running = true;
        for (name, pool) in &mut self.pools {
            if let Ok(list) = pool.service.start(now) {
                events.extend(wrap(&service_name(name), list));
            }
        }
        if let Some(nginx) = &mut self.nginx {
            if let Ok(list) = nginx.start(now) {
                events.extend(wrap("nginx", list));
            }
        }
        events
    }

    /// Stops Nginx and the pools, and cancels a reload under way.
    pub fn stop(&mut self, now: Instant) -> Vec<WebEvent> {
        let mut events = Vec::new();
        self.running = false;
        self.cancel_reload();
        self.reload_again = false;
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
        self.tick_reload(now, &mut events);
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

    /// Runs `nginx -c <main> -p <dir> <args>` once, in a group of its own.
    fn run_nginx(&self, args: &[&str]) -> Result<(GroupId, ProcessId), String> {
        let main = self.main_conf.as_ref().ok_or("no configuration yet")?;
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
            .arg(main)
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

    fn fail_reload(&mut self, reason: String, now: Instant, events: &mut Vec<WebEvent>) {
        self.cancel_reload();
        events.push(WebEvent::ReloadFailed { reason });
        self.finish_reload_round(now, events, false);
    }

    /// After a reload, successful or not: stop pools nobody uses and run the one that was
    /// asked for in the meantime.
    fn finish_reload_round(&mut self, now: Instant, events: &mut Vec<WebEvent>, ok: bool) {
        if ok {
            self.retire_unused(now);
        }
        if std::mem::take(&mut self.reload_again) {
            self.request_reload(now);
            events.push(WebEvent::ReloadStarted);
        }
    }

    fn retire_unused(&mut self, now: Instant) {
        for version in std::mem::take(&mut self.unused) {
            if let Some(mut pool) = self.pools.remove(&version) {
                let _ = pool.service.stop(now);
                self.stopping.push(pool.service);
            }
        }
        let keep: BTreeSet<String> = self.pools.keys().cloned().collect();
        let stopping_ports: BTreeSet<String> = self
            .stopping
            .iter()
            .filter_map(|s| s.name().strip_prefix("php-").map(str::to_owned))
            .collect();
        self.ports
            .retain(&keep.union(&stopping_ports).cloned().collect());
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
                let waiting = self.pools.values().any(|p| {
                    matches!(
                        p.service.state(),
                        ServiceState::Starting | ServiceState::Stopped
                    )
                });
                if !self.running {
                    self.cancel_reload();
                } else if !waiting {
                    self.begin_step(Phase::Test, &["-t"], now, events);
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
                            self.begin_step(Phase::Reload, &["-s", "reload"], now, events);
                        } else {
                            self.reload = None;
                            events.push(WebEvent::Reloaded);
                            self.finish_reload_round(now, events, true);
                        }
                    }
                    _ => {
                        let log = self
                            .paths
                            .logs_for("nginx")
                            .map(|d| d.join("reload.log"))
                            .unwrap_or_default();
                        let what = if phase == Phase::Test {
                            "nginx rejected the new configuration"
                        } else {
                            "nginx could not be asked to reload"
                        };
                        self.fail_reload(format!("{what}; see {}", log.display()), now, events);
                    }
                }
            }
        }
    }

    fn begin_step(
        &mut self,
        phase: Phase,
        args: &[&str],
        now: Instant,
        events: &mut Vec<WebEvent>,
    ) {
        if phase == Phase::Test {
            events.push(WebEvent::ReloadStarted);
        }
        if let Some(reload) = &self.reload {
            if let Some((group, _)) = reload.helper {
                self.close_helper(group);
            }
        }
        match self.run_nginx(args) {
            Ok(helper) => {
                self.reload = Some(Reload {
                    phase,
                    since: now,
                    helper: Some(helper),
                });
            }
            Err(reason) => self.fail_reload(reason, now, events),
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
