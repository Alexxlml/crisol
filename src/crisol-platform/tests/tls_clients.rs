//! Do the clients that matter apply the CA's name constraints? (ADR-0013, review of phase 6,
//! finding 6.)
//!
//! The guarantee of the local CA is that a stolen key cannot vouch for a public site. That only
//! holds if each client enforces the name constraints of the root it trusts, which RFC 5280
//! leaves optional for a trust anchor. This test serves two certificates, both signed by a real
//! local CA, and asks every client to connect to each:
//!
//! - `ok.test`, a legitimate one (made by `crisol-core::tls`): must be **accepted**;
//! - `bank.example.com`, signed by the same CA with its key (what a thief could do): must be
//!   **refused**.
//!
//! The clients are the Windows `curl` (Schannel, trusting only `--cacert`), PHP's cURL (OpenSSL,
//! with the generated `cacert.pem`) and Node (OpenSSL, with `NODE_EXTRA_CA_CERTS`). Needs
//! `nginx-1.28.3.zip`, `php-7.4.33-nts-Win32-vc15-x64.zip`, `cacert-2026-09-25.pem` and
//! `node-v22.23.4-win-x64.zip` in `CRISOL_TEST_DOWNLOADS`. Ports 19450 and 19451.
//!
//! ```text
//! CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-platform --test tls_clients -- --ignored --nocapture
//! ```

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use crisol_core::ca::{ensure_ca, write_node_ca};
use crisol_core::cacert::write_php_cacert_with;
use crisol_core::catalog::Catalog;
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::{install, InstallOptions};
use crisol_core::paths::CrisolPaths;
use crisol_core::phpini::{write_php_ini, PhpIniSettings};
use crisol_core::tls::ensure_site_cert;
use crisol_platform::{WindowsPrivateDirs, WindowsRootCertificates};
use rcgen::{CertificateParams, Issuer, KeyPair};

const LEGIT_PORT: u16 = 19450;
const FORGED_PORT: u16 = 19451;

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

fn forward(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// What one client said about one server.
#[derive(Debug)]
struct Verdict {
    client: &'static str,
    legit: String,
    forged: String,
}

impl Verdict {
    fn accepted(answer: &str) -> bool {
        !answer.starts_with("FAIL")
    }
}

#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with Nginx, PHP 7.4, Node 22 and the CA bundle (Windows)"]
#[allow(clippy::too_many_lines)]
fn a_certificate_the_ca_signs_for_a_public_site_is_refused_by_every_client() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let nginx_dir = install_from_downloads(&paths, "nginx", "1.28.3");
    let php_dir = install_from_downloads(&paths, "php", "7.4.33");
    install_from_downloads(&paths, "cacert", "2026.9.25");
    let node_dir = install_from_downloads(&paths, "node", "22.23.4");

    // The CA, and the legitimate certificate for ok.test.
    let now = SystemTime::now();
    let ca = ensure_ca(&paths, &WindowsPrivateDirs, "test", now)
        .unwrap()
        .ca;
    ensure_site_cert(&paths, &ca, &["ok.test".to_owned()], now).unwrap();

    // What a thief of certs/ca.key could do: a certificate for a public site, signed with it.
    let ca_key = KeyPair::from_pem(&fs::read_to_string(paths.ca_key()).unwrap()).unwrap();
    let issuer =
        Issuer::from_ca_cert_pem(&fs::read_to_string(paths.ca_cert()).unwrap(), &ca_key).unwrap();
    let forged_key = KeyPair::generate().unwrap();
    let forged = CertificateParams::new(vec!["bank.example.com".to_owned()])
        .unwrap()
        .signed_by(&forged_key, &issuer)
        .unwrap();
    let work = tmp.path().join("work");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("forged.pem"), forged.pem()).unwrap();
    fs::write(work.join("forged.key"), forged_key.serialize_pem()).unwrap();

    // Two TLS servers, one per certificate.
    let conf = format!(
        "worker_processes 1;\nerror_log \"{work}/error.log\";\npid \"{work}/nginx.pid\";\n\
         events {{ worker_connections 64; }}\n\
         http {{\n  access_log off;\n\
         \x20 server {{ listen 127.0.0.1:{LEGIT_PORT} ssl; server_name ok.test;\n\
         \x20   ssl_certificate \"{legit}\"; ssl_certificate_key \"{legit_key}\";\n\
         \x20   return 200 \"legit\"; }}\n\
         \x20 server {{ listen 127.0.0.1:{FORGED_PORT} ssl; server_name bank.example.com;\n\
         \x20   ssl_certificate \"{work}/forged.pem\"; ssl_certificate_key \"{work}/forged.key\";\n\
         \x20   return 200 \"forged\"; }}\n}}\n",
        work = forward(&work),
        legit = forward(&paths.site_cert()),
        legit_key = forward(&paths.site_key()),
    );
    let conf_path = work.join("nginx.conf");
    fs::write(&conf_path, conf).unwrap();
    let nginx = |args: &[&str]| {
        Command::new(nginx_dir.join("nginx.exe"))
            .arg("-c")
            .arg(&conf_path)
            .arg("-p")
            .arg(&nginx_dir)
            .args(args)
            .output()
            .unwrap()
    };
    let check = nginx(&["-t"]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let mut server = Command::new(nginx_dir.join("nginx.exe"))
        .arg("-c")
        .arg(&conf_path)
        .arg("-p")
        .arg(&nginx_dir)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1500));

    // PHP's cURL gets the generated bundle (Mozilla's plus the local CA); Node gets the copy.
    write_php_cacert_with(
        &paths,
        &WindowsRootCertificates,
        Some(&ca),
        &crisol_core::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    write_php_ini(
        &paths,
        "7.4.33",
        &PhpIniSettings::default(),
        &crisol_core::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let node_ca = write_node_ca(&paths, &WindowsPrivateDirs, &ca).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut verdicts = Vec::new();

        // curl (Schannel), trusting only the local CA.
        let curl = |host: &str, port: u16| {
            let output = Command::new("curl")
                .args(["-sS", "--max-time", "15", "--ssl-no-revoke", "--cacert"])
                .arg(paths.ca_cert())
                .arg("--resolve")
                .arg(format!("{host}:{port}:127.0.0.1"))
                .arg(format!("https://{host}:{port}/"))
                .output()
                .unwrap();
            if output.status.success() {
                text(&output)
            } else {
                format!("FAIL: {}", String::from_utf8_lossy(&output.stderr).trim())
            }
        };
        verdicts.push(Verdict {
            client: "curl (Schannel)",
            legit: curl("ok.test", LEGIT_PORT),
            forged: curl("bank.example.com", FORGED_PORT),
        });

        // PHP's cURL (OpenSSL), with the generated cacert.pem.
        let php_script = "$c = curl_init('https://' . $argv[1] . ':' . $argv[2] . '/'); \
             curl_setopt_array($c, [CURLOPT_RESOLVE => [$argv[1] . ':' . $argv[2] . ':127.0.0.1'], \
             CURLOPT_RETURNTRANSFER => true, CURLOPT_TIMEOUT => 15]); \
             $b = curl_exec($c); echo $b === false ? 'FAIL: ' . curl_error($c) : $b;";
        let php = |host: &str, port: u16| {
            text(
                &Command::new(php_dir.join("php.exe"))
                    .env("PHPRC", paths.etc_for("php").unwrap().join("7.4.33"))
                    .args(["-r", php_script, "--", host])
                    .arg(port.to_string())
                    .output()
                    .unwrap(),
            )
        };
        verdicts.push(Verdict {
            client: "PHP cURL (OpenSSL)",
            legit: php("ok.test", LEGIT_PORT),
            forged: php("bank.example.com", FORGED_PORT),
        });

        // Node (OpenSSL), with NODE_EXTRA_CA_CERTS pointing at the validated copy.
        let node_script = "const https = require('https'); \
             const [host, port] = process.argv.slice(1); \
             https.get({ host, port: Number(port), path: '/', timeout: 15000, \
             lookup: (h, o, cb) => (o && o.all ? cb(null, [{ address: '127.0.0.1', family: 4 }]) : cb(null, '127.0.0.1', 4)) }, \
             (res) => { let b = ''; res.on('data', (d) => (b += d)); \
             res.on('end', () => console.log(b)); }) \
             .on('error', (e) => console.log('FAIL: ' + e.code + ' ' + e.message));";
        let node = |host: &str, port: u16| {
            text(
                &Command::new(node_dir.join("node.exe"))
                    .env("NODE_EXTRA_CA_CERTS", &node_ca)
                    .args(["-e", node_script, host])
                    .arg(port.to_string())
                    .output()
                    .unwrap(),
            )
        };
        verdicts.push(Verdict {
            client: "Node (OpenSSL)",
            legit: node("ok.test", LEGIT_PORT),
            forged: node("bank.example.com", FORGED_PORT),
        });

        for v in &verdicts {
            eprintln!(
                "{:<20} ok.test: {:<8} bank.example.com: {}",
                v.client, v.legit, v.forged
            );
        }
        for v in &verdicts {
            assert_eq!(
                v.legit, "legit",
                "{}: the legitimate site must work",
                v.client
            );
            assert!(
                !Verdict::accepted(&v.forged) && !v.forged.contains("forged"),
                "{} ACCEPTED a certificate the CA signed for bank.example.com: {}",
                v.client,
                v.forged
            );
        }
    }));

    let _ = nginx(&["-s", "stop"]);
    let _ = server.wait();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
