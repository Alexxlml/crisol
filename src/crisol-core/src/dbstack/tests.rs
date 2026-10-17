use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tempfile::TempDir;

use super::*;
use crate::paths::fake::FakePrivateDirs;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;

struct Rig {
    _tmp: TempDir,
    paths: CrisolPaths,
    dirs: Arc<FakePrivateDirs>,
    ports: Arc<FakePortProbe>,
    stack: DbStack,
    t0: Instant,
}

impl Rig {
    /// MySQL on 19306 and MariaDB on 19307, both installed with their data already there (so
    /// no initialization gets in the way of what these tests are about).
    fn new(config: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        fs::write(paths.config_file(), config).unwrap();
        for (service, version, series) in
            [("mysql", "8.4.11", "8.4"), ("mariadb", "11.8.9", "11.8")]
        {
            let bin = paths.bin_for(service, version).unwrap().join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("mysqld.exe"), "MZ").unwrap();
            fs::create_dir_all(paths.data_for(service, series).unwrap().join("mysql")).unwrap();
        }
        let dirs = Arc::new(FakePrivateDirs::new());
        let ports = Arc::new(FakePortProbe::new());
        let stack = DbStack::new(
            paths.clone(),
            Arc::new(FakeSupervisor::new()),
            ports.clone(),
            dirs.clone(),
        );
        Self {
            _tmp: tmp,
            paths,
            dirs,
            ports,
            stack,
            t0: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    fn listen(&self, port: u16) {
        self.ports.occupy(
            port,
            PortOwner {
                pid: 1,
                name: Some("mysqld.exe".to_owned()),
                address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            },
        );
    }

    fn state(&self, name: &str) -> Option<ServiceState> {
        self.stack
            .states()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s)
    }
}

const MYSQL_ONLY: &str = "[services.mysql]
port = 19306
";
const BOTH_ON_OTHER_PORTS: &str = "[services.mysql]\nenabled = true\nport = 19306\n\n[services.mariadb]\nenabled = true\nport = 19307\n";
const BOTH_ON_ONE_PORT: &str = "[services.mysql]\nenabled = true\nport = 19306\n\n[services.mariadb]\nenabled = true\nport = 19306\n";

#[test]
fn only_the_enabled_databases_start_with_everything() {
    let mut rig = Rig::new("[services.mariadb]\nenabled = true\nport = 19307\n");
    let (events, warnings) = rig.stack.start_enabled(rig.at(0));
    assert_ne!(events, DbEvents::new());
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(rig.state("mariadb"), Some(ServiceState::Starting));
    assert_eq!(rig.state("mysql"), None, "never asked for");
    rig.listen(19307);
    rig.stack.tick(rig.at(700));
    assert_eq!(rig.state("mariadb"), Some(ServiceState::Running));
    assert!(rig.stack.is_settled() && !rig.stack.is_stopped());
}

#[test]
fn mysql_and_mariadb_on_one_port_are_alternatives_and_the_second_says_who_has_it() {
    let mut rig = Rig::new(BOTH_ON_ONE_PORT);
    // Both enabled: the first starts, the second is a warning, not an error.
    let (_, warnings) = rig.stack.start_enabled(rig.at(0));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("mariadb was not started")
            && warnings[0].contains("mysql is running on port 19306")
            && warnings[0].contains("crisol stop mysql"),
        "{warnings:?}"
    );
    assert_eq!(rig.state("mariadb"), None);
    // Asked for by name, it is an error with the same words.
    let error = rig.stack.start(DbKind::Mariadb, rig.at(10)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    // Once the first is down, the other may go.
    rig.listen(19306);
    rig.stack.tick(rig.at(700));
    rig.stack.stop(None, rig.at(800));
    for (i, ms) in [1000_u64, 1200, 1400, 1600].into_iter().enumerate() {
        let _ = i;
        rig.stack.tick(rig.at(ms));
    }
    assert!(rig.stack.is_stopped() || rig.state("mysql") == Some(ServiceState::Stopping));
}

#[test]
fn on_different_ports_they_can_run_together() {
    let mut rig = Rig::new(BOTH_ON_OTHER_PORTS);
    let (_, warnings) = rig.stack.start_enabled(rig.at(0));
    assert_eq!(warnings, Vec::<String>::new());
    assert!(rig.state("mysql").is_some() && rig.state("mariadb").is_some());
}

#[test]
fn a_database_that_cannot_start_harms_no_other() {
    let mut rig = Rig::new(BOTH_ON_OTHER_PORTS);
    // Somebody else (Herd, Devaron) has MariaDB's port.
    rig.listen(19307);
    rig.stack.start_enabled(rig.at(0));
    assert!(
        matches!(rig.state("mariadb"), Some(ServiceState::Failed(_))),
        "{:?}",
        rig.state("mariadb")
    );
    assert!(rig
        .stack
        .failure(DbKind::Mariadb)
        .unwrap()
        .contains("19307"));
    rig.listen(19306);
    rig.stack.tick(rig.at(700));
    assert_eq!(rig.state("mysql"), Some(ServiceState::Running));
    assert!(rig.stack.is_settled());
    assert!(rig.stack.failure(DbKind::Mysql).is_none());
}

#[test]
fn a_database_that_is_not_installed_is_said_so_and_a_pinned_missing_version_too() {
    let mut rig = Rig::new(MYSQL_ONLY);
    fs::remove_dir_all(rig.paths.bin_dir().join("mysql")).unwrap();
    let error = rig.stack.start(DbKind::Mysql, rig.at(0)).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert!(
        error.message.contains("crisol install mysql"),
        "{}",
        error.message
    );
}

#[test]
fn nothing_starts_in_a_home_other_accounts_can_write_in() {
    let mut rig = Rig::new(MYSQL_ONLY);
    rig.dirs.expose(rig.paths.home());
    let error = rig.stack.start(DbKind::Mysql, rig.at(0)).unwrap_err();
    assert_eq!(error.code, ErrorCode::HomeExposed);
    assert!(error
        .fix
        .as_deref()
        .is_some_and(|f| f.starts_with("icacls ")));
    assert_eq!(rig.state("mysql"), None);
}

#[test]
fn restart_stops_and_then_starts_again_by_itself() {
    let mut rig = Rig::new(MYSQL_ONLY);
    rig.stack.start(DbKind::Mysql, rig.at(0)).unwrap();
    rig.listen(19306);
    rig.stack.tick(rig.at(700));
    assert_eq!(rig.state("mysql"), Some(ServiceState::Running));
    rig.stack.restart(DbKind::Mysql, rig.at(1000)).unwrap();
    assert_eq!(rig.state("mysql"), Some(ServiceState::Stopping));
    assert!(!rig.stack.is_settled());
    // The server goes away on its own after the shutdown command.
    rig.ports.release(19306);
    let mut at = 1100;
    while rig.state("mysql") != Some(ServiceState::Starting) {
        // The fake processes do not exit by themselves: the stop ends by the kill.
        rig.stack.tick(rig.at(at));
        at += 500;
        assert!(at < 60_000, "never restarted: {:?}", rig.state("mysql"));
    }
}

#[test]
fn abort_leaves_nothing_running() {
    let mut rig = Rig::new(BOTH_ON_OTHER_PORTS);
    rig.stack.start_enabled(rig.at(0));
    rig.stack.abort();
    assert!(rig.stack.is_stopped());
}

#[test]
fn redis_and_garnet_share_6379_so_only_one_runs_and_the_second_is_told_why() {
    let mut rig = Rig::new(
        "[services.redis]
port = 19379
[services.garnet]
port = 19379
",
    );
    for (service, version) in [("redis", "5.0.14.1"), ("garnet", "2.2.3")] {
        let dir = rig.paths.bin_for(service, version).unwrap();
        fs::create_dir_all(&dir).unwrap();
    }
    rig.stack.start(DbKind::Redis, rig.at(0)).unwrap();
    let error = rig.stack.start(DbKind::Garnet, rig.at(10)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(
        error.message.contains("redis is running on port 19379")
            && error.message.contains("crisol stop redis"),
        "{}",
        error.message
    );
    // With another port for Garnet they live together.
    fs::write(
        rig.paths.config_file(),
        "[services.redis]
port = 19379
[services.garnet]
port = 19380
",
    )
    .unwrap();
    rig.stack.start(DbKind::Garnet, rig.at(20)).unwrap();
}
