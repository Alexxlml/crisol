//! The generated `php.ini` and `cacert.pem` for real (review of the manual verification of
//! phase 9, V1): the daemon's core starts Nginx, PHP 8.4 and MySQL 8.4 from a temporary home,
//! and a PHP page behind Nginx connects to MySQL with PDO using the generated password and
//! fetches an HTTPS site with cURL. Before the fix PHP ran without extensions and both failed
//! (`could not find driver`, no cURL).
//!
//! Needs the official archives of Nginx 1.28.3, PHP 8.4.26, MySQL 8.4.11 and the CA bundle in
//! `CRISOL_TEST_DOWNLOADS`, and access to the Internet for the HTTPS request. Ports of the
//! 19000 range; the home is temporary.
//!
//! ```text
//! CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-platform --test php_extensions -- --ignored --nocapture
//! ```

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crisol_core::catalog::Catalog;
use crisol_core::daemon::Daemon;
use crisol_core::dbservice::{plan, DbKind};
use crisol_core::engine::Engine;
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::{install, InstallOptions};
use crisol_core::paths::CrisolPaths;
use crisol_core::secrets::read_secret;
use crisol_protocol::Call;

const HTTP: u16 = 19080;
const MYSQL: u16 = 19306;

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

fn get(host: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", HTTP)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(40)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    answer
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with Nginx, PHP 8.4, MySQL 8.4 and the CA bundle, and the Internet (Windows)"]
fn a_php_page_uses_pdo_against_mysql_and_curl_against_an_https_site() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    for (service, version) in [
        ("nginx", "1.28.3"),
        ("php", "8.4.26"),
        ("mysql", "8.4.11"),
        ("cacert", "2026.9.25"),
    ] {
        install_from_downloads(&paths, service, version);
    }
    fs::write(
        paths.config_file(),
        format!(
            "[services.nginx]\nhttp_port = {HTTP}\nhttps_port = 19443\n\n[php]\nbase_port = 19100\nworkers = 2\n\n\
             [services.mysql]\nenabled = true\nport = {MYSQL}\n"
        ),
    )
    .unwrap();
    let www = paths.www_dir().join("app");
    fs::create_dir_all(&www).unwrap();

    let mut daemon = Daemon::new(Engine::new(
        paths.clone(),
        crisol_platform::default_deps().unwrap(),
    ));
    let started = Instant::now();
    daemon
        .handle(&Call::Start { service: None }, Instant::now(), &mut |_| {})
        .unwrap();
    let end = started + Duration::from_secs(180);
    while !daemon.settled() {
        assert!(Instant::now() < end, "never settled");
        std::thread::sleep(Duration::from_millis(200));
        let _ = daemon.tick(Instant::now());
    }
    let status = format!(
        "{:?}",
        daemon.handle(&Call::Status { service: None }, Instant::now(), &mut |_| {})
    );
    assert!(!status.contains("failed"), "{status}");

    // The password MySQL was initialised with, which only the page sees.
    let config = crisol_core::config::Config::load_from(&paths)
        .unwrap()
        .config;
    let mysql_plan = plan(&paths, &config, DbKind::Mysql).unwrap();
    let password = read_secret(&mysql_plan.secret_path(""))
        .unwrap()
        .expect("a password was generated");
    fs::write(
        www.join("index.php"),
        format!(
            "<?php\n$pdo = new PDO('mysql:host=127.0.0.1;port={MYSQL}', 'root', '{}');\n\
             echo 'pdo:', $pdo->query('SELECT 1+1')->fetchColumn(), \"\n\";\n\
             $c = curl_init('https://example.com/');\n\
             curl_setopt_array($c, [CURLOPT_RETURNTRANSFER => true, CURLOPT_TIMEOUT => 30]);\n\
             curl_exec($c);\n\
             echo 'curl:', curl_getinfo($c, CURLINFO_HTTP_CODE), ' ', curl_error($c), \"\n\";\n",
            password.expose()
        ),
    )
    .unwrap();

    let page = get("app.test", "/");
    let _ = daemon.handle(&Call::Shutdown, Instant::now(), &mut |_| {});
    let end = Instant::now() + Duration::from_secs(60);
    while !daemon.settled() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
        let _ = daemon.tick(Instant::now());
    }
    assert!(page.contains("pdo:2"), "{page}");
    assert!(page.contains("curl:200"), "{page}");
}
