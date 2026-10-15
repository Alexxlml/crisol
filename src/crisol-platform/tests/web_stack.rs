//! F5.4's acceptance test with the real thing: PHP 7.4.33 and 8.4.26, Nginx, `WebStack`
//! over Job Objects. A site is created while everything runs; Nginx is reloaded without
//! cutting the site that was already there, and each site answers with its own PHP.
//!
//! Needs `php-7.4.33-nts-Win32-vc15-x64.zip`, `php-8.4.26-nts-Win32-vs17-x64.zip` and
//! `nginx-1.28.3.zip` in `CRISOL_TEST_DOWNLOADS`. Ports 19080 and 19100+; everything is
//! stopped at the end.
//!
//! ```text
//! CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-platform --test web_stack -- --ignored --nocapture
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
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::{install, InstallOptions};
use crisol_core::paths::CrisolPaths;
use crisol_core::phpini::{write_php_ini, PhpIniSettings};
use crisol_core::project::TrustAllOwners;
use crisol_core::sites::SitesFile;
use crisol_core::web::{plan_web, WebEvent, WebStack};
use crisol_platform::{JobObjectSupervisor, WindowsPortProbe};

const HTTP: u16 = 19080;

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

/// One HTTP/1.1 request to Nginx for `host`; the whole answer as text ("" if cut).
fn get(host: &str, path: &str) -> String {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", HTTP)) else {
        return String::new();
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let _ = write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    let mut body = String::new();
    let _ = stream.read_to_string(&mut body);
    body
}

fn site(paths: &CrisolPaths, name: &str, php_pin: Option<&str>) {
    let dir = paths.www_dir().join(name);
    fs::create_dir_all(&dir).unwrap();
    if let Some(pin) = php_pin {
        fs::write(dir.join(".php-version"), pin).unwrap();
    }
    fs::write(
        dir.join("index.php"),
        format!("<?php echo '{name} PHP ', PHP_VERSION;"),
    )
    .unwrap();
}

struct Run {
    stack: WebStack,
    events: Vec<WebEvent>,
}

impl Run {
    fn tick_for(&mut self, total: Duration) {
        let end = Instant::now() + total;
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
            let events = self.stack.tick(Instant::now());
            self.events.extend(events);
        }
    }

    fn tick_until(&mut self, what: &str, done: impl Fn(&Run) -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        while !done(self) {
            assert!(Instant::now() < end, "timed out waiting for: {what}");
            self.tick_for(Duration::from_millis(100));
        }
    }

    fn saw(&self, wanted: &WebEvent) -> bool {
        self.events.contains(wanted)
    }
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with PHP 7.4.33, PHP 8.4.26 and Nginx 1.28.3 (Windows)"]
#[allow(clippy::too_many_lines)]
fn a_site_added_while_running_is_served_with_its_own_php_without_cutting_the_others() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let nginx_dir = install_from_downloads(&paths, "nginx", "1.28.3");
    install_from_downloads(&paths, "php", "7.4.33");
    install_from_downloads(&paths, "php", "8.4.26");
    for version in ["7.4.33", "8.4.26"] {
        write_php_ini(&paths, version, &PhpIniSettings::default()).unwrap();
    }
    let mut config = Config::default();
    config.services.nginx.http_port = HTTP;
    config.php.base_port = 19100;
    config.php.workers = 2;
    let catalog = Catalog::embedded().unwrap();
    let nobody = |_: &str| None;
    let plan_now = || {
        plan_web(
            &paths,
            &config,
            &SitesFile::default(),
            &catalog,
            &TrustAllOwners,
            &nobody,
        )
    };

    // One old site pinned to PHP 7.4.
    site(&paths, "old", Some("7.4\n"));
    let stack = WebStack::new(
        paths.clone(),
        &config,
        nginx_dir,
        Arc::new(JobObjectSupervisor::new()),
        Arc::new(WindowsPortProbe::new()),
    );
    let mut run = Run {
        stack,
        events: Vec::new(),
    };
    let plan = plan_now();
    assert_eq!(plan.warnings, Vec::<String>::new());
    run.stack.apply(&plan, Instant::now()).unwrap();
    let started = run.stack.start(Instant::now());
    run.events.extend(started);
    run.tick_until("nginx and the 7.4 pool to be running", |r| {
        r.stack.nginx_state() == Some(&crisol_core::service::ServiceState::Running)
            && r.stack.pool_state("7.4.33") == Some(&crisol_core::service::ServiceState::Running)
    });

    let mut outcome = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = get("old.test", "/");
        assert!(first.contains("old PHP 7.4.33"), "{first}");

        // A new site appears; the default PHP is the recommended 8.4 line.
        site(&paths, "new", None);
        let plan = plan_now();
        let applied = run.stack.apply(&plan, Instant::now()).unwrap();
        assert_eq!(applied.new_pools, ["8.4.26"]);
        assert!(applied.reload_requested);

        // While Nginx is reloaded the old site never stops answering.
        let mut during = 0;
        while !run.saw(&WebEvent::Reloaded) {
            assert!(
                !run.events
                    .iter()
                    .any(|e| matches!(e, WebEvent::ReloadFailed { .. })),
                "{:?}",
                run.events
            );
            let answer = get("old.test", "/");
            assert!(
                answer.contains("old PHP 7.4.33"),
                "cut during reload: {answer}"
            );
            during += 1;
            run.tick_for(Duration::from_millis(100));
            assert!(during < 300, "the reload never finished");
        }
        outcome.push(format!(
            "{during} requests to the old site during the reload"
        ));

        // `nginx -s reload` returns once the signal is sent; the new workers take over a
        // moment later.
        let mut new = get("new.test", "/");
        for _ in 0..50 {
            if !new.is_empty() {
                break;
            }
            run.tick_for(Duration::from_millis(100));
            new = get("new.test", "/");
        }
        assert!(new.contains("new PHP 8.4.26"), "{new}");
        let old = get("old.test", "/");
        assert!(old.contains("old PHP 7.4.33"), "{old}");

        // The new site goes away: its pool is retired after the reload.
        fs::remove_dir_all(paths.www_dir().join("new")).unwrap();
        let plan = plan_now();
        let applied = run.stack.apply(&plan, Instant::now()).unwrap();
        assert_eq!(applied.retired_pools, ["8.4.26"]);
        run.events.clear();
        run.tick_until("the second reload", |r| r.saw(&WebEvent::Reloaded));
        run.tick_until("the 8.4 pool to be gone", |r| {
            r.stack.pool_state("8.4.26").is_none()
        });
        let mut gone = get("new.test", "/");
        for _ in 0..50 {
            if gone.is_empty() {
                break;
            }
            run.tick_for(Duration::from_millis(100));
            gone = get("new.test", "/");
        }
        assert_eq!(gone, "", "an unknown host is cut");
        assert!(get("old.test", "/").contains("old PHP 7.4.33"));
    }));

    // Stop before judging, so a failure leaves nothing running.
    run.stack.stop(Instant::now());
    let end = Instant::now() + Duration::from_secs(15);
    while !run.stack.is_stopped() && Instant::now() < end {
        run.tick_for(Duration::from_millis(100));
    }
    assert!(run.stack.is_stopped(), "the stack did not stop");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    for line in outcome {
        eprintln!("{line}");
    }
}
