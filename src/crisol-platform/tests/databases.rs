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
use crisol_platform::{JobObjectSupervisor, WindowsPortProbe, WindowsPrivateDirs};

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
    _tmp: tempfile::TempDir,
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
    );
    World {
        _tmp: tmp,
        paths,
        stack,
    }
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

    /// Runs the SQL with the `mysql` client of the installed version, as root without a password.
    fn sql(&self, kind: DbKind, version: &str, port: u16, sql: &str) -> String {
        let bin = self
            .paths
            .bin_for(kind.service(), version)
            .unwrap()
            .join("bin");
        let mut command;
        if kind == DbKind::Postgres {
            command = Command::new(bin.join("psql.exe"));
            command
                .args(["-h", "127.0.0.1", "-U", "postgres", "-d", "postgres", "-tA"])
                .args(["-p", &port.to_string()])
                .arg("-c")
                .arg(sql);
        } else {
            command = Command::new(bin.join("mysql.exe"));
            command
                .args(["--protocol=tcp", "-h127.0.0.1", "-uroot", "-N", "-B"])
                .arg(format!("-P{port}"))
                .arg("-e")
                .arg(sql);
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "{sql}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
}

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

    // Root without a password, from this computer, and it persists.
    assert_eq!(world.sql(kind, version, port, "SELECT 1"), "1");
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
