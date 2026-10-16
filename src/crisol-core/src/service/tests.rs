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
            _ => None,
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
    // Preflight probes (2); then one health check at 500 and one at 1000, none at 700. The
    // health checks only look at the listeners: they never probe.
    assert_eq!(rig.ports.probed(), [HTTP, HTTPS]);
    assert_eq!(rig.ports.listen_checked(), [HTTP, HTTP]);
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
        "port 8000 is already in use by nginx.exe (pid 4321, 127.0.0.1:8000)"
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
fn the_health_check_only_looks_and_never_probes_the_port() {
    // `probe` may try to claim the port, which could make the starting service lose its own
    // bind. Waiting for readiness must go through `is_listening` only.
    let mut rig = Rig::new(nginx());
    rig.start(0);
    assert_eq!(
        rig.ports.probed(),
        [HTTP, HTTPS],
        "only the preflight probes"
    );
    rig.tick(500);
    rig.tick(1000);
    rig.listen(HTTP);
    rig.tick(1500);
    assert_eq!(rig.state(), ServiceState::Running);
    assert_eq!(rig.ports.probed(), [HTTP, HTTPS], "no probe while waiting");
    assert_eq!(rig.ports.listen_checked(), [HTTP, HTTP, HTTP]);
}

#[test]
fn a_restart_that_dies_before_it_is_healthy_counts_as_a_crash() {
    let mut rig = Rig::new(nginx());
    let mut now = rig.run(0);
    let mut seen = Vec::new();
    for _ in 0..2 {
        let process = *rig.service_processes().last().unwrap();
        rig.supervisor.exit(process, 1).unwrap();
        rig.unlisten(HTTP);
        now += 1000;
        seen.extend(rig.tick(now)); // crash, or a restart that died
        now += 1500;
        rig.tick(now); // relaunch; the new process never listens
    }
    let restarts: Vec<u32> = seen
        .iter()
        .filter_map(|e| match e {
            ServiceEvent::Restarting { crashes, .. } => Some(*crashes),
            _ => None,
        })
        .collect();
    assert_eq!(restarts, [1, 2], "the failed restart was counted");
    assert_eq!(rig.state(), ServiceState::Starting);

    // The third relaunch dies too: the watchdog gives up instead of looping forever.
    let process = *rig.service_processes().last().unwrap();
    rig.supervisor.exit(process, 1).unwrap();
    rig.tick(now + 500);
    assert!(
        matches!(
            rig.state(),
            ServiceState::Failed(FailureReason::TooManyCrashes { crashes: 3, .. })
        ),
        "{:?}",
        rig.state()
    );
    assert_eq!(rig.supervisor.group_count(), 0);
}

#[test]
fn a_restart_that_cannot_spawn_is_retried_and_counted() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let process = rig.service_processes()[0];
    rig.supervisor.exit(process, 1).unwrap();
    rig.unlisten(HTTP);
    rig.tick(now + 1000); // crash 1, restart scheduled

    rig.supervisor
        .fail_next_spawn(SupervisorError::Os("still locked".to_owned()));
    let events = rig.tick(now + 2500); // the relaunch fails
    assert!(events.contains(&ServiceEvent::Restarting {
        crashes: 2,
        delay: Duration::from_millis(1500)
    }));
    assert_eq!(rig.state(), ServiceState::Starting);

    rig.tick(now + 4000); // the next relaunch works
    rig.listen(HTTP);
    rig.tick(now + 4500);
    assert_eq!(rig.state(), ServiceState::Running);
}

#[test]
fn a_user_start_that_dies_is_still_not_retried() {
    let mut rig = Rig::new(nginx());
    rig.start(0);
    let process = rig.service_processes()[0];
    rig.supervisor.exit(process, 3).unwrap();
    rig.tick(500);
    assert_eq!(
        rig.state(),
        ServiceState::Failed(FailureReason::ExitedDuringStartup { code: Some(3) })
    );
}

#[test]
fn a_group_that_cannot_be_queried_is_not_reported_as_stopped() {
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    rig.stop(now);
    rig.supervisor.fail_active_query(true);
    rig.tick(now + 1000);
    assert_eq!(rig.state(), ServiceState::Stopping, "we do not know yet");

    rig.tick(now + 10_000); // the grace period is over: the group is killed
    rig.tick(now + 20_000); // and the kill wait is over too
    match rig.state() {
        ServiceState::Failed(FailureReason::Internal(why)) => {
            assert!(
                why.contains("cannot tell whether the service stopped"),
                "{why}"
            );
        }
        other => panic!("expected an internal failure, got {other:?}"),
    }
    assert_eq!(rig.supervisor.group_count(), 0, "everything was cleaned up");
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

// ---- a pool of workers (ADR-0009) -------------------------------------------------------

const POOL_BASE: u16 = 9100;
const WORKERS: usize = 4;

fn pool_spec() -> ServiceSpec {
    let mut spec = ServiceSpec::new("php").pool();
    for i in 0..WORKERS {
        let port = POOL_BASE + u16::try_from(i).unwrap();
        spec = spec
            .process(ProcessSpec::new("php-cgi.exe").args(["-b", &format!("127.0.0.1:{port}")]))
            .port(port);
    }
    spec.health(HealthCheck::Ports(
        (0..WORKERS)
            .map(|i| POOL_BASE + u16::try_from(i).unwrap())
            .collect(),
    ))
}

impl Rig {
    fn pool_processes(&self) -> Vec<crate::process::ProcessId> {
        self.supervisor
            .spawned()
            .into_iter()
            .filter(|r| self.supervisor.group_name(r.group).as_deref() == Some("php"))
            .map(|r| r.process)
            .collect()
    }

    /// The workers that are running now, oldest first.
    fn live_workers(&self) -> Vec<crate::process::ProcessId> {
        self.pool_processes()
            .into_iter()
            .filter(|p| matches!(self.supervisor.status(*p), Ok(ProcessStatus::Running)))
            .collect()
    }

    /// Starts the pool and brings it to `Running`; returns the time reached.
    fn run_pool(&mut self, ms: u64) -> u64 {
        self.start(ms);
        for i in 0..WORKERS {
            self.listen(POOL_BASE + u16::try_from(i).unwrap());
        }
        self.tick(ms + 500);
        assert_eq!(self.state(), ServiceState::Running);
        ms + 500
    }
}

fn pool_rig() -> Rig {
    Rig::new(pool_spec())
}

#[test]
fn a_worker_that_exits_with_zero_is_recycled_at_once_without_counting_or_changing_state() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    let before = rig.pool_processes();
    assert_eq!(before.len(), WORKERS);

    for round in 0..10_u32 {
        let worker = rig.live_workers()[0]; // the oldest, long past the minimum uptime
        rig.supervisor.exit(worker, 0).unwrap();
        let events = rig.tick(now + 300 * (u64::from(round) + 1));
        assert_eq!(events, vec![], "a recycle is silent: {events:?}");
        assert_eq!(rig.state(), ServiceState::Running);
    }
    // Ten recycles later it still has four live workers, and none of it was a crash: three
    // real crashes would give up (max_crashes = 3), and this did not.
    assert_eq!(rig.pool_processes().len(), WORKERS + 10);
    assert_eq!(rig.supervisor.alive_pids().len(), WORKERS);
}

#[test]
fn every_worker_recycling_in_the_same_tick_is_not_a_crash() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    for worker in rig.pool_processes() {
        rig.supervisor.exit(worker, 0).unwrap();
    }
    assert_eq!(rig.tick(now + 100), vec![]);
    assert_eq!(rig.state(), ServiceState::Running);
    assert_eq!(rig.supervisor.alive_pids().len(), WORKERS);
}

#[test]
fn a_crashed_worker_comes_back_after_the_delay_and_the_pool_says_it_was_degraded() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    let worker = rig.pool_processes()[1];
    rig.supervisor.exit(worker, 255).unwrap();

    let events = rig.tick(now + 100);
    assert_eq!(
        events,
        vec![
            ServiceEvent::WorkerCrashed {
                index: 1,
                code: Some(255),
                crashes: 1,
                delay: Duration::from_millis(1500),
            },
            ServiceEvent::Degraded {
                alive: 3,
                expected: 4
            },
        ]
    );
    assert_eq!(
        rig.state(),
        ServiceState::Running,
        "the others keep serving"
    );
    assert_eq!(rig.supervisor.alive_pids().len(), 3);

    // Nothing happens before the delay is over...
    assert_eq!(rig.tick(now + 1000), vec![]);
    assert_eq!(rig.supervisor.alive_pids().len(), 3);
    // ...and then the worker is relaunched and the pool is whole again.
    let events = rig.tick(now + 1700);
    assert_eq!(events, vec![ServiceEvent::Recovered { expected: 4 }]);
    assert_eq!(rig.supervisor.alive_pids().len(), 4);
    assert_eq!(rig.state(), ServiceState::Running);
}

#[test]
fn the_new_worker_runs_the_same_program_as_the_one_it_replaces() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    let worker = rig.pool_processes()[2];
    rig.supervisor.exit(worker, 1).unwrap();
    rig.tick(now + 100);
    rig.tick(now + 2000);
    let records = rig.supervisor.spawned();
    let last = records.last().unwrap();
    assert_eq!(
        last.spec.args[1], "127.0.0.1:9102",
        "the third worker's port"
    );
}

#[test]
fn crashes_of_different_workers_add_up_to_the_limit() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    let [a, b, c, ..] = rig.pool_processes()[..] else {
        panic!("four workers")
    };
    for (n, worker) in [a, b].into_iter().enumerate() {
        rig.supervisor.exit(worker, 1).unwrap();
        rig.tick(now + 100 * (n as u64 + 1));
    }
    assert_eq!(
        rig.state(),
        ServiceState::Running,
        "two crashes are tolerated"
    );
    rig.supervisor.exit(c, 1).unwrap();
    rig.tick(now + 400);
    assert_eq!(
        rig.state(),
        ServiceState::Failed(FailureReason::TooManyCrashes {
            crashes: 3,
            window: Duration::from_secs(60)
        })
    );
    assert_eq!(
        rig.supervisor.alive_pids().len(),
        0,
        "nothing is left behind"
    );
}

#[test]
fn when_every_worker_dies_it_is_a_crash_of_the_service() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    for worker in rig.pool_processes() {
        rig.supervisor.exit(worker, 3).unwrap();
    }
    let events = rig.tick(now + 100);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServiceEvent::Restarting { crashes: 1, .. })),
        "{events:?}"
    );
    assert_eq!(
        rig.state(),
        ServiceState::Starting,
        "the whole pool restarts"
    );
}

#[test]
fn the_last_worker_dying_while_others_wait_to_come_back_is_a_crash_of_the_service() {
    let mut spec = pool_spec();
    spec.policy.max_crashes = 10;
    let mut rig = Rig::new(spec);
    let now = rig.run_pool(0);
    let workers = rig.pool_processes();
    for (n, worker) in workers[..3].iter().enumerate() {
        rig.supervisor.exit(*worker, 1).unwrap();
        // One at a time, inside the delay: they are all down and waiting.
        let _ = n;
    }
    rig.tick(now + 100);
    assert_eq!(
        rig.state(),
        ServiceState::Running,
        "one worker is still alive"
    );
    rig.supervisor.exit(workers[3], 1).unwrap();
    let events = rig.tick(now + 200);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServiceEvent::Restarting { .. })),
        "{events:?}"
    );
}

#[test]
fn a_relaunch_that_cannot_start_counts_as_another_crash() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    let worker = rig.pool_processes()[0];
    rig.supervisor.exit(worker, 1).unwrap();
    rig.tick(now + 100);
    rig.supervisor
        .fail_next_spawn(SupervisorError::Os("no memory".to_owned()));
    let events = rig.tick(now + 1700);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServiceEvent::WorkerCrashed { crashes: 2, .. })),
        "{events:?}"
    );
    assert_eq!(rig.state(), ServiceState::Running);
}

#[test]
fn without_auto_restart_a_worker_crash_fails_the_pool() {
    let mut spec = pool_spec();
    spec.policy.auto_restart = false;
    let mut rig = Rig::new(spec);
    let now = rig.run_pool(0);
    let worker = rig.pool_processes()[0];
    rig.supervisor.exit(worker, 9).unwrap();
    rig.tick(now + 100);
    assert_eq!(
        rig.state(),
        ServiceState::Failed(FailureReason::Crashed { code: Some(9) })
    );
}

#[test]
fn a_single_process_service_still_treats_exit_zero_as_a_crash() {
    // ADR-0009: "exit 0 is a recycle" is only for the workers of a pool.
    let mut rig = Rig::new(nginx());
    let now = rig.run(0);
    let process = rig.service_processes()[0];
    rig.supervisor.exit(process, 0).unwrap();
    let events = rig.tick(now + 100);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServiceEvent::Restarting { .. })),
        "{events:?}"
    );
}

#[test]
fn a_pool_stops_like_any_service_even_with_workers_waiting_to_come_back() {
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    let worker = rig.pool_processes()[0];
    rig.supervisor.exit(worker, 1).unwrap();
    rig.tick(now + 100);
    rig.stop(now + 200);
    rig.tick(now + 300);
    assert_eq!(rig.state(), ServiceState::Stopped);
    assert_eq!(rig.supervisor.alive_pids().len(), 0);
    assert_eq!(rig.supervisor.group_count(), 0);
}

#[test]
fn a_worker_that_exits_with_zero_right_after_it_started_is_a_crash_not_a_recycle() {
    // Review of phase 5, finding 5: a php-cgi that always exits 0 at once must not be
    // relaunched forever without being counted.
    let mut rig = pool_rig();
    let now = rig.run_pool(0);
    let worker = rig.live_workers()[0];
    rig.supervisor.exit(worker, 0).unwrap();
    assert_eq!(
        rig.tick(now + 100),
        vec![],
        "a normal recycle (it ran 600 ms)"
    );

    // The replacement dies at once, again with 0: that is a failure to run.
    let replacement = rig.live_workers().into_iter().last().unwrap();
    rig.supervisor.exit(replacement, 0).unwrap();
    let events = rig.tick(now + 150);
    assert!(
        events.iter().any(|e| matches!(
            e,
            ServiceEvent::WorkerCrashed {
                index: 0,
                code: Some(0),
                crashes: 1,
                ..
            }
        )),
        "{events:?}"
    );

    // Keep failing and the pool gives up instead of looping.
    let mut ms = now + 150;
    for _ in 0..6 {
        ms += 1500; // the restart delay
        rig.tick(ms);
        if matches!(rig.state(), ServiceState::Failed(_)) {
            break;
        }
        let again = rig.live_workers().into_iter().last().unwrap();
        rig.supervisor.exit(again, 0).unwrap();
        ms += 50;
        rig.tick(ms);
    }
    assert!(
        matches!(
            rig.state(),
            ServiceState::Failed(FailureReason::TooManyCrashes { .. })
        ),
        "{:?}",
        rig.state()
    );
}
