//! What `crisold` does, without the pipe (F8.2).
//!
//! [`Daemon`] owns the running services (the [`WebStack`]) and answers every
//! [`crisol_protocol::Call`]: the ones about files through the [`Engine`], and the ones about
//! services itself. It has no I/O of its own and no thread: the binary feeds it requests and
//! calls [`tick`](Daemon::tick) every few hundred milliseconds, and tests do the same with
//! fakes and a made-up clock.
//!
//! Nothing here waits. `start`, `stop` and `restart` begin the change and answer at once; the
//! services move on with each tick, and [`settled`](Daemon::settled) says when nothing is in
//! transition any more, which is when the binary answers a client that asked to wait.
//!
//! The daemon also watches the user's files (`crisol.toml`, `sites.toml`, the parked folders and
//! the pins of the sites) and, when the stack is running, applies what changed: a new site is
//! served and a removed one is not, without anybody asking.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use crisol_protocol::{
    Call, Check, CheckStatus, DoneInfo, ErrorCode, Event, HelloInfo, Reply, RpcError, ServiceInfo,
    StatusInfo, PROTOCOL_VERSION,
};

use crate::config::Config;
use crate::dbservice::DbKind;
use crate::dbstack::{DbEvents, DbStack};
use crate::engine::{home_error, Engine};
use crate::manage::list_installed;
use crate::ports::PortStatus;
use crate::service::{FailureReason, ServiceState};
use crate::sites::SitesFile;
use crate::web::{plan_web, WebError, WebEvent, WebPlan, WebStack};

/// How often the user's files are looked at.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// A database that takes longer than this to be ready, with its data already made, is slow:
/// they start in a few seconds when nothing gets in the way.
const SLOW_START: Duration = Duration::from_secs(10);

/// The daemon's state.
#[derive(Debug)]
pub struct Daemon {
    engine: Engine,
    stack: Option<WebStack>,
    /// MySQL and MariaDB (F9.1): each one on its own, so one that cannot start harms no other.
    dbs: DbStack,
    /// The ports and suffix the stack was built with, to notice a change that needs a restart.
    built_with: Option<Built>,
    /// `start` was asked and `stop` was not.
    wanted: bool,
    /// `restart`: start again as soon as everything has stopped.
    restart_pending: bool,
    shutting_down: bool,
    fingerprint: Option<u64>,
    last_watch: Option<Instant>,
    /// When the ports of the running databases were last compared with `crisol.toml`.
    last_db_watch: Option<Instant>,
    /// The port changes already reported, so each is said once.
    reported_db_ports: Vec<String>,
    /// Facts without an action for the user, from the last time the web stack was applied
    /// (see [`Daemon::take_notes`]).
    notes: Vec<String>,
    /// The notes already handed out, so a reload does not repeat them.
    reported_notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Built {
    http: u16,
    https: u16,
    tld: String,
    php_base_port: u16,
    php_workers: u16,
}

impl Built {
    fn of(config: &Config) -> Self {
        Self {
            http: config.services.nginx.http_port,
            https: config.services.nginx.https_port,
            tld: config.sites.tld.clone(),
            php_base_port: config.php.base_port,
            php_workers: config.php.workers,
        }
    }
}

impl Daemon {
    /// A daemon with nothing running.
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        let dbs = DbStack::new(
            engine.paths().clone(),
            engine.deps().supervisor.clone(),
            engine.deps().probe.clone(),
            engine.deps().dirs.clone(),
        )
        .with_admin(engine.deps().db_admin.clone());
        Self {
            engine,
            stack: None,
            dbs,
            built_with: None,
            wanted: false,
            restart_pending: false,
            shutting_down: false,
            fingerprint: None,
            last_watch: None,
            last_db_watch: None,
            reported_db_ports: Vec::new(),
            notes: Vec::new(),
            reported_notes: Vec::new(),
        }
    }

    /// The notes that have not been handed out yet. They are for the daemon's log (W1 of the
    /// review of phase 9): they need no action, so `start` does not show them; `doctor` does.
    pub fn take_notes(&mut self) -> Vec<String> {
        let fresh: Vec<String> = std::mem::take(&mut self.notes)
            .into_iter()
            .filter(|n| !self.reported_notes.contains(n))
            .collect();
        self.reported_notes.extend(fresh.iter().cloned());
        fresh
    }

    /// The engine, for operations that do not need the services.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Whether `shutdown` was asked: the binary should leave once everything has stopped.
    #[must_use]
    pub fn shutdown_requested(&self) -> bool {
        self.shutting_down
    }

    /// Kills every service at once. The binary uses it when a `shutdown` did not settle in time.
    pub fn abort(&mut self) {
        self.wanted = false;
        self.restart_pending = false;
        if let Some(stack) = &mut self.stack {
            stack.abort();
        }
        self.dbs.abort();
    }

    /// Whether `call` can take long enough (a download, a scan of the system) to be run by a
    /// worker thread instead of the one that owns the daemon. They are the only ones the binary
    /// takes off the actor.
    #[must_use]
    pub fn is_long(call: &Call) -> bool {
        matches!(
            call,
            Call::Install { .. } | Call::Uninstall { .. } | Call::Doctor
        )
    }

    /// Whether nothing is in transition: every service is running, stopped or failed, no
    /// reload is under way and no restart is waiting. With `shutdown`, it is also true only
    /// once everything is down.
    #[must_use]
    pub fn settled(&self) -> bool {
        if self.restart_pending || !self.dbs.is_settled() {
            return false;
        }
        let stack = match &self.stack {
            None => true,
            Some(stack) if self.shutting_down => stack.is_stopped() && stack.is_settled(),
            Some(stack) => stack.is_settled(),
        };
        stack && (!self.shutting_down || self.dbs.is_stopped())
    }

    /// Whether any service ended up failed (the message says which).
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        self.stack
            .as_ref()?
            .services()
            .into_iter()
            .find_map(|(name, state)| match state {
                ServiceState::Failed(reason) => Some(format!("{name} failed: {reason}")),
                _ => None,
            })
    }

    /// Why the service a command named (or, with none, the web stack) failed, if it did. A
    /// database that failed is not the web stack's failure.
    #[must_use]
    pub fn failure_of(&self, service: Option<&str>) -> Option<String> {
        match service.and_then(DbKind::from_service) {
            Some(kind) => self.dbs.failure(kind),
            None => self.failure(),
        }
    }

    /// The databases that failed, as warnings for the answer to a plain `start`: they do not
    /// make it an error.
    #[must_use]
    pub fn database_warnings(&self) -> Vec<String> {
        DbKind::ALL
            .into_iter()
            .filter_map(|kind| self.dbs.failure(kind))
            .collect()
    }

    /// Answers a request. `events` receives what happens as a result (progress of a download,
    /// the first events of a start).
    pub fn handle(
        &mut self,
        call: &Call,
        now: Instant,
        events: &mut dyn FnMut(Event),
    ) -> Result<Reply, RpcError> {
        if let Some(answer) = self.engine.handle(call, events) {
            // A change to the user's files is applied to the running services at once.
            if answer.is_ok() && changes_sites(call) {
                self.refresh(now, events);
            }
            if matches!(call, Call::Doctor) {
                return answer.map(|reply| self.complete_doctor(reply));
            }
            return answer;
        }
        if let Some(answer) = self.database_call(call, now, events) {
            return answer;
        }
        match call {
            Call::Hello { protocol, .. } => self.hello(*protocol),
            Call::Status { .. } if !self.engine.paths().home().exists() => Err(RpcError::new(
                ErrorCode::NotFound,
                self.engine.no_home_message(),
            )),
            Call::Status { service } => self.status(service.as_deref()),
            Call::Start { service } => {
                self.require_whole_stack(service.as_deref())?;
                self.start_everything(now, events)
            }
            Call::Stop { service } => {
                self.require_whole_stack(service.as_deref())?;
                Ok(self.stop(now, events, false))
            }
            Call::Restart { service } => {
                self.require_whole_stack(service.as_deref())?;
                Ok(self.restart(now, events))
            }
            Call::Shutdown => {
                self.shutting_down = true;
                self.wanted = false;
                self.restart_pending = false;
                events(Event::ShuttingDown);
                let mut reply = self.stop(now, events, true);
                if let Reply::Done(info) = &mut reply {
                    info.warnings = self
                        .engine
                        .deps()
                        .system
                        .shutdown_warnings(self.engine.paths());
                }
                Ok(reply)
            }
            // Delivered by the binary, which owns the connection.
            Call::Subscribe => Ok(done("Subscribed to events", false)),
            _ => Err(RpcError::new(
                ErrorCode::Internal,
                format!("`{}` was not handled", call.method()),
            )),
        }
    }

    /// Advances the services and the watch. Call it every 150 to 500 ms.
    pub fn tick(&mut self, now: Instant) -> Vec<Event> {
        let mut out = Vec::new();
        if let Some(stack) = &mut self.stack {
            let events = stack.tick(now);
            out.extend(events.into_iter().filter_map(map_event));
        }
        if self.restart_pending && self.stack.as_ref().is_none_or(WebStack::is_stopped) {
            self.restart_pending = false;
            let mut push = |e| out.push(e);
            if let Err(error) = self.start(now, &mut push) {
                out.push(Event::ReloadFailed {
                    reason: error.message,
                });
            }
        }
        let databases = self.dbs.tick(now);
        emit_databases(&mut |e| out.push(e), databases);
        if self.wanted && self.watch_due(now) {
            self.refresh(now, &mut |e| out.push(e));
        }
        self.watch_database_ports(now, &mut |e| out.push(e));
        out
    }

    /// Says, once, that a running database is not on the port `crisol.toml` has now. Nothing is
    /// restarted.
    fn watch_database_ports(&mut self, now: Instant, events: &mut dyn FnMut(Event)) {
        if self.dbs.is_stopped() {
            self.reported_db_ports.clear();
            return;
        }
        let due = self
            .last_db_watch
            .is_none_or(|last| now.saturating_duration_since(last) >= WATCH_EVERY);
        if !due {
            return;
        }
        self.last_db_watch = Some(now);
        let changes = self.dbs.port_changes();
        for change in &changes {
            if !self.reported_db_ports.contains(change) {
                events(Event::ReloadFailed {
                    reason: change.clone(),
                });
            }
        }
        self.reported_db_ports = changes;
    }

    // ---- the calls ----------------------------------------------------------------------

    fn hello(&self, protocol: u32) -> Result<Reply, RpcError> {
        if protocol != PROTOCOL_VERSION {
            return Err(RpcError::new(
                ErrorCode::UnsupportedProtocol,
                format!(
                    "this daemon speaks protocol {PROTOCOL_VERSION} and the client {protocol}; \
                     use the same version of crisol and crisold"
                ),
            ));
        }
        Ok(Reply::Hello(HelloInfo {
            daemon: crate::VERSION.to_owned(),
            protocol: PROTOCOL_VERSION,
            home: self.engine.paths().home().display().to_string(),
        }))
    }

    fn status(&self, service: Option<&str>) -> Result<Reply, RpcError> {
        let mut services = self.service_list();
        if let Some(name) = service {
            services.retain(|s| s.name == name);
            if services.is_empty() {
                return Err(RpcError::new(
                    ErrorCode::NotFound,
                    format!("there is no service called `{name}`; `crisol status` lists them"),
                ));
            }
        }
        Ok(Reply::Status(StatusInfo {
            services,
            warnings: Vec::new(),
        }))
    }

    /// The services as the user sees them. Before the first start, `nginx` is listed as
    /// stopped if it is installed.
    fn service_list(&self) -> Vec<ServiceInfo> {
        let mut list: Vec<ServiceInfo> = self
            .stack
            .as_ref()
            .map(|stack| {
                stack
                    .services()
                    .into_iter()
                    .map(|(name, state)| service_info(name, &state))
                    .collect()
            })
            .unwrap_or_default();
        let installed = list_installed(self.engine.paths()).unwrap_or_default();
        if list.is_empty() && installed.iter().any(|i| i.service == "nginx") {
            list.push(ServiceInfo {
                name: "nginx".to_owned(),
                state: "stopped".to_owned(),
                detail: None,
            });
        }
        // The databases that were started show their state; the installed ones that were not
        // are stopped.
        let started = self.dbs.states();
        list.extend(
            started
                .iter()
                .map(|(name, state)| service_info(name.clone(), state)),
        );
        for kind in DbKind::ALL {
            let name = kind.service();
            if installed.iter().any(|i| i.service == name)
                && !started.iter().any(|(n, _)| n == name)
            {
                list.push(ServiceInfo {
                    name: name.to_owned(),
                    state: "stopped".to_owned(),
                    detail: None,
                });
            }
        }
        list
    }

    /// Nginx and the pools start and stop together for now; a service named on its own is
    /// either unknown or told to be handled with the rest. (There is no "web" alias: the whole
    /// stack is what a command without a service name means.)
    fn require_whole_stack(&self, service: Option<&str>) -> Result<(), RpcError> {
        let Some(name) = service else { return Ok(()) };
        if !self.service_list().iter().any(|s| s.name == name) {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                format!("there is no service called `{name}`; `crisol status` lists them"),
            ));
        }
        Err(RpcError::new(
            ErrorCode::Conflict,
            format!(
                "`{name}` is part of the web stack, which starts and stops as one; \
                 use the command without a service name"
            ),
        ))
    }

    /// A plain `start`: the web stack and the databases `crisol.toml` enables. A database that
    /// cannot start is a warning; so is a missing Nginx when there is a database to run.
    fn start_everything(
        &mut self,
        now: Instant,
        events: &mut dyn FnMut(Event),
    ) -> Result<Reply, RpcError> {
        let web = self.start(now, events);
        if self.shutting_down {
            return web;
        }
        let (started, mut warnings) = self.dbs.start_enabled(now);
        warnings.extend(self.dbs.take_notices());
        let any_database = !started.is_empty();
        emit_databases(events, started);
        match web {
            Ok(Reply::Done(mut info)) => {
                info.warnings.extend(warnings);
                Ok(Reply::Done(info))
            }
            Err(error) if error.code == ErrorCode::NotAvailable && any_database => {
                warnings.insert(
                    0,
                    format!("the web stack was not started: {}", error.message),
                );
                Ok(Reply::Done(DoneInfo {
                    message: "Starting the databases".to_owned(),
                    changed: true,
                    warnings,
                }))
            }
            other => other,
        }
    }

    /// `start`, `stop` and `restart` of a database named in the call; `None` for the rest.
    fn database_call(
        &mut self,
        call: &Call,
        now: Instant,
        events: &mut dyn FnMut(Event),
    ) -> Option<Result<Reply, RpcError>> {
        let kind = |service: &Option<String>| service.as_deref().and_then(DbKind::from_service);
        match call {
            Call::Start { service } => {
                kind(service).map(|kind| self.start_database(kind, now, events))
            }
            Call::Stop { service } => {
                kind(service).map(|kind| Ok(self.stop_database(kind, now, events)))
            }
            Call::ResetPassword { service } => {
                let Some(kind) = DbKind::from_service(service) else {
                    return Some(Err(RpcError::new(
                        ErrorCode::NotFound,
                        format!("no data service called `{service}`"),
                    )));
                };
                Some(
                    self.dbs
                        .reset_password(kind)
                        .map(|message| done(&message, true)),
                )
            }
            Call::Restart { service } => kind(service).map(|kind| {
                let started = self.dbs.restart(kind, now)?;
                emit_databases(events, started);
                Ok(done(&format!("Restarting {}", kind.service()), true))
            }),
            _ => None,
        }
    }

    fn start_database(
        &mut self,
        kind: DbKind,
        now: Instant,
        events: &mut dyn FnMut(Event),
    ) -> Result<Reply, RpcError> {
        if self.shutting_down {
            return Err(RpcError::new(
                ErrorCode::Conflict,
                "the daemon is shutting down",
            ));
        }
        let started = self.dbs.start(kind, now)?;
        if started.is_empty() {
            return Ok(done(
                &format!("{} is already running", kind.service()),
                false,
            ));
        }
        emit_databases(events, started);
        Ok(Reply::Done(DoneInfo {
            message: format!("Starting {}", kind.service()),
            changed: true,
            warnings: self.dbs.take_notices(),
        }))
    }

    fn stop_database(
        &mut self,
        kind: DbKind,
        now: Instant,
        events: &mut dyn FnMut(Event),
    ) -> Reply {
        let stopped = self.dbs.stop(Some(kind), now);
        if stopped.is_empty() {
            return done(&format!("{} was not running", kind.service()), false);
        }
        emit_databases(events, stopped);
        done(&format!("Stopping {}", kind.service()), true)
    }

    fn start(&mut self, now: Instant, events: &mut dyn FnMut(Event)) -> Result<Reply, RpcError> {
        if self.shutting_down {
            return Err(RpcError::new(
                ErrorCode::Conflict,
                "the daemon is shutting down",
            ));
        }
        if self.wanted && self.stack.as_ref().is_some_and(|s| !s.is_stopped()) {
            // A change to the user's files that the watch has not seen yet is applied now, so
            // that the answer is given only when the running Nginx serves what the files say
            // (V2 of the manual verification of phase 9).
            self.refresh(now, events);
            return Ok(done("The web stack is already running", false));
        }
        let (stack, config, plan) = self.build()?;
        self.built_with = Some(Built::of(&config));
        let mut stack = stack;
        let applied = match stack.apply(&plan, now) {
            Ok(applied) => applied,
            Err(error) => return Err(web_error(error)),
        };
        let started = stack.start(now);
        // `start` refuses (and says why) when the home is open.
        if let Some(reason) = started.iter().find_map(|e| match e {
            WebEvent::Blocked { reason } => Some(reason.clone()),
            _ => None,
        }) {
            return Err(RpcError::new(ErrorCode::HomeExposed, reason));
        }
        for event in started.into_iter().filter_map(map_event) {
            events(event);
        }
        self.stack = Some(stack);
        self.wanted = true;
        self.fingerprint = Some(self.watch_fingerprint());
        self.last_watch = Some(now);
        let mut reply = DoneInfo {
            message: "Starting the web stack".to_owned(),
            changed: true,
            warnings: plan.warnings,
        };
        reply.warnings.extend(applied.warnings);
        self.notes.extend(applied.notes);
        reply.warnings.extend(self.config_warnings());
        Ok(Reply::Done(reply))
    }

    fn stop(&mut self, now: Instant, events: &mut dyn FnMut(Event), shutdown: bool) -> Reply {
        self.wanted = false;
        self.restart_pending = false;
        let databases_running = !self.dbs.is_stopped();
        let stopped = self.dbs.stop(None, now);
        emit_databases(events, stopped);
        let Some(stack) = &mut self.stack else {
            return if databases_running {
                done("Stopping the databases", true)
            } else {
                done("Nothing was running", false)
            };
        };
        if stack.is_stopped() && !shutdown && !databases_running {
            return done("Nothing was running", false);
        }
        for event in stack.stop(now).into_iter().filter_map(map_event) {
            events(event);
        }
        done("Stopping the services", true)
    }

    fn restart(&mut self, now: Instant, events: &mut dyn FnMut(Event)) -> Reply {
        let running = self.stack.as_ref().is_some_and(|s| !s.is_stopped());
        if !running {
            return match self.start(now, events) {
                Ok(reply) => reply,
                Err(error) => {
                    events(Event::ReloadFailed {
                        reason: error.message.clone(),
                    });
                    done(&format!("Could not start: {}", error.message), false)
                }
            };
        }
        self.stop(now, events, false);
        self.restart_pending = true;
        done("Restarting the web stack", true)
    }

    // ---- doctor -------------------------------------------------------------------------

    /// Adds to the answer of `doctor` what only the daemon knows (F11.1): the services that
    /// failed and why, the ports of the enabled services it is not running that something else
    /// holds, and how long the databases took to be ready. They go right after `sites`. Nothing
    /// is added while there is no home.
    #[must_use]
    pub fn complete_doctor(&self, reply: Reply) -> Reply {
        match reply {
            Reply::Doctor { mut checks } if self.engine.paths().home().exists() => {
                let at = checks
                    .iter()
                    .position(|c| c.name == "sites")
                    .map_or(checks.len(), |i| i + 1);
                let mut mine = vec![self.check_services()];
                mine.extend(self.check_ports());
                mine.extend(self.check_startup_times());
                checks.splice(at..at, mine);
                Reply::Doctor { checks }
            }
            other => other,
        }
    }

    /// Every service the daemon has, web stack and databases, with its state.
    fn all_states(&self) -> Vec<(String, ServiceState)> {
        let mut states = self
            .stack
            .as_ref()
            .map(WebStack::services)
            .unwrap_or_default();
        states.extend(self.dbs.states());
        states
    }

    fn check_services(&self) -> Check {
        let name = "services".to_owned();
        let states = self.all_states();
        let system = &self.engine.deps().system;
        let mut failed = Vec::new();
        let mut first = None;
        for (service, state) in &states {
            let ServiceState::Failed(reason) = state else {
                continue;
            };
            let mut line = format!("{service} {reason}");
            if let Some(why) = exit_code(reason).and_then(|code| system.explain_exit(code)) {
                let _ = write!(line, " ({why})");
            }
            failed.push(line);
            first.get_or_insert_with(|| service.clone());
        }
        if let Some(first) = first {
            return Check {
                name,
                status: CheckStatus::Problem,
                detail: format!("failed: {}", failed.join("; ")),
                fix: Some(format!("crisol logs {first}")),
            };
        }
        let running: Vec<&str> = states
            .iter()
            .filter(|(_, state)| *state == ServiceState::Running)
            .map(|(service, _)| service.as_str())
            .collect();
        Check {
            name,
            status: CheckStatus::Ok,
            detail: if running.is_empty() {
                "nothing is running; `crisol start` starts the services".to_owned()
            } else {
                format!("running: {}", running.join(", "))
            },
            fix: None,
        }
    }

    /// The ports of the enabled services that are not running and that something else holds:
    /// they would fail at `crisol start`. The ports of the running ones are theirs.
    fn check_ports(&self) -> Option<Check> {
        let config = Config::load_from(self.engine.paths()).ok()?.config;
        let active: Vec<String> = self
            .all_states()
            .into_iter()
            .filter(|(_, state)| !matches!(state, ServiceState::Stopped | ServiceState::Failed(_)))
            .map(|(service, _)| service)
            .collect();
        let probe = &self.engine.deps().probe;
        let mut taken = Vec::new();
        let mut fix = None;
        let mut seen = BTreeSet::new();
        for (service, field, port) in config.enabled_service_ports() {
            if active.iter().any(|a| a == service) || !seen.insert(port) {
                continue;
            }
            let reason = match probe.probe(port) {
                Ok(PortStatus::Free) => continue,
                Ok(PortStatus::InUse(owners)) => {
                    FailureReason::PortBusy { port, owners }.to_string()
                }
                Ok(PortStatus::Reserved) => FailureReason::PortReserved(port).to_string(),
                Err(error) => format!("port {port} could not be checked: {error}"),
            };
            taken.push(format!("{service}: {reason}"));
            if fix.is_none() {
                if let Ok(Some(free)) = probe.suggest_free(port, 100) {
                    fix = Some(set_port_fix(field, free));
                }
            }
        }
        (!taken.is_empty()).then(|| Check {
            name: "ports".to_owned(),
            status: CheckStatus::Problem,
            detail: format!(
                "{}; Crisol never stops another program to take its port",
                taken.join("; ")
            ),
            fix,
        })
    }

    /// How long the databases took to be ready the last time they started with their data
    /// already made. A slow one is usually an antivirus reading every file of its data.
    fn check_startup_times(&self) -> Option<Check> {
        let times = self.dbs.startup_times();
        if times.is_empty() {
            return None;
        }
        let list = |only_slow: bool| {
            times
                .iter()
                .filter(|(_, took)| !only_slow || *took >= SLOW_START)
                .map(|(service, took)| format!("{service} {:.1} s", took.as_secs_f32()))
                .collect::<Vec<_>>()
        };
        let slow = list(true);
        let name = "database startup".to_owned();
        if slow.is_empty() {
            return Some(Check {
                name,
                status: CheckStatus::Ok,
                detail: format!("ready in {}", list(false).join(", ")),
                fix: None,
            });
        }
        let hint = self
            .engine
            .deps()
            .system
            .slow_start_hint(self.engine.paths());
        Some(Check {
            name,
            status: CheckStatus::Warning,
            detail: format!(
                "slow to be ready: {} (they usually take a few seconds); an antivirus that reads every file of {} is the usual cause",
                slow.join(", "),
                self.engine.paths().data_dir().display()
            ),
            fix: hint,
        })
    }

    // ---- building and refreshing the stack ----------------------------------------------

    fn config(&self) -> Result<Config, RpcError> {
        Config::load_from(self.engine.paths())
            .map(|loaded| loaded.config)
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string()))
    }

    fn config_warnings(&self) -> Vec<String> {
        Config::load_from(self.engine.paths())
            .map(|loaded| loaded.warnings.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    fn plan(&self, config: &Config) -> Result<WebPlan, RpcError> {
        let linked = SitesFile::load(self.engine.paths())
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string()))?;
        let deps = self.engine.deps();
        Ok(plan_web(
            self.engine.paths(),
            config,
            &linked,
            &deps.catalog,
            deps.owners.as_ref(),
            deps.env.as_ref(),
        ))
    }

    fn build(&self) -> Result<(WebStack, Config, WebPlan), RpcError> {
        let config = self.config()?;
        let nginx = list_installed(self.engine.paths())
            .map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?
            .into_iter()
            .rfind(|i| i.service == "nginx")
            .ok_or_else(|| {
                RpcError::new(
                    ErrorCode::NotAvailable,
                    "nginx is not installed, so there is nothing to start; run `crisol install nginx`",
                )
            })?;
        let plan = self.plan(&config)?;
        let deps = self.engine.deps();
        let stack = WebStack::new(
            self.engine.paths().clone(),
            &config,
            nginx.path,
            deps.supervisor.clone(),
            deps.probe.clone(),
            deps.dirs.clone(),
        )
        .with_root_certificates(deps.roots.clone())
        .with_config_probe(deps.config_probe.clone());
        Ok((stack, config, plan))
    }

    /// Applies what changed in the user's files to the running stack.
    fn refresh(&mut self, now: Instant, events: &mut dyn FnMut(Event)) {
        let Some(stack) = &self.stack else { return };
        if !self.wanted || stack.is_stopped() {
            return;
        }
        self.fingerprint = Some(self.watch_fingerprint());
        self.last_watch = Some(now);
        let config = match self.config() {
            Ok(config) => config,
            Err(error) => {
                events(Event::ReloadFailed {
                    reason: error.message,
                });
                return;
            }
        };
        if self
            .built_with
            .as_ref()
            .is_some_and(|b| *b != Built::of(&config))
        {
            events(Event::ReloadFailed {
                reason: "the ports, the PHP pool ports or the suffix in crisol.toml changed; run `crisol restart` to apply them"
                    .to_owned(),
            });
            return;
        }
        let plan = match self.plan(&config) {
            Ok(plan) => plan,
            Err(error) => {
                events(Event::ReloadFailed {
                    reason: error.message,
                });
                return;
            }
        };
        if let Some(stack) = &mut self.stack {
            // `[php]` may have changed: the `php.ini` is regenerated and the pools that need it
            // restart.
            stack.set_php_config(config.php.clone());
            match stack.apply(&plan, now) {
                Ok(applied) => {
                    self.notes.extend(applied.notes);
                    if applied.reload_requested {
                        events(Event::ReloadStarted);
                    }
                }
                Err(WebError::Home(error)) => events(Event::Blocked {
                    reason: format!("{} ({})", home_error(&error).message, home_fix(&error)),
                }),
                Err(error) => events(Event::ReloadFailed {
                    reason: error.to_string(),
                }),
            }
        }
    }

    // ---- watching the user's files --------------------------------------------------------

    /// Whether the user's files are due to be looked at and have changed since the last time.
    /// Looking is cheap, but not worth doing on every tick.
    fn watch_due(&mut self, now: Instant) -> bool {
        let due = self
            .last_watch
            .is_none_or(|last| now.saturating_duration_since(last) >= WATCH_EVERY);
        if !due {
            return false;
        }
        self.last_watch = Some(now);
        Some(self.watch_fingerprint()) != self.fingerprint
    }

    /// A number that changes when a file that decides what is served changes: the two user
    /// files, the names inside the parked folders, and the version pins of every site.
    fn watch_fingerprint(&self) -> u64 {
        let paths = self.engine.paths();
        let mut hasher = DefaultHasher::new();
        let stamp = |path: &std::path::Path, hasher: &mut DefaultHasher| match fs::metadata(path) {
            Ok(meta) => (meta.len(), meta.modified().ok()).hash(hasher),
            Err(_) => 0_u8.hash(hasher),
        };
        stamp(&paths.config_file(), &mut hasher);
        stamp(&paths.sites_file(), &mut hasher);
        stamp(&paths.ca_cert(), &mut hasher);
        if let Ok(loaded) = Config::load_from(paths) {
            let linked = SitesFile::load(paths).unwrap_or_default();
            let list = crate::sites::discover(paths, &loaded.config, &linked);
            for site in &list.sites {
                (&site.name, &site.root, site.secure).hash(&mut hasher);
                for pin in [".crisol.toml", ".php-version", ".node-version"] {
                    stamp(&site.root.join(pin), &mut hasher);
                }
            }
        }
        hasher.finish()
    }
}

/// The exit code a failure carries, if any.
fn exit_code(reason: &FailureReason) -> Option<i32> {
    match reason {
        FailureReason::ExitedDuringStartup { code } | FailureReason::Crashed { code } => *code,
        _ => None,
    }
}

/// How to give a service another port: `services.mysql.port` is `port` under
/// `[services.mysql]`.
fn set_port_fix(field: &str, free: u16) -> String {
    match field.rsplit_once('.') {
        Some((table, key)) => {
            format!("in crisol.toml, set `{key} = {free}` under `[{table}]` (that port is free)")
        }
        None => format!("in crisol.toml, set `{field} = {free}` (that port is free)"),
    }
}

fn emit_databases(events: &mut dyn FnMut(Event), list: DbEvents) {
    for (name, event) in list {
        if let Some(mapped) = map_event(WebEvent::Service { name, event }) {
            events(mapped);
        }
    }
}

fn changes_sites(call: &Call) -> bool {
    matches!(
        call,
        Call::Park { .. }
            | Call::Unpark { .. }
            | Call::Link { .. }
            | Call::Unlink { .. }
            | Call::Secure { .. }
            | Call::Unsecure { .. }
            | Call::Use { .. }
            // Mailpit's name (`mail.<tld>`) is served by Nginx only while it is enabled.
            | Call::Enable { .. }
            | Call::Disable { .. }
    )
}

fn done(message: &str, changed: bool) -> Reply {
    Reply::Done(DoneInfo {
        message: message.to_owned(),
        changed,
        warnings: Vec::new(),
    })
}

fn service_info(name: String, state: &ServiceState) -> ServiceInfo {
    let (text, detail) = state_text(state);
    ServiceInfo {
        name,
        state: text.to_owned(),
        detail,
    }
}

fn state_text(state: &ServiceState) -> (&'static str, Option<String>) {
    match state {
        ServiceState::Stopped => ("stopped", None),
        ServiceState::Starting => ("starting", None),
        ServiceState::Running => ("running", None),
        ServiceState::Stopping => ("stopping", None),
        ServiceState::Failed(reason) => ("failed", Some(reason.to_string())),
    }
}

fn map_event(event: WebEvent) -> Option<Event> {
    Some(match event {
        WebEvent::Service { name, event } => match event {
            crate::service::ServiceEvent::StateChanged { to, .. } => {
                let (state, detail) = state_text(&to);
                Event::ServiceState {
                    service: name,
                    state: state.to_owned(),
                    detail,
                }
            }
            crate::service::ServiceEvent::StopPath { ordered, took } => Event::ServiceState {
                service: name,
                state: "stopping".to_owned(),
                detail: Some(if ordered {
                    format!("left in order after {:.1} s", took.as_secs_f32())
                } else {
                    format!(
                        "did not leave in order; its processes were killed after {:.1} s",
                        took.as_secs_f32()
                    )
                }),
            },
            _ => return None,
        },
        WebEvent::ReloadStarted => Event::ReloadStarted,
        WebEvent::Reloaded => Event::Reloaded,
        WebEvent::ReloadFailed { reason } => Event::ReloadFailed { reason },
        WebEvent::Blocked { reason } => Event::Blocked { reason },
    })
}

fn home_fix(error: &crate::paths::HomeError) -> String {
    home_error(error).fix.unwrap_or_default()
}

fn web_error(error: WebError) -> RpcError {
    match error {
        WebError::Home(home) => home_error(&home),
        other => RpcError::new(ErrorCode::Io, other.to_string()),
    }
}

#[cfg(test)]
mod tests;
