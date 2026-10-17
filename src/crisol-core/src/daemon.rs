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
use std::fs;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use crisol_protocol::{
    Call, DoneInfo, ErrorCode, Event, HelloInfo, Reply, RpcError, ServiceInfo, StatusInfo,
    PROTOCOL_VERSION,
};

use crate::config::Config;
use crate::engine::{home_error, Engine};
use crate::manage::list_installed;
use crate::service::ServiceState;
use crate::sites::SitesFile;
use crate::web::{plan_web, WebError, WebEvent, WebPlan, WebStack};

/// How often the user's files are looked at.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// The daemon's state.
#[derive(Debug)]
pub struct Daemon {
    engine: Engine,
    stack: Option<WebStack>,
    /// The ports and suffix the stack was built with, to notice a change that needs a restart.
    built_with: Option<Built>,
    /// `start` was asked and `stop` was not.
    wanted: bool,
    /// `restart`: start again as soon as everything has stopped.
    restart_pending: bool,
    shutting_down: bool,
    fingerprint: Option<u64>,
    last_watch: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Built {
    http: u16,
    https: u16,
    tld: String,
}

impl Built {
    fn of(config: &Config) -> Self {
        Self {
            http: config.services.nginx.http_port,
            https: config.services.nginx.https_port,
            tld: config.sites.tld.clone(),
        }
    }
}

impl Daemon {
    /// A daemon with nothing running.
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            stack: None,
            built_with: None,
            wanted: false,
            restart_pending: false,
            shutting_down: false,
            fingerprint: None,
            last_watch: None,
        }
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
        if self.restart_pending {
            return false;
        }
        match &self.stack {
            None => true,
            Some(stack) if self.shutting_down => stack.is_stopped() && stack.is_settled(),
            Some(stack) => stack.is_settled(),
        }
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
            return answer;
        }
        match call {
            Call::Hello { protocol, .. } => self.hello(*protocol),
            Call::Status { service } => self.status(service.as_deref()),
            Call::Start { service } => {
                self.require_whole_stack(service.as_deref())?;
                self.start(now, events)
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
        if self.wanted && self.watch_due(now) {
            self.refresh(now, &mut |e| out.push(e));
        }
        out
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
        let from_stack: Vec<ServiceInfo> = self
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
        if !from_stack.is_empty() {
            return from_stack;
        }
        let nginx_installed = list_installed(self.engine.paths())
            .is_ok_and(|list| list.iter().any(|i| i.service == "nginx"));
        if nginx_installed {
            vec![ServiceInfo {
                name: "nginx".to_owned(),
                state: "stopped".to_owned(),
                detail: None,
            }]
        } else {
            Vec::new()
        }
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

    fn start(&mut self, now: Instant, events: &mut dyn FnMut(Event)) -> Result<Reply, RpcError> {
        if self.shutting_down {
            return Err(RpcError::new(
                ErrorCode::Conflict,
                "the daemon is shutting down",
            ));
        }
        if self.wanted && self.stack.as_ref().is_some_and(|s| !s.is_stopped()) {
            return Ok(done("The web stack is already running", false));
        }
        let (stack, config, plan) = self.build()?;
        self.built_with = Some(Built::of(&config));
        let mut stack = stack;
        if let Err(error) = stack.apply(&plan, now) {
            return Err(web_error(error));
        }
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
        reply.warnings.extend(self.config_warnings());
        Ok(Reply::Done(reply))
    }

    fn stop(&mut self, now: Instant, events: &mut dyn FnMut(Event), shutdown: bool) -> Reply {
        self.wanted = false;
        self.restart_pending = false;
        let Some(stack) = &mut self.stack else {
            return done("Nothing was running", false);
        };
        if stack.is_stopped() && !shutdown {
            return done("Nothing was running", false);
        }
        for event in stack.stop(now).into_iter().filter_map(map_event) {
            events(event);
        }
        done("Stopping the web stack", true)
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
        );
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
                reason: "the ports or the suffix in crisol.toml changed; run `crisol restart` to apply them"
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
            match stack.apply(&plan, now) {
                Ok(applied) if applied.reload_requested => events(Event::ReloadStarted),
                Ok(_) => {}
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
