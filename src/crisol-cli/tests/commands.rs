//! Every command of `crisol` against a daemon in a temporary `CRISOL_HOME` (F8.3).
//!
//! The daemon is the real one (`crisol_daemon::run`, over a real named pipe that only this
//! user can open), in a thread of the test; the CLI is the real `run`. What is faked is what is
//! behind them: the supervisor and the port probe are doubles, driven here by a thread that plays
//! Nginx (it passes `nginx -t`, "listens", and leaves when told to quit), and the downloads are a
//! fake fetcher serving real zip files. Nothing touches the real Crisol home, the registry, the
//! hosts file or the NRPT.

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{Cursor, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crisol_cli::{run, Context, Link, Remote};
use crisol_core::autostart::{Autostart, FakeAutostart};
use crisol_core::cacert::NoRoots;
use crisol_core::catalog::{Arch, Catalog, Os, Target};
use crisol_core::dbservice::NoAdmin;
use crisol_core::engine::{Deps, SystemChecks};
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::ipc::pipe_name;
use crisol_core::paths::fake::FakePrivateDirs;
use crisol_core::paths::CrisolPaths;
use crisol_core::ports::fake::FakePortProbe;
use crisol_core::ports::PortOwner;
use crisol_core::process::fake::FakeSupervisor;
use crisol_core::process::ProcessId;
use crisol_core::project::TrustAllOwners;
use crisol_core::setup::{FakeSetup, SetupReport, SystemSetup};
use crisol_core::userpath::fake::FakeUserPath;
use crisol_core::userpath::{PathValue, UserPath};
use crisol_core::web::AlwaysEffective;
use crisol_daemon::{run as run_daemon, Options, Timing};
use crisol_platform::connect_to_daemon;
use crisol_protocol::{Check, CheckStatus};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

const HTTP: u16 = 19080;

fn zip_of(executable: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(executable, SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"MZ binary").unwrap();
    writer.finish().unwrap().into_inner()
}

/// A catalog and a fetcher serving it.
fn world(builds: &[(&str, &str)]) -> (Catalog, FakeFetcher) {
    let fetcher = FakeFetcher::new();
    let mut text = String::from("schema = 1\nserial = 1\n");
    for (service, version) in builds {
        let bytes = zip_of(&format!("{service}.exe"));
        let hash = Sha256::digest(&bytes)
            .iter()
            .fold(String::new(), |mut out, b| {
                let _ = write!(out, "{b:02x}");
                out
            });
        let url = format!("https://example.invalid/{service}-{version}.zip");
        let _ = write!(
            text,
            "\n[[package]]\nservice = \"{service}\"\nversion = \"{version}\"\nos = \"windows\"\n\
             arch = \"x86_64\"\nurl = \"{url}\"\nsha256 = \"{hash}\"\nformat = \"zip\"\n\
             executable = \"{service}.exe\"\nsize = {}\n",
            bytes.len()
        );
        fetcher.serve(&url, bytes);
    }
    (Catalog::parse_for_tests(&text).unwrap(), fetcher)
}

/// One sign-in entry shared by every command of a test (each `run` builds a new context).
struct SharedAutostart(Arc<FakeAutostart>);

impl Autostart for SharedAutostart {
    fn current(&self) -> std::io::Result<Option<String>> {
        self.0.current()
    }

    fn set(&self, command: &str) -> std::io::Result<()> {
        self.0.set(command)
    }

    fn remove(&self) -> std::io::Result<bool> {
        self.0.remove()
    }
}

/// One fake setup shared by every command of a test.
struct SharedSetup(Arc<FakeSetup>);

impl SystemSetup for SharedSetup {
    fn install(&self, suffix: &str) -> std::io::Result<SetupReport> {
        self.0.install(suffix)
    }

    fn uninstall(&self, suffix: &str) -> std::io::Result<SetupReport> {
        self.0.uninstall(suffix)
    }
}

/// One user PATH shared by every command of a test.
struct SharedUserPath(Arc<FakeUserPath>);

impl UserPath for SharedUserPath {
    fn read(&self) -> std::io::Result<Option<PathValue>> {
        self.0.read()
    }

    fn write(&self, value: Option<&PathValue>) -> std::io::Result<()> {
        self.0.write(value)
    }

    fn system_path(&self) -> std::io::Result<Option<String>> {
        self.0.system_path()
    }

    fn expand(&self, text: &str) -> String {
        self.0.expand(text)
    }

    fn notify_changed(&self) -> std::io::Result<()> {
        self.0.notify_changed()
    }
}

struct Marked(Vec<Check>);

impl SystemChecks for Marked {
    fn checks(&self, _paths: &CrisolPaths) -> Vec<Check> {
        self.0.clone()
    }

    fn shutdown_warnings(&self, _paths: &CrisolPaths) -> Vec<String> {
        vec!["the local names stop resolving while the daemon is stopped".to_owned()]
    }
}

struct Env {
    _tmp: TempDir,
    paths: CrisolPaths,
    deps: Deps,
    pipe: String,
    dirs: Arc<FakePrivateDirs>,
    daemon: Arc<Mutex<Option<JoinHandle<std::io::Result<()>>>>>,
    daemon_exe: std::path::PathBuf,
    autostart: Arc<FakeAutostart>,
    user_path: Arc<FakeUserPath>,
    shim_exe: std::path::PathBuf,
    setup: Arc<FakeSetup>,
    driver_stop: Arc<AtomicBool>,
    driver: Option<JoinHandle<()>>,
}

impl Env {
    fn new(builds: &[(&str, &str)]) -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        fs::write(
            paths.config_file(),
            format!("[services.nginx]\nhttp_port = {HTTP}\nhttps_port = 19443\n\n[php]\nbase_port = 19100\n"),
        )
        .unwrap();
        let (catalog, fetcher) = world(builds);
        let supervisor = Arc::new(FakeSupervisor::new());
        let probe = Arc::new(FakePortProbe::new());
        let dirs = Arc::new(FakePrivateDirs::new());
        let deps = Deps {
            supervisor: supervisor.clone(),
            probe: probe.clone(),
            dirs: dirs.clone(),
            fetcher: Arc::new(fetcher),
            owners: Arc::new(TrustAllOwners),
            env: Arc::new(|_| None),
            system: Arc::new(Marked(vec![Check {
                name: "windows thing".to_owned(),
                status: CheckStatus::Warning,
                detail: "from the system".to_owned(),
                fix: None,
            }])),
            roots: Arc::new(NoRoots),
            config_probe: Arc::new(AlwaysEffective),
            db_admin: Arc::new(NoAdmin),
            catalog,
            target: Target {
                os: Os::Windows,
                arch: Arch::X86_64,
            },
        };
        let daemon_exe = tmp.path().join("crisold.exe");
        fs::write(&daemon_exe, b"MZ").unwrap();
        let shim_exe = tmp.path().join("crisol-shim.exe");
        fs::write(&shim_exe, b"MZ shim").unwrap();
        let driver_stop = Arc::new(AtomicBool::new(false));
        let driver = Some(play_nginx(supervisor, probe, Arc::clone(&driver_stop)));
        Self {
            pipe: pipe_name(paths.home(), "S-1-5-21-1-2-3-1001"),
            _tmp: tmp,
            paths,
            deps,
            dirs,
            daemon: Arc::new(Mutex::new(None)),
            daemon_exe,
            autostart: Arc::new(FakeAutostart::new()),
            user_path: Arc::new(FakeUserPath::with_user(r"C:\Tools;%USERPROFILE%\bin")),
            shim_exe,
            setup: Arc::new(FakeSetup::new()),
            driver_stop,
            driver,
        }
    }

    fn context(&self) -> Context {
        let pipe = self.pipe.clone();
        let (deps, daemon_deps) = (self.deps.clone(), self.deps.clone());
        let (paths, daemon_paths) = (self.paths.clone(), self.paths.clone());
        let (pipe_for_daemon, daemon_slot) = (self.pipe.clone(), Arc::clone(&self.daemon));
        Context {
            paths,
            cwd: self.paths.home().join("work"),
            deps: Box::new(move || Ok(deps.clone())),
            connect: Box::new(move || {
                let client = connect_to_daemon(&pipe)?;
                Ok(Box::new(Remote(client)) as Box<dyn Link>)
            }),
            // "Starting the daemon" is starting it in a thread of this test.
            spawn_daemon: Box::new(move || {
                let options = Options {
                    paths: daemon_paths.clone(),
                    deps: daemon_deps.clone(),
                    pipe: pipe_for_daemon.clone(),
                    dns: None,
                    timing: Timing::default(),
                };
                *daemon_slot.lock().unwrap() =
                    Some(std::thread::spawn(move || run_daemon(options)));
                Ok(())
            }),
            setup: Box::new(SharedSetup(Arc::clone(&self.setup))),
            autostart: Box::new(SharedAutostart(Arc::clone(&self.autostart))),
            daemon_exe: Some(self.daemon_exe.clone()),
            user_path: Box::new(SharedUserPath(Arc::clone(&self.user_path))),
            shim_exe: Some(self.shim_exe.clone()),
            is_system_program: Box::new(|name| name == "ping"),
            env: Box::new(|name| (name == "PROMPT").then(|| "$P$G".to_owned())),
            dirs: self.dirs.clone(),
            daemon_wait: Duration::from_secs(10),
            follow_every: Duration::from_millis(25),
            follow_rounds: Some(40),
        }
    }

    /// Runs `crisol <args>` and gives the exit code, stdout and stderr.
    fn run(&self, args: &[&str]) -> (u8, String, String) {
        let mut arguments = vec!["crisol".to_owned()];
        arguments.extend(args.iter().map(|a| (*a).to_owned()));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&arguments, &self.context(), &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(
            code,
            0,
            "crisol {args:?} failed\nstdout: {out}\nstderr: {err}\nprobe now: {:?} slot: {}",
            connect_to_daemon(&self.pipe).map(|_| ()),
            self.daemon.lock().unwrap().is_some()
        );
        out
    }

    fn failing(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(
            code, 1,
            "crisol {args:?} should fail\nstdout: {out}\nstderr: {err}"
        );
        format!("{out}{err}")
    }

    fn folder(&self, name: &str) -> String {
        let dir = self.paths.home().join("projects").join(name);
        fs::create_dir_all(&dir).unwrap();
        dir.display().to_string()
    }

    fn daemon_running(&self) -> bool {
        connect_to_daemon(&self.pipe).is_ok()
    }

    /// Makes sure no daemon is left behind.
    fn finish(&mut self) {
        if self.daemon_running() {
            let _ = self.run(&["shutdown"]);
        }
        if let Some(thread) = self.daemon.lock().unwrap().take() {
            let _ = thread.join();
        }
        self.driver_stop.store(true, Ordering::Relaxed);
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Plays Nginx behind the fake supervisor: a configuration test passes, the server "listens"
/// once launched, a reload passes, and it leaves when told to quit.
fn play_nginx(
    supervisor: Arc<FakeSupervisor>,
    probe: Arc<FakePortProbe>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut handled: HashSet<ProcessId> = HashSet::new();
        let mut main: Option<ProcessId> = None;
        while !stop.load(Ordering::Relaxed) {
            for record in supervisor.spawned() {
                if !handled.insert(record.process) {
                    continue;
                }
                if record.spec.program.to_string_lossy().contains("php-cgi") {
                    // A PHP worker: it listens on the port it was given.
                    if let Some(port) = record
                        .spec
                        .args
                        .get(1)
                        .and_then(|a| a.to_string_lossy().rsplit(':').next()?.parse().ok())
                    {
                        probe.occupy(
                            port,
                            PortOwner {
                                pid: record.pid,
                                name: Some("php-cgi.exe".to_owned()),
                                address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                            },
                        );
                    }
                    continue;
                }
                let args: Vec<String> = record
                    .spec
                    .args
                    .iter()
                    .skip(4)
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                if args == ["-t"] || args == ["-s", "reload"] {
                    let _ = supervisor.exit(record.process, 0);
                } else if args == ["-s", "quit"] {
                    let _ = supervisor.exit(record.process, 0);
                    if let Some(main) = main.take() {
                        let _ = supervisor.exit(main, 0);
                    }
                    probe.release(HTTP);
                } else if args.is_empty() {
                    main = Some(record.process);
                    probe.occupy(
                        HTTP,
                        PortOwner {
                            pid: record.pid,
                            name: Some("nginx.exe".to_owned()),
                            address: SocketAddr::from((Ipv4Addr::LOCALHOST, HTTP)),
                        },
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    })
}

// ---- versions ------------------------------------------------------------------------------

#[test]
fn install_list_and_uninstall() {
    let env = Env::new(&[("php", "8.4.26"), ("php", "7.4.33"), ("nginx", "1.28.3")]);
    let (code, out, err) = env.run(&["install", "php", "8.4"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Installed php 8.4.26"), "{out}");
    assert!(
        err.contains("Downloading php 8.4"),
        "progress goes to stderr: {err}"
    );

    let again = env.ok(&["install", "php", "8.4.26"]);
    assert!(again.contains("was already installed"), "{again}");
    env.ok(&["install", "nginx"]);

    let list = env.ok(&["list", "--available"]);
    assert!(
        list.contains("Installed:") && list.contains("php 8.4.26"),
        "{list}"
    );
    assert!(
        list.contains("Available:") && list.contains("php 7.4.33"),
        "{list}"
    );
    assert!(list.contains("php 8.4.26  [installed]"), "{list}");
    assert!(!env.ok(&["list"]).contains("Available:"));

    assert!(env
        .ok(&["uninstall", "php", "8.4"])
        .contains("Removed php 8.4.26"));
    assert!(!env.ok(&["list"]).contains("php 8.4.26"));
    assert!(env
        .failing(&["uninstall", "php", "8.4"])
        .contains("not installed"));
}

#[test]
fn use_chooses_the_default_or_a_folder() {
    let env = Env::new(&[("php", "8.4.26"), ("php", "7.4.33"), ("node", "22.23.3")]);
    for spec in [("php", "8.4"), ("php", "7.4"), ("node", "22")] {
        env.ok(&["install", spec.0, spec.1]);
    }
    let global = env.ok(&["use", "php", "7.4"]);
    assert!(
        global.contains("php 7.4.33") && global.contains("crisol.toml"),
        "{global}"
    );
    let project = env.folder("shop");
    let local = env.ok(&["use", "node", "22", "--dir", &project]);
    assert!(local.contains("node 22.23.3"), "{local}");
    assert!(fs::read_to_string(format!("{project}/.crisol.toml"))
        .unwrap()
        .contains("node"));
    assert!(env
        .ok(&["use", "node", "22", "--dir", &project])
        .contains("already said so"));
    // `--local` is the current folder of the command.
    fs::create_dir_all(env.paths.home().join("work")).unwrap();
    env.ok(&["use", "php", "8.4", "--local"]);
    assert!(env.paths.home().join("work").join(".crisol.toml").is_file());
    assert!(env
        .failing(&["use", "php", "9.9"])
        .contains("not installed"));
}

// ---- sites ---------------------------------------------------------------------------------

#[test]
fn park_link_secure_and_their_opposites() {
    let env = Env::new(&[]);
    let projects = env.folder("big");
    fs::create_dir_all(format!("{projects}/blog")).unwrap();
    let alone = env.folder("alone");

    assert!(env.ok(&["park", &projects]).contains("Parked"));
    assert!(env.ok(&["park", &projects]).contains("already parked"));
    assert!(env.ok(&["link", "shop", &alone]).contains("Linked shop"));
    // The name defaults to the folder's.
    let default_name = env.folder("Docs");
    assert!(env
        .ok(&["link", "--docroot", "public", "docs", &default_name])
        .contains("Linked docs"));

    let sites = env.ok(&["sites"]);
    assert!(
        sites.contains("blog.test") && sites.contains("shop.test") && sites.contains("docs.test"),
        "{sites}"
    );
    assert!(
        sites.contains("[parked]") && sites.contains("[linked]"),
        "{sites}"
    );

    assert!(env.ok(&["secure", "shop"]).contains("HTTPS"));
    assert!(env
        .ok(&["sites"])
        .lines()
        .any(|l| l.starts_with("https") && l.contains("shop.test")));
    assert!(env.ok(&["unsecure", "shop"]).contains("HTTP only"));
    assert!(env.failing(&["secure", "ghost"]).contains("crisol sites"));

    assert!(env.ok(&["unlink", "shop"]).contains("Unlinked shop"));
    assert!(env.failing(&["unlink", "shop"]).contains("no linked site"));
    assert!(env.ok(&["unpark", &projects]).contains("Unparked"));
    assert!(env
        .failing(&["unpark", &projects])
        .contains("not a parked folder"));
    assert!(env.ok(&["sites"]).contains("docs.test"));
}

#[test]
fn relative_paths_are_taken_from_the_folder_the_command_ran_in() {
    let env = Env::new(&[]);
    let work = env.paths.home().join("work");
    fs::create_dir_all(work.join("app")).unwrap();
    env.ok(&["link", "app", "app"]);
    let sites = env.ok(&["sites"]);
    assert!(
        sites.contains("app.test") && sites.contains("work"),
        "{sites}"
    );
    // `..` is resolved by name too.
    fs::create_dir_all(env.paths.home().join("other")).unwrap();
    env.ok(&["link", "other", "../other"]);
    assert!(env.ok(&["sites"]).contains("other.test"));
}

// ---- services ------------------------------------------------------------------------------

#[test]
fn status_without_a_daemon_says_stopped_and_nothing_starts() {
    let env = Env::new(&[("nginx", "1.28.3")]);
    assert!(env.ok(&["status"]).contains("No services yet"));
    env.ok(&["install", "nginx"]);
    let status = env.ok(&["status"]);
    assert!(
        status.contains("nginx") && status.contains("stopped"),
        "{status}"
    );
    assert!(!env.daemon_running(), "asking does not start the daemon");
    // With nothing running there is nothing to stop, and that is not an error.
    assert!(env.ok(&["stop"]).contains("Nothing was running"));
    assert!(env
        .ok(&["stop", "--daemon"])
        .contains("Nothing was running"));
}

#[test]
fn start_status_restart_and_stop_run_the_web_stack_through_the_daemon() {
    let mut env = Env::new(&[("nginx", "1.28.3")]);
    // Nothing to start before nginx is installed.
    assert!(env.failing(&["start"]).contains("crisol install nginx"));
    // That `start` had to start the daemon to be told so; let it go before the real one.
    env.ok(&["shutdown"]);
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while env.daemon_running() {
        assert!(std::time::Instant::now() < end, "the daemon did not leave");
        std::thread::sleep(Duration::from_millis(50));
    }
    env.daemon
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .join()
        .unwrap()
        .unwrap();
    env.ok(&["install", "nginx"]);
    fs::create_dir_all(env.paths.www_dir().join("blog")).unwrap();

    // `start` starts the daemon by itself, and answers when Nginx is up.
    let (code, out, err) = env.run(&["start"]);
    assert_eq!(code, 0, "stdout: {out}\nstderr: {err}");
    assert!(err.contains("Starting the daemon"), "{err}");
    assert!(out.contains("nginx") && out.contains("running"), "{out}");
    assert!(env.daemon_running());

    let status = env.ok(&["status"]);
    assert!(status.contains("running"), "{status}");
    assert!(env.ok(&["status", "nginx"]).contains("running"));
    assert!(env
        .failing(&["status", "mysql"])
        .contains("no service called `mysql`"));
    assert!(env
        .failing(&["start", "nginx"])
        .contains("without a service name"));
    assert!(env
        .failing(&["start", "web"])
        .contains("no service called `web`"));

    // A second `start` has nothing to do and says the stack is up.
    assert!(env.ok(&["start"]).contains("running"));

    let restart = env.ok(&["restart"]);
    assert!(restart.contains("running"), "{restart}");

    // `stop` stops the services and leaves the daemon (and the DNS of the local names) alone, so
    // it has no warning about names; closing the daemon is said out loud, and warns that the
    // local names will not resolve any more.
    let stopped = env.ok(&["stop"]);
    assert!(stopped.contains("stopped"), "{stopped}");
    assert!(!stopped.contains("warning"), "{stopped}");
    assert!(env.ok(&["status"]).contains("stopped"));
    assert!(env.daemon_running());
    env.ok(&["start"]);
    let shutdown = env.ok(&["stop", "--daemon"]);
    assert!(shutdown.contains("daemon is leaving"), "{shutdown}");
    assert!(
        shutdown.contains("warning: the local names stop resolving"),
        "{shutdown}"
    );
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while env.daemon_running() {
        assert!(std::time::Instant::now() < end, "the daemon did not leave");
        std::thread::sleep(Duration::from_millis(50));
    }
    env.finish();
}

#[test]
fn a_new_site_is_served_while_the_stack_runs() {
    let mut env = Env::new(&[("nginx", "1.28.3")]);
    env.ok(&["install", "nginx"]);
    env.ok(&["start"]);
    let alone = env.folder("late");
    env.ok(&["link", "late", &alone]);
    // The daemon reloaded Nginx for it without being asked to.
    let end = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if env
            .paths
            .etc_for("nginx")
            .unwrap()
            .join("sites")
            .join("late.conf")
            .is_file()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < end,
            "the site was never configured"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    env.finish();
}

#[test]
fn an_open_home_blocks_start_and_the_error_carries_the_fix() {
    let mut env = Env::new(&[("nginx", "1.28.3")]);
    env.ok(&["install", "nginx"]);
    env.dirs.expose(env.paths.home());
    let (code, out, err) = env.run(&["start"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(
        err.contains("error:") && err.contains("will not go on"),
        "{err}"
    );
    assert!(
        err.contains("  fix: icacls "),
        "the command is on its own line: {err}"
    );
    // Reading still works, and doctor says it too.
    assert!(env.ok(&["sites"]).len() <= 100);
    let doctor = env.failing(&["doctor"]);
    assert!(
        doctor.contains("PROBLEM") && doctor.contains("home permissions"),
        "{doctor}"
    );
    env.dirs.close(env.paths.home());
    env.ok(&["start"]);
    env.finish();
}

#[test]
fn the_first_command_without_a_daemon_makes_the_home_private() {
    // A new user whose first command is `install` or `park`: the home does not exist yet and
    // must be born private, not inherit the open permissions of the drive it is on.
    for args in [vec!["install", "php", "8.4"], vec!["park", "PARKED"]] {
        let env = Env::new(&[("php", "8.4.26")]);
        fs::remove_dir_all(env.paths.home()).unwrap();
        let folder = env.paths.home().with_file_name("elsewhere");
        fs::create_dir_all(&folder).unwrap();
        let folder = folder.display().to_string();
        let args: Vec<&str> = args
            .iter()
            .map(|a| if *a == "PARKED" { folder.as_str() } else { *a })
            .collect();
        env.ok(&args);
        assert_eq!(
            env.dirs.created(),
            [env.paths.home().to_path_buf()],
            "{args:?}"
        );
        assert!(!env.daemon_running());
    }
}

#[test]
fn commands_that_only_look_do_not_make_the_home_and_say_it_is_not_there() {
    let env = Env::new(&[("php", "8.4.26")]);
    fs::remove_dir_all(env.paths.home()).unwrap();
    for args in [["list"], ["sites"], ["status"]] {
        let said = env.failing(&args);
        assert!(said.contains("no Crisol home in"), "{args:?}: {said}");
        assert!(
            said.contains("first `crisol install` or `crisol start`"),
            "{said}"
        );
    }
    assert!(env
        .failing(&["logs", "nginx"])
        .contains("no Crisol home in"));
    // `doctor` is a warning, not a failure: a computer that never used Crisol is not broken.
    let doctor = env.ok(&["doctor"]);
    assert!(
        doctor.contains("warning") && doctor.contains("no Crisol home"),
        "{doctor}"
    );
    assert!(!env.paths.home().exists(), "none of them made it");
    assert_eq!(env.dirs.created(), Vec::<std::path::PathBuf>::new());

    // The first command that writes does make it, private.
    env.ok(&["install", "php", "8.4"]);
    assert_eq!(env.dirs.created(), [env.paths.home().to_path_buf()]);
}

#[test]
fn db_credentials_shows_the_env_lines_of_a_service_and_says_what_is_missing() {
    let env = Env::new(&[("mysql", "8.4.11")]);
    assert!(env
        .failing(&["db", "credentials", "mysql"])
        .contains("crisol install mysql"));
    env.ok(&["install", "mysql"]);
    let shown = env.ok(&["db", "credentials", "mysql"]);
    for line in [
        "DB_CONNECTION=mysql",
        "DB_HOST=127.0.0.1",
        "DB_PORT=3306",
        "DB_USERNAME=root",
        "DB_PASSWORD=",
    ] {
        assert!(shown.lines().any(|l| l == line), "{line}: {shown}");
    }
    // The password is generated when the data is made, which has not happened yet.
    let json = env.ok(&["--json", "db", "credentials", "mysql"]);
    let value: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(
        (&value["type"], &value["service"]),
        (&"credentials".into(), &"mysql".into())
    );
    assert!(env
        .failing(&["db", "credentials", "nginx"])
        .contains("has no credentials"));
}

#[test]
fn enable_and_disable_edit_crisol_toml_and_nothing_else() {
    let env = Env::new(&[]);
    fs::write(
        env.paths.config_file(),
        "# mine
[services.mysql]
port = 3307 # mine too
",
    )
    .unwrap();
    let said = env.ok(&["enable", "mysql"]);
    assert!(said.contains("mysql is enabled"), "{said}");
    let text = fs::read_to_string(env.paths.config_file()).unwrap();
    assert!(text.contains("# mine") && text.contains("port = 3307 # mine too"));
    assert!(text.contains("enabled = true"), "{text}");
    assert!(env.ok(&["enable", "mysql"]).contains("already enabled"));
    assert!(env.ok(&["disable", "mysql"]).contains("disabled"));
    assert!(env
        .failing(&["enable", "nginx"])
        .contains("cannot be enabled"));
}

// ---- logs and doctor ------------------------------------------------------------------------

#[test]
fn logs_show_the_end_and_follow_what_is_added() {
    let env = Env::new(&[]);
    let log = env.paths.runtime_log("nginx").unwrap();
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(&log, "one\ntwo\nthree\n").unwrap();
    assert_eq!(env.ok(&["logs", "nginx", "-n", "2"]), "two\nthree\n");
    assert!(env.failing(&["logs", "php-9.9.9"]).contains("no log for"));

    // Following prints what is already there and then what gets added while it runs.
    let writer_log = log.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&writer_log)
            .unwrap();
        writeln!(file, "four").unwrap();
    });
    let followed = env.ok(&["logs", "nginx", "-f", "-n", "1"]);
    writer.join().unwrap();
    assert_eq!(followed, "three\nfour\n");
}

#[test]
fn doctor_lists_the_checks_and_the_ones_of_the_system() {
    let env = Env::new(&[]);
    let doctor = env.ok(&["doctor"]);
    for name in [
        "home permissions",
        "configuration",
        "installed tools",
        "shims",
        "sites",
        "windows thing",
    ] {
        assert!(doctor.contains(name), "{name}: {doctor}");
    }
    assert!(
        doctor.contains("warning  installed tools") || doctor.contains("warning  installed tools:"),
        "{doctor}"
    );
}

// ---- output formats and mistakes ------------------------------------------------------------

#[test]
fn json_gives_one_object_per_command_for_answers_and_errors() {
    let env = Env::new(&[("php", "8.4.26")]);
    env.ok(&["install", "php"]);
    let list = env.ok(&["--json", "list"]);
    let value: serde_json::Value = serde_json::from_str(list.trim()).unwrap();
    assert_eq!(value["type"], "versions");
    assert_eq!(value["installed"][0]["version"], "8.4.26");

    let (code, out, err) = env.run(&["list", "--json", "--available"]);
    assert_eq!(code, 0, "{err}");
    assert!(serde_json::from_str::<serde_json::Value>(out.trim()).is_ok());
    assert!(err.is_empty(), "--json keeps stderr quiet: {err}");

    let (code, out, err) = env.run(&["--json", "uninstall", "php", "9"]);
    assert_eq!(code, 1);
    let error: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(error["code"], 2);
    assert!(error["message"].as_str().unwrap().contains("not installed"));
    assert!(err.is_empty(), "{err}");
}

#[test]
fn bad_arguments_exit_with_2_and_help_and_version_exit_with_0() {
    let env = Env::new(&[]);
    let (code, _, err) = env.run(&["frobnicate"]);
    assert_eq!(code, 2);
    assert!(err.contains("unrecognized subcommand"), "{err}");
    let (code, _, err) = env.run(&["use", "ruby", "3"]);
    assert_eq!(code, 2);
    assert!(err.contains("invalid value"), "{err}");
    let (code, out, _) = env.run(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("install") && out.contains("doctor"), "{out}");
    let (code, out, _) = env.run(&["--version"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("crisol "), "{out}");
}

// ---- starting at sign-in -------------------------------------------------------------------

#[test]
fn autostart_is_a_setting_of_the_user_that_can_be_turned_on_and_off() {
    let env = Env::new(&[]);
    assert!(env
        .ok(&["autostart"])
        .contains("does not start when you sign in"));
    assert!(env
        .ok(&["autostart", "on"])
        .contains("will start when you sign in"));
    assert!(env.ok(&["autostart", "on"]).contains("already starts"));
    assert!(env
        .ok(&["autostart", "status"])
        .contains("starts when you sign in"));
    // The entry is the quoted path of crisold and nothing else.
    let entry = env.autostart.current().unwrap().unwrap();
    assert_eq!(entry, format!("\"{}\"", env.daemon_exe.display()));
    assert_eq!(
        env.ok(&["--json", "autostart"]).trim(),
        r#"{"autostart":"on"}"#
    );
    assert!(env.ok(&["autostart", "off"]).contains("will not start"));
    assert!(env.ok(&["autostart", "off"]).contains("did not start"));
    assert_eq!(
        env.ok(&["--json", "autostart", "status"]).trim(),
        r#"{"autostart":"off"}"#
    );
    // An entry that points elsewhere is shown as such, and replaced by `on`.
    env.autostart.set(r#""D:\old\crisold.exe""#).unwrap();
    assert!(env.ok(&["autostart"]).contains("another crisold"));
    assert!(env.ok(&["autostart", "on"]).contains("replaced"));
    assert!(
        !env.daemon_running(),
        "this setting never starts anything now"
    );
}

#[test]
fn a_crisold_other_accounts_could_replace_is_neither_registered_nor_launched() {
    let env = Env::new(&[("nginx", "1.28.3")]);
    env.dirs.expose(env.daemon_exe.parent().unwrap());
    let refused = env.failing(&["autostart", "on"]);
    assert!(
        refused.contains("will not") && refused.contains("  fix: icacls "),
        "{refused}"
    );
    // The folder is named, and the command that closes it with everything inside is apart.
    let folder = env.daemon_exe.parent().unwrap().display().to_string();
    let sentence = refused.lines().next().unwrap();
    assert!(
        sentence.contains(&folder) && !sentence.contains("icacls"),
        "{refused}"
    );
    assert!(refused.contains("(OI)(CI)"), "{refused}");
    assert!(env.autostart.current().unwrap().is_none());
    // `start` would have to launch the daemon: it does not, and says the same.
    let refused = env.failing(&["start"]);
    assert!(refused.contains("icacls"), "{refused}");
    assert!(!env.daemon_running());
    // Closing the folder is all it takes.
    env.dirs.close(env.daemon_exe.parent().unwrap());
    env.ok(&["autostart", "on"]);
}

// ---- the shims, the user PATH and `env` (F10.2) -----------------------------------------------

#[test]
fn shims_install_status_and_remove_only_touch_the_shims_folder() {
    let env = Env::new(&[]);
    let shims = env.paths.shims_dir();
    assert!(env.ok(&["shims"]).contains("There are no shims"));

    let done = env.ok(&["shims", "install"]);
    assert!(done.contains("10 shim(s) written"), "{done}");
    assert!(done.contains("crisol path add"), "{done}");
    for name in [
        "php", "node", "npm", "npx", "composer", "corepack", "pnpm", "pnpx", "yarn", "yarnpkg",
    ] {
        assert!(shims.join(format!("{name}.exe")).is_file(), "{name}");
    }
    assert!(env
        .ok(&["shims", "install"])
        .contains("0 shim(s) written and 10 already"));
    assert!(env.ok(&["shims", "status"]).contains("All the shims"));
    let json = env.ok(&["--json", "shims", "status"]);
    assert!(
        json.contains(r#""command":"pnpm","present":true"#),
        "{json}"
    );
    assert_eq!(
        env.user_path.writes(),
        0,
        "installing the files never touches the PATH"
    );

    fs::write(shims.join("my-tool.cmd"), "@echo hi").unwrap();
    assert!(env.ok(&["shims", "remove"]).contains("10 shim(s) removed"));
    assert!(
        shims.join("my-tool.cmd").is_file(),
        "what is not a shim stays"
    );
    assert!(env.ok(&["shims", "remove"]).contains("There were no shims"));
}

#[test]
fn shims_sync_gives_a_command_to_each_global_program_and_install_and_remove_follow_it() {
    let env = Env::new(&[]);
    let version = env.paths.bin_for("node", "24.22.0").unwrap();
    fs::create_dir_all(&version).unwrap();
    let global = env.paths.node_global("24.22.0").unwrap();
    fs::create_dir_all(&global).unwrap();
    fs::write(global.join("tsc.cmd"), "@echo off").unwrap();
    fs::write(global.join("ping.cmd"), "@echo off").unwrap();

    let out = env.ok(&["shims", "sync"]);
    assert!(
        out.contains("no command `ping`: Windows has a program"),
        "{out}"
    );
    assert!(!env.paths.shims_dir().join("ping.exe").exists());
    assert!(
        out.contains("up to date") && out.contains("added: tsc"),
        "{out}"
    );
    assert!(env.paths.shims_dir().join("tsc.exe").is_file());
    assert!(env
        .ok(&["shims", "status"])
        .contains("Commands of global programs: tsc"));
    assert!(env
        .ok(&["--json", "shims", "sync"])
        .contains(r#""added":[]"#));

    // Installing the shims again refreshes them; removing takes the commands too.
    env.ok(&["shims", "install"]);
    assert!(env.ok(&["shims", "remove"]).contains("11 shim(s) removed"));
    assert!(!env.paths.shims_dir().exists());
}

#[test]
fn a_shim_program_other_accounts_could_replace_is_not_installed() {
    let env = Env::new(&[]);
    env.dirs.expose(env.shim_exe.parent().unwrap());
    let refused = env.failing(&["shims", "install"]);
    assert!(refused.contains("icacls"), "{refused}");
    assert!(!env.paths.shims_dir().join("php.exe").exists());
    env.dirs.close(env.shim_exe.parent().unwrap());
    env.ok(&["shims", "install"]);
}

#[test]
fn path_add_and_remove_change_one_entry_and_give_the_value_back_as_it_was() {
    let env = Env::new(&[]);
    let before = env.user_path.user();
    assert!(env.ok(&["path"]).contains("is not in your user PATH"));

    let added = env.ok(&["path", "add"]);
    assert!(added.contains("is now first in your user PATH"), "{added}");
    let value = env.user_path.user().unwrap().text;
    assert!(
        value.starts_with(&env.paths.shims_dir().display().to_string())
            && value.ends_with(r"C:\Tools;%USERPROFILE%\bin"),
        "{value}"
    );
    assert!(env
        .ok(&["path", "add"])
        .contains("already in your user PATH"));
    assert_eq!(env.user_path.writes(), 1);
    assert!(env
        .ok(&["--json", "path", "status"])
        .contains(r#""in_path":true"#));

    assert!(env
        .ok(&["path", "remove"])
        .contains("no longer in your user PATH"));
    assert!(env
        .ok(&["path", "remove"])
        .contains("was not in your user PATH"));
    assert_eq!(env.user_path.user(), before);
}

#[test]
fn path_says_what_wins_over_the_shims_and_how_to_get_around_it() {
    let env = Env::new(&[]);
    // A real file in a real folder stands for the Node the machine PATH has.
    let machine = env.paths.home().join("machine-node");
    fs::create_dir_all(&machine).unwrap();
    fs::write(machine.join("node.exe"), b"MZ").unwrap();
    env.user_path.set_system(&machine.display().to_string());

    let out = env.ok(&["path", "add"]);
    assert!(
        out.contains("`node` in a terminal runs the one in"),
        "{out}"
    );
    assert!(
        out.contains("machine PATH") && out.contains("crisol env"),
        "{out}"
    );
    let json = env.ok(&["--json", "path", "status"]);
    assert!(
        json.contains(r#""command":"node""#) && json.contains(r#""machine_path":true"#),
        "{json}"
    );
}

#[test]
fn env_prints_only_the_lines_for_the_shell_and_writes_nothing() {
    let env = Env::new(&[]);
    let dir = env.paths.shims_dir().display().to_string();
    // The test context says it is `cmd.exe` (it has PROMPT).
    assert_eq!(env.ok(&["env"]), format!("set \"PATH={dir};%PATH%\"\n"));
    let power = env.ok(&["env", "--shell", "powershell"]);
    assert!(
        power.starts_with("if (($env:Path -split ';')[0] -ne '"),
        "{power}"
    );
    assert!(env.ok(&["env", "--shell", "bash"]).contains("export PATH="));
    assert!(env
        .ok(&["--json", "env", "--shell", "cmd"])
        .starts_with(r#"{"script":"set"#));
    assert_eq!(env.user_path.writes(), 0);
    assert!(!env.paths.shims_dir().join("php.exe").exists());
}

// ---- the one-time setup that needs an administrator ----------------------------------------

#[test]
fn system_install_and_uninstall_ask_the_setup_once_each_with_the_sites_suffix() {
    let env = Env::new(&[]);
    let installed = env.ok(&["system", "install"]);
    assert!(
        installed.contains("installed the helper and the rule for .test"),
        "{installed}"
    );
    let removed = env.ok(&["system", "uninstall"]);
    assert!(
        removed.contains("removed the helper and the rule for .test"),
        "{removed}"
    );
    assert_eq!(env.setup.calls(), ["install test", "uninstall test"]);
    let json = env.ok(&["--json", "system", "install"]);
    let value: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(
        value["steps"][0],
        "installed the helper and the rule for .test"
    );
    // It never starts the daemon: it is not about the services.
    assert!(!env.daemon_running());
    let (code, _, err) = env.run(&["system"]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn after_start_the_pool_reads_a_php_ini_that_loads_the_database_and_curl_extensions() {
    let mut env = Env::new(&[("nginx", "1.28.3"), ("php", "8.4.26")]);
    env.ok(&["install", "nginx"]);
    env.ok(&["install", "php"]);
    // The fake build is an executable only: give it what a real one ships.
    let install = env.paths.bin_for("php", "8.4.26").unwrap();
    fs::create_dir_all(install.join("ext")).unwrap();
    fs::write(
        install.join("php.ini-development"),
        "[PHP]\n;extension=curl\n;extension=pdo_mysql\n",
    )
    .unwrap();
    for dll in ["php_curl.dll", "php_pdo_mysql.dll"] {
        fs::write(install.join("ext").join(dll), "MZ").unwrap();
    }
    fs::create_dir_all(env.paths.www_dir().join("blog")).unwrap();
    assert!(!env.paths.php_ini("8.4.26").unwrap().exists());

    env.ok(&["start"]);

    let ini = fs::read_to_string(env.paths.php_ini("8.4.26").unwrap()).unwrap();
    assert!(ini.contains("extension=curl"), "{ini}");
    assert!(ini.contains("extension=pdo_mysql"), "{ini}");
    env.finish();
}

#[test]
fn db_reset_password_needs_the_daemon_because_it_asks_a_running_server() {
    let env = Env::new(&[]);
    let shown = env.failing(&["db", "reset-password", "mysql"]);
    assert!(shown.contains("daemon is not running"), "{shown}");
    assert!(shown.contains("crisol start"), "{shown}");
}
