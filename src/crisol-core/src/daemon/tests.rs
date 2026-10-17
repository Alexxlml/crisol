use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::catalog::{Arch, Catalog, Os, Target};
use crate::engine::{Deps, NoSystemChecks};
use crate::fetch::fake::FakeFetcher;
use crate::paths::fake::FakePrivateDirs;
use crate::paths::CrisolPaths;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;
use crate::process::ProcessId;
use crate::project::TrustAllOwners;

const HTTP: u16 = 19080;

struct Rig {
    _tmp: TempDir,
    paths: CrisolPaths,
    daemon: Daemon,
    supervisor: Arc<FakeSupervisor>,
    probe: Arc<FakePortProbe>,
    dirs: Arc<FakePrivateDirs>,
    t0: Instant,
}

impl Rig {
    /// A home with nginx installed, one site, and ports that are not the real ones.
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        fs::create_dir_all(paths.bin_for("nginx", "1.28.3").unwrap()).unwrap();
        fs::write(
            paths.config_file(),
            format!("[services.nginx]\nhttp_port = {HTTP}\nhttps_port = 19443\n\n[php]\nbase_port = 19100\n"),
        )
        .unwrap();
        fs::create_dir_all(paths.www_dir().join("blog")).unwrap();
        Self::over(tmp, paths)
    }

    fn over(tmp: TempDir, paths: CrisolPaths) -> Self {
        let supervisor = Arc::new(FakeSupervisor::new());
        let probe = Arc::new(FakePortProbe::new());
        let dirs = Arc::new(FakePrivateDirs::new());
        let deps = Deps {
            supervisor: supervisor.clone(),
            probe: probe.clone(),
            dirs: dirs.clone(),
            fetcher: Arc::new(FakeFetcher::new()),
            owners: Arc::new(TrustAllOwners),
            env: Arc::new(|_| None),
            system: Arc::new(NoSystemChecks),
            catalog: Catalog::parse("schema = 1\nserial = 1\n").unwrap(),
            target: Target {
                os: Os::Windows,
                arch: Arch::X86_64,
            },
        };
        let daemon = Daemon::new(Engine::new(paths.clone(), deps));
        Self {
            _tmp: tmp,
            paths,
            daemon,
            supervisor,
            probe,
            dirs,
            t0: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    fn call(&mut self, call: &Call, ms: u64) -> Result<Reply, RpcError> {
        let now = self.at(ms);
        self.daemon.handle(call, now, &mut |_| {})
    }

    fn call_events(&mut self, call: &Call, ms: u64) -> (Result<Reply, RpcError>, Vec<Event>) {
        let now = self.at(ms);
        let mut events = Vec::new();
        let reply = self.daemon.handle(call, now, &mut |e| events.push(e));
        (reply, events)
    }

    fn tick(&mut self, ms: u64) -> Vec<Event> {
        let now = self.at(ms);
        self.daemon.tick(now)
    }

    /// The control commands of Nginx (`-t`, `-s reload`) started so far.
    fn controls(&self) -> Vec<(ProcessId, Vec<String>)> {
        self.supervisor
            .spawned()
            .into_iter()
            .filter_map(|r| {
                let args: Vec<String> = r
                    .spec
                    .args
                    .iter()
                    .skip(4)
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                (args == ["-t"] || args == ["-s", "reload"]).then_some((r.process, args))
            })
            .collect()
    }

    fn listen(&self, port: u16) {
        self.probe.occupy(
            port,
            PortOwner {
                pid: 1,
                name: Some("nginx.exe".to_owned()),
                address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            },
        );
    }

    fn state(&mut self) -> Vec<(String, String)> {
        match self.call(&Call::Status { service: None }, 0).unwrap() {
            Reply::Status(info) => info
                .services
                .into_iter()
                .map(|s| (s.name, s.state))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    /// Starts and brings Nginx up: `nginx -t` passes, Nginx is launched and listens.
    fn start_running(&mut self) {
        self.call(&Call::Start { service: None }, 0).unwrap();
        let (test, _) = self.controls().pop().unwrap();
        self.supervisor.exit(test, 0).unwrap();
        self.tick(100);
        self.listen(HTTP);
        self.tick(600);
        assert!(self.daemon.settled(), "{:?}", self.state());
        assert_eq!(self.state(), [("nginx".to_owned(), "running".to_owned())]);
    }

    /// Lets the timers of a graceful stop run out.
    fn finish_stopping(&mut self, from: u64) {
        for ms in [100, 6000, 6100, 11_000] {
            self.tick(from + ms);
        }
    }
}

fn done(reply: Reply) -> DoneInfo {
    match reply {
        Reply::Done(info) => info,
        other => panic!("{other:?}"),
    }
}

#[test]
fn hello_checks_the_protocol_version() {
    let mut rig = Rig::new();
    let hello = |protocol| Call::Hello {
        client: "test".to_owned(),
        protocol,
    };
    match rig.call(&hello(PROTOCOL_VERSION), 0).unwrap() {
        Reply::Hello(info) => {
            assert_eq!(info.protocol, PROTOCOL_VERSION);
            assert_eq!(info.daemon, crate::VERSION);
            assert!(info.home.ends_with("home"), "{}", info.home);
        }
        other => panic!("{other:?}"),
    }
    let error = rig.call(&hello(PROTOCOL_VERSION + 1), 0).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedProtocol);
    assert!(error.message.contains("same version"), "{error}");
}

#[test]
fn nothing_runs_until_asked_and_status_lists_what_could() {
    let mut rig = Rig::new();
    assert_eq!(rig.state(), [("nginx".to_owned(), "stopped".to_owned())]);
    assert!(
        rig.supervisor.spawned().is_empty(),
        "no process before `start`"
    );
    assert!(rig.daemon.settled());
    assert!(
        !rig.call(&Call::Stop { service: None }, 0)
            .map(done)
            .unwrap()
            .changed
    );

    // An empty home lists nothing.
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    let mut empty = Rig::over(tmp, paths);
    assert_eq!(empty.state(), Vec::new());
}

#[test]
fn start_without_nginx_says_what_to_install() {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    let mut rig = Rig::over(tmp, paths);
    let error = rig.call(&Call::Start { service: None }, 0).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert!(error.message.contains("crisol install nginx"), "{error}");
    assert!(rig.daemon.settled());
}

#[test]
fn start_brings_nginx_up_through_the_test_and_stop_takes_it_down() {
    let mut rig = Rig::new();
    let reply = done(rig.call(&Call::Start { service: None }, 0).unwrap());
    assert!(reply.changed);
    assert!(!rig.daemon.settled(), "the configuration is being tested");

    let (test, args) = rig.controls().pop().unwrap();
    assert_eq!(args, ["-t"]);
    rig.supervisor.exit(test, 0).unwrap();
    let events = rig.tick(100);
    assert!(events.iter().any(|e| matches!(e, Event::ServiceState { service, state, .. } if service == "nginx" && state == "starting")), "{events:?}");
    rig.listen(HTTP);
    let events = rig.tick(600);
    assert!(events.iter().any(|e| matches!(e, Event::ServiceState { service, state, .. } if service == "nginx" && state == "running")), "{events:?}");
    assert!(rig.daemon.settled());
    assert_eq!(rig.daemon.failure(), None);

    // Starting again changes nothing.
    assert!(!done(rig.call(&Call::Start { service: None }, 700).unwrap()).changed);

    let stop = done(rig.call(&Call::Stop { service: None }, 1000).unwrap());
    assert!(stop.changed);
    assert!(!rig.daemon.settled(), "stopping takes a moment");
    rig.probe.release(HTTP);
    rig.finish_stopping(1000);
    assert!(rig.daemon.settled());
    assert_eq!(rig.state(), [("nginx".to_owned(), "stopped".to_owned())]);
}

#[test]
fn a_busy_port_is_a_failed_service_with_the_reason_and_a_settled_daemon() {
    let mut rig = Rig::new();
    rig.probe.occupy(
        HTTP,
        PortOwner {
            pid: 4,
            name: Some("herd.exe".to_owned()),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, HTTP)),
        },
    );
    rig.call(&Call::Start { service: None }, 0).unwrap();
    let (test, _) = rig.controls().pop().unwrap();
    // `nginx -t` itself fails on a busy port; the reason names who has it.
    rig.supervisor.exit(test, 1).unwrap();
    let events = rig.tick(100);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ReloadFailed { reason } if reason.contains("herd.exe"))),
        "{events:?}"
    );
    assert!(rig.daemon.settled());
}

#[test]
fn an_open_home_blocks_start_with_the_command_and_leaves_status_working() {
    let mut rig = Rig::new();
    rig.dirs.expose(rig.paths.home());
    let error = rig.call(&Call::Start { service: None }, 0).unwrap_err();
    assert_eq!(error.code, ErrorCode::HomeExposed);
    assert!(
        error
            .fix
            .as_deref()
            .is_some_and(|f| f.starts_with("icacls ")),
        "{error:?}"
    );
    assert!(rig.supervisor.spawned().is_empty(), "nothing was started");
    assert_eq!(rig.state(), [("nginx".to_owned(), "stopped".to_owned())]);
    // Closed, it starts.
    rig.dirs.close(rig.paths.home());
    assert!(rig.call(&Call::Start { service: None }, 10).is_ok());
}

#[test]
fn a_service_cannot_be_started_alone_and_an_unknown_one_is_not_found() {
    let mut rig = Rig::new();
    let alone = rig
        .call(
            &Call::Start {
                service: Some("nginx".to_owned()),
            },
            0,
        )
        .unwrap_err();
    assert_eq!(alone.code, ErrorCode::Conflict);
    assert!(alone.message.contains("without a service name"), "{alone}");
    for call in [
        Call::Start {
            service: Some("mysql".to_owned()),
        },
        Call::Status {
            service: Some("mysql".to_owned()),
        },
    ] {
        assert_eq!(rig.call(&call, 0).unwrap_err().code, ErrorCode::NotFound);
    }
    assert!(rig
        .call(
            &Call::Status {
                service: Some("nginx".to_owned())
            },
            0
        )
        .is_ok());
}

#[test]
fn a_new_site_is_served_without_anybody_asking() {
    let mut rig = Rig::new();
    rig.start_running();
    let before = rig.controls().len();
    assert!(rig.tick(3000).is_empty(), "nothing changed, nothing to do");
    assert_eq!(rig.controls().len(), before);

    fs::create_dir_all(rig.paths.www_dir().join("shop")).unwrap();
    // Not before the watch is due...
    assert!(rig
        .tick(3500)
        .iter()
        .all(|e| !matches!(e, Event::ReloadStarted)));
    // ...and then, the configuration is tested for the new site.
    let events = rig.tick(5500);
    assert!(events.contains(&Event::ReloadStarted), "{events:?}");
    // The test starts on the next tick, then the reload.
    rig.tick(5600);
    let (test, args) = rig.controls().pop().unwrap();
    assert_eq!(args, ["-t"]);
    rig.supervisor.exit(test, 0).unwrap();
    rig.tick(5700);
    let (reload, args) = rig.controls().pop().unwrap();
    assert_eq!(args, ["-s", "reload"]);
    rig.supervisor.exit(reload, 0).unwrap();
    let events = rig.tick(5800);
    assert!(events.contains(&Event::Reloaded), "{events:?}");
    assert!(rig
        .paths
        .etc_for("nginx")
        .unwrap()
        .join("sites")
        .join("shop.conf")
        .is_file());
}

#[test]
fn a_command_that_changes_the_sites_reloads_a_running_stack_at_once() {
    let mut rig = Rig::new();
    rig.start_running();
    let project = rig.paths.home().join("elsewhere");
    fs::create_dir_all(&project).unwrap();
    let (reply, events) = rig.call_events(
        &Call::Link {
            name: "extra".to_owned(),
            path: project.display().to_string(),
            docroot: None,
        },
        1000,
    );
    assert!(done(reply.unwrap()).changed);
    assert!(events.contains(&Event::ReloadStarted), "{events:?}");
    rig.tick(1100);
    let (_, args) = rig.controls().pop().unwrap();
    assert_eq!(args, ["-t"]);
}

#[test]
fn changing_the_ports_asks_for_a_restart_instead_of_guessing() {
    let mut rig = Rig::new();
    rig.start_running();
    fs::write(
        rig.paths.config_file(),
        "[services.nginx]\nhttp_port = 19081\nhttps_port = 19443\n\n[php]\nbase_port = 19100\n",
    )
    .unwrap();
    let events = rig.tick(5000);
    assert!(
        events.iter().any(
            |e| matches!(e, Event::ReloadFailed { reason } if reason.contains("crisol restart"))
        ),
        "{events:?}"
    );
}

#[test]
fn restart_stops_and_starts_again_by_itself() {
    let mut rig = Rig::new();
    rig.start_running();
    assert!(done(rig.call(&Call::Restart { service: None }, 1000).unwrap()).changed);
    assert!(!rig.daemon.settled());
    rig.probe.release(HTTP);
    rig.finish_stopping(1000);
    // Stopped; the restart starts it again. The configuration on disk is the one that ran,
    // so Nginx is launched without testing it again.
    assert_eq!(rig.state(), [("nginx".to_owned(), "starting".to_owned())]);
    assert!(!rig.daemon.settled());
    rig.listen(HTTP);
    rig.tick(13_600);
    assert!(rig.daemon.settled());
    assert_eq!(rig.state(), [("nginx".to_owned(), "running".to_owned())]);
    // A restart of what is stopped is just a start.
    let mut stopped = Rig::new();
    assert!(done(stopped.call(&Call::Restart { service: None }, 0).unwrap()).changed);
}

#[test]
fn shutdown_stops_what_it_started_and_is_settled_only_when_it_is_down() {
    let mut rig = Rig::new();
    rig.start_running();
    let (reply, events) = rig.call_events(&Call::Shutdown, 1000);
    assert!(done(reply.unwrap()).changed);
    assert!(events.contains(&Event::ShuttingDown));
    assert!(rig.daemon.shutdown_requested());
    assert!(!rig.daemon.settled());
    rig.probe.release(HTTP);
    rig.finish_stopping(1000);
    assert!(rig.daemon.settled());
    assert_eq!(rig.state(), [("nginx".to_owned(), "stopped".to_owned())]);
    // Nothing starts again once it is leaving.
    assert_eq!(
        rig.call(&Call::Start { service: None }, 20_000)
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[test]
fn file_operations_work_with_the_daemon_and_with_nothing_running() {
    let mut rig = Rig::new();
    match rig.call(&Call::Sites, 0).unwrap() {
        Reply::Sites { sites, .. } => assert_eq!(
            sites.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["blog"]
        ),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        rig.call(&Call::Doctor, 0).unwrap(),
        Reply::Doctor { .. }
    ));
    let path: PathBuf = rig.paths.home().to_path_buf();
    assert!(rig
        .call(
            &Call::Park {
                path: path.display().to_string()
            },
            0
        )
        .is_ok());
    assert!(
        rig.supervisor.spawned().is_empty(),
        "no reload without a running stack"
    );
}
