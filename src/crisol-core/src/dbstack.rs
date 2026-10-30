//! The databases the daemon runs (F9.1): which are up, which may start, and what happens when
//! one cannot.
//!
//! Each database is a [`DbService`] of its own, so one that cannot start (a busy port, a failed
//! initialization, a missing install) is a `Failed` entry in `status` with its reason and
//! nothing more: the web stack and the other databases are unaffected. MySQL and MariaDB are
//! alternatives (ADR-0007): they share port 3306 by default, and a second one is refused with a
//! message that says who has the port. If the user gave them different ports they can run
//! together, since nothing then collides.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crisol_protocol::{ErrorCode, RpcError};

use crate::config::Config;
use crate::dbservice::{plan, Auth, DbAdmin, DbError, DbKind, DbService, NoAdmin};
use crate::engine::home_error;
use crate::paths::{require_private_home, CrisolPaths, PrivateDirs};
use crate::ports::PortProbe;
use crate::process::ProcessSupervisor;
use crate::secrets::{self, SecretsError};
use crate::service::{ServiceEvent, ServiceState};

/// Events of the databases, with the name of the service each came from.
pub type DbEvents = Vec<(String, ServiceEvent)>;

/// The databases of one daemon.
pub struct DbStack {
    paths: CrisolPaths,
    supervisor: Arc<dyn ProcessSupervisor>,
    ports: Arc<dyn PortProbe>,
    dirs: Arc<dyn PrivateDirs>,
    admin: Arc<dyn DbAdmin>,
    services: BTreeMap<DbKind, DbService>,
    /// Restarts waiting for the old process to be gone.
    restarting: BTreeSet<DbKind>,
    /// Things to tell whoever asked for the start (a new series that begins empty).
    notices: Vec<String>,
    /// When each database that already had its data was launched, until it is ready.
    launched: BTreeMap<DbKind, Instant>,
    /// How long the last such start took to be ready, for `doctor` (F11.1).
    startup_times: BTreeMap<DbKind, Duration>,
}

impl std::fmt::Debug for DbStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbStack")
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn active(state: &ServiceState) -> bool {
    !matches!(state, ServiceState::Stopped | ServiceState::Failed(_))
}

fn tagged(name: &str, events: Vec<ServiceEvent>) -> DbEvents {
    events
        .into_iter()
        .map(|event| (name.to_owned(), event))
        .collect()
}

impl DbStack {
    /// A stack with nothing running.
    #[must_use]
    pub fn new(
        paths: CrisolPaths,
        supervisor: Arc<dyn ProcessSupervisor>,
        ports: Arc<dyn PortProbe>,
        dirs: Arc<dyn PrivateDirs>,
    ) -> Self {
        Self {
            paths,
            supervisor,
            ports,
            dirs,
            admin: Arc::new(NoAdmin),
            services: BTreeMap::new(),
            restarting: BTreeSet::new(),
            notices: Vec::new(),
            launched: BTreeMap::new(),
            startup_times: BTreeMap::new(),
        }
    }

    /// How passwords are changed on running servers (`reset-password`).
    #[must_use]
    pub fn with_admin(mut self, admin: Arc<dyn DbAdmin>) -> Self {
        self.admin = admin;
        self
    }

    /// Gives `kind` a new password without losing its data. A running server is asked to change
    /// it; one that is stopped and only reads its password when it starts (Redis, Garnet,
    /// Meilisearch) gets the new one at its next start. The others need the server running, to
    /// be asked. Returns what to tell the user.
    pub fn reset_password(&mut self, kind: DbKind) -> Result<String, RpcError> {
        require_private_home(&self.paths, self.dirs.as_ref()).map_err(|e| home_error(&e))?;
        let name = kind.service();
        if kind.auth() == Auth::None {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                format!("{name} has no password to reset"),
            ));
        }
        secrets::ensure_dir(&self.paths, self.dirs.as_ref())
            .map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?;
        if let Some(service) = self.services.get_mut(&kind) {
            if *service.state() == ServiceState::Running {
                let admin = Arc::clone(&self.admin);
                return service
                    .reset_password(admin.as_ref())
                    .map_err(|why| RpcError::new(ErrorCode::Conflict, why));
            }
            if active(service.state()) {
                return Err(RpcError::new(
                    ErrorCode::Conflict,
                    format!("{name} is starting or stopping; try again in a moment"),
                ));
            }
        }
        let config = self.load_config()?;
        let plan = plan(&self.paths, &config, kind).map_err(|e| match e {
            DbError::NotInstalled(_) => RpcError::new(ErrorCode::NotAvailable, e.to_string()),
            other => RpcError::new(ErrorCode::Io, other.to_string()),
        })?;
        if kind.auth() == Auth::Applied {
            let new = secrets::generate_password()
                .map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?;
            let file = plan.secret_path("");
            secrets::write_file(&file, new.expose())
                .map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?;
            return Ok(format!(
                "{name}: new password kept in {}; it takes effect when {name} starts",
                file.display()
            ));
        }
        Err(RpcError::new(
            ErrorCode::NotAvailable,
            format!(
                "{name} is not running, and its server has to be asked to change the password: start it with `crisol start {name}` and run this again"
            ),
        ))
    }

    /// What the starts since the last call have to say (and forgets it).
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    fn load_config(&self) -> Result<Config, RpcError> {
        Config::load_from(&self.paths)
            .map(|loaded| loaded.config)
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string()))
    }

    /// Starts one database. An error here is the answer to whoever asked for exactly this one.
    pub fn start(&mut self, kind: DbKind, now: Instant) -> Result<DbEvents, RpcError> {
        let config = self.load_config()?;
        // Nothing is run from a home that other accounts can write in (ADR-0008).
        require_private_home(&self.paths, self.dirs.as_ref()).map_err(|e| home_error(&e))?;
        if self.services.get(&kind).is_some_and(|s| active(s.state())) {
            return Ok(Vec::new());
        }
        self.require_free_port(kind, &config)?;
        let plan = plan(&self.paths, &config, kind).map_err(|e| match e {
            DbError::NotInstalled(_) => RpcError::new(ErrorCode::NotAvailable, e.to_string()),
            other => RpcError::new(ErrorCode::Io, other.to_string()),
        })?;
        // The passwords live in `secrets/`, a private folder (ADR-0006, ADR-0008): if other
        // accounts can write in it, none is read from it or put in it.
        if kind.auth() != Auth::None {
            secrets::ensure_dir(&self.paths, self.dirs.as_ref()).map_err(|e| match e {
                SecretsError::Exposed(path) => RpcError::new(
                    ErrorCode::HomeExposed,
                    format!(
                        "`{}` can be modified by other accounts of this computer, so the passwords in it cannot be trusted",
                        path.display()
                    ),
                )
                .with_fix(
                    self.dirs
                        .fix_command(&path)
                        .unwrap_or_else(|| format!("remove the write access of other accounts on `{}`", path.display())),
                ),
                other => RpcError::new(ErrorCode::Io, other.to_string()),
            })?;
        }
        self.notices
            .extend(crate::dbservice::new_series_notice(&self.paths, &plan));
        // A first start also makes the data, which says nothing about how fast the server is.
        let measured = plan.data.is_dir();
        let mut service = DbService::new(plan, self.supervisor.clone(), self.ports.clone());
        let events = tagged(kind.service(), service.start(now));
        self.services.insert(kind, service);
        if measured {
            self.launched.insert(kind, now);
        }
        Ok(events)
    }

    /// Two databases on the same port cannot both run.
    fn require_free_port(&self, kind: DbKind, config: &Config) -> Result<(), RpcError> {
        for other in DbKind::ALL.into_iter().filter(|o| *o != kind) {
            let running = self.services.get(&other).is_some_and(|s| active(s.state()));
            if running && other.port(config) == kind.port(config) {
                return Err(RpcError::new(
                    ErrorCode::Conflict,
                    format!(
                        "{} is running on port {}, which {} would use too (they are \
                         alternatives, only one at a time). Stop it first with `crisol stop {}`, \
                         or give one of them another port in crisol.toml",
                        other.service(),
                        other.port(config),
                        kind.service(),
                        other.service()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Starts the databases `crisol.toml` enables. What cannot start does not stop the rest: it
    /// is a warning (and the service shows as failed in `status`).
    pub fn start_enabled(&mut self, now: Instant) -> (DbEvents, Vec<String>) {
        let mut events = Vec::new();
        let mut warnings = Vec::new();
        let Ok(config) = self.load_config() else {
            return (events, warnings);
        };
        for kind in DbKind::ALL.into_iter().filter(|k| k.enabled(&config)) {
            match self.start(kind, now) {
                Ok(started) => events.extend(started),
                Err(error) => warnings.push(format!(
                    "{} was not started: {}",
                    kind.service(),
                    error.message
                )),
            }
        }
        (events, warnings)
    }

    /// Stops one database, or all of them.
    pub fn stop(&mut self, kind: Option<DbKind>, now: Instant) -> DbEvents {
        let mut events = Vec::new();
        for (k, service) in &mut self.services {
            if kind.is_none_or(|wanted| wanted == *k) {
                self.restarting.remove(k);
                events.extend(tagged(k.service(), service.stop(now)));
            }
        }
        events
    }

    /// Stops one database and starts it again when it is down.
    pub fn restart(&mut self, kind: DbKind, now: Instant) -> Result<DbEvents, RpcError> {
        if self.services.get(&kind).is_some_and(|s| active(s.state())) {
            let events = self.stop(Some(kind), now);
            self.restarting.insert(kind);
            Ok(events)
        } else {
            self.start(kind, now)
        }
    }

    /// Advances every database.
    pub fn tick(&mut self, now: Instant) -> DbEvents {
        let mut events = Vec::new();
        for (kind, service) in &mut self.services {
            let ticked = service.tick(now);
            for event in &ticked {
                if let ServiceEvent::StateChanged { to, .. } = event {
                    match to {
                        ServiceState::Starting => {}
                        ServiceState::Running => {
                            if let Some(at) = self.launched.remove(kind) {
                                self.startup_times
                                    .insert(*kind, now.saturating_duration_since(at));
                            }
                        }
                        _ => {
                            self.launched.remove(kind);
                        }
                    }
                }
            }
            events.extend(tagged(kind.service(), ticked));
        }
        let ready: Vec<DbKind> = self
            .restarting
            .iter()
            .copied()
            .filter(|k| {
                self.services.get(k).is_none_or(|s| {
                    matches!(s.state(), ServiceState::Stopped | ServiceState::Failed(_))
                })
            })
            .collect();
        for kind in ready {
            self.restarting.remove(&kind);
            // A restart that cannot start shows up as not running; `status` tells why.
            if let Ok(started) = self.start(kind, now) {
                events.extend(started);
            }
        }
        events
    }

    /// The databases running on a port other than the one `crisol.toml` says now, as sentences
    /// that name the command. A running database is not restarted by itself: that would cut the
    /// connections of the applications using it.
    #[must_use]
    pub fn port_changes(&self) -> Vec<String> {
        let Ok(config) = self.load_config() else {
            return Vec::new();
        };
        self.services
            .iter()
            .filter(|(_, service)| active(service.state()))
            .filter_map(|(kind, service)| {
                let plan = service.plan();
                let running: Vec<u16> = std::iter::once(plan.port).chain(plan.extra_port).collect();
                let wanted = kind.configured_ports(&config);
                (running != wanted).then(|| {
                    let list = |ports: &[u16]| {
                        ports
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" and ")
                    };
                    format!(
                        "`{name}` is still on port {now}; `crisol restart {name}` to use {next}",
                        name = kind.service(),
                        now = list(&running),
                        next = list(&wanted)
                    )
                })
            })
            .collect()
    }

    /// Every database that has been started, with its state.
    #[must_use]
    pub fn states(&self) -> Vec<(String, ServiceState)> {
        self.services
            .iter()
            .map(|(kind, service)| (kind.service().to_owned(), service.state().clone()))
            .collect()
    }

    /// How long the databases took to be ready the last time they started with their data
    /// already made (a first start, which makes it, is not measured).
    #[must_use]
    pub fn startup_times(&self) -> Vec<(String, Duration)> {
        self.startup_times
            .iter()
            .map(|(kind, took)| (kind.service().to_owned(), *took))
            .collect()
    }

    /// Whether nothing is starting or stopping.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.restarting.is_empty() && self.services.values().all(DbService::is_settled)
    }

    /// Whether none is running (a failed one is not).
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.restarting.is_empty()
            && self
                .services
                .values()
                .all(|s| matches!(s.state(), ServiceState::Stopped | ServiceState::Failed(_)))
    }

    /// Why `kind` failed, if it did.
    #[must_use]
    pub fn failure(&self, kind: DbKind) -> Option<String> {
        match self.services.get(&kind)?.state() {
            ServiceState::Failed(reason) => Some(format!("{} failed: {reason}", kind.service())),
            _ => None,
        }
    }

    /// Kills everything now.
    pub fn abort(&mut self) {
        self.restarting.clear();
        for service in self.services.values_mut() {
            service.abort();
        }
    }
}

#[cfg(test)]
mod tests;
