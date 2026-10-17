//! V2 of the manual verification of phase 9, with the real programs: with the daemon's core
//! already running, `enable mailpit` and `start` are answered only once Nginx runs the new
//! configuration, so a request right after reaches `mail.test` instead of the default server.
//! Before the fix that request got a closed connection. A temporary home, ports of the 19000
//! range.
//!
//! Needs `nginx-1.28.3.zip` and `mailpit-windows-amd64.zip` in `CRISOL_TEST_DOWNLOADS`.
//!
//! ```text
//! CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-platform --test reload_effective -- --ignored --nocapture
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
use crisol_core::engine::Engine;
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::{install, InstallOptions};
use crisol_core::paths::CrisolPaths;
use crisol_protocol::Call;

const HTTP: u16 = 19083;

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

fn get(host: &str) -> String {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", HTTP)) else {
        return String::new();
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let _ = write!(
        stream,
        "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    answer
}

fn settle(daemon: &mut Daemon, seconds: u64) {
    let end = Instant::now() + Duration::from_secs(seconds);
    loop {
        let _ = daemon.tick(Instant::now());
        if daemon.settled() {
            return;
        }
        assert!(Instant::now() < end, "never settled");
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with Nginx 1.28.3 and Mailpit (Windows)"]
fn enable_then_start_is_answered_when_the_running_nginx_already_serves_the_new_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    install_from_downloads(&paths, "nginx", "1.28.3");
    install_from_downloads(&paths, "mailpit", "1.31.6");
    fs::write(
        paths.config_file(),
        format!(
            "[services.nginx]\nhttp_port = {HTTP}\nhttps_port = 19447\n\n\
             [services.mailpit]\nport = 19029\nsmtp_port = 19030\n"
        ),
    )
    .unwrap();
    let mut daemon = Daemon::new(Engine::new(
        paths.clone(),
        crisol_platform::default_deps().unwrap(),
    ));
    let call = |daemon: &mut Daemon, call: Call| {
        daemon
            .handle(&call, Instant::now(), &mut |_| {})
            .unwrap_or_else(|e| panic!("{e}"))
    };

    // A cold start without Mailpit: Nginx runs and `mail.test` is not a name it knows.
    call(&mut daemon, Call::Start { service: None });
    settle(&mut daemon, 60);
    assert_eq!(get("mail.test"), "");

    // The way the owner did it by hand: enable, start, and ask at once.
    call(
        &mut daemon,
        Call::Enable {
            service: "mailpit".to_owned(),
        },
    );
    call(&mut daemon, Call::Start { service: None });
    settle(&mut daemon, 60);
    let page = get("mail.test");
    let _ = daemon.handle(&Call::Shutdown, Instant::now(), &mut |_| {});
    settle(&mut daemon, 60);
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
}
