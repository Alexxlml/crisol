//! Hand test of HTTPS in a real browser (F6): serves one secured site, `secure.test`, with the
//! local CA of `CRISOL_HOME`, until you press Enter.
//!
//! ```text
//! set CRISOL_HOME=D:\scratch\crisol-trust          (the folder `trust_manual` used)
//! set CRISOL_TEST_DOWNLOADS=<folder with nginx-1.28.3.zip>
//! cargo run -p crisol-platform --example serve_manual
//! ```
//!
//! It installs Nginx there if needed (from `CRISOL_TEST_DOWNLOADS`, checked against the
//! catalog's hash), makes the site folder `www\secure` with an `index.html`, secures it, starts
//! Nginx on 19080 (HTTP) and 19443 (HTTPS), and prints how to open it. It touches nothing
//! outside `CRISOL_HOME`: no hosts file, no trust store (it only *reads* the trust store to tell
//! you whether the CA is trusted). On Enter, or when it ends, Nginx stops.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crisol_core::ca::load_ca;
use crisol_core::catalog::Catalog;
use crisol_core::config::Config;
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::{install, InstallOptions};
use crisol_core::manage::list_installed;
use crisol_core::paths::CrisolPaths;
use crisol_core::project::TrustAllOwners;
use crisol_core::service::ServiceState;
use crisol_core::sites::{set_secure, SitesFile};
use crisol_core::trust::TrustStore;
use crisol_core::web::{plan_web, WebStack};
use crisol_platform::{JobObjectSupervisor, WindowsPortProbe, WindowsTrustStore};

const HTTP: u16 = 19080;
const HTTPS: u16 = 19443;

fn main() {
    if let Err(message) = run() {
        eprintln!("serve_manual: {message}");
        std::process::exit(1);
    }
}

fn nginx_dir(paths: &CrisolPaths) -> Result<PathBuf, String> {
    let installed = list_installed(paths).map_err(|e| e.to_string())?;
    if let Some(found) = installed.iter().rfind(|i| i.service == "nginx") {
        return Ok(found.path.clone());
    }
    let folder = std::env::var_os("CRISOL_TEST_DOWNLOADS")
        .map(PathBuf::from)
        .ok_or("Nginx is not installed there: set CRISOL_TEST_DOWNLOADS to the folder with nginx-1.28.3.zip")?;
    let catalog = Catalog::embedded().map_err(|e| e.to_string())?;
    let package = catalog
        .packages()
        .iter()
        .find(|p| p.service == "nginx" && p.version.to_string() == "1.28.3")
        .ok_or("the catalog has no nginx 1.28.3")?;
    let file = folder.join(package.url.rsplit('/').next().unwrap_or_default());
    let bytes = fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let fetcher = FakeFetcher::new();
    fetcher.serve(&package.url, bytes);
    println!("Installing Nginx 1.28.3 from {} …", file.display());
    let done = install(
        package,
        paths,
        &fetcher,
        &InstallOptions::default(),
        &mut |_| {},
    )
    .map_err(|e| e.to_string())?;
    Ok(done.path)
}

fn run() -> Result<(), String> {
    let home = std::env::var_os("CRISOL_HOME").ok_or("set CRISOL_HOME to the scratch folder")?;
    let paths = CrisolPaths::new(home).map_err(|e| e.to_string())?;
    let nginx = nginx_dir(&paths)?;

    let site = paths.www_dir().join("secure");
    fs::create_dir_all(&site).map_err(|e| e.to_string())?;
    fs::write(
        site.join("index.html"),
        "<!doctype html><meta charset=utf-8><title>Crisol HTTPS</title>\
         <h1>Crisol HTTPS works</h1><p>If you see this without a warning, the browser trusts \
         the local CA.</p>",
    )
    .map_err(|e| e.to_string())?;
    set_secure(&paths, "secure", true).map_err(|e| e.to_string())?;

    let mut config = Config::default();
    config.services.nginx.http_port = HTTP;
    config.services.nginx.https_port = HTTPS;
    let catalog = Catalog::embedded().map_err(|e| e.to_string())?;
    let nobody = |_: &str| None;
    let plan = plan_web(
        &paths,
        &config,
        &SitesFile::load(&paths).map_err(|e| e.to_string())?,
        &catalog,
        &TrustAllOwners,
        &nobody,
    );
    let mut stack = WebStack::new(
        paths.clone(),
        &config,
        nginx,
        Arc::new(JobObjectSupervisor::new()),
        Arc::new(WindowsPortProbe::new()),
    );
    let applied = stack
        .apply(&plan, Instant::now())
        .map_err(|e| e.to_string())?;
    for warning in applied.warnings.iter().chain(&plan.warnings) {
        // A site without PHP is expected here: it is a static page.
        println!("note: {warning}");
    }
    stack.start(Instant::now());
    let end = Instant::now() + Duration::from_secs(30);
    while stack.nginx_state() != Some(&ServiceState::Running) {
        if Instant::now() > end || matches!(stack.nginx_state(), Some(ServiceState::Failed(_))) {
            stack.stop(Instant::now());
            return Err(format!("Nginx did not start: {:?}", stack.nginx_state()));
        }
        std::thread::sleep(Duration::from_millis(150));
        for event in stack.tick(Instant::now()) {
            println!("{event:?}");
        }
    }

    let ca = load_ca(&paths, SystemTime::now())
        .map_err(|e| e.to_string())?
        .ok_or("no CA")?;
    let trusted = WindowsTrustStore::current_user_root()
        .and_then(|store| store.contains(ca.cert_der()))
        .map_err(|e| e.to_string())?;
    println!();
    println!(
        "CA {} — trusted by the current user: {trusted}",
        ca.info().common_name
    );
    println!("Serving https://secure.test:{HTTPS}/ (plain http://secure.test:{HTTP}/ redirects).");
    println!(
        "secure.test does not resolve by itself. Open it in a Chromium browser with a mapping,"
    );
    println!("in a throw-away profile, no hosts file needed:");
    println!();
    println!(
        "  Start-Process msedge -ArgumentList \"--user-data-dir=$env:TEMP\\crisol-edge\", '--no-first-run', '--host-resolver-rules=\"MAP secure.test 127.0.0.1\"', \"https://secure.test:{HTTPS}/\""
    );
    println!();
    println!("Press Enter to stop Nginx.");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);

    stack.stop(Instant::now());
    let end = Instant::now() + Duration::from_secs(15);
    while !stack.is_stopped() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(150));
        stack.tick(Instant::now());
    }
    println!("Stopped.");
    Ok(())
}
