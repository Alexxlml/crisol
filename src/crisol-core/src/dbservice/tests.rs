use std::net::{Ipv4Addr, SocketAddr};

use tempfile::TempDir;

use super::*;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;

struct Rig {
    _tmp: TempDir,
    paths: CrisolPaths,
    config: Config,
    supervisor: Arc<FakeSupervisor>,
    ports: Arc<FakePortProbe>,
    t0: Instant,
}

fn words(spec: &ProcessSpec) -> Vec<String> {
    spec.args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

impl Rig {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let mut config = Config::default();
        config.services.mysql.port = 19306;
        config.services.mariadb.port = 19307;
        Self {
            _tmp: tmp,
            paths,
            config,
            supervisor: Arc::new(FakeSupervisor::new()),
            ports: Arc::new(FakePortProbe::new()),
            t0: Instant::now(),
        }
    }

    fn install(&self, service: &str, version: &str) {
        let bin = self.paths.bin_for(service, version).unwrap().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("mysqld.exe"), "MZ").unwrap();
    }

    fn plan(&self, kind: DbKind) -> DbPlan {
        plan(&self.paths, &self.config, kind).unwrap()
    }

    fn service(&self, kind: DbKind) -> DbService {
        DbService::new(self.plan(kind), self.supervisor.clone(), self.ports.clone())
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    /// The server comes up: the fake never opens a socket.
    fn listen(&self, port: u16) {
        self.ports.occupy(
            port,
            PortOwner {
                pid: 4242,
                name: Some("mysqld.exe".to_owned()),
                address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            },
        );
    }

    fn last_process(&self) -> ProcessId {
        self.supervisor.spawned().last().unwrap().process
    }
}

#[test]
fn the_plan_picks_the_highest_installed_or_the_pinned_version_and_splits_data_by_series() {
    let mut rig = Rig::new();
    assert!(matches!(
        plan(&rig.paths, &rig.config, DbKind::Mysql),
        Err(DbError::NotInstalled(_))
    ));
    rig.install("mysql", "8.4.9");
    rig.install("mysql", "8.4.11");
    rig.install("mariadb", "11.8.9");
    rig.install("mariadb", "10.11.19");
    let mysql = rig.plan(DbKind::Mysql);
    assert_eq!(mysql.version.version.to_string(), "8.4.11");
    assert_eq!(mysql.series, "8.4");
    assert_eq!(
        mysql.data,
        rig.paths.home().join("data").join("mysql").join("8.4")
    );
    assert_eq!(
        mysql.staging,
        rig.paths.home().join("data").join("mysql").join("8.4.init")
    );
    assert_eq!(
        mysql.ini,
        rig.paths
            .etc_for("mysql")
            .unwrap()
            .join("8.4")
            .join("my.ini")
    );
    // Two versions of MariaDB: each series has its own data.
    assert_eq!(rig.plan(DbKind::Mariadb).series, "11.8");
    rig.config.services.mariadb.version = Some("10.11".to_owned());
    let old = rig.plan(DbKind::Mariadb);
    assert_eq!(
        (old.series.as_str(), old.version.version.to_string()),
        ("10.11", "10.11.19".to_owned())
    );
    assert_ne!(old.data, rig.plan(DbKind::Mysql).data);
    rig.config.services.mariadb.version = Some("9.9".to_owned());
    assert!(matches!(
        plan(&rig.paths, &rig.config, DbKind::Mariadb),
        Err(DbError::NotInstalled(_))
    ));
}

#[test]
fn the_configuration_listens_on_this_computer_only_with_the_data_inside_the_home() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    rig.install("mariadb", "11.8.9");
    let text = render_ini(&rig.plan(DbKind::Mysql)).unwrap();
    assert!(text.contains("bind-address = 127.0.0.1"), "{text}");
    assert!(text.contains("port = 19306"), "{text}");
    assert!(text.contains("mysqlx = OFF"), "{text}");
    assert!(
        !text.contains("skip-name-resolve"),
        "root is root@localhost: {text}"
    );
    assert!(text.contains("!includedir"), "{text}");
    assert!(!text.contains('\\'), "forward slashes only: {text}");
    assert!(
        !text.to_lowercase().contains("password"),
        "no secrets in a file: {text}"
    );
    let home = rig.paths.home().to_string_lossy().replace('\\', "/");
    assert!(
        text.contains(&format!("datadir = \"{home}/data/mysql/8.4\"")),
        "{text}"
    );
    // MariaDB has no X protocol.
    let maria = render_ini(&rig.plan(DbKind::Mariadb)).unwrap();
    assert!(
        !maria.contains("mysqlx") && maria.contains("port = 19307"),
        "{maria}"
    );

    // A write is idempotent and leaves the user's folder alone.
    let plan = rig.plan(DbKind::Mysql);
    write_ini(&plan).unwrap();
    fs::write(
        plan.overrides.join("mine.ini"),
        "[mysqld]\nmax_connections = 50\n",
    )
    .unwrap();
    let before = fs::metadata(&plan.ini).unwrap().modified().unwrap();
    write_ini(&plan).unwrap();
    assert_eq!(fs::metadata(&plan.ini).unwrap().modified().unwrap(), before);
    assert!(plan.overrides.join("mine.ini").is_file());
}

#[test]
fn a_path_the_configuration_would_misread_is_refused_not_written() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let mut plan = rig.plan(DbKind::Mysql);
    plan.data = PathBuf::from("C:/a#b/data");
    let error = render_ini(&plan).unwrap_err();
    assert!(error.to_string().contains("a#b"), "{error}");
}

#[test]
fn the_first_start_initializes_into_a_staging_folder_and_then_runs_the_server() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let mut db = rig.service(DbKind::Mysql);
    let plan = rig.plan(DbKind::Mysql);
    let events = db.start(rig.at(0));
    assert_eq!(*db.state(), ServiceState::Starting);
    assert!(events.iter().any(|e| matches!(
        e,
        ServiceEvent::StateChanged {
            to: ServiceState::Starting,
            ..
        }
    )));
    let init = rig.supervisor.spawned().last().unwrap().clone();
    let args = words(&init.spec);
    assert!(
        args.iter().any(|a| a == "--initialize-insecure"),
        "{args:?}"
    );
    assert!(
        args.iter()
            .any(|a| a == &format!("--datadir={}", plan.staging.display())),
        "{args:?}"
    );
    assert!(plan.ini.is_file(), "the configuration is written before");
    assert!(!plan.data.exists());

    // Still initializing: nothing else is started.
    db.tick(rig.at(500));
    assert_eq!(rig.supervisor.spawned().len(), 1);
    assert!(!db.is_settled());

    // The initializer made its data and left cleanly.
    fs::create_dir_all(plan.staging.join("mysql")).unwrap();
    rig.supervisor.exit(init.process, 0).unwrap();
    db.tick(rig.at(1000));
    assert!(
        plan.data.join("mysql").is_dir(),
        "the staging folder became the data"
    );
    assert!(!plan.staging.exists());
    let server = rig.supervisor.spawned().last().unwrap().clone();
    assert_ne!(server.process, init.process);
    let args = words(&server.spec);
    assert_eq!(args[0], format!("--defaults-file={}", plan.ini.display()));
    assert!(!args.iter().any(|a| a.contains("initialize")), "{args:?}");

    rig.listen(19306);
    db.tick(rig.at(1700));
    assert_eq!(*db.state(), ServiceState::Running);
    assert!(db.is_settled());
}

#[test]
fn data_that_exists_is_never_initialized_again() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let plan = rig.plan(DbKind::Mysql);
    fs::create_dir_all(plan.data.join("mysql")).unwrap();
    fs::write(plan.data.join("mine.txt"), "keep").unwrap();
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    assert_eq!(rig.supervisor.spawned().len(), 1, "straight to the server");
    let args = words(&rig.supervisor.spawned()[0].spec);
    assert!(!args.iter().any(|a| a.contains("initialize")), "{args:?}");
    assert_eq!(
        fs::read_to_string(plan.data.join("mine.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn a_failed_initialization_leaves_no_data_that_could_pass_for_good_and_says_where_to_look() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let plan = rig.plan(DbKind::Mysql);
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    fs::create_dir_all(plan.staging.join("half")).unwrap();
    rig.supervisor.exit(rig.last_process(), 1).unwrap();
    db.tick(rig.at(300));
    let ServiceState::Failed(reason) = db.state().clone() else {
        panic!("{:?}", db.state())
    };
    let text = reason.to_string();
    assert!(
        text.contains("code 1") && text.contains("runtime.log"),
        "{text}"
    );
    assert!(!plan.data.exists() && !plan.staging.exists());
    assert_eq!(rig.supervisor.group_count(), 0, "the group was closed");

    // The next start tries again from nothing.
    db.start(rig.at(1000));
    assert_eq!(*db.state(), ServiceState::Starting);
    assert_eq!(rig.supervisor.spawned().len(), 2);
}

#[test]
fn an_initializer_that_succeeds_without_data_is_a_failure() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    rig.supervisor.exit(rig.last_process(), 0).unwrap();
    db.tick(rig.at(300));
    assert!(
        matches!(db.state(), ServiceState::Failed(_)),
        "{:?}",
        db.state()
    );
}

#[test]
fn a_leftover_staging_folder_is_cleared_and_stopping_cancels_the_initialization() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let plan = rig.plan(DbKind::Mysql);
    fs::create_dir_all(plan.staging.join("old")).unwrap();
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    assert!(!plan.staging.join("old").exists(), "the leftover is gone");
    fs::create_dir_all(plan.staging.join("new")).unwrap();
    db.stop(rig.at(100));
    assert_eq!(*db.state(), ServiceState::Stopped);
    assert!(!plan.staging.exists());
    assert_eq!(
        rig.supervisor.group_count(),
        0,
        "the group of the initializer was closed"
    );
}

#[test]
fn an_initialization_that_never_ends_fails_after_its_time() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    db.tick(rig.at(179_000));
    assert_eq!(*db.state(), ServiceState::Starting);
    db.tick(rig.at(181_000));
    assert!(matches!(db.state(), ServiceState::Failed(_)));
}

#[test]
fn mariadb_initializes_with_its_own_program_and_the_busy_port_is_named() {
    let rig = Rig::new();
    rig.install("mariadb", "11.8.9");
    let plan = rig.plan(DbKind::Mariadb);
    let mut db = rig.service(DbKind::Mariadb);
    db.start(rig.at(0));
    let init = rig.supervisor.spawned()[0].clone();
    assert!(
        init.spec.program.ends_with("mariadb-install-db.exe"),
        "{:?}",
        init.spec.program
    );
    fs::create_dir_all(plan.staging.join("mysql")).unwrap();
    rig.supervisor.exit(init.process, 0).unwrap();
    // Herd's MySQL (or anybody) has the port.
    rig.ports.occupy(
        19307,
        PortOwner {
            pid: 77,
            name: Some("mysqld.exe".to_owned()),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 19307)),
        },
    );
    db.tick(rig.at(300));
    let ServiceState::Failed(reason) = db.state().clone() else {
        panic!("{:?}", db.state())
    };
    assert!(reason.to_string().contains("19307"), "{reason}");
    assert_eq!(
        rig.supervisor.spawned().len(),
        1,
        "no server was started, nobody was killed"
    );
    assert!(plan.data.is_dir(), "the data it made is kept");
}

#[test]
fn stopping_a_running_database_asks_it_to_shut_down_first() {
    let rig = Rig::new();
    rig.install("mysql", "8.4.11");
    let plan = rig.plan(DbKind::Mysql);
    fs::create_dir_all(plan.data.join("mysql")).unwrap();
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    rig.listen(19306);
    db.tick(rig.at(700));
    assert_eq!(*db.state(), ServiceState::Running);
    let server = rig.last_process();
    db.stop(rig.at(1000));
    let helper = rig.supervisor.spawned().last().unwrap().clone();
    assert!(helper.spec.program.ends_with("mysqladmin.exe"));
    let args = words(&helper.spec);
    assert!(
        args.contains(&"shutdown".to_owned()) && args.contains(&"--port=19306".to_owned()),
        "{args:?}"
    );
    assert_eq!(*db.state(), ServiceState::Stopping);
    rig.supervisor.exit(server, 0).unwrap();
    rig.ports.release(19306);
    db.tick(rig.at(1500));
    assert_eq!(*db.state(), ServiceState::Stopped);
}
