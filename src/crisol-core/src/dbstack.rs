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
use std::time::Instant;

use crisol_protocol::{ErrorCode, RpcError};

use crate::config::Config;
use crate::dbservice::{plan, Auth, DbError, DbKind, DbService};
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
    services: BTreeMap<DbKind, DbService>,
    /// Restarts waiting for the old process to be gone.
    restarting: BTreeSet<DbKind>,
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
            services: BTreeMap::new(),
            restarting: BTreeSet::new(),
        }
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
        let mut service = DbService::new(plan, self.supervisor.clone(), self.ports.clone());
        let events = tagged(kind.service(), service.start(now));
        self.services.insert(kind, service);
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
            events.extend(tagged(kind.service(), service.tick(now)));
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

    /// Every database that has been started, with its state.
    #[must_use]
    pub fn states(&self) -> Vec<(String, ServiceState)> {
        self.services
            .iter()
            .map(|(kind, service)| (kind.service().to_owned(), service.state().clone()))
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
