//! F9.1's acceptance test with the real thing: the official MySQL 8.4 and MariaDB 11.8 (and 10.11)
//! for Windows, installed from their archives, initialized once, started, queried, stopped
//! gracefully and started again over the same data. `DbStack` over Job Objects, a temporary
//! home, and ports of the 19000 range; nothing of Herd, Devaron or any other installation is
//! touched.
//!
//! Needs the archives in `CRISOL_TEST_DOWNLOADS`: `mysql-8.4.11-winx64.zip`,
//! `mariadb-11.8.9-winx64.zip` and `mariadb-10.11.19-winx64.zip`.
//!
//! ```text
//! CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-platform --test databases -- --ignored --nocapture
//! ```

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crisol_core::catalog::Catalog;
use crisol_core::dbservice::DbKind;
use crisol_core::dbstack::DbStack;
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::{install, InstallOptions};
use crisol_core::paths::CrisolPaths;
use crisol_core::service::ServiceState;
use crisol_platform::{JobObjectSupervisor, WindowsDbAdmin, WindowsPortProbe, WindowsPrivateDirs};

/// The tests of this file share ports, so they run one at a time.
static PORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn install_from_downloads(paths: &CrisolPaths, service: &str, version: &str) {
    let folder = PathBuf::from(std::env::var_os("CRISOL_TEST_DOWNLOADS").expect("set it"));
    let catalog = Catalog::embedded().unwrap();
    let package = catalog
        .packages()
        .iter()
        .find(|p| p.service == service && p.version.to_string() == version)
        .unwrap();
    let fetcher = FakeFetcher::new();
    let file = folder.join(package.url.rsplit('/').next().unwrap());
    fetcher.serve(&package.url, fs::read(file).unwrap());
    install(
        package,
        paths,
        &fetcher,
        &InstallOptions::default(),
        &mut |_| {},
    )
    .unwrap();
}

struct World {
    tmp: tempfile::TempDir,
    paths: CrisolPaths,
    stack: DbStack,
}

fn world(config: &str) -> World {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    fs::write(paths.config_file(), config).unwrap();
    let stack = DbStack::new(
        paths.clone(),
        Arc::new(JobObjectSupervisor::new()),
        Arc::new(WindowsPortProbe::new()),
        Arc::new(WindowsPrivateDirs::new()),
    )
    .with_admin(Arc::new(WindowsDbAdmin));
    World { tmp, paths, stack }
}

impl World {
    fn tick_until(&mut self, what: &str, seconds: u64, done: impl Fn(&World) -> bool) {
        let end = Instant::now() + Duration::from_secs(seconds);
        while !done(self) {
            assert!(Instant::now() < end, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(200));
            let _ = self.stack.tick(Instant::now());
        }
    }

    fn state(&self, kind: DbKind) -> Option<ServiceState> {
        self.stack
            .states()
            .into_iter()
            .find(|(n, _)| n == kind.service())
            .map(|(_, s)| s)
    }

    /// The password Crisol generated for `kind`.
    fn password(&self, kind: DbKind) -> String {
        let config = crisol_core::config::Config::load_from(&self.paths)
            .unwrap()
            .config;
        let plan = crisol_core::dbservice::plan(&self.paths, &config, kind).unwrap();
        crisol_core::secrets::read_secret(&plan.secret_path(""))
            .unwrap()
            .expect("a password was generated")
            .expose()
            .to_owned()
    }

    /// Runs the SQL with the client of the installed version: with the password in the
    /// environment of the client (never in its arguments), or with none.
    fn try_sql(
        &self,
        kind: DbKind,
        version: &str,
        port: u16,
        sql: &str,
        password: Option<&str>,
    ) -> std::process::Output {
        let bin = self
            .paths
            .bin_for(kind.service(), version)
            .unwrap()
            .join("bin");
        let mut command;
        if kind == DbKind::Postgres {
            command = Command::new(bin.join("psql.exe"));
            command
                .args([
                    "-h",
                    "127.0.0.1",
                    "-U",
                    "postgres",
                    "-d",
                    "postgres",
                    "-tA",
                    "-w",
                ])
                .args(["-p", &port.to_string()])
                .arg("-c")
                .arg(sql)
                // Not the user's own pgpass.conf.
                .env("PGPASSFILE", self.tmp.path().join("no-pgpass"))
                .env_remove("PGPASSWORD");
            if let Some(password) = password {
                command.env("PGPASSWORD", password);
            }
        } else {
            command = Command::new(bin.join("mysql.exe"));
            command
                .args(["--protocol=tcp", "-h127.0.0.1", "-uroot", "-N", "-B"])
                .arg(format!("-P{port}"))
                .arg("-e")
                .arg(sql)
                .env_remove("MYSQL_PWD");
            if let Some(password) = password {
                command.env("MYSQL_PWD", password);
            }
        }
        command.output().unwrap()
    }

    /// Like [`try_sql`](Self::try_sql) with the generated password; fails if the SQL does.
    fn sql(&self, kind: DbKind, version: &str, port: u16, sql: &str) -> String {
        let password = self.password(kind);
        let out = self.try_sql(kind, version, port, sql, Some(&password));
        assert!(
            out.status.success(),
            "{sql}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
}

#[allow(clippy::too_many_lines)]
fn exercise(kind: DbKind, version: &str, port: u16, series: &str) {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let name = kind.service();
    let mut world = world(&format!(
        "[services.{name}]\nenabled = true\nport = {port}\n"
    ));
    install_from_downloads(&world.paths, name, version);

    // First start: the data is made once, then the server runs.
    let started = Instant::now();
    world.stack.start(kind, Instant::now()).unwrap();
    world.tick_until("the database to run", 240, |w| {
        matches!(
            w.state(kind),
            Some(ServiceState::Running | ServiceState::Failed(_))
        )
    });
    assert_eq!(world.state(kind), Some(ServiceState::Running));
    println!(
        "{name} {version}: first start (initialization included) took {:?}",
        started.elapsed()
    );
    let data = world.paths.data_for(name, series).unwrap();
    let marker = if kind == DbKind::Postgres {
        "base"
    } else {
        "mysql"
    };
    assert!(
        data.join(marker).is_dir(),
        "the data is in {}",
        data.display()
    );
    assert!(!world
        .paths
        .data_for(name, &format!("{series}.init"))
        .unwrap()
        .exists());

    // Without the password the connection is refused, from 127.0.0.1 too; with it, it works.
    let refused = world.try_sql(kind, version, port, "SELECT 1", None);
    let why = String::from_utf8_lossy(&refused.stderr).to_lowercase();
    assert!(
        !refused.status.success()
            && (why.contains("access denied")
                || why.contains("password")
                || why.contains("authentication")),
        "a connection without the password was not refused: {why}"
    );
    let wrong = world.try_sql(kind, version, port, "SELECT 1", Some("not-the-password"));
    assert!(!wrong.status.success(), "a wrong password was accepted");
    assert_eq!(world.sql(kind, version, port, "SELECT 1"), "1");
    if kind != DbKind::Postgres {
        // `INTO OUTFILE` works in the folder of this series and nowhere else.
        let outside = std::env::temp_dir().join("crisol-outfile-test.txt");
        let _ = fs::remove_file(&outside);
        let denied = world.try_sql(
            kind,
            version,
            port,
            &format!(
                "SELECT 1 INTO OUTFILE '{}'",
                outside.to_string_lossy().replace('\\', "/")
            ),
            Some(&world.password(kind)),
        );
        assert!(
            !denied.status.success() && !outside.exists(),
            "wrote outside secure_file_priv"
        );
        let files = world
            .paths
            .data_for(name, &format!("{series}-files"))
            .unwrap();
        world.sql(
            kind,
            version,
            port,
            &format!(
                "SELECT 1 INTO OUTFILE '{}'",
                files
                    .join("inside.txt")
                    .to_string_lossy()
                    .replace('\\', "/")
            ),
        );
        assert!(files.join("inside.txt").is_file());
    }
    let (setup, query) = if kind == DbKind::Postgres {
        (
            "CREATE TABLE crisol_t (n int); INSERT INTO crisol_t VALUES (42)",
            "SELECT n FROM crisol_t",
        )
    } else {
        (
            "CREATE DATABASE crisol_t; CREATE TABLE crisol_t.t (n INT); INSERT INTO crisol_t.t VALUES (42)",
            "SELECT n FROM crisol_t.t",
        )
    };
    world.sql(kind, version, port, setup);
    // It listens on the loopback only.
    let listeners = Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .output()
        .unwrap();
    let listeners = String::from_utf8_lossy(&listeners.stdout).into_owned();
    assert!(
        listeners
            .lines()
            .filter(|l| l.contains(&format!(":{port} ")) && l.contains("LISTENING"))
            .all(|l| l.contains("127.0.0.1")),
        "{listeners}"
    );

    // A graceful stop, timed.
    let asked = Instant::now();
    world.stack.stop(Some(kind), Instant::now());
    world.tick_until("the database to stop", 60, |w| w.stack.is_stopped());
    println!("{name} {version}: stop took {:?}", asked.elapsed());
    assert!(
        asked.elapsed() < Duration::from_secs(25),
        "the stop fell back to the kill"
    );

    // The second start finds the data and does not initialize again.
    world.stack.start(kind, Instant::now()).unwrap();
    world.tick_until("the database to run again", 120, |w| {
        matches!(
            w.state(kind),
            Some(ServiceState::Running | ServiceState::Failed(_))
        )
    });
    assert_eq!(world.state(kind), Some(ServiceState::Running));
    assert_eq!(world.sql(kind, version, port, query), "42");
    world.stack.stop(Some(kind), Instant::now());
    world.tick_until("the database to stop", 60, |w| w.stack.is_stopped());
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mysql-8.4.11-winx64.zip (Windows); slow"]
fn mysql_8_4_initializes_once_runs_persists_and_stops_gracefully() {
    exercise(DbKind::Mysql, "8.4.11", 19306, "8.4");
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mariadb-11.8.9-winx64.zip (Windows); slow"]
fn mariadb_11_8_initializes_once_runs_persists_and_stops_gracefully() {
    exercise(DbKind::Mariadb, "11.8.9", 19307, "11.8");
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mariadb-10.11.19-winx64.zip (Windows); slow"]
fn mariadb_10_11_does_the_same_in_its_own_data_series() {
    exercise(DbKind::Mariadb, "10.11.19", 19308, "10.11");
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with postgresql-18.6-1-windows-x64-binaries.zip (Windows); slow"]
fn postgresql_18_initializes_once_runs_persists_and_stops_gracefully() {
    exercise(DbKind::Postgres, "18.6", 19432, "18");
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with postgresql-17.11-1-windows-x64-binaries.zip (Windows); slow"]
fn postgresql_17_does_the_same_in_its_own_data_series() {
    exercise(DbKind::Postgres, "17.11", 19433, "17");
}

// ---- the simple data services (F9.4) -------------------------------------------------------

fn http(port: u16, method: &str, path: &str, body: &str, key: Option<&str>) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let auth = key.map_or_else(String::new, |key| {
        format!("Authorization: Bearer {key}\r\n")
    });
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    answer
}

fn run_and_wait(world: &mut World, kind: DbKind) {
    world.stack.start(kind, Instant::now()).unwrap();
    world.tick_until("the service to run", 120, |w| {
        matches!(
            w.state(kind),
            Some(ServiceState::Running | ServiceState::Failed(_))
        )
    });
    assert_eq!(
        world.state(kind),
        Some(ServiceState::Running),
        "{:?}",
        world.stack.failure(kind)
    );
}

fn stop_and_wait(world: &mut World, kind: DbKind) -> Duration {
    let asked = Instant::now();
    world.stack.stop(Some(kind), Instant::now());
    world.tick_until("the service to stop", 60, |w| w.stack.is_stopped());
    asked.elapsed()
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mailpit-windows-amd64.zip (Windows)"]
fn mailpit_catches_a_message_on_its_smtp_port_and_shows_it_on_its_web_port() {
    use std::io::{BufRead, BufReader, Write};
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.mailpit]\nenabled = true\nport = 19025\nsmtp_port = 19026\n");
    install_from_downloads(&world.paths, "mailpit", "1.31.6");
    run_and_wait(&mut world, DbKind::Mailpit);

    // A message through SMTP, the way PHP's mail() or Laravel would send it.
    let stream = std::net::TcpStream::connect(("127.0.0.1", 19026)).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("220"), "{line}");
    for command in [
        "HELO localhost\r\n",
        "MAIL FROM:<a@crisol.test>\r\n",
        "RCPT TO:<b@crisol.test>\r\n",
        "DATA\r\n",
    ] {
        writer.write_all(command.as_bytes()).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(
            line.starts_with('2') || line.starts_with('3'),
            "{command:?} -> {line}"
        );
    }
    writer
        .write_all(b"Subject: hola crisol\r\n\r\nbody\r\n.\r\nQUIT\r\n")
        .unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("250"), "{line}");

    let listed = http(19025, "GET", "/api/v1/messages", "", None);
    assert!(listed.contains("hola crisol"), "{listed}");
    // The messages survive a restart (they are in `data/mailpit/1/`).
    println!(
        "mailpit: stop took {:?}",
        stop_and_wait(&mut world, DbKind::Mailpit)
    );
    run_and_wait(&mut world, DbKind::Mailpit);
    assert!(http(19025, "GET", "/api/v1/messages", "", None).contains("hola crisol"));
    stop_and_wait(&mut world, DbKind::Mailpit);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with meilisearch-windows-amd64.exe (Windows); big"]
fn meilisearch_indexes_a_document_and_keeps_it_across_a_restart() {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.meilisearch]\nenabled = true\nport = 19700\n");
    install_from_downloads(&world.paths, "meilisearch", "1.55.0");
    run_and_wait(&mut world, DbKind::Meilisearch);
    let key = world.password(DbKind::Meilisearch);
    let key = key.as_str();
    assert!(http(19700, "GET", "/health", "", None).contains("available"));
    // Without the key the API says no; with it, yes.
    let refused = http(19700, "GET", "/indexes", "", None);
    assert!(
        refused.contains("401") || refused.contains("missing_authorization_header"),
        "{refused}"
    );
    let wrong = http(19700, "GET", "/indexes", "", Some("not-the-key"));
    assert!(
        wrong.contains("403") || wrong.contains("invalid_api_key"),
        "{wrong}"
    );
    let created = http(
        19700,
        "POST",
        "/indexes/films/documents",
        r#"[{"id":1,"title":"Crisol"}]"#,
        Some(key),
    );
    assert!(created.contains("taskUid"), "{created}");
    // Indexing is asynchronous: wait for the task.
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let found = http(
            19700,
            "POST",
            "/indexes/films/search",
            r#"{"q":"crisol"}"#,
            Some(key),
        );
        if found.contains("\"title\":\"Crisol\"") {
            break;
        }
        assert!(Instant::now() < end, "never indexed: {found}");
        std::thread::sleep(Duration::from_millis(300));
    }
    println!(
        "meilisearch: stop took {:?}",
        stop_and_wait(&mut world, DbKind::Meilisearch)
    );
    run_and_wait(&mut world, DbKind::Meilisearch);
    assert!(http(
        19700,
        "POST",
        "/indexes/films/search",
        r#"{"q":"crisol"}"#,
        Some(key)
    )
    .contains("\"title\":\"Crisol\""));
    stop_and_wait(&mut world, DbKind::Meilisearch);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mongodb-windows-x86_64-8.0.34.zip (Windows); very big"]
fn mongodb_asks_for_a_login_shuts_down_by_its_own_command_and_restarts_over_its_data() {
    use crisol_core::mongowire::{self, MongoError};
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world(
        "[services.mongodb]
enabled = true
port = 19017
",
    );
    install_from_downloads(&world.paths, "mongodb", "8.0.34");
    run_and_wait(&mut world, DbKind::Mongodb);
    let data = world.paths.data_for("mongodb", "8.0").unwrap();
    assert!(
        data.join("WiredTiger").is_file(),
        "{:?}",
        fs::read_dir(&data).map(Iterator::count)
    );
    let netstat = Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .output()
        .unwrap();
    let netstat = String::from_utf8_lossy(&netstat.stdout).into_owned();
    assert!(
        netstat
            .lines()
            .filter(|l| l.contains(":19017 ") && l.contains("LISTENING"))
            .all(|l| l.contains("127.0.0.1")),
        "{netstat}"
    );

    // The first user was made and the login is on: nothing is listed without it (code 13), and
    // the generated password logs in.
    let secrets = world.paths.secrets_dir();
    let password = fs::read_to_string(secrets.join("mongodb-8.0")).unwrap();
    let password = password.trim();
    assert!(!secrets.join("mongodb-8.0.pending").exists());
    match mongowire::list_databases_without_login(19017) {
        Err(MongoError::Server { code, .. }) => assert_eq!(code, 13),
        other => panic!("a MongoDB with no login answered: {other:?}"),
    }
    assert!(matches!(
        mongowire::shutdown(19017, "not-the-password"),
        Err(MongoError::Auth(_))
    ));
    std::net::TcpStream::connect(("127.0.0.1", 19017)).expect("a wrong password stops nothing");

    // Stopped by its own command: it goes within seconds, well before the grace period, and its
    // log says it was asked.
    let stopped_in = stop_and_wait(&mut world, DbKind::Mongodb);
    println!("mongodb: stop took {stopped_in:?}");
    assert!(stopped_in < Duration::from_secs(25), "{stopped_in:?}");
    let log = fs::read_to_string(world.paths.runtime_log("mongodb").unwrap()).unwrap_or_default();
    let asked: Vec<&str> = log
        .lines()
        .filter(|l| l.to_lowercase().contains("shutdown"))
        .collect();
    println!("mongodb log, shutdown lines: {}", asked.len());
    assert!(!asked.is_empty(), "the log never mentions a shutdown");

    // Over the same data, with the same password and no second first user.
    run_and_wait(&mut world, DbKind::Mongodb);
    assert_eq!(
        fs::read_to_string(secrets.join("mongodb-8.0")).unwrap(),
        password
    );
    assert!(matches!(
        mongowire::list_databases_without_login(19017),
        Err(MongoError::Server { code: 13, .. })
    ));
    stop_and_wait(&mut world, DbKind::Mongodb);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with memcached-1.6.8-win64-mingw.zip (Windows)"]
fn memcached_stores_and_returns_a_value_on_the_loopback_only() {
    use std::io::{BufRead, BufReader, Write};
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world(
        "[services.memcached]
enabled = true
port = 19211
",
    );
    install_from_downloads(&world.paths, "memcached", "1.6.8");
    run_and_wait(&mut world, DbKind::Memcached);

    let stream = std::net::TcpStream::connect(("127.0.0.1", 19211)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut line = String::new();
    write!(writer, "set crisol 0 0 5\r\nhello\r\n").unwrap();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "STORED");
    write!(writer, "get crisol\r\n").unwrap();
    let mut answer = String::new();
    for _ in 0..3 {
        line.clear();
        reader.read_line(&mut line).unwrap();
        answer.push_str(&line);
    }
    assert!(
        answer.contains("VALUE crisol 0 5") && answer.contains("hello"),
        "{answer}"
    );

    let netstat = Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .output()
        .unwrap();
    let netstat = String::from_utf8_lossy(&netstat.stdout).into_owned();
    assert!(
        netstat
            .lines()
            .filter(|l| l.contains(":19211 ") && l.contains("LISTENING"))
            .all(|l| l.contains("127.0.0.1")),
        "{netstat}"
    );
    println!(
        "memcached: stop took {:?}",
        stop_and_wait(&mut world, DbKind::Memcached)
    );
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mariadb-10.11.19-winx64.zip and mariadb-11.8.9-winx64.zip (Windows)"]
fn two_series_of_one_database_keep_their_data_apart_and_switching_loses_nothing() {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let config = |pin: &str| {
        format!("[services.mariadb]\nenabled = true\nport = 19309\nversion = \"{pin}\"\n")
    };
    let mut world = world(&config("10.11"));
    install_from_downloads(&world.paths, "mariadb", "10.11.19");
    install_from_downloads(&world.paths, "mariadb", "11.8.9");
    let kind = DbKind::Mariadb;

    // 10.11 makes a table.
    run_and_wait(&mut world, kind);
    world.sql(kind, "10.11.19", 19309, "CREATE DATABASE old_one; CREATE TABLE old_one.t (n INT); INSERT INTO old_one.t VALUES (10)");
    stop_and_wait(&mut world, kind);

    // The pin moves to 11.8: a new, empty series; the old one is not opened by the new server.
    fs::write(world.paths.config_file(), config("11.8")).unwrap();
    run_and_wait(&mut world, kind);
    assert_eq!(
        world.sql(
            kind,
            "11.8.9",
            19309,
            "SELECT COUNT(*) FROM information_schema.schemata WHERE schema_name = 'old_one'"
        ),
        "0"
    );
    world.sql(kind, "11.8.9", 19309, "CREATE DATABASE new_one");
    stop_and_wait(&mut world, kind);

    // Back to 10.11: its data is as it was, and 11.8's database is not in it.
    fs::write(world.paths.config_file(), config("10.11")).unwrap();
    run_and_wait(&mut world, kind);
    assert_eq!(
        world.sql(kind, "10.11.19", 19309, "SELECT n FROM old_one.t"),
        "10"
    );
    assert_eq!(
        world.sql(
            kind,
            "10.11.19",
            19309,
            "SELECT COUNT(*) FROM information_schema.schemata WHERE schema_name = 'new_one'"
        ),
        "0"
    );
    stop_and_wait(&mut world, kind);
    assert!(world.paths.data_for("mariadb", "10.11").unwrap().is_dir());
    assert!(world.paths.data_for("mariadb", "11.8").unwrap().is_dir());
}

// ---- Redis and Garnet (F9.3, ADR-0017) ----------------------------------------------------

/// One command of the Redis protocol over a plain socket, answer as text.
fn resp(port: u16, password: Option<&str>, args: &[&str]) -> String {
    use std::fmt::Write as _;
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut command = String::new();
    if let Some(password) = password {
        let _ = write!(
            command,
            "*2\r\n$4\r\nAUTH\r\n${}\r\n{password}\r\n",
            password.len()
        );
    }
    let _ = write!(command, "*{}\r\n", args.len());
    for arg in args {
        let _ = write!(command, "${}\r\n{arg}\r\n", arg.len());
    }
    stream.write_all(command.as_bytes()).unwrap();
    let mut buffer = [0_u8; 256];
    std::thread::sleep(Duration::from_millis(150));
    let read = stream.read(&mut buffer).unwrap_or(0);
    let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
    // With a password the answer to AUTH comes first: only the reply is returned.
    if password.is_some() {
        text.split_once("\r\n")
            .map_or(String::new(), |(_, rest)| rest.to_owned())
    } else {
        text
    }
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with Redis-x64-5.0.14.1.zip (Windows)"]
fn redis_5_runs_saves_on_shutdown_and_has_its_data_back() {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.redis]\nenabled = true\nport = 19379\n");
    install_from_downloads(&world.paths, "redis", "5.0.14.1");
    run_and_wait(&mut world, DbKind::Redis);
    let pw = world.password(DbKind::Redis);
    let pw = Some(pw.as_str());
    // Without the password every command is refused; with it they work.
    assert!(resp(19379, None, &["PING"]).starts_with("-NOAUTH"));
    assert!(!resp(19379, Some("not-it"), &["PING"]).starts_with("+PONG"));
    assert!(resp(19379, pw, &["PING"]).starts_with("+PONG"));
    assert!(resp(19379, pw, &["SET", "crisol", "42"]).starts_with("+OK"));
    let stopped = stop_and_wait(&mut world, DbKind::Redis);
    println!("redis: stop took {stopped:?}");
    assert!(
        stopped < Duration::from_secs(10),
        "the graceful stop fell back to the kill"
    );
    // `SHUTDOWN` saved the data, and the second start reads it.
    let data = world.paths.data_for("redis", "5.0").unwrap();
    assert!(
        data.join("dump.rdb").is_file(),
        "{:?}",
        fs::read_dir(&data).map(Iterator::count)
    );
    run_and_wait(&mut world, DbKind::Redis);
    assert!(resp(19379, pw, &["GET", "crisol"]).contains("42"));
    stop_and_wait(&mut world, DbKind::Redis);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with win-x64-based-readytorun.zip of Garnet (Windows)"]
fn garnet_answers_the_redis_protocol_on_the_loopback() {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.garnet]\nenabled = true\nport = 19380\n");
    install_from_downloads(&world.paths, "garnet", "2.2.3");
    run_and_wait(&mut world, DbKind::Garnet);
    let pw = world.password(DbKind::Garnet);
    let pw = Some(pw.as_str());
    assert!(resp(19380, None, &["PING"]).starts_with("-NOAUTH"));
    assert!(resp(19380, pw, &["PING"]).starts_with("+PONG"));
    assert!(resp(19380, pw, &["SET", "crisol", "42"]).starts_with("+OK"));
    assert!(resp(19380, pw, &["GET", "crisol"]).contains("42"));
    // What Laravel's queues need: Lua, lists and expiry.
    let lua = resp(
        19380,
        pw,
        &["EVAL", "return redis.call('set', KEYS[1], 'x')", "1", "k"],
    );
    println!("garnet EVAL answered: {lua:?}");
    // A difference with Redis, found here: a script that returns the status reply of `SET`
    // gets `+OK` from Redis and the bulk string `$2 OK` from Garnet (ADR-0017).
    assert!(lua.contains("OK"), "{lua}");
    assert!(resp(19380, pw, &["RPUSH", "q", "a"]).starts_with(":1"));
    assert!(resp(19380, pw, &["EXPIRE", "q", "100"]).starts_with(":1"));
    println!(
        "garnet: stop took {:?}",
        stop_and_wait(&mut world, DbKind::Garnet)
    );
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with both Redis and Garnet archives (Windows)"]
fn redis_and_garnet_cannot_run_together_on_6379_and_the_second_is_told_who_has_it() {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.redis]\nport = 19381\n[services.garnet]\nport = 19381\n");
    install_from_downloads(&world.paths, "redis", "5.0.14.1");
    install_from_downloads(&world.paths, "garnet", "2.2.3");
    run_and_wait(&mut world, DbKind::Redis);
    let error = world
        .stack
        .start(DbKind::Garnet, Instant::now())
        .unwrap_err();
    assert!(
        error.message.contains("redis is running on port 19381"),
        "{}",
        error.message
    );
    stop_and_wait(&mut world, DbKind::Redis);
    // Once Redis is down, Garnet may take the port.
    run_and_wait(&mut world, DbKind::Garnet);
    let pw = world.password(DbKind::Garnet);
    assert!(resp(19381, Some(pw.as_str()), &["PING"]).starts_with("+PONG"));
    stop_and_wait(&mut world, DbKind::Garnet);
}

// ---- reset-password (V3 of the manual verification of phase 9) ----------------------------

/// Changes the password of a running SQL database and checks that the old one stops working, the
/// new one works, the data is still there, the graceful stop still logs in, and the next start
/// uses the new one.
fn reset_sql(kind: DbKind, version: &str, port: u16) {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let name = kind.service();
    let mut world = world(&format!(
        "[services.{name}]\nenabled = true\nport = {port}\n"
    ));
    install_from_downloads(&world.paths, name, version);
    run_and_wait(&mut world, kind);
    let (setup, query) = if kind == DbKind::Postgres {
        (
            "CREATE TABLE crisol_t (n int); INSERT INTO crisol_t VALUES (42)",
            "SELECT n FROM crisol_t",
        )
    } else {
        (
            "CREATE DATABASE crisol_t; CREATE TABLE crisol_t.t (n INT); INSERT INTO crisol_t.t VALUES (42)",
            "SELECT n FROM crisol_t.t",
        )
    };
    world.sql(kind, version, port, setup);
    let old = world.password(kind);

    let message = world.stack.reset_password(kind).unwrap();
    println!("{name}: {message}");
    let new = world.password(kind);
    assert_ne!(old, new);
    assert!(!world
        .paths
        .secrets_dir()
        .join(format!("{name}-{}.next", kind.series(version)))
        .exists());

    // The old password is refused, the new one works, and nothing was lost.
    let refused = world.try_sql(kind, version, port, "SELECT 1", Some(&old));
    assert!(!refused.status.success(), "the old password still works");
    assert_eq!(world.sql(kind, version, port, query), "42");

    // The graceful stop logs in with the new one (it would be killed after the grace otherwise).
    let stopped = stop_and_wait(&mut world, kind);
    println!("{name}: stop took {stopped:?}");
    assert!(
        stopped < Duration::from_secs(25),
        "the stop fell back to the kill"
    );
    run_and_wait(&mut world, kind);
    assert_eq!(world.sql(kind, version, port, query), "42");
    stop_and_wait(&mut world, kind);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mysql-8.4.11-winx64.zip (Windows); slow"]
fn mysql_gets_a_new_password_while_running_and_keeps_its_data() {
    reset_sql(DbKind::Mysql, "8.4.11", 19306);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mariadb-11.8.9-winx64.zip (Windows); slow"]
fn mariadb_gets_a_new_password_while_running_and_keeps_its_data() {
    reset_sql(DbKind::Mariadb, "11.8.9", 19307);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with postgresql-18.6-1-windows-x64-binaries.zip (Windows); slow"]
fn postgresql_gets_a_new_password_while_running_and_keeps_its_data() {
    reset_sql(DbKind::Postgres, "18.6", 19432);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with Redis-x64-5.0.14.1.zip (Windows)"]
fn redis_gets_a_new_password_while_running_and_keeps_its_data() {
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.redis]\nenabled = true\nport = 19379\n");
    install_from_downloads(&world.paths, "redis", "5.0.14.1");
    run_and_wait(&mut world, DbKind::Redis);
    let old = world.password(DbKind::Redis);
    assert!(resp(19379, Some(&old), &["SET", "crisol", "42"]).starts_with("+OK"));

    world.stack.reset_password(DbKind::Redis).unwrap();
    let new = world.password(DbKind::Redis);
    assert_ne!(old, new);
    assert!(!resp(19379, Some(&old), &["PING"]).starts_with("+PONG"));
    assert!(resp(19379, Some(&new), &["GET", "crisol"]).contains("42"));

    // The graceful stop (`redis-cli` with the new one) saves, and the next start has it.
    let stopped = stop_and_wait(&mut world, DbKind::Redis);
    assert!(stopped < Duration::from_secs(10), "{stopped:?}");
    run_and_wait(&mut world, DbKind::Redis);
    assert!(!resp(19379, Some(&old), &["PING"]).starts_with("+PONG"));
    assert!(resp(19379, Some(&new), &["GET", "crisol"]).contains("42"));
    stop_and_wait(&mut world, DbKind::Redis);
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mongodb-windows-x86_64-8.0.34.zip (Windows); very big"]
fn mongodb_gets_a_new_password_while_running_and_still_stops_by_its_own_command() {
    use crisol_core::mongowire::{self, MongoError};
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.mongodb]\nenabled = true\nport = 19017\n");
    install_from_downloads(&world.paths, "mongodb", "8.0.34");
    run_and_wait(&mut world, DbKind::Mongodb);
    let old = world.password(DbKind::Mongodb);

    world.stack.reset_password(DbKind::Mongodb).unwrap();
    let new = world.password(DbKind::Mongodb);
    assert_ne!(old, new);
    assert!(matches!(
        mongowire::shutdown(19017, &old),
        Err(MongoError::Auth(_))
    ));
    std::net::TcpStream::connect(("127.0.0.1", 19017)).expect("a wrong password stops nothing");

    // The stop hook reads the password when it runs: it goes by its own command, not the kill.
    let stopped = stop_and_wait(&mut world, DbKind::Mongodb);
    assert!(stopped < Duration::from_secs(25), "{stopped:?}");
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with mongodb-windows-x86_64-8.0.34.zip (Windows); very big"]
fn mongodb_first_user_is_made_before_the_real_port_ever_opens() {
    use crisol_core::mongowire::{self, MongoError};
    let _ports = PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut world = world("[services.mongodb]\nenabled = true\nport = 19017\n");
    install_from_downloads(&world.paths, "mongodb", "8.0.34");
    world.stack.start(DbKind::Mongodb, Instant::now()).unwrap();
    // As soon as anything answers on the real port, the localhost exception must be gone: the
    // first user was made on the temporary port of the first start.
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let _ = world.stack.tick(Instant::now());
        if std::net::TcpStream::connect(("127.0.0.1", 19017)).is_ok() {
            break;
        }
        assert!(Instant::now() < end, "the real port never opened");
        std::thread::sleep(Duration::from_millis(20));
    }
    match mongowire::list_databases_without_login(19017) {
        Err(MongoError::Server { code, .. }) => assert_eq!(code, 13),
        other => panic!("the first user was still open when the real port opened: {other:?}"),
    }
    world.tick_until("mongodb to run", 60, |w| {
        w.state(DbKind::Mongodb) == Some(ServiceState::Running)
    });
    assert!(!world
        .paths
        .secrets_dir()
        .join("mongodb-8.0.pending")
        .exists());
    stop_and_wait(&mut world, DbKind::Mongodb);
}
