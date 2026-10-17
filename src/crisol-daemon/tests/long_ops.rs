//! A long installation does not stop the daemon (review of phase 8, finding 2).
//!
//! The daemon is the real one over a real named pipe in a temporary home; the downloads come
//! from a fetcher that holds one of them back until the test lets it go, and the services are
//! doubles driven by a thread that plays Nginx and the PHP workers. Nothing here touches the
//! real Crisol home, the registry, the hosts file or the NRPT.

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crisol_core::cacert::NoRoots;
use crisol_core::catalog::{Arch, Catalog, Os, Target};
use crisol_core::engine::{Deps, NoSystemChecks};
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::fetch::{Fetch, FetchError, Response};
use crisol_core::ipc::pipe_name;
use crisol_core::paths::fake::FakePrivateDirs;
use crisol_core::paths::CrisolPaths;
use crisol_core::ports::fake::FakePortProbe;
use crisol_core::ports::PortOwner;
use crisol_core::process::fake::FakeSupervisor;
use crisol_core::process::ProcessId;
use crisol_core::project::TrustAllOwners;
use crisol_core::web::AlwaysEffective;
use crisol_daemon::{run, Options, Timing};
use crisol_platform::connect_to_daemon;
use crisol_protocol::{Call, ClientError, ErrorCode, Event, Reply};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

const HTTP: u16 = 19081;

/// Holds the body of one URL back after its first bytes until [`Gate::open`].
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }
}

/// Serves the fake downloads; the one in `slow` stalls halfway until the gate opens.
struct SlowFetcher {
    inner: FakeFetcher,
    slow: String,
    gate: Arc<Gate>,
}

struct Stalling {
    inner: Box<dyn Read + Send>,
    gate: Arc<Gate>,
    first: bool,
}

impl Read for Stalling {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.first {
            self.gate.wait();
        }
        self.first = false;
        // A few bytes at a time, so the first read ends before the wait.
        let end = buf.len().min(16);
        self.inner.read(&mut buf[..end])
    }
}

impl Fetch for SlowFetcher {
    fn fetch(&self, url: &str) -> Result<Response, FetchError> {
        let response = self.inner.fetch(url)?;
        if url != self.slow {
            return Ok(response);
        }
        Ok(Response {
            content_length: response.content_length,
            body: Box::new(Stalling {
                inner: response.body,
                gate: Arc::clone(&self.gate),
                first: true,
            }),
        })
    }
}

fn zip_of(files: &[&str]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for file in files {
        writer
            .start_file(*file, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"MZ binary").unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn world(builds: &[(&str, &str, &[&str])], slow: &str) -> (Catalog, SlowFetcher, Arc<Gate>) {
    let fetcher = FakeFetcher::new();
    let mut text = String::from("schema = 1\nserial = 1\n");
    let mut slow_url = String::new();
    for (service, version, files) in builds {
        let bytes = zip_of(files);
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
             executable = \"{}\"\nsize = {}\n",
            files[0],
            bytes.len()
        );
        if *service == slow {
            slow_url.clone_from(&url);
        }
        fetcher.serve(&url, bytes);
    }
    let gate = Arc::new(Gate::default());
    (
        Catalog::parse_for_tests(&text).unwrap(),
        SlowFetcher {
            inner: fetcher,
            slow: slow_url,
            gate: Arc::clone(&gate),
        },
        gate,
    )
}

struct Running {
    _tmp: TempDir,
    paths: CrisolPaths,
    pipe: String,
    supervisor: Arc<FakeSupervisor>,
    gate: Arc<Gate>,
    thread: Option<JoinHandle<std::io::Result<()>>>,
    driver_stop: Arc<AtomicBool>,
    driver: Option<JoinHandle<()>>,
}

fn start_daemon() -> Running {
    start_daemon_with(Timing::default(), false)
}

/// `stuck`: Nginx ignores the order to quit, as a service that hangs would.
fn start_daemon_with(timing: Timing, stuck: bool) -> Running {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    fs::write(
        paths.config_file(),
        format!("[services.nginx]\nhttp_port = {HTTP}\nhttps_port = 19444\n\n[php]\nbase_port = 19120\n"),
    )
    .unwrap();
    fs::create_dir_all(paths.www_dir().join("blog")).unwrap();
    let (catalog, fetcher, gate) = world(
        &[
            ("nginx", "1.28.3", &["nginx.exe"]),
            ("php", "8.4.26", &["php.exe", "php-cgi.exe"]),
            ("node", "22.23.3", &["node.exe"]),
        ],
        "node",
    );
    let supervisor = Arc::new(FakeSupervisor::new());
    let probe = Arc::new(FakePortProbe::new());
    let deps = Deps {
        supervisor: supervisor.clone(),
        probe: probe.clone(),
        dirs: Arc::new(FakePrivateDirs::new()),
        fetcher: Arc::new(fetcher),
        owners: Arc::new(TrustAllOwners),
        env: Arc::new(|_| None),
        system: Arc::new(NoSystemChecks),
        roots: Arc::new(NoRoots),
        config_probe: Arc::new(AlwaysEffective),
        catalog,
        target: Target {
            os: Os::Windows,
            arch: Arch::X86_64,
        },
    };
    let pipe = pipe_name(paths.home(), "S-1-5-21-1-2-3-1001");
    let options = Options {
        paths: paths.clone(),
        deps,
        pipe: pipe.clone(),
        dns: None,
        timing,
    };
    let thread = std::thread::spawn(move || run(options));
    let end = Instant::now() + Duration::from_secs(10);
    while connect_to_daemon(&pipe).is_err() {
        assert!(Instant::now() < end, "the daemon never opened its pipe");
        std::thread::sleep(Duration::from_millis(50));
    }
    let driver_stop = Arc::new(AtomicBool::new(false));
    let driver = Some(play_services(
        Arc::clone(&supervisor),
        probe,
        Arc::clone(&driver_stop),
        stuck,
    ));
    Running {
        _tmp: tmp,
        paths,
        pipe,
        supervisor,
        gate,
        thread: Some(thread),
        driver_stop,
        driver,
    }
}

/// Plays Nginx and the PHP workers behind the fake supervisor: a configuration test passes,
/// the programs "listen" once launched, and Nginx leaves when told to quit.
fn play_services(
    supervisor: Arc<FakeSupervisor>,
    probe: Arc<FakePortProbe>,
    stop: Arc<AtomicBool>,
    stuck: bool,
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
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                let nginx_args: Vec<&str> = args.iter().skip(4).map(String::as_str).collect();
                let listen = |port: u16, name: &str| {
                    probe.occupy(
                        port,
                        PortOwner {
                            pid: record.pid,
                            name: Some(name.to_owned()),
                            address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                        },
                    );
                };
                if args.first().map(String::as_str) == Some("-b") {
                    if let Some(port) = args[1].rsplit(':').next().and_then(|p| p.parse().ok()) {
                        listen(port, "php-cgi.exe");
                    }
                } else if nginx_args == ["-t"] || nginx_args == ["-s", "reload"] {
                    let _ = supervisor.exit(record.process, 0);
                } else if nginx_args == ["-s", "quit"] {
                    if stuck {
                        continue;
                    }
                    let _ = supervisor.exit(record.process, 0);
                    if let Some(main) = main.take() {
                        let _ = supervisor.exit(main, 0);
                    }
                    probe.release(HTTP);
                } else if nginx_args.is_empty() {
                    main = Some(record.process);
                    listen(HTTP, "nginx.exe");
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    })
}

impl Running {
    fn shut_down(&mut self) {
        self.gate.open();
        if let Ok(mut client) = connect_to_daemon(&self.pipe) {
            let _ = client.call(&Call::Shutdown);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.driver_stop.store(true, Ordering::Relaxed);
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shut_down();
    }
}

#[test]
fn while_a_download_goes_on_the_daemon_answers_restarts_workers_and_reports_progress() {
    let mut daemon = start_daemon();
    let mut client = connect_to_daemon(&daemon.pipe).unwrap();
    for (service, version) in [("nginx", "1.28.3"), ("php", "8.4.26")] {
        client
            .call(&Call::Install {
                service: service.to_owned(),
                version: version.to_owned(),
            })
            .unwrap();
    }
    client.call(&Call::Start { service: None }).unwrap();
    let workers = |daemon: &Running| {
        daemon
            .supervisor
            .spawned()
            .iter()
            .filter(|r| r.spec.args.first().is_some_and(|a| a == "-b"))
            .count()
    };
    let before = workers(&daemon);
    assert!(before > 0, "the PHP pool never started");

    let mut listener = connect_to_daemon(&daemon.pipe).unwrap();
    listener.call(&Call::Subscribe).unwrap();

    // The download starts and stalls half way.
    let pipe = daemon.pipe.clone();
    let installing = std::thread::spawn(move || {
        let mut client = connect_to_daemon(&pipe).unwrap();
        client.call(&Call::Install {
            service: "node".to_owned(),
            version: "22".to_owned(),
        })
    });
    let progress = loop {
        match listener.next_event().unwrap() {
            Some(Event::Progress { what, .. }) => break what,
            Some(_) => {}
            None => panic!("the daemon hung up"),
        }
    };
    assert!(progress.contains("node"), "{progress}");

    // Meanwhile: it answers at once…
    let asked = Instant::now();
    assert!(matches!(
        client.call(&Call::Status { service: None }).unwrap(),
        Reply::Status(_)
    ));
    assert!(
        asked.elapsed() < Duration::from_secs(3),
        "status waited for the download"
    );
    // …a start is refused with a message that says why…
    match client.call(&Call::Start { service: None }) {
        Err(ClientError::Rpc(error)) => {
            assert_eq!(error.code, ErrorCode::Conflict);
            assert!(
                error.message.contains("installing node"),
                "{}",
                error.message
            );
        }
        other => panic!("{other:?}"),
    }
    // …and a worker that dies is started again.
    let crashed = daemon
        .supervisor
        .spawned()
        .iter()
        .find(|r| r.spec.args.first().is_some_and(|a| a == "-b"))
        .map(|r| r.process)
        .unwrap();
    daemon.supervisor.exit(crashed, 1).unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while workers(&daemon) <= before {
        assert!(Instant::now() < end, "the dead worker was never restarted");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The download ends, and its answer arrives.
    daemon.gate.open();
    match installing.join().unwrap().unwrap() {
        Reply::Installed(info) => assert_eq!(info.service, "node"),
        other => panic!("{other:?}"),
    }
    assert!(daemon.paths.bin_for("node", "22.23.3").unwrap().is_dir());
    daemon.shut_down();
}

#[test]
fn several_installations_at_once_all_finish_and_doctor_still_answers() {
    let mut daemon = start_daemon();
    let handles: Vec<_> = ["nginx 1.28.3", "php 8.4.26"]
        .into_iter()
        .map(|spec| {
            let pipe = daemon.pipe.clone();
            let (service, version) = spec.split_once(' ').unwrap();
            let (service, version) = (service.to_owned(), version.to_owned());
            std::thread::spawn(move || {
                let mut client = connect_to_daemon(&pipe).unwrap();
                client.call(&Call::Install { service, version })
            })
        })
        .collect();
    for handle in handles {
        assert!(matches!(
            handle.join().unwrap().unwrap(),
            Reply::Installed(_)
        ));
    }
    let mut client = connect_to_daemon(&daemon.pipe).unwrap();
    assert!(matches!(
        client.call(&Call::Doctor).unwrap(),
        Reply::Doctor { .. }
    ));
    daemon.shut_down();
}

/// Starts Nginx, asks the daemon to shut down while Nginx ignores the order to quit, and gives
/// back how long the daemon took to leave.
fn shut_down_with_a_stuck_nginx(timing: Timing, unkillable: bool) -> (Running, Duration) {
    let mut daemon = start_daemon_with(timing, true);
    let mut client = connect_to_daemon(&daemon.pipe).unwrap();
    client
        .call(&Call::Install {
            service: "nginx".to_owned(),
            version: "1.28.3".to_owned(),
        })
        .unwrap();
    client.call(&Call::Start { service: None }).unwrap();
    let alive = daemon.supervisor.alive_pids();
    assert_ne!(alive, Vec::<u32>::new(), "nginx never started");
    if unkillable {
        for record in daemon.supervisor.spawned() {
            daemon.supervisor.make_unkillable(record.group);
        }
    }
    let asked = Instant::now();
    let _ = client.call(&Call::Shutdown);
    daemon.thread.take().unwrap().join().unwrap().unwrap();
    (daemon, asked.elapsed())
}

#[test]
fn a_service_that_will_not_stop_is_killed_by_the_daemon_and_the_daemon_leaves() {
    // The cap of the daemon (settle + margin = 1.7 s) comes before the service gives up on its
    // own graceful stop and kills its group (2 s): what ends the stuck Nginx is the daemon.
    let timing = Timing {
        settle: Duration::from_millis(1500),
        exit_margin: Duration::from_millis(200),
        job_grace: Duration::from_millis(300),
    };
    let (mut daemon, took) = shut_down_with_a_stuck_nginx(timing, false);
    assert!(took < Duration::from_secs(10), "left after {took:?}");
    let log = fs::read_to_string(daemon.paths.runtime_log("crisold").unwrap()).unwrap();
    assert!(log.contains("did not stop in time"), "{log}");
    assert!(
        !log.contains("killed after"),
        "the service killed itself first; the daemon's cut did not come before: {log}"
    );
    let alive = daemon.supervisor.alive_pids();
    assert_eq!(
        alive,
        Vec::<u32>::new(),
        "the stuck service was left running"
    );
    daemon.shut_down();
}

#[test]
fn a_service_that_cannot_even_be_killed_still_lets_the_daemon_leave() {
    // It ignores the kill as well, so its own stop outlasts the daemon's cap; the daemon leaves
    // anyway and says so. (A real process cannot ignore its group being closed; this is for
    // whatever else could keep the daemon from ending.)
    let timing = Timing {
        settle: Duration::from_secs(3),
        exit_margin: Duration::from_millis(500),
        job_grace: Duration::from_millis(500),
    };
    let (mut daemon, took) = shut_down_with_a_stuck_nginx(timing, true);
    assert!(took < Duration::from_secs(10), "left after {took:?}");
    let log = fs::read_to_string(daemon.paths.runtime_log("crisold").unwrap()).unwrap();
    assert!(log.contains("did not stop in time"), "{log}");
    daemon.shut_down();
}
