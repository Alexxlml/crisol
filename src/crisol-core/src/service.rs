//! Service lifecycle: start, health, watchdog and stop.
//!
//! ```text
//!            start                healthy
//!  Stopped ─────────▶ Starting ─────────────▶ Running
//!     ▲                  │  ▲                    │
//!     │ gone       failed│  │ crash, restart     │ crash, gave up
//!     │                  ▼  └────────────────────┤
//!  Stopping ◀──── stop ── any active state       ▼
//!                                              Failed(reason)
//! ```
//!
//! [`Service`] is a state machine with no I/O of its own and no clock: the caller (the
//! daemon) calls [`Service::tick`] every few hundred milliseconds and passes the current
//! [`Instant`]. Processes go through [`ProcessSupervisor`] and ports through [`PortProbe`],
//! so tests drive the whole thing with the fakes and a made-up clock, without sleeping.
//!
//! Policy:
//!
//! - **A busy port is never freed by force.** Before starting, every declared port is
//!   checked; if one is taken or reserved the service fails with the owner's pid and name.
//! - **Healthy means ready**, not merely started: the ports listen, or a probe command exits
//!   with 0. A process that dies before that is a startup failure and is **not** retried
//!   (it is almost always a configuration error); the watchdog only covers a service that
//!   was `Running`.
//! - **The watchdog** restarts a crashed service after a short delay, and gives up after too
//!   many crashes in a time window.
//! - **Stopping is graceful first**: an optional command (e.g. `nginx -s quit`) gets a grace
//!   period, then the whole group is killed. A group that survives the kill is reported as
//!   a failure; it is never ignored.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ports::{PortOwner, PortProbe, PortStatus};
use crate::process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
    SupervisorError,
};

/// Timing and restart behaviour of one service. Every field can be overridden per service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecyclePolicy {
    /// Minimum time between two health checks.
    pub probe_interval: Duration,
    /// How long a service may take to become healthy before it is killed.
    pub startup_timeout: Duration,
    /// How long a graceful stop may take before the group is killed.
    pub stop_grace: Duration,
    /// How long to wait for the group to disappear after killing it.
    pub kill_wait: Duration,
    /// Restart a service that crashes while running.
    pub auto_restart: bool,
    /// Pause between a crash and the restart.
    pub restart_delay: Duration,
    /// Give up when this many crashes happen within `crash_window`.
    pub max_crashes: u32,
    /// Window in which crashes are counted.
    pub crash_window: Duration,
}

impl Default for LifecyclePolicy {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_millis(500),
            startup_timeout: Duration::from_secs(20),
            stop_grace: Duration::from_millis(4500),
            kill_wait: Duration::from_millis(3750),
            auto_restart: true,
            restart_delay: Duration::from_millis(1500),
            max_crashes: 3,
            crash_window: Duration::from_secs(60),
        }
    }
}

/// A command whose exit code tells whether the service is healthy
/// (`pg_isready`, `redis-cli ping`, `mysqladmin ping`...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthCommand {
    /// What to run. Healthy means it exits with code 0.
    pub spec: ProcessSpec,
    /// A run that takes longer than this is killed and counts as unhealthy.
    pub timeout: Duration,
}

/// How to tell that a service is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthCheck {
    /// Ready as soon as its processes are alive.
    None,
    /// Ready when every one of these ports has a listener.
    Ports(Vec<u16>),
    /// Ready when the command succeeds.
    Command(HealthCommand),
}

/// Everything needed to run one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Name for logs and messages (`"nginx"`).
    pub name: String,
    /// Processes started together in one group (a PHP pool has several).
    pub processes: Vec<ProcessSpec>,
    /// Ports that must be free before starting.
    pub ports: Vec<u16>,
    /// Readiness check.
    pub health: HealthCheck,
    /// Command that asks the service to stop by itself; `None` kills it right away.
    pub graceful_stop: Option<ProcessSpec>,
    /// Timing and restart behaviour.
    pub policy: LifecyclePolicy,
}

impl ServiceSpec {
    /// A service with no processes yet, default policy and no health check.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            processes: Vec::new(),
            ports: Vec::new(),
            health: HealthCheck::None,
            graceful_stop: None,
            policy: LifecyclePolicy::default(),
        }
    }

    /// Adds a process to start.
    #[must_use]
    pub fn process(mut self, process: ProcessSpec) -> Self {
        self.processes.push(process);
        self
    }

    /// Adds a port that must be free to start.
    #[must_use]
    pub fn port(mut self, port: u16) -> Self {
        self.ports.push(port);
        self
    }

    /// Sets the readiness check.
    #[must_use]
    pub fn health(mut self, health: HealthCheck) -> Self {
        self.health = health;
        self
    }

    /// Sets the command that asks the service to stop gracefully.
    #[must_use]
    pub fn graceful_stop(mut self, command: ProcessSpec) -> Self {
        self.graceful_stop = Some(command);
        self
    }

    /// Replaces the policy.
    #[must_use]
    pub fn policy(mut self, policy: LifecyclePolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// Why a service is in the `Failed` state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureReason {
    /// A port it needs is taken. Nothing was started and nobody was killed.
    PortBusy {
        /// The contested port.
        port: u16,
        /// Who listens on it, as far as the system says.
        owners: Vec<PortOwner>,
    },
    /// A port it needs belongs to the operating system.
    PortReserved(u16),
    /// A process could not be started. Whatever had already started was stopped.
    SpawnFailed {
        /// Program that failed.
        program: String,
        /// What the system said.
        reason: String,
    },
    /// Every process exited before the service became healthy.
    ExitedDuringStartup {
        /// Exit code of the first one that exited by itself, if any.
        code: Option<i32>,
    },
    /// The service did not become healthy in time and was killed.
    StartupTimeout {
        /// How long it was given.
        waited: Duration,
    },
    /// It crashed while running and automatic restart is off.
    Crashed {
        /// Exit code of the first process that exited by itself, if any.
        code: Option<i32>,
    },
    /// It kept crashing and the watchdog gave up.
    TooManyCrashes {
        /// Crashes inside the window.
        crashes: u32,
        /// The window.
        window: Duration,
    },
    /// The processes survived a kill.
    CouldNotStop {
        /// Processes still alive.
        remaining: usize,
    },
    /// The supervisor or the port probe reported an error.
    Internal(String),
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortBusy { port, owners } if owners.is_empty() => {
                write!(f, "port {port} is already in use")
            }
            Self::PortBusy { port, owners } => {
                let who: Vec<String> = owners
                    .iter()
                    .map(|o| format!("{} (pid {})", o.name.as_deref().unwrap_or("unknown"), o.pid))
                    .collect();
                write!(f, "port {port} is already in use by {}", who.join(", "))
            }
            Self::PortReserved(port) => {
                write!(f, "port {port} is reserved by the operating system")
            }
            Self::SpawnFailed { program, reason } => {
                write!(f, "cannot start `{program}`: {reason}")
            }
            Self::ExitedDuringStartup { code: Some(code) } => {
                write!(f, "exited during startup with code {code}")
            }
            Self::ExitedDuringStartup { code: None } => write!(f, "exited during startup"),
            Self::StartupTimeout { waited } => {
                write!(f, "not ready after {} s", waited.as_secs_f32())
            }
            Self::Crashed { code: Some(code) } => write!(f, "crashed with code {code}"),
            Self::Crashed { code: None } => write!(f, "crashed"),
            Self::TooManyCrashes { crashes, window } => {
                write!(f, "crashed {crashes} times within {} s", window.as_secs())
            }
            Self::CouldNotStop { remaining } => {
                write!(f, "could not stop: {remaining} process(es) still alive")
            }
            Self::Internal(message) => f.write_str(message),
        }
    }
}

/// Where a service is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// Not running.
    Stopped,
    /// Launched (or about to be relaunched) and not healthy yet.
    Starting,
    /// Healthy.
    Running,
    /// Being stopped.
    Stopping,
    /// Could not start, kept crashing or could not be stopped.
    Failed(FailureReason),
}

/// Something that happened, for the daemon to log or broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceEvent {
    /// The state changed.
    StateChanged {
        /// Previous state.
        from: ServiceState,
        /// New state.
        to: ServiceState,
    },
    /// The watchdog will restart a crashed service.
    Restarting {
        /// Crashes counted in the window, this one included.
        crashes: u32,
        /// Wait before the restart.
        delay: Duration,
    },
}

/// Why a command was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    /// `start` was called on a service that is already starting, running or stopping.
    #[error("the service is already {0}")]
    AlreadyActive(&'static str),
}

#[derive(Debug)]
struct Run {
    group: GroupId,
    processes: Vec<ProcessId>,
}

#[derive(Debug)]
struct Probe {
    group: GroupId,
    process: ProcessId,
    started: Instant,
}

#[derive(Debug)]
struct Stopping {
    requested: Instant,
    helper: Option<GroupId>,
    killed: Option<Instant>,
}

/// One service and its lifecycle.
pub struct Service {
    spec: ServiceSpec,
    supervisor: Arc<dyn ProcessSupervisor>,
    ports: Arc<dyn PortProbe>,
    state: ServiceState,
    run: Option<Run>,
    started: Option<Instant>,
    next_probe: Option<Instant>,
    probe: Option<Probe>,
    restart_at: Option<Instant>,
    /// The current `Starting` is a restart by the watchdog, not a start the user asked for:
    /// a restart that fails also counts as a crash.
    restarting: bool,
    crashes: VecDeque<Instant>,
    stopping: Option<Stopping>,
    events: Vec<ServiceEvent>,
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service")
            .field("name", &self.spec.name)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// What the processes of a run are doing.
struct Survey {
    alive: usize,
    /// Exit code of the first process that exited by itself.
    code: Option<i32>,
}

impl Service {
    /// A stopped service.
    #[must_use]
    pub fn new(
        spec: ServiceSpec,
        supervisor: Arc<dyn ProcessSupervisor>,
        ports: Arc<dyn PortProbe>,
    ) -> Self {
        Self {
            spec,
            supervisor,
            ports,
            state: ServiceState::Stopped,
            run: None,
            started: None,
            next_probe: None,
            probe: None,
            restart_at: None,
            restarting: false,
            crashes: VecDeque::new(),
            stopping: None,
            events: Vec::new(),
        }
    }

    /// The service name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> &ServiceState {
        &self.state
    }

    /// Starts the service. From `Stopped` or `Failed` only; a failed start is reported as
    /// the `Failed` state and in the events, not as an error. Clears the crash history.
    pub fn start(&mut self, now: Instant) -> Result<Vec<ServiceEvent>, LifecycleError> {
        match self.state {
            ServiceState::Stopped | ServiceState::Failed(_) => {}
            ServiceState::Starting => return Err(LifecycleError::AlreadyActive("starting")),
            ServiceState::Running => return Err(LifecycleError::AlreadyActive("running")),
            ServiceState::Stopping => return Err(LifecycleError::AlreadyActive("stopping")),
        }
        self.crashes.clear();
        self.restarting = false;
        self.launch(now);
        Ok(std::mem::take(&mut self.events))
    }

    /// Asks the service to stop. Idempotent: on a stopped service it does nothing, and on a
    /// failed one it just acknowledges the failure by moving to `Stopped`.
    pub fn stop(&mut self, now: Instant) -> Vec<ServiceEvent> {
        match self.state {
            ServiceState::Stopped | ServiceState::Stopping => {}
            ServiceState::Failed(_) => self.set_state(ServiceState::Stopped),
            ServiceState::Starting | ServiceState::Running => self.begin_stop(now),
        }
        std::mem::take(&mut self.events)
    }

    /// Moves the state machine forward. Call it every 150–500 ms.
    pub fn tick(&mut self, now: Instant) -> Vec<ServiceEvent> {
        match self.state {
            ServiceState::Starting => self.tick_starting(now),
            ServiceState::Running => self.tick_running(now),
            ServiceState::Stopping => self.tick_stopping(now),
            ServiceState::Stopped | ServiceState::Failed(_) => {}
        }
        std::mem::take(&mut self.events)
    }

    // ---- start ----------------------------------------------------------------------

    fn launch(&mut self, now: Instant) {
        if let Err(reason) = self.try_launch(now) {
            self.fail(reason);
        }
    }

    /// Starts every process of the service. On failure nothing is left running and the state
    /// is untouched: the caller decides what the failure means.
    fn try_launch(&mut self, now: Instant) -> Result<(), FailureReason> {
        if let Some(reason) = self.preflight() {
            return Err(reason);
        }
        let group = self
            .supervisor
            .create_group(&self.spec.name, GroupOptions::default())
            .map_err(|e| FailureReason::Internal(e.to_string()))?;
        let mut processes = Vec::new();
        for spec in &self.spec.processes {
            match self.supervisor.spawn(group, spec) {
                Ok(id) => processes.push(id),
                Err(e) => {
                    let _ = self.supervisor.kill_group(group);
                    let _ = self.supervisor.close_group(group);
                    return Err(FailureReason::SpawnFailed {
                        program: spec.program.display().to_string(),
                        reason: e.to_string(),
                    });
                }
            }
        }
        self.run = Some(Run { group, processes });
        self.started = Some(now);
        self.next_probe = Some(now + self.spec.policy.probe_interval);
        self.set_state(ServiceState::Starting);
        Ok(())
    }

    /// Checks that every declared port is free. Never frees anything.
    fn preflight(&self) -> Option<FailureReason> {
        for &port in &self.spec.ports {
            match self.ports.probe(port) {
                Ok(PortStatus::Free) => {}
                Ok(PortStatus::InUse(owners)) => {
                    return Some(FailureReason::PortBusy { port, owners })
                }
                Ok(PortStatus::Reserved) => return Some(FailureReason::PortReserved(port)),
                Err(e) => return Some(FailureReason::Internal(e.to_string())),
            }
        }
        None
    }

    fn tick_starting(&mut self, now: Instant) {
        if let Some(at) = self.restart_at {
            if now >= at {
                self.restart_at = None;
                if let Err(reason) = self.try_launch(now) {
                    // A restart that cannot even start is one more crash.
                    self.on_crash(now, None, Some(reason));
                }
            }
            return;
        }
        let Some(survey) = self.survey() else {
            return;
        };
        if survey.alive == 0 {
            if self.restarting {
                self.teardown();
                self.on_crash(now, survey.code, None);
            } else {
                self.fail(FailureReason::ExitedDuringStartup { code: survey.code });
            }
            return;
        }
        if self.probe_due(now) && self.healthy(now) {
            self.cleanup_probe();
            self.set_state(ServiceState::Running);
            return;
        }
        let waited = self.spec.policy.startup_timeout;
        if self
            .started
            .is_some_and(|t| now.saturating_duration_since(t) >= waited)
        {
            self.fail(FailureReason::StartupTimeout { waited });
        }
    }

    /// Whether a health check may run now, and schedules the next one.
    fn probe_due(&mut self, now: Instant) -> bool {
        if self.next_probe.is_some_and(|at| now < at) {
            return false;
        }
        self.next_probe = Some(now + self.spec.policy.probe_interval);
        true
    }

    fn healthy(&mut self, now: Instant) -> bool {
        match self.spec.health.clone() {
            HealthCheck::None => true,
            // Only looks: asking with `probe` could make the starting service lose its bind.
            HealthCheck::Ports(ports) => ports
                .iter()
                .all(|port| matches!(self.ports.is_listening(*port), Ok(true))),
            HealthCheck::Command(command) => self.command_healthy(&command, now),
        }
    }

    /// Advances the health command: judges the run in flight, or starts a new one.
    fn command_healthy(&mut self, command: &HealthCommand, now: Instant) -> bool {
        if let Some(probe) = self.probe.take() {
            return match self.supervisor.status(probe.process) {
                Ok(ProcessStatus::Running)
                    if now.saturating_duration_since(probe.started) < command.timeout =>
                {
                    self.probe = Some(probe);
                    false
                }
                Ok(ProcessStatus::Exited(ExitReason::Code(0))) => {
                    self.close_probe_group(probe.group);
                    true
                }
                // Failed, timed out or vanished: clean up; the next interval tries again.
                _ => {
                    self.close_probe_group(probe.group);
                    false
                }
            };
        }
        if let Ok(group) = self
            .supervisor
            .create_group("health", GroupOptions::default())
        {
            match self.supervisor.spawn(group, &command.spec) {
                Ok(process) => {
                    self.probe = Some(Probe {
                        group,
                        process,
                        started: now,
                    });
                }
                Err(_) => self.close_probe_group(group),
            }
        }
        false
    }

    fn close_probe_group(&self, group: GroupId) {
        let _ = self.supervisor.kill_group(group);
        let _ = self.supervisor.close_group(group);
    }

    fn cleanup_probe(&mut self) {
        if let Some(probe) = self.probe.take() {
            self.close_probe_group(probe.group);
        }
    }

    // ---- running --------------------------------------------------------------------

    fn tick_running(&mut self, now: Instant) {
        let Some(survey) = self.survey() else {
            return;
        };
        if survey.alive > 0 {
            return;
        }
        self.teardown();
        self.on_crash(now, survey.code, None);
    }

    /// Counts a crash and either schedules a restart or gives up. `cause` is the specific
    /// failure to report if this is the end of the line (a restart that could not start);
    /// otherwise the generic reasons are used.
    fn on_crash(&mut self, now: Instant, code: Option<i32>, cause: Option<FailureReason>) {
        self.crashes.push_back(now);
        let window = self.spec.policy.crash_window;
        while self
            .crashes
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > window)
        {
            self.crashes.pop_front();
        }
        let crashes = u32::try_from(self.crashes.len()).unwrap_or(u32::MAX);

        if !self.spec.policy.auto_restart {
            let reason = cause.unwrap_or(FailureReason::Crashed { code });
            self.set_state(ServiceState::Failed(reason));
        } else if crashes >= self.spec.policy.max_crashes {
            let reason = cause.unwrap_or(FailureReason::TooManyCrashes { crashes, window });
            self.set_state(ServiceState::Failed(reason));
        } else {
            let delay = self.spec.policy.restart_delay;
            self.restart_at = Some(now + delay);
            self.restarting = true;
            self.events
                .push(ServiceEvent::Restarting { crashes, delay });
            self.set_state(ServiceState::Starting);
        }
    }

    // ---- stop -----------------------------------------------------------------------

    fn begin_stop(&mut self, now: Instant) {
        self.restart_at = None;
        self.cleanup_probe();
        if self.run.is_none() {
            // Waiting for a restart: nothing is running.
            self.set_state(ServiceState::Stopped);
            return;
        }
        let mut stopping = Stopping {
            requested: now,
            helper: None,
            killed: None,
        };
        let graceful = self.spec.graceful_stop.clone().and_then(|command| {
            let group = self
                .supervisor
                .create_group("stop", GroupOptions::default())
                .ok()?;
            if self.supervisor.spawn(group, &command).is_ok() {
                Some(group)
            } else {
                self.close_probe_group(group);
                None
            }
        });
        if let Some(group) = graceful {
            stopping.helper = Some(group);
        } else {
            // No graceful command, or it would not start: go straight to the kill.
            self.kill_run();
            stopping.killed = Some(now);
        }
        self.stopping = Some(stopping);
        self.set_state(ServiceState::Stopping);
    }

    fn tick_stopping(&mut self, now: Instant) {
        let Some(group) = self.run.as_ref().map(|run| run.group) else {
            self.finish_stop();
            return;
        };
        // A group the supervisor no longer knows has nothing alive. Any other error means we
        // cannot tell, which is not the same as "stopped": keep waiting, and if it never
        // answers, say so instead of claiming success.
        let (remaining, unknown) = match self.supervisor.active_processes(group) {
            Ok(n) => (n, None),
            Err(SupervisorError::UnknownGroup(_)) => (0, None),
            Err(e) => (1, Some(e.to_string())),
        };
        if remaining == 0 {
            self.finish_stop();
            return;
        }
        let Some((requested, killed)) = self.stopping.as_ref().map(|s| (s.requested, s.killed))
        else {
            return;
        };
        match killed {
            None if now.saturating_duration_since(requested) >= self.spec.policy.stop_grace => {
                self.kill_run();
                if let Some(stopping) = self.stopping.as_mut() {
                    stopping.killed = Some(now);
                }
            }
            Some(at) if now.saturating_duration_since(at) >= self.spec.policy.kill_wait => {
                self.teardown();
                let reason = match unknown {
                    Some(why) => FailureReason::Internal(format!(
                        "cannot tell whether the service stopped: {why}"
                    )),
                    None => FailureReason::CouldNotStop { remaining },
                };
                self.set_state(ServiceState::Failed(reason));
            }
            _ => {}
        }
    }

    fn kill_run(&self) {
        if let Some(run) = &self.run {
            let _ = self.supervisor.kill_group(run.group);
        }
    }

    fn finish_stop(&mut self) {
        self.teardown();
        self.set_state(ServiceState::Stopped);
    }

    // ---- shared ---------------------------------------------------------------------

    fn survey(&self) -> Option<Survey> {
        let run = self.run.as_ref()?;
        let mut survey = Survey {
            alive: 0,
            code: None,
        };
        for process in &run.processes {
            match self.supervisor.status(*process) {
                Ok(ProcessStatus::Running) => survey.alive += 1,
                Ok(ProcessStatus::Exited(ExitReason::Code(code))) => {
                    survey.code.get_or_insert(code);
                }
                Ok(ProcessStatus::Exited(ExitReason::Killed)) | Err(_) => {}
            }
        }
        Some(survey)
    }

    /// Kills and closes everything this service owns.
    fn teardown(&mut self) {
        self.cleanup_probe();
        if let Some(stopping) = self.stopping.take() {
            if let Some(helper) = stopping.helper {
                self.close_probe_group(helper);
            }
        }
        if let Some(run) = self.run.take() {
            let _ = self.supervisor.kill_group(run.group);
            let _ = self.supervisor.close_group(run.group);
        }
        self.started = None;
    }

    fn fail(&mut self, reason: FailureReason) {
        self.teardown();
        self.restart_at = None;
        self.set_state(ServiceState::Failed(reason));
    }

    fn set_state(&mut self, to: ServiceState) {
        if to != ServiceState::Starting {
            self.restarting = false;
        }
        if self.state != to {
            let from = std::mem::replace(&mut self.state, to.clone());
            self.events.push(ServiceEvent::StateChanged { from, to });
        }
    }
}

#[cfg(test)]
mod tests;
