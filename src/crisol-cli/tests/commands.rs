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
use crisol_core::catalog::{Arch, Catalog, Os, Target};
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
use crisol_daemon::{run as run_daemon, Options};
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
            catalog,
            target: Target {
                os: Os::Windows,
                arch: Arch::X86_64,
            },
        };
        let driver_stop = Arc::new(AtomicBool::new(false));
        let driver = Some(play_nginx(supervisor, probe, Arc::clone(&driver_stop)));
        Self {
            pipe: pipe_name(paths.home()),
            _tmp: tmp,
            paths,
            deps,
            dirs,
            daemon: Arc::new(Mutex::new(None)),
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
                };
                *daemon_slot.lock().unwrap() =
                    Some(std::thread::spawn(move || run_daemon(options)));
                Ok(())
            }),
            daemon_wait: Duration::from_secs(10),
            follow_every: Duration::from_millis(50),
            follow_rounds: Some(3),
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
            code, 0,
            "crisol {args:?} failed\nstdout: {out}\nstderr: {err}"
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
            let _ = self.run(&["stop"]);
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
        .ok(&["stop", "--keep-daemon"])
        .contains("Nothing was running"));
}

#[test]
fn start_status_restart_and_stop_run_the_web_stack_through_the_daemon() {
    let mut env = Env::new(&[("nginx", "1.28.3")]);
    // Nothing to start before nginx is installed.
    assert!(env.failing(&["start"]).contains("crisol install nginx"));
    // That `start` had to start the daemon to be told so; let it go before the real one.
    env.ok(&["stop"]);
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

    // A second `start` has nothing to do and says the stack is up.
    assert!(env.ok(&["start"]).contains("running"));

    let restart = env.ok(&["restart"]);
    assert!(restart.contains("running"), "{restart}");

    // Stopping the services leaves the daemon; stopping for good takes it too, and warns that
    // the local names will not resolve any more.
    let stopped = env.ok(&["stop", "--keep-daemon"]);
    assert!(stopped.contains("stopped"), "{stopped}");
    assert!(env.ok(&["status"]).contains("stopped"));
    assert!(env.daemon_running());
    env.ok(&["start"]);
    let shutdown = env.ok(&["stop"]);
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
