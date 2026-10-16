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
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream};
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
use crisol_platform::{JobObjectSupervisor, WindowsPortProbe, WindowsPrivateDirs};

const HTTP: u16 = 19080;
const HTTPS: u16 = 19443;

/// The tests of this file use the same ports, so they run one at a time (cargo runs the tests
/// of a file in parallel).
static PORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ports() -> std::sync::MutexGuard<'static, ()> {
    PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
    let _ports = ports();
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
        Arc::new(WindowsPrivateDirs),
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

        // A mistake in a user override: the new configuration is rejected in the staging
        // folder, the files on disk stay as they were and the running sites are not touched.
        let overrides = paths.overrides_for("nginx").unwrap();
        fs::create_dir_all(&overrides).unwrap();
        fs::write(overrides.join("broken.conf"), "this is not nginx;\n").unwrap();
        let main = paths.etc_for("nginx").unwrap().join("nginx.conf");
        let before = fs::read(&main).unwrap();
        site(&paths, "third", None);
        let plan = plan_now();
        run.events.clear();
        run.stack.apply(&plan, Instant::now()).unwrap();
        run.tick_until("the rejected reload", |r| {
            r.events
                .iter()
                .any(|e| matches!(e, WebEvent::ReloadFailed { .. }))
        });
        assert_eq!(fs::read(&main).unwrap(), before, "etc/nginx/ is untouched");
        assert!(!paths
            .etc_for("nginx")
            .unwrap()
            .join("sites")
            .join("third.conf")
            .exists());
        assert!(get("old.test", "/").contains("old PHP 7.4.33"));
        assert_eq!(get("third.test", "/"), "", "the new site is not served");

        // The override is fixed: the same change now goes through.
        fs::remove_file(overrides.join("broken.conf")).unwrap();
        run.events.clear();
        run.stack.apply(&plan_now(), Instant::now()).unwrap();
        run.tick_until("the reload after the fix", |r| r.saw(&WebEvent::Reloaded));
        let mut third = get("third.test", "/");
        for _ in 0..50 {
            if !third.is_empty() {
                break;
            }
            run.tick_for(Duration::from_millis(100));
            third = get("third.test", "/");
        }
        assert!(third.contains("third PHP 8.4.26"), "{third}");
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

// ---- HTTPS (F6.2) -----------------------------------------------------------------------

/// `curl` (the one that ships with Windows, built on Schannel) trusting **only** `ca`, for a
/// name that is pointed at 127.0.0.1 without touching the hosts file.
fn curl(ca: Option<&std::path::Path>, resolve: &str, args: &[&str]) -> std::process::Output {
    let mut command = std::process::Command::new("curl");
    command
        .args([
            "-sS",
            "--max-time",
            "15",
            "--ssl-no-revoke",
            "--resolve",
            resolve,
        ])
        .args(args);
    if let Some(ca) = ca {
        command.arg("--cacert").arg(ca);
    }
    command.output().unwrap()
}

fn text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with PHP 7.4.33 and Nginx 1.28.3, and curl (Windows)"]
#[allow(clippy::too_many_lines)]
fn a_secured_site_is_served_over_https_to_a_client_that_trusts_only_the_local_ca() {
    let _ports = ports();
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let nginx_dir = install_from_downloads(&paths, "nginx", "1.28.3");
    let php_dir = install_from_downloads(&paths, "php", "7.4.33");
    install_from_downloads(&paths, "cacert", "2026.9.25");
    // PHP's cURL with Mozilla's bundle only: it does not know the local CA yet.
    crisol_core::cacert::write_php_cacert(&paths, &crisol_core::cacert::NoRoots).unwrap();
    write_php_ini(&paths, "7.4.33", &PhpIniSettings::default()).unwrap();
    let mut config = Config::default();
    config.services.nginx.http_port = HTTP;
    config.services.nginx.https_port = HTTPS;
    config.php.base_port = 19100;
    config.php.workers = 2;
    let catalog = Catalog::embedded().unwrap();

    for name in ["secure", "plain"] {
        let dir = paths.www_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("v.php"),
            format!(
                "<?php echo '{name} https=', $_SERVER['HTTPS'] ?? 'off', ' PHP ', PHP_VERSION;"
            ),
        )
        .unwrap();
    }
    crisol_core::sites::set_secure(&paths, "secure", true).unwrap();

    let nobody = |_: &str| None;
    let plan = plan_web(
        &paths,
        &config,
        &SitesFile::load(&paths).unwrap(),
        &catalog,
        &TrustAllOwners,
        &nobody,
    );
    let mut run = Run {
        stack: WebStack::new(
            paths.clone(),
            &config,
            nginx_dir,
            Arc::new(JobObjectSupervisor::new()),
            Arc::new(WindowsPortProbe::new()),
            Arc::new(WindowsPrivateDirs),
        ),
        events: Vec::new(),
    };
    let applied = run.stack.apply(&plan, Instant::now()).unwrap();
    assert!(applied.ca_created, "the first secured site makes the CA");
    assert_eq!(applied.warnings, Vec::<String>::new());
    let started = run.stack.start(Instant::now());
    run.events.extend(started);
    run.tick_until("nginx and the pool to be running", |r| {
        r.stack.nginx_state() == Some(&crisol_core::service::ServiceState::Running)
            && r.stack.pool_state("7.4.33") == Some(&crisol_core::service::ServiceState::Running)
    });

    let ca = paths.ca_cert();
    let on = format!("secure.test:{HTTPS}:127.0.0.1");
    let url = format!("https://secure.test:{HTTPS}");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // A client that trusts only the local CA: the page, with PHP told it is HTTPS.
        let page = curl(Some(&ca), &on, &[&format!("{url}/v.php")]);
        assert!(
            page.status.success(),
            "{}",
            String::from_utf8_lossy(&page.stderr)
        );
        assert_eq!(text(&page), "secure https=on PHP 7.4.33");

        // The same request without the CA: the certificate is not trusted by anything else.
        let untrusted = curl(None, &on, &[&format!("{url}/v.php")]);
        assert!(
            !untrusted.status.success(),
            "the system must not trust it yet"
        );
        assert!(!text(&untrusted).contains("PHP 7.4.33"));

        // The names Windows treats as the same file run the script over TLS too; the source
        // is never sent.
        for path in ["/v.PHP", "/v.Php", "/v.php.", "/v.php%20"] {
            let answer = curl(Some(&ca), &on, &[&format!("{url}{path}")]);
            let body = text(&answer);
            assert!(
                body.contains("PHP 7.4.33") && !body.contains("<?php"),
                "{path}: {body} {}",
                String::from_utf8_lossy(&answer.stderr)
            );
        }

        // Plain HTTP only redirects, with the port.
        let redirect = curl(
            None,
            &format!("secure.test:{HTTP}:127.0.0.1"),
            &["-I", &format!("http://secure.test:{HTTP}/v.php?x=1")],
        );
        let head = text(&redirect);
        assert!(head.starts_with("HTTP/1.1 302"), "{head}");
        assert!(
            head.contains(&format!("Location: https://secure.test:{HTTPS}/v.php?x=1")),
            "{head}"
        );

        // PHP's cURL: refuses the site until the local CA is in `cacert.pem`, then accepts it,
        // without the system store being involved (ADR-0013).
        let script = format!(
            "$c = curl_init('{url}/v.php'); curl_setopt_array($c, [CURLOPT_RESOLVE => \
             ['secure.test:{HTTPS}:127.0.0.1'], CURLOPT_RETURNTRANSFER => true, \
             CURLOPT_TIMEOUT => 15]); $b = curl_exec($c); \
             echo $b === false ? 'FAIL: ' . curl_error($c) : $b;"
        );
        let php = |script: &str| {
            let output = std::process::Command::new(php_dir.join("php.exe"))
                .env("PHPRC", paths.etc_for("php").unwrap().join("7.4.33"))
                .args(["-r", script])
                .output()
                .unwrap();
            text(&output)
        };
        let before = php(&script);
        assert!(
            before.starts_with("FAIL:"),
            "PHP must not trust it yet: {before}"
        );
        let local =
            crisol_core::ca::load_ca(&paths, &WindowsPrivateDirs, std::time::SystemTime::now())
                .unwrap()
                .unwrap();
        crisol_core::cacert::write_php_cacert_with(
            &paths,
            &crisol_core::cacert::NoRoots,
            Some(&local),
        )
        .unwrap();
        assert_eq!(php(&script), "secure https=on PHP 7.4.33");

        // A site that was not secured is not vouched for: the handshake is refused.
        let other = curl(
            Some(&ca),
            &format!("plain.test:{HTTPS}:127.0.0.1"),
            &[&format!("https://plain.test:{HTTPS}/v.php")],
        );
        assert!(!other.status.success() && text(&other).is_empty());
        // And over plain HTTP it still works.
        assert!(get("plain.test", "/v.php").contains("plain https=off PHP 7.4.33"));

        // Securing `plain` while an override is broken: Nginx rejects the pair under test, and
        // neither the certificate nor the configuration on disk change (review of phase 6,
        // finding 3). The sites keep working with the pair they had.
        let overrides = paths.overrides_for("nginx").unwrap();
        fs::create_dir_all(&overrides).unwrap();
        fs::write(overrides.join("broken.conf"), "this is not nginx;\n").unwrap();
        let cert_before = fs::read(paths.site_cert()).unwrap();
        let conf_before = fs::read(
            paths
                .etc_for("nginx")
                .unwrap()
                .join("sites")
                .join("plain.conf"),
        )
        .unwrap();
        crisol_core::sites::set_secure(&paths, "plain", true).unwrap();
        run.events.clear();
        let plan = plan_web(
            &paths,
            &config,
            &SitesFile::load(&paths).unwrap(),
            &catalog,
            &TrustAllOwners,
            &nobody,
        );
        run.stack.apply(&plan, Instant::now()).unwrap();
        run.tick_until("the rejected reload", |r| {
            r.events
                .iter()
                .any(|e| matches!(e, WebEvent::ReloadFailed { .. }))
        });
        assert_eq!(fs::read(paths.site_cert()).unwrap(), cert_before);
        assert_eq!(
            fs::read(
                paths
                    .etc_for("nginx")
                    .unwrap()
                    .join("sites")
                    .join("plain.conf")
            )
            .unwrap(),
            conf_before
        );
        let still = curl(Some(&ca), &on, &[&format!("{url}/v.php")]);
        assert_eq!(text(&still), "secure https=on PHP 7.4.33");

        // Fixed: the same change goes in and `plain` is now served over TLS too.
        fs::remove_file(overrides.join("broken.conf")).unwrap();
        run.events.clear();
        run.stack.apply(&plan, Instant::now()).unwrap();
        run.tick_until("the reload after the fix", |r| r.saw(&WebEvent::Reloaded));
        let plain_on = format!("plain.test:{HTTPS}:127.0.0.1");
        let mut page = String::new();
        for _ in 0..50 {
            page = text(&curl(
                Some(&ca),
                &plain_on,
                &[&format!("https://plain.test:{HTTPS}/v.php")],
            ));
            if !page.is_empty() {
                break;
            }
            run.tick_for(Duration::from_millis(100));
        }
        assert_eq!(page, "plain https=on PHP 7.4.33");
    }));

    run.stack.stop(Instant::now());
    let end = Instant::now() + Duration::from_secs(15);
    while !run.stack.is_stopped() && Instant::now() < end {
        run.tick_for(Duration::from_millis(100));
    }
    assert!(run.stack.is_stopped(), "the stack did not stop");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

// ---- review of phase 7, finding 2: Nginx listens on [::1] too ------------------------------

/// One HTTP/1.1 request to `address` for `host`; the whole answer as text ("" if cut).
fn get_at(address: SocketAddr, host: &str) -> String {
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_secs(5)) else {
        return String::new();
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let _ = write!(
        stream,
        "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    let mut body = String::new();
    let _ = stream.read_to_string(&mut body);
    body
}

/// Needs PHP 8.4.26 and Nginx 1.28.3. A program holding `[::1]:HTTP` keeps Nginx from starting
/// and is named in the failure; once it lets go, the same site answers on `[::1]` and on
/// `127.0.0.1`, which is where `shop.test` goes when a client tries IPv6 first.
#[allow(clippy::too_many_lines)]
#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with PHP 8.4.26 and Nginx 1.28.3 (Windows, IPv6 loopback)"]
fn a_program_on_ipv6_loopback_blocks_nginx_and_without_it_the_site_answers_on_both_addresses() {
    let _ports = ports();
    let ipv6 = SocketAddr::from((Ipv6Addr::LOCALHOST, HTTP));
    let Ok(squatter) = TcpListener::bind(ipv6) else {
        eprintln!("no IPv6 loopback on this machine: nothing to test");
        return;
    };

    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let nginx_dir = install_from_downloads(&paths, "nginx", "1.28.3");
    install_from_downloads(&paths, "php", "8.4.26");
    write_php_ini(&paths, "8.4.26", &PhpIniSettings::default()).unwrap();
    let mut config = Config::default();
    config.services.nginx.http_port = HTTP;
    config.php.base_port = 19100;
    config.php.workers = 1;
    site(&paths, "shop", None);
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
    let new_stack = || {
        WebStack::new(
            paths.clone(),
            &config,
            nginx_dir.clone(),
            Arc::new(JobObjectSupervisor::new()),
            Arc::new(WindowsPortProbe::new()),
            Arc::new(WindowsPrivateDirs),
        )
    };
    let stop = |run: &mut Run| {
        run.stack.stop(Instant::now());
        let end = Instant::now() + Duration::from_secs(15);
        while !run.stack.is_stopped() && Instant::now() < end {
            run.tick_for(Duration::from_millis(100));
        }
        assert!(run.stack.is_stopped(), "the stack did not stop");
    };

    // 1. Somebody else has [::1]:HTTP: Nginx does not start, and the failure names the port.
    let mut run = Run {
        stack: new_stack(),
        events: Vec::new(),
    };
    run.stack.apply(&plan, Instant::now()).unwrap();
    let started = run.stack.start(Instant::now());
    run.events.extend(started);
    // The configuration is tested and promoted first; only then Nginx is started and fails.
    run.tick_until("nginx to fail or run", |r| {
        r.events
            .iter()
            .any(|e| matches!(e, WebEvent::ReloadFailed { .. }))
            || matches!(
                r.stack.nginx_state(),
                Some(
                    crisol_core::service::ServiceState::Failed(_)
                        | crisol_core::service::ServiceState::Running
                )
            )
    });
    // `nginx -t` fails because the port is not free; the reason names who has it.
    let reason = run
        .events
        .iter()
        .find_map(|e| match e {
            WebEvent::ReloadFailed { reason } => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let busy = reason.contains(&format!("port {HTTP} is already in use by"))
        && reason.contains(&ipv6.to_string())
        && run.stack.nginx_state() != Some(&crisol_core::service::ServiceState::Running);
    let state = format!("{:?} {reason}", run.stack.nginx_state());
    stop(&mut run);
    assert!(busy, "{state}");
    // The squatter was not asked anything: nothing connected to it.
    squatter.set_nonblocking(true).unwrap();
    assert!(
        squatter.accept().is_err(),
        "a request reached the other program"
    );
    drop(squatter);

    // 2. With the port free, the site answers on both loopback addresses.
    let mut run = Run {
        stack: new_stack(),
        events: Vec::new(),
    };
    run.stack.apply(&plan, Instant::now()).unwrap();
    let started = run.stack.start(Instant::now());
    run.events.extend(started);
    run.tick_until("nginx and the pool to be running", |r| {
        r.stack.nginx_state() == Some(&crisol_core::service::ServiceState::Running)
            && r.stack.pool_state("8.4.26") == Some(&crisol_core::service::ServiceState::Running)
    });
    let both = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let v6 = get_at(ipv6, "shop.test");
        let v4 = get_at(SocketAddr::from(([127, 0, 0, 1], HTTP)), "shop.test");
        assert!(v6.contains("shop PHP 8.4.26"), "[::1]: {v6}");
        assert!(v4.contains("shop PHP 8.4.26"), "127.0.0.1: {v4}");
        assert_eq!(
            get_at(ipv6, "unknown.test"),
            "",
            "an unknown host is cut on [::1] too"
        );
    }));
    stop(&mut run);
    if let Err(panic) = both {
        std::panic::resume_unwind(panic);
    }
}
