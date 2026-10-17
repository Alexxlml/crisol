//! Mailpit behind Nginx with the real programs (review of phase 9, finding 2): `mail.test` goes
//! through Nginx to Mailpit; any other name, through Nginx or straight to Mailpit's port, gets
//! nothing. A temporary home and ports of the 19000 range; nothing of Herd or Devaron is touched.
//!
//! Needs `nginx-1.28.3.zip` and `mailpit-windows-amd64.zip` in `CRISOL_TEST_DOWNLOADS`.
//!
//! ```text
//! CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-platform --test mail_behind_nginx -- --ignored --nocapture
//! ```

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crisol_core::catalog::Catalog;
use crisol_core::config::Config;
use crisol_core::dbservice::DbKind;
use crisol_core::dbstack::DbStack;
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::{install, InstallOptions};
use crisol_core::paths::CrisolPaths;
use crisol_core::project::TrustAllOwners;
use crisol_core::service::ServiceState;
use crisol_core::sites::SitesFile;
use crisol_core::web::{plan_web, WebStack};
use crisol_platform::{JobObjectSupervisor, WindowsPortProbe, WindowsPrivateDirs};

const HTTP: u16 = 19082;
const MAILPIT: u16 = 19027;

fn install_from_downloads(paths: &CrisolPaths, service: &str, version: &str) -> PathBuf {
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
    .unwrap()
    .path
}

/// A GET with the `Host` given; the whole answer ("" if the connection was cut).
fn get(port: u16, host: &str, path: &str) -> String {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return String::new();
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let _ = write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    answer
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with Nginx 1.28.3 and Mailpit (Windows)"]
fn only_mail_test_reaches_mailpit_through_nginx_and_mailpit_answers_to_no_other_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    let nginx_dir = install_from_downloads(&paths, "nginx", "1.28.3");
    install_from_downloads(&paths, "mailpit", "1.31.6");
    let text = format!(
        "[services.nginx]\nhttp_port = {HTTP}\nhttps_port = 19445\n\n\
         [services.mailpit]\nenabled = true\nport = {MAILPIT}\nsmtp_port = 19028\n"
    );
    fs::write(paths.config_file(), &text).unwrap();
    let config = Config::load_from(&paths).unwrap().config;

    let catalog = Catalog::embedded().unwrap();
    let nobody = |_: &str| None;
    let plan = plan_web(
        &paths,
        &config,
        &SitesFile::default(),
        &catalog,
        &TrustAllOwners,
        &nobody,
    );
    assert_eq!(plan.mail.as_ref().unwrap().domain, "mail.test");

    let supervisor = Arc::new(JobObjectSupervisor::new());
    let probe = Arc::new(WindowsPortProbe::new());
    let dirs = Arc::new(WindowsPrivateDirs::new());
    let mut web = WebStack::new(
        paths.clone(),
        &config,
        nginx_dir,
        supervisor.clone(),
        probe.clone(),
        dirs.clone(),
    );
    let mut mail = DbStack::new(paths.clone(), supervisor, probe, dirs);
    web.apply(&plan, Instant::now()).unwrap();
    web.start(Instant::now());
    mail.start(DbKind::Mailpit, Instant::now()).unwrap();
    let end = Instant::now() + Duration::from_secs(40);
    loop {
        let _ = web.tick(Instant::now());
        let _ = mail.tick(Instant::now());
        let mail_up = mail
            .states()
            .iter()
            .any(|(n, s)| n == "mailpit" && *s == ServiceState::Running);
        if mail_up && web.nginx_state() == Some(&ServiceState::Running) {
            break;
        }
        assert!(Instant::now() < end, "never came up");
        std::thread::sleep(Duration::from_millis(150));
    }

    // Through Nginx under its name: the page and the API.
    let page = get(HTTP, "mail.test", "/api/v1/info");
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(get(HTTP, "mail.test", "/").starts_with("HTTP/1.1 200"));
    // Through Nginx under any other name: nothing (Nginx closes the connection).
    for host in ["evil.example", "127.0.0.1", "mail.test.evil.example"] {
        let answer = get(HTTP, host, "/api/v1/messages");
        assert!(
            !answer.contains("\"messages\"") && !answer.starts_with("HTTP/1.1 200"),
            "{host} reached Mailpit through Nginx: {answer}"
        );
    }
    // Straight to Mailpit's port under another name (DNS rebinding): refused by Mailpit itself.
    // (Under the loopback names (`127.0.0.1:<port>`, `localhost`), Mailpit does answer: that is a request typed
    // or sent from this computer, and a page cannot read it (its CORS header is empty).)
    for host in [
        "evil.example",
        "mail.test.evil.example",
        "mail.test.example",
    ] {
        let answer = get(MAILPIT, host, "/api/v1/messages");
        assert!(
            !answer.starts_with("HTTP/1.1 200"),
            "{host} reached Mailpit directly: {answer}"
        );
        println!("direct with Host {host}: {:?}", answer.lines().next());
    }

    web.stop(Instant::now());
    mail.stop(None, Instant::now());
    let end = Instant::now() + Duration::from_secs(30);
    while !(web.is_stopped() && mail.is_stopped()) {
        let _ = web.tick(Instant::now());
        let _ = mail.tick(Instant::now());
        assert!(Instant::now() < end, "never stopped");
        std::thread::sleep(Duration::from_millis(150));
    }
    let _ = Config::default();
}
