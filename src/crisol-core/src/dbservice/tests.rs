use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;
use crate::process::ProcessSpec;

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
        fs::write(bin.join("postgres.exe"), "MZ").unwrap();
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

#[test]
fn postgresql_data_is_split_by_major_and_listens_on_this_computer_only() {
    let mut rig = Rig::new();
    rig.config.services.postgresql.port = 19432;
    rig.install("postgresql", "17.11");
    rig.install("postgresql", "18.6");
    let latest = rig.plan(DbKind::Postgres);
    assert_eq!(latest.version.version.to_string(), "18.6");
    assert_eq!(latest.series, "18");
    assert_eq!(
        latest.data,
        rig.paths.home().join("data").join("postgresql").join("18")
    );
    assert!(
        latest.ini.ends_with("postgresql/18/postgresql.conf")
            || latest.ini.ends_with(r"postgresql\18\postgresql.conf")
    );
    rig.config.services.postgresql.version = Some("17".to_owned());
    let old = rig.plan(DbKind::Postgres);
    assert_eq!(
        (old.series.as_str(), old.version.version.to_string()),
        ("17", "17.11".to_owned())
    );
    assert_ne!(old.data, latest.data);

    let text = render_ini(&latest).unwrap();
    assert!(text.contains("listen_addresses = '127.0.0.1'"), "{text}");
    assert!(text.contains("port = 19432"), "{text}");
    assert!(text.contains("include_dir = '"), "{text}");
    assert!(
        !text.contains('\\') && !text.to_lowercase().contains("password"),
        "{text}"
    );
    // A path with a single quote cannot be written between single quotes.
    let mut odd = latest.clone();
    odd.overrides = PathBuf::from("C:/it's/overrides");
    assert!(render_ini(&odd).is_err());
}

#[test]
fn postgresql_initializes_with_initdb_and_runs_postgres_directly() {
    let mut rig = Rig::new();
    rig.config.services.postgresql.port = 19432;
    rig.install("postgresql", "17.11");
    let plan = rig.plan(DbKind::Postgres);
    let mut db = rig.service(DbKind::Postgres);
    db.start(rig.at(0));
    let init = rig.supervisor.spawned()[0].clone();
    assert!(
        init.spec.program.ends_with("initdb.exe"),
        "{:?}",
        init.spec.program
    );
    let args = words(&init.spec);
    assert!(
        args.contains(&"scram-sha-256".to_owned()) && args.contains(&"postgres".to_owned()),
        "{args:?}"
    );
    assert!(!args.contains(&"trust".to_owned()), "{args:?}");
    assert!(
        args.contains(&plan.staging.display().to_string()),
        "{args:?}"
    );
    fs::create_dir_all(plan.staging.join("base")).unwrap();
    rig.supervisor.exit(init.process, 0).unwrap();
    db.tick(rig.at(500));
    assert!(plan.data.join("base").is_dir());
    let server = rig.supervisor.spawned().last().unwrap().clone();
    assert!(server.spec.program.ends_with("postgres.exe"));
    let args = words(&server.spec);
    assert!(args.contains(&plan.data.display().to_string()), "{args:?}");
    assert!(
        args.contains(&format!("config_file={}", plan.ini.display())),
        "{args:?}"
    );
    // Ready only when `pg_isready` says so: an open port alone is not enough.
    rig.listen(19432);
    db.tick(rig.at(1200));
    let probe = rig.supervisor.spawned().last().unwrap().clone();
    assert!(
        probe.spec.program.ends_with("pg_isready.exe"),
        "{:?}",
        probe.spec.program
    );
    assert_eq!(*db.state(), ServiceState::Starting);
    rig.supervisor.exit(probe.process, 0).unwrap();
    db.tick(rig.at(1500));
    db.tick(rig.at(2200));
    assert_eq!(*db.state(), ServiceState::Running);
    // And it is stopped with `pg_ctl stop -m fast`.
    db.stop(rig.at(3000));
    let stop = rig.supervisor.spawned().last().unwrap().clone();
    assert!(stop.spec.program.ends_with("pg_ctl.exe"));
    let args = words(&stop.spec);
    assert!(
        args.contains(&"fast".to_owned()) && args.contains(&"stop".to_owned()),
        "{args:?}"
    );
}

fn started_args(
    rig: &Rig,
    kind: DbKind,
    version_dir: (&str, &str),
) -> (Vec<String>, DbPlan, DbService) {
    rig.install(version_dir.0, version_dir.1);
    let plan = rig.plan(kind);
    let mut db = rig.service(kind);
    let before = rig.supervisor.spawned().len();
    db.start(rig.at(0));
    let spawned = rig.supervisor.spawned();
    assert_eq!(
        spawned.len(),
        before + 1,
        "no initializer: the server starts at once"
    );
    (words(&spawned[before].spec), plan, db)
}

#[test]
fn mongodb_mailpit_and_meilisearch_start_straight_from_an_empty_folder_on_this_computer_only() {
    let mut rig = Rig::new();
    rig.config.services.mongodb.port = 19017;
    rig.config.services.mailpit.port = 19025;
    rig.config.services.mailpit.smtp_port = 19026;
    rig.config.services.meilisearch.port = 19700;

    let (mongo, plan, _db) = started_args(&rig, DbKind::Mongodb, ("mongodb", "8.0.34"));
    assert_eq!(plan.series, "8.0");
    assert!(plan.data.is_dir(), "the empty data folder was made");
    assert!(!plan.ini.exists(), "no configuration file of ours");
    assert!(
        mongo.windows(2).any(|w| w == ["--bind_ip", "127.0.0.1"]),
        "{mongo:?}"
    );
    assert!(
        mongo.windows(2).any(|w| w == ["--port", "19017"]),
        "{mongo:?}"
    );
    assert!(
        mongo.contains(&plan.data.display().to_string()),
        "{mongo:?}"
    );

    let (mail, plan, _db) = started_args(&rig, DbKind::Mailpit, ("mailpit", "1.31.6"));
    assert_eq!(plan.series, "1");
    assert!(
        mail.windows(2)
            .any(|w| w == ["--listen", "127.0.0.1:19025"]),
        "{mail:?}"
    );
    assert!(
        mail.windows(2).any(|w| w == ["--smtp", "127.0.0.1:19026"]),
        "{mail:?}"
    );
    assert!(
        mail.windows(2)
            .any(|w| w == ["--allowed-hosts", "mail.test"]),
        "Mailpit answers to the name Nginx serves it under and no other: {mail:?}"
    );
    assert!(
        mail.contains(&"--disable-version-check".to_owned()),
        "no phoning home: {mail:?}"
    );
    assert!(mail.iter().any(|a| a.ends_with("mailpit.db")), "{mail:?}");

    let (meili, plan, _db) = started_args(&rig, DbKind::Meilisearch, ("meilisearch", "1.55.0"));
    assert_eq!(
        plan.series, "1.55",
        "Meilisearch changes its data format between minors"
    );
    assert!(
        meili
            .windows(2)
            .any(|w| w == ["--http-addr", "127.0.0.1:19700"]),
        "{meili:?}"
    );
    assert!(
        meili.contains(&"--no-analytics".to_owned()),
        "ADR-0004: {meili:?}"
    );
}

#[test]
fn a_simple_service_is_ready_when_all_its_ports_listen_and_stops_by_ending_its_group() {
    let mut rig = Rig::new();
    rig.config.services.mailpit.port = 19025;
    rig.config.services.mailpit.smtp_port = 19026;
    let (_, _, mut db) = started_args(&rig, DbKind::Mailpit, ("mailpit", "1.31.6"));
    rig.listen(19025);
    db.tick(rig.at(700));
    assert_eq!(
        *db.state(),
        ServiceState::Starting,
        "the SMTP port is not up yet"
    );
    rig.listen(19026);
    db.tick(rig.at(1300));
    assert_eq!(*db.state(), ServiceState::Running);
    let processes = rig.supervisor.spawned().len();
    db.stop(rig.at(2000));
    assert_eq!(
        rig.supervisor.spawned().len(),
        processes,
        "no shutdown command to run"
    );
    rig.ports.release(19025);
    rig.ports.release(19026);
    db.tick(rig.at(2200));
    assert_eq!(*db.state(), ServiceState::Stopped);
}

#[test]
fn redis_and_garnet_listen_on_this_computer_only_and_redis_is_stopped_with_shutdown() {
    let mut rig = Rig::new();
    rig.config.services.redis.port = 19379;
    rig.config.services.garnet.port = 19380;

    let (redis, plan, mut db) = started_args(&rig, DbKind::Redis, ("redis", "5.0.14.1"));
    assert_eq!(plan.series, "5.0");
    // Everything, the password included, is in a private configuration file: the command line
    // is that file and nothing else.
    let conf = plan.secret_path(".conf");
    assert_eq!(redis, [conf.display().to_string()]);
    let text = fs::read_to_string(&conf).unwrap();
    for line in [
        "bind 127.0.0.1",
        "port 19379",
        "protected-mode yes",
        "requirepass ",
    ] {
        assert!(text.contains(line), "{line}: {text}");
    }
    assert!(
        text.contains(&format!(
            "dir \"{}\"",
            plan.data.display().to_string().replace('\\', "/")
        )),
        "{text}"
    );
    rig.listen(19379);
    db.tick(rig.at(700));
    assert_eq!(*db.state(), ServiceState::Running);
    db.stop(rig.at(1000));
    let helper = rig.supervisor.spawned().last().unwrap().clone();
    assert!(helper.spec.program.ends_with("redis-cli.exe"));
    assert!(words(&helper.spec).contains(&"shutdown".to_owned()));

    let (garnet, plan, _db) = started_args(&rig, DbKind::Garnet, ("garnet", "2.2.3"));
    assert_eq!(plan.series, "2.2");
    assert!(
        garnet.windows(2).any(|w| w == ["--bind", "127.0.0.1"]),
        "{garnet:?}"
    );
    assert!(
        garnet.windows(2).any(|w| w == ["--port", "19380"]),
        "{garnet:?}"
    );
    let program = rig
        .supervisor
        .spawned()
        .last()
        .unwrap()
        .spec
        .program
        .clone();
    assert!(
        program.ends_with("net10.0/GarnetServer.exe")
            || program.ends_with(r"net10.0\GarnetServer.exe"),
        "{program:?}"
    );
}

// ---- passwords (ADR-0016) --------------------------------------------------------------

fn password_of(rig: &Rig, kind: DbKind) -> String {
    crate::secrets::read_secret(&rig.plan(kind).secret_path(""))
        .unwrap()
        .expect("a password was generated")
        .expose()
        .to_owned()
}

/// Every file under `dir` (recursively) that holds `needle`.
fn files_with(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if fs::read(&path).is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle)) {
                found.push(path);
            }
        }
    }
    found
}

/// Brings `kind` up from nothing the way the daemon would, with the fakes.
fn bring_up(rig: &Rig, kind: DbKind, version: (&str, &str), port: u16) -> DbService {
    rig.install(version.0, version.1);
    let plan = rig.plan(kind);
    let mut db = rig.service(kind);
    db.start(rig.at(0));
    if kind.descriptor().initializer != Initializer::None {
        // The initializer made its data and left cleanly.
        let marker = if kind == DbKind::Postgres {
            "base"
        } else {
            "mysql"
        };
        fs::create_dir_all(plan.staging.join(marker)).unwrap();
        rig.supervisor.exit(rig.last_process(), 0).unwrap();
        db.tick(rig.at(500));
    }
    rig.listen(port);
    db.tick(rig.at(1200));
    // PostgreSQL waits for `pg_isready`.
    if kind == DbKind::Postgres {
        rig.supervisor.exit(rig.last_process(), 0).unwrap();
        db.tick(rig.at(1500));
        db.tick(rig.at(2200));
    }
    db
}

#[test]
fn a_generated_password_is_never_in_an_argument_a_spec_a_configuration_or_a_log() {
    let mut rig = Rig::new();
    let cases = [
        (DbKind::Mysql, ("mysql", "8.4.11"), 19306),
        (DbKind::Mariadb, ("mariadb", "11.8.9"), 19307),
        (DbKind::Postgres, ("postgresql", "17.11"), 19432),
        (DbKind::Redis, ("redis", "5.0.14.1"), 19379),
        (DbKind::Garnet, ("garnet", "2.2.3"), 19380),
        (DbKind::Meilisearch, ("meilisearch", "1.55.0"), 19700),
    ];
    rig.config.services.mysql.port = 19306;
    rig.config.services.mariadb.port = 19307;
    rig.config.services.postgresql.port = 19432;
    rig.config.services.redis.port = 19379;
    rig.config.services.garnet.port = 19380;
    rig.config.services.meilisearch.port = 19700;
    let mut running = Vec::new();
    for (kind, version, port) in cases {
        running.push(bring_up(&rig, kind, version, port));
        // The server's own log, where a password would be if the program echoed its options.
        let plan = rig.plan(kind);
        fs::create_dir_all(plan.runtime_log.parent().unwrap()).unwrap();
        fs::write(&plan.runtime_log, "started\n").unwrap();
    }
    for (kind, _, _) in cases {
        let password = password_of(&rig, kind);
        assert_eq!(password.len(), crate::secrets::PASSWORD_LENGTH, "{kind:?}");
        for record in rig.supervisor.spawned() {
            let spec = &record.spec;
            let visible = format!(
                "{} {} {:?} {:?} {:?}",
                spec.program.display(),
                words(spec).join(" "),
                spec.cwd,
                spec.log_file,
                spec
            );
            assert!(
                !visible.contains(&password),
                "the password of {kind:?} is in a spec: {visible}"
            );
        }
        // Nothing in the home but `secrets/` holds it.
        let elsewhere: Vec<PathBuf> = files_with(rig.paths.home(), &password)
            .into_iter()
            .filter(|f| !f.starts_with(rig.paths.secrets_dir()))
            .collect();
        assert_eq!(elsewhere, Vec::<PathBuf>::new(), "{kind:?}");
        assert!(
            !files_with(&rig.paths.secrets_dir(), &password).is_empty(),
            "{kind:?}: it is kept in secrets/"
        );
    }
    // The only place a password travels outside a file is the environment of a child: the key
    // of Meilisearch, and `redis-cli`'s when it is asked to stop Redis.
    let with_env: Vec<String> = rig
        .supervisor
        .spawned()
        .iter()
        .filter(|r| !r.spec.env.is_empty())
        .map(|r| {
            r.spec
                .program
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(with_env, ["meilisearch-windows-amd64.exe"]);
    running[3].stop(rig.at(3000));
    let stop = rig.supervisor.spawned().last().unwrap().clone();
    assert!(stop.spec.program.ends_with("redis-cli.exe"));
    assert_eq!(
        stop.spec.env,
        [(
            "REDISCLI_AUTH".into(),
            password_of(&rig, DbKind::Redis).into()
        )]
    );
}

#[test]
fn mysql_gets_its_password_from_a_private_init_file_that_is_deleted_afterwards() {
    let mut rig = Rig::new();
    rig.config.services.mysql.port = 19306;
    rig.install("mysql", "8.4.11");
    let plan = rig.plan(DbKind::Mysql);
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    let password = password_of(&rig, DbKind::Mysql);
    let init = rig.supervisor.spawned()[0].clone();
    let sql = plan.secret_path(".init.sql");
    assert!(words(&init.spec).contains(&format!("--init-file={}", sql.display())));
    let text = fs::read_to_string(&sql).unwrap();
    assert_eq!(
        text,
        format!("ALTER USER 'root'@'localhost' IDENTIFIED BY '{password}';\n")
    );
    assert!(sql.starts_with(rig.paths.secrets_dir()));
    fs::create_dir_all(plan.staging.join("mysql")).unwrap();
    rig.supervisor.exit(init.process, 0).unwrap();
    db.tick(rig.at(500));
    assert!(!sql.exists(), "spent, so deleted");
    // The server runs from the data that has the password; `mysqladmin` reads an options file.
    let server = words(&rig.supervisor.spawned().last().unwrap().spec);
    assert!(
        !server.iter().any(|a| a.contains("init-file")),
        "{server:?}"
    );
    rig.listen(19306);
    db.tick(rig.at(1200));
    db.stop(rig.at(2000));
    let admin = rig.supervisor.spawned().last().unwrap().clone();
    let args = words(&admin.spec);
    assert!(admin.spec.program.ends_with("mysqladmin.exe"));
    assert_eq!(
        args[0],
        format!(
            "--defaults-extra-file={}",
            plan.secret_path(".client.cnf").display()
        )
    );
    assert_eq!(args.last().map(String::as_str), Some("shutdown"));
    let options = fs::read_to_string(plan.secret_path(".client.cnf")).unwrap();
    assert!(
        options.contains(&format!("password=\"{password}\"")) && options.contains("port=19306"),
        "{options}"
    );
    // And the SQL cannot read or write files anywhere else.
    let ini = fs::read_to_string(&plan.ini).unwrap();
    assert!(ini.contains("secure_file_priv = \""), "{ini}");
    assert!(
        plan.files.is_dir() && ini.contains(&plan.files.display().to_string().replace('\\', "/"))
    );
}

#[test]
fn a_failed_or_cancelled_initialization_leaves_no_password_file_and_the_next_has_a_new_password() {
    let mut rig = Rig::new();
    rig.config.services.postgresql.port = 19432;
    rig.install("postgresql", "17.11");
    let plan = rig.plan(DbKind::Postgres);
    let mut db = rig.service(DbKind::Postgres);
    db.start(rig.at(0));
    let first = password_of(&rig, DbKind::Postgres);
    let pwfile = plan.secret_path(".pwfile");
    assert_eq!(fs::read_to_string(&pwfile).unwrap(), format!("{first}\n"));
    rig.supervisor.exit(rig.last_process(), 1).unwrap();
    db.tick(rig.at(300));
    assert!(matches!(db.state(), ServiceState::Failed(_)));
    assert!(
        !pwfile.exists(),
        "the password file is deleted when the initialization fails"
    );
    db.start(rig.at(1000));
    let second = password_of(&rig, DbKind::Postgres);
    assert_ne!(first, second, "a new initialization, a new password");
    db.stop(rig.at(1100));
    assert!(!pwfile.exists(), "and when it is cancelled");
}

#[test]
fn mariadb_carries_its_password_to_the_first_launch_in_a_file_that_goes_when_it_runs() {
    let mut rig = Rig::new();
    rig.config.services.mariadb.port = 19307;
    rig.install("mariadb", "11.8.9");
    let plan = rig.plan(DbKind::Mariadb);
    let mut db = rig.service(DbKind::Mariadb);
    db.start(rig.at(0));
    let password = password_of(&rig, DbKind::Mariadb);
    let init = rig.supervisor.spawned()[0].clone();
    assert!(
        !words(&init.spec).iter().any(|a| a.contains("password")),
        "install-db is not given it"
    );
    fs::create_dir_all(plan.staging.join("mysql")).unwrap();
    rig.supervisor.exit(init.process, 0).unwrap();
    db.tick(rig.at(500));
    let sql = plan.secret_path(".init.sql");
    let text = fs::read_to_string(&sql).unwrap();
    assert!(
        text.contains(&format!(
            "ALTER USER IF EXISTS 'root'@'127.0.0.1' IDENTIFIED BY '{password}'"
        )),
        "{text}"
    );
    let server = rig.supervisor.spawned().last().unwrap().clone();
    assert!(words(&server.spec).contains(&format!("--init-file={}", sql.display())));
    rig.listen(19307);
    db.tick(rig.at(1200));
    assert_eq!(*db.state(), ServiceState::Running);
    assert!(!sql.exists(), "the password is set, so the file is gone");
    // A later start does not ask for it again.
    db.stop(rig.at(2000));
    rig.supervisor.exit(server.process, 0).unwrap();
    rig.ports.release(19307);
    db.tick(rig.at(2500));
    db.start(rig.at(3000));
    let again = words(&rig.supervisor.spawned().last().unwrap().spec);
    assert!(!again.iter().any(|a| a.contains("init-file")), "{again:?}");
}

#[test]
fn data_made_before_there_were_passwords_runs_as_it_was_made_and_the_credentials_say_so() {
    let mut rig = Rig::new();
    rig.config.services.mysql.port = 19306;
    rig.install("mysql", "8.4.11");
    let plan = rig.plan(DbKind::Mysql);
    fs::create_dir_all(plan.data.join("mysql")).unwrap();
    let mut db = rig.service(DbKind::Mysql);
    db.start(rig.at(0));
    assert!(
        !plan.secret_path("").exists(),
        "nothing is invented for data that exists"
    );
    rig.listen(19306);
    db.tick(rig.at(700));
    db.stop(rig.at(1000));
    let admin = words(&rig.supervisor.spawned().last().unwrap().spec);
    assert!(
        admin.contains(&"--user=root".to_owned()) && admin.contains(&"--port=19306".to_owned()),
        "{admin:?}"
    );
    let details = connection_details(&rig.paths, &rig.config, DbKind::Mysql).unwrap();
    assert!(
        details
            .notes
            .iter()
            .any(|n| n.contains("before Crisol generated passwords")),
        "{details:?}"
    );
    assert!(details
        .lines
        .contains(&("DB_PASSWORD".to_owned(), String::new())));
}

#[test]
fn the_credentials_are_the_lines_a_project_needs_in_its_env() {
    let mut rig = Rig::new();
    rig.config.services.redis.port = 19379;
    rig.config.services.meilisearch.port = 19700;
    rig.config.services.postgresql.port = 19432;
    let _redis = bring_up(&rig, DbKind::Redis, ("redis", "5.0.14.1"), 19379);
    let _meili = bring_up(&rig, DbKind::Meilisearch, ("meilisearch", "1.55.0"), 19700);
    let _pg = bring_up(&rig, DbKind::Postgres, ("postgresql", "17.11"), 19432);
    let get = |kind| {
        connection_details(&rig.paths, &rig.config, kind)
            .unwrap()
            .lines
    };
    let redis = get(DbKind::Redis);
    assert!(redis.contains(&("REDIS_HOST".to_owned(), "127.0.0.1".to_owned())));
    assert!(redis.contains(&("REDIS_PORT".to_owned(), "19379".to_owned())));
    assert!(redis.contains(&(
        "REDIS_PASSWORD".to_owned(),
        password_of(&rig, DbKind::Redis)
    )));
    let meili = get(DbKind::Meilisearch);
    assert!(meili.contains(&(
        "MEILISEARCH_HOST".to_owned(),
        "http://127.0.0.1:19700".to_owned()
    )));
    assert!(meili.contains(&(
        "MEILISEARCH_KEY".to_owned(),
        password_of(&rig, DbKind::Meilisearch)
    )));
    let pg = get(DbKind::Postgres);
    assert!(pg.contains(&("DB_CONNECTION".to_owned(), "pgsql".to_owned())));
    assert!(pg.contains(&("DB_USERNAME".to_owned(), "postgres".to_owned())));
    assert!(pg.contains(&(
        "DB_PASSWORD".to_owned(),
        password_of(&rig, DbKind::Postgres)
    )));
}

// ---- MongoDB: its own user and its own shutdown ------------------------------------------

#[derive(Default)]
struct FakeMongo {
    users: std::sync::Mutex<Vec<(u16, String)>>,
    shutdowns: std::sync::Mutex<Vec<(u16, Option<String>)>>,
    refuse_users: std::sync::atomic::AtomicBool,
}

impl MongoAdmin for FakeMongo {
    fn create_root_user(&self, port: u16, password: &Secret) -> Result<(), String> {
        if self.refuse_users.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("not authorized".to_owned());
        }
        self.users
            .lock()
            .unwrap()
            .push((port, password.expose().to_owned()));
        Ok(())
    }

    fn shutdown(&self, port: u16, password: Option<&Secret>) -> Result<(), String> {
        self.shutdowns
            .lock()
            .unwrap()
            .push((port, password.map(|p| p.expose().to_owned())));
        Ok(())
    }
}

fn mongo_rig() -> (Rig, Arc<FakeMongo>) {
    let mut rig = Rig::new();
    rig.config.services.mongodb.port = 19017;
    rig.install("mongodb", "8.0.34");
    (rig, Arc::new(FakeMongo::default()))
}

#[test]
fn mongodb_starts_with_auth_and_its_first_user_is_made_once_with_the_generated_password() {
    let (rig, admin) = mongo_rig();
    let mut db = rig.service(DbKind::Mongodb).with_mongo(admin.clone());
    db.start(rig.at(0));
    let password = password_of(&rig, DbKind::Mongodb);
    let spawned = rig.supervisor.spawned();
    let args = words(&spawned[0].spec);
    assert!(args.contains(&"--auth".to_owned()), "{args:?}");
    assert!(
        !args.iter().any(|a| a.contains(&password)),
        "the password is in the command line"
    );
    let pending = rig.plan(DbKind::Mongodb).secret_path(".pending");
    assert!(pending.is_file(), "the first user is owed until it exists");

    rig.listen(19017);
    db.tick(rig.at(1200));
    db.tick(rig.at(1500));
    assert_eq!(
        *admin.users.lock().unwrap(),
        [(19017, password)],
        "made once, with the generated password"
    );
    assert!(!pending.exists());
}

#[test]
fn a_mongodb_that_refuses_its_first_user_is_a_failed_start_that_says_so() {
    let (rig, admin) = mongo_rig();
    admin
        .refuse_users
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut db = rig.service(DbKind::Mongodb).with_mongo(admin);
    db.start(rig.at(0));
    rig.listen(19017);
    for step in 0..10 {
        db.tick(rig.at(1200 + step * 100));
    }
    match db.state() {
        ServiceState::Failed(FailureReason::Internal(why)) => {
            assert!(why.contains("administrator"), "{why}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn stopping_mongodb_asks_it_to_shut_down_logged_in_and_data_without_a_password_asks_without() {
    let (rig, admin) = mongo_rig();
    let mut db = rig.service(DbKind::Mongodb).with_mongo(admin.clone());
    db.start(rig.at(0));
    let password = password_of(&rig, DbKind::Mongodb);
    rig.listen(19017);
    db.tick(rig.at(1200));
    db.stop(rig.at(2000));
    // The hook runs in a thread of its own.
    let end = Instant::now() + Duration::from_secs(5);
    while admin.shutdowns.lock().unwrap().is_empty() {
        assert!(Instant::now() < end, "the shutdown was never asked for");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(*admin.shutdowns.lock().unwrap(), [(19017, Some(password))]);

    // Data made before there were passwords: no secret, no --auth, no login.
    let (rig, admin) = mongo_rig();
    fs::create_dir_all(rig.plan(DbKind::Mongodb).data).unwrap();
    let mut db = rig.service(DbKind::Mongodb).with_mongo(admin.clone());
    db.start(rig.at(0));
    let args = words(&rig.supervisor.spawned()[0].spec);
    assert!(!args.contains(&"--auth".to_owned()), "{args:?}");
    rig.listen(19017);
    db.tick(rig.at(1200));
    db.stop(rig.at(2000));
    let end = Instant::now() + Duration::from_secs(5);
    while admin.shutdowns.lock().unwrap().is_empty() {
        assert!(Instant::now() < end, "the shutdown was never asked for");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(*admin.shutdowns.lock().unwrap(), [(19017, None)]);
}

// ---- Memcached ---------------------------------------------------------------------------

#[test]
fn memcached_listens_on_this_computer_only_has_no_login_and_says_so() {
    let mut rig = Rig::new();
    rig.config.services.memcached.port = 19211;
    let (args, plan, _db) = started_args(&rig, DbKind::Memcached, ("memcached", "1.6.8"));
    assert_eq!(plan.series, "1.6");
    assert!(
        args.windows(2).any(|w| w == ["-l", "127.0.0.1"]),
        "{args:?}"
    );
    assert!(args.windows(2).any(|w| w == ["-p", "19211"]), "{args:?}");
    assert!(
        !args.contains(&"-Y".to_owned()),
        "no authentication file: this build has no SASL ({args:?})"
    );
    assert!(plan.secret_path("").parent().is_some_and(|d| !d.exists()));

    let shown = credentials(&plan, None);
    assert!(shown
        .lines
        .contains(&("MEMCACHED_HOST".to_owned(), "127.0.0.1".to_owned())));
    assert!(shown
        .lines
        .contains(&("MEMCACHED_PORT".to_owned(), "19211".to_owned())));
    assert!(
        shown.notes.iter().any(|n| n.contains("no login")),
        "{:?}",
        shown.notes
    );
}
