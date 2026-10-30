use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::cacert::NoRoots;
use crate::catalog::{Arch, Catalog, Os, Target};
use crate::dbservice::NoAdmin;
use crate::engine::{Deps, NoSystemChecks, SystemChecks};
use crate::fetch::fake::FakeFetcher;
use crate::paths::fake::FakePrivateDirs;
use crate::paths::CrisolPaths;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;
use crate::process::ProcessId;
use crate::project::TrustAllOwners;
use crate::web::ConfigProbe;

const HTTP: u16 = 19080;

struct Rig {
    _tmp: TempDir,
    paths: CrisolPaths,
    daemon: Daemon,
    supervisor: Arc<FakeSupervisor>,
    probe: Arc<FakePortProbe>,
    dirs: Arc<FakePrivateDirs>,
    /// What the probe of the configuration answers: whether Nginx already runs the new one.
    effective: Arc<AtomicBool>,
    t0: Instant,
}

struct Flag(Arc<AtomicBool>);

impl ConfigProbe for Flag {
    fn is_effective(&self, _http_port: u16, _token: &str) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Rig {
    /// A home with nginx installed, one site, and ports that are not the real ones.
    fn new() -> Self {
        Self::with_system(Arc::new(NoSystemChecks))
    }

    fn with_system(system: Arc<dyn SystemChecks>) -> Self {
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
        Self::over_with(tmp, paths, system)
    }

    fn over(tmp: TempDir, paths: CrisolPaths) -> Self {
        Self::over_with(tmp, paths, Arc::new(NoSystemChecks))
    }

    fn over_with(tmp: TempDir, paths: CrisolPaths, system: Arc<dyn SystemChecks>) -> Self {
        let supervisor = Arc::new(FakeSupervisor::new());
        let probe = Arc::new(FakePortProbe::new());
        let dirs = Arc::new(FakePrivateDirs::new());
        let effective = Arc::new(AtomicBool::new(true));
        let deps = Deps {
            supervisor: supervisor.clone(),
            probe: probe.clone(),
            dirs: dirs.clone(),
            fetcher: Arc::new(FakeFetcher::new()),
            owners: Arc::new(TrustAllOwners),
            env: Arc::new(|_| None),
            system,
            roots: Arc::new(NoRoots),
            config_probe: Arc::new(Flag(effective.clone())),
            db_admin: Arc::new(NoAdmin),
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
            effective,
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
    // MySQL is a service (F9.1) that is not installed here: it says what to install.
    let missing = rig
        .call(
            &Call::Start {
                service: Some("mysql".to_owned()),
            },
            0,
        )
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotAvailable);
    assert!(
        missing.message.contains("crisol install mysql"),
        "{missing}"
    );
    assert_eq!(
        rig.call(
            &Call::Status {
                service: Some("mysql".to_owned())
            },
            0
        )
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
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
fn a_running_database_whose_port_changed_in_the_file_is_told_once_and_not_restarted() {
    let mut rig = Rig::new();
    rig.with_mysql();
    rig.call(
        &Call::Start {
            service: Some("mysql".to_owned()),
        },
        0,
    )
    .unwrap();
    rig.listen_mysql();
    rig.tick(700);
    assert_eq!(rig.service_state("mysql").as_deref(), Some("running"));
    assert!(rig.tick(3000).is_empty(), "same port: nothing to say");

    let text = fs::read_to_string(rig.paths.config_file())
        .unwrap()
        .replace("port = 19306", "port = 19307");
    fs::write(rig.paths.config_file(), text).unwrap();
    let events = rig.tick(6000);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ReloadFailed { reason }
            if reason.contains("`mysql` is still on port 19306")
                && reason.contains("crisol restart mysql")
                && reason.contains("19307"))),
        "{events:?}"
    );
    // Said once, and the database was left alone.
    let again = rig.tick(9000);
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(rig.service_state("mysql").as_deref(), Some("running"));
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

/// Review of phase 8, finding 9: how much does one look at the user's files cost with a lot of
/// sites? Run it on request: `cargo test -p crisol-core --release watching_ -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check; run it on request"]
fn watching_two_hundred_sites_costs_little() {
    let rig = Rig::new();
    for n in 0..200 {
        let site = rig.paths.www_dir().join(format!("site-{n:03}"));
        fs::create_dir_all(site.join("public")).unwrap();
        fs::write(site.join("composer.json"), "{}").unwrap();
        if n % 3 == 0 {
            fs::write(
                site.join(".php-version"),
                "8.4
",
            )
            .unwrap();
        }
        if n % 5 == 0 {
            fs::write(
                site.join(".crisol.toml"),
                "[php]
version = \"8.4\"
",
            )
            .unwrap();
        }
    }
    let mut times: Vec<Duration> = (0..21)
        .map(|_| {
            let start = Instant::now();
            let _ = rig.daemon.watch_fingerprint();
            start.elapsed()
        })
        .collect();
    times.sort();
    println!(
        "watch_fingerprint with 200 sites: median {:?}, slowest {:?}, fastest {:?}",
        times[times.len() / 2],
        times[times.len() - 1],
        times[0]
    );
    // Generous: the decision threshold is a few tens of milliseconds, every two seconds.
    assert!(times[times.len() / 2] < Duration::from_millis(200));
}

// ---- databases (F9.1) ---------------------------------------------------------------------

const MYSQL_PORT: u16 = 19306;

impl Rig {
    /// MySQL 8.4.11 installed, with its data already made, on a port of the tests.
    fn with_mysql(&self) {
        let bin = self.paths.bin_for("mysql", "8.4.11").unwrap().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("mysqld.exe"), "MZ").unwrap();
        fs::create_dir_all(self.paths.data_for("mysql", "8.4").unwrap().join("mysql")).unwrap();
        let mut config = fs::read_to_string(self.paths.config_file()).unwrap();
        config.push_str("\n[services.mysql]\nenabled = true\nport = 19306\n");
        fs::write(self.paths.config_file(), config).unwrap();
    }

    fn listen_mysql(&self) {
        self.probe.occupy(
            MYSQL_PORT,
            PortOwner {
                pid: 2,
                name: Some("mysqld.exe".to_owned()),
                address: SocketAddr::from((Ipv4Addr::LOCALHOST, MYSQL_PORT)),
            },
        );
    }

    /// The MySQL server leaves after the shutdown command, as the real one does.
    fn mysql_leaves(&self) {
        for record in self.supervisor.spawned() {
            if record.spec.program.ends_with("mysqld.exe") {
                let _ = self.supervisor.exit(record.process, 0);
            }
        }
        self.probe.release(MYSQL_PORT);
    }

    fn service_state(&mut self, name: &str) -> Option<String> {
        self.state()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s)
    }
}

#[test]
fn a_database_is_started_and_stopped_by_name_without_touching_the_web_stack() {
    let mut rig = Rig::new();
    rig.with_mysql();
    assert_eq!(
        rig.service_state("mysql").as_deref(),
        Some("stopped"),
        "installed, not started"
    );
    let reply = done(
        rig.call(
            &Call::Start {
                service: Some("mysql".to_owned()),
            },
            0,
        )
        .unwrap(),
    );
    assert!(
        reply.changed && reply.message.contains("mysql"),
        "{reply:?}"
    );
    assert_eq!(rig.service_state("mysql").as_deref(), Some("starting"));
    assert_eq!(
        rig.service_state("nginx").as_deref(),
        Some("stopped"),
        "the web stack was not asked"
    );
    assert!(!rig.daemon.settled());
    rig.listen_mysql();
    rig.tick(700);
    assert_eq!(rig.service_state("mysql").as_deref(), Some("running"));
    assert!(rig.daemon.settled());
    // Asking again changes nothing.
    assert!(
        !done(
            rig.call(
                &Call::Start {
                    service: Some("mysql".to_owned())
                },
                800
            )
            .unwrap()
        )
        .changed
    );

    let stop = done(
        rig.call(
            &Call::Stop {
                service: Some("mysql".to_owned()),
            },
            1000,
        )
        .unwrap(),
    );
    assert!(stop.changed);
    assert_eq!(rig.service_state("mysql").as_deref(), Some("stopping"));
    rig.mysql_leaves();
    rig.tick(1500);
    assert_eq!(rig.service_state("mysql").as_deref(), Some("stopped"));
}

#[test]
fn a_plain_start_runs_the_web_stack_and_the_enabled_databases_and_a_failed_database_is_a_warning() {
    let mut rig = Rig::new();
    rig.with_mysql();
    // Somebody else has MySQL's port.
    rig.probe.occupy(
        MYSQL_PORT,
        PortOwner {
            pid: 9,
            name: Some("herd-mysql.exe".to_owned()),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, MYSQL_PORT)),
        },
    );
    let reply = done(rig.call(&Call::Start { service: None }, 0).unwrap());
    assert!(reply.changed);
    let (test, _) = rig.controls().pop().unwrap();
    rig.supervisor.exit(test, 0).unwrap();
    rig.tick(100);
    rig.listen(HTTP);
    rig.tick(600);
    // The web stack is up; MySQL failed and says why; the daemon is settled and fine.
    assert_eq!(rig.service_state("nginx").as_deref(), Some("running"));
    assert_eq!(rig.service_state("mysql").as_deref(), Some("failed"));
    assert!(rig.daemon.settled());
    assert_eq!(rig.daemon.failure(), None, "the web stack did not fail");
    let why = rig.daemon.database_warnings();
    assert!(
        why.len() == 1 && why[0].contains("19306") && why[0].contains("herd-mysql.exe"),
        "{why:?}"
    );
    assert_eq!(rig.daemon.failure_of(Some("mysql")), Some(why[0].clone()));
    // Stopping everything takes the failed entry to rest too.
    rig.call(&Call::Stop { service: None }, 1000).unwrap();
    rig.probe.release(HTTP);
    rig.finish_stopping(1000);
    assert!(rig.daemon.settled());
}

#[test]
fn without_nginx_a_plain_start_still_starts_the_databases_and_says_so() {
    let mut rig = Rig::new();
    rig.with_mysql();
    fs::remove_dir_all(rig.paths.bin_dir().join("nginx")).unwrap();
    let reply = done(rig.call(&Call::Start { service: None }, 0).unwrap());
    assert!(reply.message.contains("databases"), "{reply:?}");
    assert!(
        reply
            .warnings
            .iter()
            .any(|w| w.contains("web stack was not started") && w.contains("crisol install nginx")),
        "{reply:?}"
    );
    assert_eq!(rig.service_state("mysql").as_deref(), Some("starting"));
}

#[test]
fn shutdown_waits_for_the_databases_too() {
    let mut rig = Rig::new();
    rig.with_mysql();
    rig.call(
        &Call::Start {
            service: Some("mysql".to_owned()),
        },
        0,
    )
    .unwrap();
    rig.listen_mysql();
    rig.tick(700);
    rig.call(&Call::Shutdown, 1000).unwrap();
    assert!(!rig.daemon.settled(), "mysql is still shutting down");
    rig.mysql_leaves();
    rig.tick(1500);
    assert!(rig.daemon.settled());
}

// ---- the generated PHP files (review of the manual verification of phase 9, V1) ----------

impl Rig {
    /// PHP 8.4.26 (just enough of it) and Mozilla's CA bundle.
    fn with_php(&self) {
        let install = self.paths.bin_for("php", "8.4.26").unwrap();
        fs::create_dir_all(install.join("ext")).unwrap();
        fs::write(install.join("php-cgi.exe"), "MZ").unwrap();
        fs::write(
            install.join("php.ini-development"),
            "[PHP]\n;extension=curl\n;date.timezone =\nmemory_limit = 128M\n",
        )
        .unwrap();
        fs::write(install.join("ext").join("php_curl.dll"), "MZ").unwrap();
        let bundle = self.paths.bin_for("cacert", "2026.9.25").unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("cacert.pem"), "## Mozilla\n").unwrap();
    }

    fn php_cgi_spawns(&self) -> usize {
        self.supervisor
            .spawned()
            .iter()
            .filter(|r| r.spec.program.to_string_lossy().contains("php-cgi"))
            .count()
    }
}

#[test]
fn a_start_leaves_the_ini_and_the_bundle_where_the_pool_will_read_them() {
    let mut rig = Rig::new();
    rig.with_php();
    rig.call(&Call::Start { service: None }, 0).unwrap();

    let ini = fs::read_to_string(rig.paths.php_ini("8.4.26").unwrap()).unwrap();
    assert!(ini.contains("extension=curl"), "{ini}");
    assert!(ini.contains("curl.cainfo"), "{ini}");
    assert!(rig.paths.php_cacert().is_file());
    // The pool was launched with `PHPRC` on the folder of that file.
    let worker = rig
        .supervisor
        .spawned()
        .into_iter()
        .find(|r| r.spec.program.to_string_lossy().contains("php-cgi"))
        .expect("a php-cgi worker");
    let phprc = worker
        .spec
        .env
        .iter()
        .find(|(k, _)| k == "PHPRC")
        .map(|(_, v)| std::path::PathBuf::from(v))
        .unwrap();
    assert!(phprc.join("php.ini").is_file(), "{}", phprc.display());
}

#[test]
fn a_start_without_the_bundle_says_so_and_the_ini_has_no_cainfo() {
    let mut rig = Rig::new();
    rig.with_php();
    fs::remove_dir_all(rig.paths.bin_for("cacert", "2026.9.25").unwrap()).unwrap();
    let reply = done(rig.call(&Call::Start { service: None }, 0).unwrap());
    assert!(
        reply
            .warnings
            .iter()
            .any(|w| w.contains("crisol install cacert")),
        "{:?}",
        reply.warnings
    );
    let ini = fs::read_to_string(rig.paths.php_ini("8.4.26").unwrap()).unwrap();
    assert!(!ini.contains("curl.cainfo = "), "{ini}");
}

#[test]
fn changing_php_in_the_file_regenerates_the_ini_and_restarts_only_its_pool() {
    let mut rig = Rig::new();
    rig.with_php();
    rig.call(&Call::Start { service: None }, 0).unwrap();
    let (test, _) = rig.controls().pop().unwrap();
    rig.supervisor.exit(test, 0).unwrap();
    rig.tick(100);
    rig.listen(HTTP);
    for port in 19100..19104 {
        rig.listen(port);
    }
    rig.tick(600);
    assert_eq!(rig.php_cgi_spawns(), 4);

    fs::write(
        rig.paths.config_file(),
        format!("[services.nginx]\nhttp_port = {HTTP}\nhttps_port = 19443\n\n[php]\nbase_port = 19100\ntimezone = \"Europe/Madrid\"\n"),
    )
    .unwrap();
    rig.tick(5000);
    let ini = fs::read_to_string(rig.paths.php_ini("8.4.26").unwrap()).unwrap();
    assert!(ini.contains("date.timezone = \"Europe/Madrid\""), "{ini}");

    // The workers go down (their ports free up) and come up again with the new file.
    assert_eq!(
        rig.php_cgi_spawns(),
        4,
        "waits for the ports of the old workers"
    );
    for port in 19100..19104 {
        rig.probe.release(port);
    }
    for ms in [5200, 5300, 5400] {
        rig.tick(ms);
    }
    assert_eq!(rig.php_cgi_spawns(), 8, "{:?}", rig.state());
    for port in 19100..19104 {
        rig.listen(port);
    }
    rig.tick(6000);
    assert_eq!(rig.service_state("php-8.4.26").as_deref(), Some("running"));
    assert_eq!(rig.php_cgi_spawns(), 8, "{:?}", rig.state());
}

#[test]
fn a_reload_is_done_only_when_nginx_answers_for_the_new_configuration() {
    let mut rig = Rig::new();
    rig.start_running();
    rig.effective.store(false, Ordering::SeqCst);
    let project = rig.paths.home().join("elsewhere");
    fs::create_dir_all(&project).unwrap();
    rig.call(
        &Call::Link {
            name: "extra".to_owned(),
            path: project.display().to_string(),
            docroot: None,
        },
        1000,
    )
    .unwrap();
    rig.tick(1100);
    let (test, _) = rig.controls().pop().unwrap();
    rig.supervisor.exit(test, 0).unwrap();
    rig.tick(1200);
    let (reload, args) = rig.controls().pop().unwrap();
    assert_eq!(args, ["-s", "reload"]);
    rig.supervisor.exit(reload, 0).unwrap();

    // The signal was sent, but the old workers still answer: not done, not settled.
    let events = rig.tick(1300);
    assert!(!events.contains(&Event::Reloaded), "{events:?}");
    assert!(!rig.daemon.settled());
    let events = rig.tick(3000);
    assert!(!events.contains(&Event::Reloaded), "{events:?}");
    assert!(!rig.daemon.settled());

    // The new workers take over.
    rig.effective.store(true, Ordering::SeqCst);
    let events = rig.tick(3200);
    assert!(events.contains(&Event::Reloaded), "{events:?}");
    assert!(rig.daemon.settled());
}

#[test]
fn a_reload_that_never_shows_says_so_after_a_few_seconds_and_goes_on() {
    let mut rig = Rig::new();
    rig.start_running();
    rig.effective.store(false, Ordering::SeqCst);
    let project = rig.paths.home().join("elsewhere");
    fs::create_dir_all(&project).unwrap();
    rig.call(
        &Call::Link {
            name: "extra".to_owned(),
            path: project.display().to_string(),
            docroot: None,
        },
        1000,
    )
    .unwrap();
    rig.tick(1100);
    let (test, _) = rig.controls().pop().unwrap();
    rig.supervisor.exit(test, 0).unwrap();
    rig.tick(1200);
    let (reload, _) = rig.controls().pop().unwrap();
    rig.supervisor.exit(reload, 0).unwrap();
    rig.tick(1300);

    let events = rig.tick(7000);
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::ReloadFailed { reason } if reason.contains("old configuration")
        )),
        "{events:?}"
    );
    assert!(rig.daemon.settled());
}

#[test]
fn stopping_everything_says_each_state_of_each_service_once() {
    let mut rig = Rig::new();
    rig.with_mysql();
    rig.call(&Call::Start { service: None }, 0).unwrap();
    rig.listen_mysql();
    let (test, _) = rig.controls().pop().unwrap();
    rig.supervisor.exit(test, 0).unwrap();
    rig.tick(100);
    rig.listen(HTTP);
    rig.tick(700);

    let mut all: Vec<Event> = Vec::new();
    let (reply, events) = rig.call_events(&Call::Shutdown, 1000);
    reply.unwrap();
    all.extend(events);
    rig.mysql_leaves();
    for ms in [1500, 2000, 3000, 7000, 12_000] {
        all.extend(rig.tick(ms));
    }
    let states: Vec<String> = all
        .iter()
        .filter_map(|e| match e {
            Event::ServiceState {
                service,
                state,
                detail,
            } => Some(format!("{service}: {state} {detail:?}")),
            _ => None,
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for line in &states {
        assert!(seen.insert(line.clone()), "twice: {line} in {states:?}");
    }
}

#[test]
fn reset_password_says_what_it_cannot_do_and_changes_a_stopped_one_that_reads_it_at_start() {
    let mut rig = Rig::new();
    let reset = |rig: &mut Rig, service: &str| {
        rig.call(
            &Call::ResetPassword {
                service: service.to_owned(),
            },
            0,
        )
    };
    assert_eq!(
        reset(&mut rig, "nope").unwrap_err().code,
        ErrorCode::NotFound
    );
    assert!(reset(&mut rig, "memcached")
        .unwrap_err()
        .message
        .contains("no password"));

    // A server that has to be asked is not asked while it is stopped.
    rig.with_mysql();
    let error = reset(&mut rig, "mysql").unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert!(error.message.contains("crisol start mysql"), "{error}");

    // One that reads its password at the start just gets the new one for then.
    let redis = rig.paths.bin_for("redis", "5.0.14.1").unwrap();
    fs::create_dir_all(&redis).unwrap();
    fs::write(redis.join("redis-server.exe"), "MZ").unwrap();
    let file = rig.paths.secrets_dir().join("redis-5.0");
    fs::create_dir_all(rig.paths.secrets_dir()).unwrap();
    fs::write(&file, "OldPassword1\n").unwrap();
    let reply = done(reset(&mut rig, "redis").unwrap());
    assert!(
        reply.changed && reply.message.contains("takes effect"),
        "{reply:?}"
    );
    let kept = fs::read_to_string(&file).unwrap();
    assert_ne!(kept.trim(), "OldPassword1");
    assert_eq!(kept.trim().len(), crate::secrets::PASSWORD_LENGTH);
}

// ---- doctor (F11.1) -------------------------------------------------------------------------

/// A system that explains one exit code and knows what slows databases down.
struct Explains;

impl SystemChecks for Explains {
    fn checks(&self, _paths: &CrisolPaths) -> Vec<Check> {
        Vec::new()
    }

    fn explain_exit(&self, code: i32) -> Option<String> {
        (code == -1_073_741_515).then(|| "a DLL is missing".to_owned())
    }

    fn slow_start_hint(&self, _paths: &CrisolPaths) -> Option<String> {
        Some("exclude data/".to_owned())
    }
}

fn doctor(rig: &mut Rig, ms: u64) -> Vec<Check> {
    match rig.call(&Call::Doctor, ms).unwrap() {
        Reply::Doctor { checks } => checks,
        other => panic!("{other:?}"),
    }
}

fn line<'a>(checks: &'a [Check], name: &str) -> Option<&'a Check> {
    checks.iter().find(|c| c.name == name)
}

#[test]
fn doctor_tells_who_holds_the_port_of_a_service_that_is_not_running_and_not_of_one_that_is() {
    let mut rig = Rig::new();
    let quiet = doctor(&mut rig, 0);
    let names: Vec<&str> = quiet.iter().map(|c| c.name.as_str()).collect();
    let sites = names.iter().position(|n| *n == "sites").unwrap();
    assert_eq!(
        names[sites + 1],
        "services",
        "the daemon's lines follow `sites`"
    );
    let services = line(&quiet, "services").unwrap();
    assert_eq!(services.status, CheckStatus::Ok);
    assert!(
        services.detail.contains("nothing is running"),
        "{}",
        services.detail
    );
    assert!(line(&quiet, "ports").is_none());

    rig.probe.occupy(
        HTTP,
        PortOwner {
            pid: 4,
            name: Some("herd.exe".to_owned()),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, HTTP)),
        },
    );
    let taken = doctor(&mut rig, 0);
    let ports = line(&taken, "ports").unwrap();
    assert_eq!(ports.status, CheckStatus::Problem);
    assert!(
        ports
            .detail
            .contains("nginx: port 19080 is already in use by herd.exe (pid 4"),
        "{}",
        ports.detail
    );
    assert_eq!(
        ports.fix.as_deref(),
        Some(
            "in crisol.toml, set `http_port = 19081` under `[services.nginx]` (that port is free)"
        )
    );

    rig.probe.release(HTTP);
    rig.start_running();
    let running = doctor(&mut rig, 700);
    assert!(line(&running, "ports").is_none(), "Nginx's port is its own");
    assert_eq!(line(&running, "services").unwrap().detail, "running: nginx");
}

#[test]
fn doctor_says_which_service_failed_and_what_its_exit_code_means() {
    let mut rig = Rig::with_system(Arc::new(Explains));
    rig.with_mysql();
    rig.call(
        &Call::Start {
            service: Some("mysql".to_owned()),
        },
        0,
    )
    .unwrap();
    let mysqld = rig
        .supervisor
        .spawned()
        .into_iter()
        .find(|r| r.spec.program.ends_with("mysqld.exe"))
        .unwrap();
    rig.supervisor.exit(mysqld.process, -1_073_741_515).unwrap();
    rig.tick(300);
    assert_eq!(rig.service_state("mysql").as_deref(), Some("failed"));
    let found = doctor(&mut rig, 400);
    let services = line(&found, "services").unwrap();
    assert_eq!(services.status, CheckStatus::Problem);
    assert!(
        services
            .detail
            .contains("mysql exited during startup with code -1073741515 (a DLL is missing)"),
        "{}",
        services.detail
    );
    assert_eq!(services.fix.as_deref(), Some("crisol logs mysql"));
    assert!(line(&found, "ports").is_none(), "MySQL's port is free");
}

#[test]
fn doctor_measures_how_long_a_database_took_to_be_ready_and_says_when_it_was_slow() {
    for (ready_at, slow) in [(700, false), (12_500, true)] {
        let mut rig = Rig::with_system(Arc::new(Explains));
        rig.with_mysql();
        assert!(line(&doctor(&mut rig, 0), "database startup").is_none());
        rig.call(
            &Call::Start {
                service: Some("mysql".to_owned()),
            },
            0,
        )
        .unwrap();
        rig.tick(ready_at - 100);
        rig.listen_mysql();
        // The health check runs every so often: the start ends at the first one after this.
        let mut now = ready_at;
        while rig.service_state("mysql").as_deref() != Some("running") {
            assert!(now < ready_at + 2000, "{:?}", rig.service_state("mysql"));
            rig.tick(now);
            now += 100;
        }
        let found = doctor(&mut rig, now);
        let startup = line(&found, "database startup").unwrap();
        if slow {
            assert_eq!(startup.status, CheckStatus::Warning);
            assert!(startup.detail.contains("mysql 1"), "{}", startup.detail);
            assert!(startup.detail.contains("antivirus"), "{}", startup.detail);
            assert_eq!(startup.fix.as_deref(), Some("exclude data/"));
        } else {
            assert_eq!(startup.status, CheckStatus::Ok);
            assert!(
                startup.detail.starts_with("ready in mysql "),
                "{}",
                startup.detail
            );
            assert_eq!(startup.fix, None);
        }
    }
}

#[test]
fn doctor_adds_nothing_while_there_is_no_home() {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let mut rig = Rig::over(tmp, paths);
    let found = doctor(&mut rig, 0);
    let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["home"]);
}

#[test]
fn a_first_start_that_makes_the_data_is_not_measured() {
    let mut rig = Rig::new();
    rig.with_mysql();
    fs::remove_dir_all(rig.paths.data_for("mysql", "8.4").unwrap()).unwrap();
    rig.call(
        &Call::Start {
            service: Some("mysql".to_owned()),
        },
        0,
    )
    .unwrap();
    let init = rig.supervisor.spawned().last().unwrap().clone();
    fs::create_dir_all(
        rig.paths
            .data_for("mysql", "8.4.init")
            .unwrap()
            .join("mysql"),
    )
    .unwrap();
    rig.supervisor.exit(init.process, 0).unwrap();
    rig.tick(1000);
    rig.listen_mysql();
    let mut now = 1100;
    while rig.service_state("mysql").as_deref() != Some("running") {
        assert!(now < 4000, "{:?}", rig.service_state("mysql"));
        rig.tick(now);
        now += 100;
    }
    assert!(line(&doctor(&mut rig, now), "database startup").is_none());
}
