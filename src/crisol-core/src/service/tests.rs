//! Lifecycle tests: the fakes plus a made-up clock, so nothing sleeps.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;
use crate::process::{ProcessSpec, SupervisorError};

const HTTP: u16 = 8000;
const HTTPS: u16 = 8443;

struct Rig {
    service: Service,
    supervisor: Arc<FakeSupervisor>,
    ports: Arc<FakePortProbe>,
    t0: Instant,
}

impl Rig {
    fn new(spec: ServiceSpec) -> Self {
        let supervisor = Arc::new(FakeSupervisor::new());
        let ports = Arc::new(FakePortProbe::new());
        let service = Service::new(spec, supervisor.clone(), ports.clone());
        Self {
            service,
            supervisor,
            ports,
            t0: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    fn start(&mut self, ms: u64) -> Vec<ServiceEvent> {
        let now = self.at(ms);
        self.service.start(now).expect("start is allowed here")
    }

    fn tick(&mut self, ms: u64) -> Vec<ServiceEvent> {
        let now = self.at(ms);
        self.service.tick(now)
    }

    fn stop(&mut self, ms: u64) -> Vec<ServiceEvent> {
        let now = self.at(ms);
        self.service.stop(now)
    }

    fn state(&self) -> ServiceState {
        self.service.state().clone()
    }

    /// The listener appears (the fake never opens a real socket).
    fn listen(&self, port: u16) {
        self.ports.occupy(port, owner(port));
    }

    /// The port is free again (the process died).
    fn unlisten(&self, port: u16) {
        self.ports.release(port);
    }

    /// Processes the service itself spawned, in order (health and stop helpers excluded).
    fn service_processes(&self) -> Vec<crate::process::ProcessId> {
        self.supervisor
            .spawned()
            .into_iter()
            .filter(|r| self.supervisor.group_name(r.group).as_deref() == Some("nginx"))
            .map(|r| r.process)
            .collect()
    }

    /// Starts and brings the service to `Running` at `ms`; returns the time reached.
    fn run(&mut self, ms: u64) -> u64 {
        self.start(ms);
        self.listen(HTTP);
        self.tick(ms + 500);
        assert_eq!(
            self.state(),
            ServiceState::Running,
            "setup should reach Running"
        );
        ms + 500
    }
}

fn owner(port: u16) -> PortOwner {
    PortOwner {
        pid: 4321,
        name: Some("nginx.exe".to_owned()),
        address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
    }
}

fn nginx() -> ServiceSpec {
    ServiceSpec::new("nginx")
        .process(ProcessSpec::new("nginx.exe").args(["-c", "nginx.conf"]))
        .port(HTTP)
        .port(HTTPS)
        .health(HealthCheck::Ports(vec![HTTP]))
        .graceful_stop(ProcessSpec::new("nginx.exe").args(["-s", "quit"]))
}

fn changes(events: &[ServiceEvent]) -> Vec<ServiceState> {
    events
        .iter()
        .filter_map(|e| match e {
            ServiceEvent::StateChanged { to, .. } => Some(to.clone()),
            ServiceEvent::Restarting { .. } => None,
        })
        .collect()
}

#[test]
fn the_default_policy_has_the_documented_values() {
    let p = LifecyclePolicy::default();
    assert_eq!(p.probe_interval, Duration::from_millis(500));
    assert_eq!(p.startup_timeout, Duration::from_secs(20));
    assert_eq!(p.stop_grace, Duration::from_millis(4500));
    assert_eq!(p.kill_wait, Duration::from_millis(3750));
    assert_eq!(p.restart_delay, Duration::from_millis(1500));
    assert_eq!(
        (p.max_crashes, p.crash_window),
        (3, Duration::from_secs(60))
    );
    assert!(p.auto_restart);
}

// ---- start and health -----------------------------------------------------------

#[test]
fn start_checks_the_ports_then_spawns_and_enters_starting() {
    let mut rig = Rig::new(nginx());
    assert_eq!(rig.state(), ServiceState::Stopped);

    let events = rig.start(0);
    assert_eq!(
        events,
        vec![ServiceEvent::StateChanged {
            from: ServiceState::Stopped,
            to: ServiceState::Starting
        }]
    );
    assert_eq!(
        rig.ports.probed(),
        [HTTP, HTTPS],
        "preflight looks at every declared port"
    );
    let spawned = rig.supervisor.spawned();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].spec, nginx().processes[0]);
    assert_eq!(
        rig.supervisor.group_name(spawned[0].group).as_deref(),
        Some("nginx")
    );
}

#[test]
fn it_becomes_running_when_the_port_listens_and_not_before() {
    let mut rig = Rig::new(nginx());
    rig.start(0);

    assert_eq!(rig.tick(500), Vec::new(), "nothing listens yet");
    rig.listen(HTTP);
    assert_eq!(
        rig.tick(700),
        Vec::new(),
        "too soon for another health check"
    );
    assert_eq!(rig.state(), ServiceState::Starting);

    let events = rig.tick(1000);
    assert_eq!(changes(&events), [ServiceState::Running]);
    // Preflight (2), then one health check at 500 and one at 1000; none at 700.
    assert_eq!(rig.ports.probed(), [HTTP, HTTPS, HTTP, HTTP]);
}

#[test]
fn every_health_port_must_listen() {
    let spec = nginx().health(HealthCheck::Ports(vec![HTTP, HTTPS]));
    let mut rig = Rig::new(spec);
    rig.start(0);
    rig.listen(HTTP);
    rig.tick(500);
    assert_eq!(rig.state(), ServiceState::Starting);
    rig.listen(HTTPS);
    rig.tick(1000);
    assert_eq!(rig.state(), ServiceState::Running);
}

#[test]
fn without_a_health_check_a_live_process_is_ready_at_the_first_check() {
    let mut rig = Rig::new(nginx().health(HealthCheck::None));
    rig.start(0);
    assert_eq!(
        rig.tick(100),
        Vec::new(),
        "first check is one interval after the launch"
    );
    rig.tick(500);
    assert_eq!(rig.state(), ServiceState::Running);
}

#[test]
fn a_command_health_check_succeeds_on_exit_code_zero() {
    let command = HealthCommand {
        spec: ProcessSpec::new("pg_isready.exe"),
        timeout: Duration::from_secs(2),
    };
    let mut rig = Rig::new(nginx().health(HealthCheck::Command(command)));
    rig.start(0);

    rig.tick(500); // launches the check
    let probe = rig.supervisor.spawned().last().unwrap().clone();
    assert_eq!(probe.spec.program.to_string_lossy(), "pg_isready.exe");
    assert_eq!(rig.state(), ServiceState::Starting);

    rig.supervisor.exit(probe.process, 0).unwrap();
    rig.tick(1000);
    assert_eq!(rig.state(), ServiceState::Running);
    assert_eq!(
        rig.supervisor.group_count(),
        1,
        "the health group must be cleaned up"
    );
}

#[test]
fn a_failing_command_check_is_retried_on_the_next_interval() {
    let command = HealthCommand {
        spec: ProcessSpec::new("pg_isready.exe"),
        timeout: Duration::from_secs(2),
    };
    let mut rig = Rig::new(nginx().health(HealthCheck::Command(command)));
    rig.start(0);
    rig.tick(500);
    let first = rig.supervisor.spawned().last().unwrap().process;
    rig.supervisor.exit(first, 2).unwrap();

    rig.tick(1000);
    assert_eq!(
        rig.state(),
        ServiceState::Starting,
        "exit code 2 is unhealthy"
    );
    assert_eq!(
        rig.supervisor.group_count(),
        1,
        "the failed check was cleaned up"
    );

    rig.tick(1500); // a fresh check
    let second = rig.supervisor.spawned().last().unwrap().process;
    assert_ne!(first, second);
    rig.supervisor.exit(second, 0).unwrap();
    rig.tick(2000);
    assert_eq!(rig.state(), ServiceState::Running);
}

#[test]
fn a_command_check_that_hangs_is_killed_after_its_timeout() {
    let command = HealthCommand {
        spec: ProcessSpec::new("pg_isready.exe"),
        timeout: Duration::from_secs(2),
    };
    let mut rig = Rig::new(nginx().health(HealthCheck::Command(command)));
    rig.start(0);
    rig.tick(500); // check launched at 500
    let hung = rig.supervisor.spawned().last().unwrap().process;

    rig.tick(1000);
    rig.tick(2000);
    assert_eq!(
        rig.supervisor.status(hung).unwrap(),
        ProcessStatus::Running,
        "still within 2 s"
    );
    rig.tick(2500); // 2 s after launch
    assert_eq!(
        rig.supervisor.alive_pids().len(),
        1,
        "only the service itself may be left; the hung check is gone"
    );
    rig.tick(3000); // a new check starts
    assert_eq!(rig.supervisor.spawned().len(), 3);
}

// ---- start failures -------------------------------------------------------------

#[test]
fn a_busy_port_fails_the_start_and_nothing_is_killed_or_spawned() {
    let mut rig = Rig::new(nginx());
    rig.ports.occupy(HTTP, owner(HTTP));

    let events = rig.start(0);
    let reason = FailureReason::PortBusy {
        port: HTTP,
        owners: vec![owner(HTTP)],
    };
    assert_eq!(rig.state(), ServiceState::Failed(reason.clone()));
    assert_eq!(changes(&events), [ServiceState::Failed(reason.clone())]);
    assert_eq!(rig.supervisor.spawned(), Vec::new());
    assert_eq!(rig.supervisor.group_count(), 0);
    assert_eq!(
        reason.to_string(),
        "port 8000 is already in use by nginx.exe (pid 4321)"
    );
}

#[test]
fn a_port_with_an_unknown_owner_and_a_reserved_port_are_reported_distinctly() {
    let mut rig = Rig::new(nginx());
    rig.ports.occupy_unknown(HTTPS);
    rig.start(0);
    assert_eq!(
        rig.state(),
        ServiceState::Failed(FailureReason::PortBusy {
            port: HTTPS,
            owners: Vec::new()
        })
    );

    let mut rig = Rig::new(nginx());
    rig.ports.reserve(HTTP..=HTTP);
    rig.start(0);
    let reason = FailureReason::PortReserved(HTTP);
    assert_eq!(rig.state(), ServiceState::Failed(reason.clone()));
    assert_eq!(
        reason.to_string(),
        "port 8000 is reserved by the operating system"
    );
}

#[test]
fn a_port_probe_error_fails_the_start() {
    let mut rig = Rig::new(nginx());
    rig.ports.fail_with("no tables");
    rig.start(0);
    assert_eq!(
        rig.state(),
        ServiceState::Failed(FailureReason::Internal("no tables".into()))
    );
    assert_eq!(rig.supervisor.group_count(), 0);
}

#[test]
fn a_spawn_failure_stops_whatever_had_already_started() {
    let spec = nginx()
        .process(ProcessSpec::new("php-cgi.exe"))
        .process(ProcessSpec::new("php-cgi.exe"));
    let mut rig = Rig::new(spec);
    rig.supervisor.fail_spawn_after(
        2,
        SupervisorError::Spawn {
            program: "php-cgi.exe".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        },
    );

    rig.start(0);
    match rig.state() {
        ServiceState::Failed(FailureReason::SpawnFailed { program, .. }) => {
            assert_eq!(program, "php-cgi.exe");
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(
        rig.supervisor.alive_pids(),
        Vec::<u32>::new(),
        "no orphan may remain"
    );
    assert_eq!(rig.supervisor.group_count(), 0);
}

#[test]
fn exiting_during_startup_is_a_failure_and_is_not_retried() {
    let mut rig = Rig::new(nginx());
    rig.start(0);
    let process = rig.service_processes()[0];
    rig.supervisor.exit(process, 3).unwrap();

    rig.tick(100);
    assert_eq!(
        rig.state(),
        ServiceState::Failed(FailureReason::ExitedDuringStartup { code: Some(3) })
    );
    for ms in [2000, 5000, 20_000] {
        rig.tick(ms);
    }
    assert_eq!(
        rig.supervisor.spawned().len(),
        1,
        "a startup failure is never restarted"
    );
    assert_eq!(rig.supervisor.group_count(), 0);
}

#[test]
fn a_service_that_never_becomes_healthy_is_killed_at_the_timeout() {
    let mut rig = Rig::new(nginx());
    rig.start(0);
    for ms in (500..20_000).step_by(500) {
        rig.tick(ms);
    }
    assert_eq!(rig.state(), ServiceState::Starting, "still within 20 s");

    let events = rig.tick(20_000);
    let reason = FailureReason::StartupTimeout {
        waited: Duration::from_secs(20),
    };
    assert_eq!(changes(&events), [ServiceState::Failed(reason.clone())]);
    assert_eq!(rig.supervisor.alive_pids(), Vec::<u32>::new());
    assert_eq!(reason.to_string(), "not ready after 20 s");
}

// ---- watchdog -------------------------------------------------------------------

#[test]
fn a_crash_is_restarted_after_the_delay() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let process = rig.service_processes()[0];
    rig.supervisor.exit(process, 1).unwrap();
    rig.unlisten(HTTP); // the dead process released its port

    let events = rig.tick(now + 2000);
    assert_eq!(
        events,
        vec![
            ServiceEvent::Restarting {
                crashes: 1,
                delay: Duration::from_millis(1500)
            },
            ServiceEvent::StateChanged {
                from: ServiceState::Running,
                to: ServiceState::Starting
            },
        ]
    );
    assert_eq!(
        rig.supervisor.spawned().len(),
        1,
        "not relaunched before the delay"
    );
    assert_eq!(
        rig.supervisor.group_count(),
        0,
        "the dead group was cleaned up"
    );

    rig.tick(now + 3000);
    assert_eq!(rig.supervisor.spawned().len(), 1, "1 s into a 1.5 s delay");
    rig.tick(now + 3500);
    assert_eq!(
        rig.supervisor.spawned().len(),
        2,
        "relaunched after the delay"
    );

    rig.listen(HTTP);
    rig.tick(now + 4000);
    assert_eq!(rig.state(), ServiceState::Running);
}

#[test]
fn a_service_that_keeps_crashing_is_given_up_on() {
    let mut rig = Rig::new(nginx());
    let mut now = rig.run(0);
    for round in 1..=2 {
        let process = *rig.service_processes().last().unwrap();
        rig.supervisor.exit(process, 1).unwrap();
        rig.unlisten(HTTP);
        now += 1000;
        let events = rig.tick(now);
        assert!(events.contains(&ServiceEvent::Restarting {
            crashes: round,
            delay: Duration::from_millis(1500)
        }));
        now += 1500;
        rig.tick(now); // relaunch
        rig.listen(HTTP);
        now += 500;
        rig.tick(now);
        assert_eq!(rig.state(), ServiceState::Running, "round {round}");
    }

    let process = *rig.service_processes().last().unwrap();
    rig.supervisor.exit(process, 1).unwrap();
    rig.unlisten(HTTP);
    now += 1000;
    rig.tick(now);
    let reason = FailureReason::TooManyCrashes {
        crashes: 3,
        window: Duration::from_secs(60),
    };
    assert_eq!(rig.state(), ServiceState::Failed(reason.clone()));
    assert_eq!(reason.to_string(), "crashed 3 times within 60 s");
    assert_eq!(rig.supervisor.group_count(), 0);
}

#[test]
fn crashes_older_than_the_window_are_forgotten() {
    let mut rig = Rig::new(nginx());
    let mut now = rig.run(0);
    for round in 0..6 {
        let process = *rig.service_processes().last().unwrap();
        rig.supervisor.exit(process, 1).unwrap();
        rig.unlisten(HTTP);
        now += 61_000; // well outside the 60 s window
        let events = rig.tick(now);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServiceEvent::Restarting { crashes: 1, .. })),
            "round {round}: only the latest crash counts"
        );
        now += 1500;
        rig.tick(now);
        rig.listen(HTTP);
        now += 500;
        rig.tick(now);
        assert_eq!(rig.state(), ServiceState::Running, "round {round}");
    }
}

#[test]
fn without_auto_restart_a_crash_is_a_failure() {
    let policy = LifecyclePolicy {
        auto_restart: false,
        ..LifecyclePolicy::default()
    };
    let mut rig = Rig::new(nginx().policy(policy));
    let now = rig.run(0);
    let process = rig.service_processes()[0];
    rig.supervisor.exit(process, 9).unwrap();

    rig.tick(now + 1000);
    assert_eq!(
        rig.state(),
        ServiceState::Failed(FailureReason::Crashed { code: Some(9) })
    );
    rig.tick(now + 60_000);
    assert_eq!(rig.supervisor.spawned().len(), 1);
}

#[test]
fn a_pool_survives_losing_some_workers_but_not_all_of_them() {
    let spec = ServiceSpec::new("nginx")
        .process(ProcessSpec::new("php-cgi.exe"))
        .process(ProcessSpec::new("php-cgi.exe"))
        .process(ProcessSpec::new("php-cgi.exe"))
        .health(HealthCheck::None);
    let mut rig = Rig::new(spec);
    rig.start(0);
    rig.tick(500);
    assert_eq!(rig.state(), ServiceState::Running);

    let workers = rig.service_processes();
    rig.supervisor.exit(workers[0], 1).unwrap();
    rig.supervisor.exit(workers[1], 1).unwrap();
    rig.tick(1000);
    assert_eq!(
        rig.state(),
        ServiceState::Running,
        "one worker is still alive"
    );

    rig.supervisor.exit(workers[2], 1).unwrap();
    let events = rig.tick(1500);
    assert!(events
        .iter()
        .any(|e| matches!(e, ServiceEvent::Restarting { .. })));
}

// ---- stop -----------------------------------------------------------------------

#[test]
fn a_graceful_stop_asks_nicely_and_waits_for_the_tree_to_go() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let process = rig.service_processes()[0];

    let events = rig.stop(now + 1000);
    assert_eq!(changes(&events), [ServiceState::Stopping]);
    let helper = rig.supervisor.spawned().last().unwrap().clone();
    assert_eq!(helper.spec.args, ["-s", "quit"], "the graceful command ran");
    assert_eq!(
        rig.supervisor.status(process).unwrap(),
        ProcessStatus::Running,
        "not killed yet"
    );

    assert_eq!(rig.tick(now + 1500), Vec::new(), "still shutting down");
    rig.supervisor.exit(process, 0).unwrap(); // nginx quit by itself
    let events = rig.tick(now + 2000);
    assert_eq!(changes(&events), [ServiceState::Stopped]);
    assert_eq!(
        rig.supervisor.group_count(),
        0,
        "service and helper groups are closed"
    );
}

#[test]
fn a_stop_that_is_not_obeyed_is_killed_after_the_grace_period() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let process = rig.service_processes()[0];
    rig.stop(now);

    rig.tick(now + 4499);
    assert_eq!(
        rig.supervisor.status(process).unwrap(),
        ProcessStatus::Running
    );
    rig.tick(now + 4500);
    assert_eq!(
        rig.supervisor.status(process).unwrap(),
        ProcessStatus::Exited(ExitReason::Killed),
        "the grace period is over"
    );
    assert_eq!(
        rig.state(),
        ServiceState::Stopping,
        "confirmed on the next check"
    );
    rig.tick(now + 4650);
    assert_eq!(rig.state(), ServiceState::Stopped);
    assert_eq!(rig.supervisor.alive_pids(), Vec::<u32>::new());
}

#[test]
fn without_a_graceful_command_the_stop_kills_at_once() {
    let mut spec = nginx();
    spec.graceful_stop = None;
    let mut rig = Rig::new(spec);
    let now = rig.run(0);
    let process = rig.service_processes()[0];

    rig.stop(now);
    assert_eq!(
        rig.supervisor.status(process).unwrap(),
        ProcessStatus::Exited(ExitReason::Killed)
    );
    rig.tick(now + 150);
    assert_eq!(rig.state(), ServiceState::Stopped);
}

#[test]
fn a_graceful_command_that_cannot_start_falls_back_to_the_kill() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let process = rig.service_processes()[0];
    rig.supervisor
        .fail_next_spawn(SupervisorError::Os("no".into()));

    rig.stop(now);
    assert_eq!(
        rig.supervisor.status(process).unwrap(),
        ProcessStatus::Exited(ExitReason::Killed)
    );
    rig.tick(now + 150);
    assert_eq!(rig.state(), ServiceState::Stopped);
}

#[test]
fn processes_that_survive_a_kill_make_the_stop_fail() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let group = rig.supervisor.spawned()[0].group;
    rig.supervisor.make_unkillable(group);

    rig.stop(now);
    rig.tick(now + 4500); // kill attempt
    assert_eq!(rig.state(), ServiceState::Stopping);
    rig.tick(now + 4500 + 3749);
    assert_eq!(
        rig.state(),
        ServiceState::Stopping,
        "still inside the kill wait"
    );
    rig.tick(now + 4500 + 3750);
    let reason = FailureReason::CouldNotStop { remaining: 1 };
    assert_eq!(rig.state(), ServiceState::Failed(reason.clone()));
    assert_eq!(
        reason.to_string(),
        "could not stop: 1 process(es) still alive"
    );
}

#[test]
fn stopping_while_starting_kills_what_was_launched() {
    let mut rig = Rig::new(nginx());
    rig.start(0);
    let process = rig.service_processes()[0];
    rig.stop(100);
    assert_eq!(rig.state(), ServiceState::Stopping);
    rig.supervisor.exit(process, 0).unwrap();
    rig.tick(300);
    assert_eq!(rig.state(), ServiceState::Stopped);
    assert_eq!(rig.supervisor.group_count(), 0);
}

#[test]
fn stopping_during_the_restart_delay_cancels_the_restart() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let process = rig.service_processes()[0];
    rig.supervisor.exit(process, 1).unwrap();
    rig.unlisten(HTTP);
    rig.tick(now + 1000); // crash noticed, restart scheduled

    let events = rig.stop(now + 1200);
    assert_eq!(changes(&events), [ServiceState::Stopped]);
    rig.tick(now + 10_000);
    assert_eq!(
        rig.supervisor.spawned().len(),
        1,
        "no relaunch after a stop"
    );
    assert_eq!(rig.state(), ServiceState::Stopped);
}

// ---- commands and events --------------------------------------------------------

#[test]
fn start_is_refused_while_the_service_is_active() {
    let mut rig = Rig::new(nginx());
    rig.start(0);
    let now = rig.at(1);
    assert_eq!(
        rig.service.start(now),
        Err(LifecycleError::AlreadyActive("starting"))
    );

    rig.listen(HTTP);
    rig.tick(500);
    let now = rig.at(600);
    assert_eq!(
        rig.service.start(now),
        Err(LifecycleError::AlreadyActive("running"))
    );

    rig.stop(700);
    let now = rig.at(701);
    assert_eq!(
        rig.service.start(now),
        Err(LifecycleError::AlreadyActive("stopping"))
    );
}

#[test]
fn starting_again_after_a_failure_forgets_the_crash_history() {
    let mut rig = Rig::new(nginx());
    let mut now = rig.run(0);
    for _ in 0..3 {
        let process = *rig.service_processes().last().unwrap();
        rig.supervisor.exit(process, 1).unwrap();
        rig.unlisten(HTTP);
        now += 1000;
        rig.tick(now);
        if matches!(rig.state(), ServiceState::Failed(_)) {
            break;
        }
        now += 1500;
        rig.tick(now);
        rig.listen(HTTP);
        now += 500;
        rig.tick(now);
    }
    assert!(matches!(
        rig.state(),
        ServiceState::Failed(FailureReason::TooManyCrashes { .. })
    ));

    rig.unlisten(HTTP);
    now += 1000;
    rig.start(now);
    rig.listen(HTTP);
    now += 500;
    rig.tick(now);
    assert_eq!(rig.state(), ServiceState::Running);

    let process = *rig.service_processes().last().unwrap();
    rig.supervisor.exit(process, 1).unwrap();
    rig.unlisten(HTTP);
    now += 1000;
    let events = rig.tick(now);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServiceEvent::Restarting { crashes: 1, .. })),
        "the count starts again from one"
    );
}

#[test]
fn stop_is_idempotent_and_acknowledges_a_failure() {
    let mut rig = Rig::new(nginx());
    assert_eq!(
        rig.stop(0),
        Vec::new(),
        "stopping a stopped service does nothing"
    );

    rig.ports.occupy(HTTP, owner(HTTP));
    rig.start(0);
    assert!(matches!(rig.state(), ServiceState::Failed(_)));
    let events = rig.stop(10);
    assert_eq!(changes(&events), [ServiceState::Stopped]);
    assert_eq!(rig.stop(20), Vec::new());
}

#[test]
fn events_are_delivered_once() {
    let mut rig = Rig::new(nginx());
    rig.start(0);
    rig.listen(HTTP);
    assert_ne!(rig.tick(500), Vec::new());
    assert_eq!(rig.tick(500), Vec::new());
    assert_eq!(rig.tick(1000), Vec::new());
}

#[test]
fn failure_messages_are_readable() {
    let cases = [
        (
            FailureReason::ExitedDuringStartup { code: None },
            "exited during startup",
        ),
        (
            FailureReason::Crashed { code: Some(2) },
            "crashed with code 2",
        ),
        (FailureReason::Crashed { code: None }, "crashed"),
        (
            FailureReason::SpawnFailed {
                program: "x.exe".into(),
                reason: "not found".into(),
            },
            "cannot start `x.exe`: not found",
        ),
        (
            FailureReason::PortBusy {
                port: 80,
                owners: Vec::new(),
            },
            "port 80 is already in use",
        ),
    ];
    for (reason, text) in cases {
        assert_eq!(reason.to_string(), text);
    }
}

#[test]
fn the_service_is_debuggable_and_exposes_its_name() {
    let rig = Rig::new(nginx());
    assert_eq!(rig.service.name(), "nginx");
    assert!(format!("{:?}", rig.service).contains("Stopped"));
}
