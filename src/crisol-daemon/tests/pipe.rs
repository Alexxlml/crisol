//! The daemon over a real named pipe, with fake services behind it (F8.2).
//!
//! Each test has its own temporary home and so its own pipe name; nothing here touches the real
//! Crisol home, the registry, the hosts file or the NRPT, and no service is really started.

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::fs;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crisol_core::cacert::NoRoots;
use crisol_core::catalog::{Arch, Catalog, Os, Target};
use crisol_core::dbservice::NoAdmin;
use crisol_core::engine::{Deps, NoSystemChecks};
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::ipc::pipe_name;
use crisol_core::paths::fake::FakePrivateDirs;
use crisol_core::paths::CrisolPaths;
use crisol_core::ports::fake::FakePortProbe;
use crisol_core::process::fake::FakeSupervisor;
use crisol_core::project::TrustAllOwners;
use crisol_core::web::AlwaysEffective;
use crisol_daemon::{run, Options, Timing};
use crisol_platform::{connect_to_daemon, DaemonConnection};
use crisol_protocol::{Call, ClientError, ErrorCode, Event, Reply, PROTOCOL_VERSION};
use tempfile::TempDir;

struct Running {
    _tmp: TempDir,
    paths: CrisolPaths,
    pipe: String,
    thread: Option<JoinHandle<std::io::Result<()>>>,
}

fn deps() -> Deps {
    Deps {
        supervisor: Arc::new(FakeSupervisor::new()),
        probe: Arc::new(FakePortProbe::new()),
        dirs: Arc::new(FakePrivateDirs::new()),
        fetcher: Arc::new(FakeFetcher::new()),
        owners: Arc::new(TrustAllOwners),
        env: Arc::new(|_| None),
        system: Arc::new(NoSystemChecks),
        roots: Arc::new(NoRoots),
        config_probe: Arc::new(AlwaysEffective),
        db_admin: Arc::new(NoAdmin),
        catalog: Catalog::embedded().unwrap(),
        target: Target {
            os: Os::Windows,
            arch: Arch::X86_64,
        },
    }
}

fn start_daemon() -> Running {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    fs::create_dir_all(paths.www_dir().join("blog")).unwrap();
    let pipe = pipe_name(paths.home(), "S-1-5-21-1-2-3-1001");
    let options = Options {
        paths: paths.clone(),
        deps: deps(),
        pipe: pipe.clone(),
        dns: None,
        timing: Timing::default(),
    };
    let thread = std::thread::spawn(move || run(options));
    // The pipe appears a moment after the thread starts.
    let end = Instant::now() + Duration::from_secs(10);
    while connect_to_daemon(&pipe).is_err() {
        assert!(Instant::now() < end, "the daemon never opened its pipe");
        std::thread::sleep(Duration::from_millis(50));
    }
    Running {
        _tmp: tmp,
        paths,
        pipe,
        thread: Some(thread),
    }
}

impl Running {
    fn connect(&self) -> DaemonConnection {
        connect_to_daemon(&self.pipe).unwrap()
    }

    fn shut_down(&mut self) {
        let mut client = self.connect();
        client.call(&Call::Shutdown).unwrap();
        let result = self.thread.take().unwrap().join().unwrap();
        result.unwrap();
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.thread.is_some() {
            if let Ok(mut client) = connect_to_daemon(&self.pipe) {
                let _ = client.call(&Call::Shutdown);
            }
        }
    }
}

#[test]
fn a_client_says_hello_asks_and_the_daemon_leaves_when_told_to() {
    let mut daemon = start_daemon();
    let mut client = daemon.connect();

    match client
        .call(&Call::Hello {
            client: "pipe test".to_owned(),
            protocol: PROTOCOL_VERSION,
        })
        .unwrap()
    {
        Reply::Hello(info) => {
            assert_eq!(info.protocol, PROTOCOL_VERSION);
            assert_eq!(info.home, daemon.paths.home().display().to_string());
        }
        other => panic!("{other:?}"),
    }
    match client.call(&Call::Sites).unwrap() {
        Reply::Sites { sites, .. } => {
            assert_eq!(
                sites.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                ["blog"]
            );
        }
        other => panic!("{other:?}"),
    }
    // An error comes back as an error, on the same connection, which stays usable.
    match client.call(&Call::Unlink {
        name: "ghost".to_owned(),
    }) {
        Err(ClientError::Rpc(error)) => assert_eq!(error.code, ErrorCode::NotFound),
        other => panic!("{other:?}"),
    }
    assert!(client.call(&Call::Status { service: None }).is_ok());
    drop(client);

    daemon.shut_down();
    // Gone: nobody listens any more.
    assert!(connect_to_daemon(&daemon.pipe).is_err());

    // The daemon told its story in `logs/crisold/runtime.log`, one line each, with a time a
    // person can read (not seconds since 1970).
    let log = fs::read_to_string(daemon.paths.runtime_log("crisold").unwrap()).unwrap();
    let first = log.lines().next().unwrap();
    assert!(
        first.contains("INFO crisold ") && first.contains("starting in"),
        "{log}"
    );
    assert!(
        first.starts_with(char::is_numeric) && first.as_bytes()[4] == b'-',
        "{first}"
    );
    assert!(
        log.lines().any(|l| l.ends_with("INFO shutting down")),
        "{log}"
    );
    assert!(
        log.lines()
            .last()
            .unwrap()
            .ends_with("INFO crisold stopped"),
        "{log}"
    );
}

#[test]
fn several_clients_at_once_each_get_their_own_answers() {
    let mut daemon = start_daemon();
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let pipe = daemon.pipe.clone();
            std::thread::spawn(move || {
                let mut client = connect_to_daemon(&pipe).unwrap();
                for _ in 0..5 {
                    let reply = client.call(&Call::Sites).unwrap();
                    assert!(matches!(reply, Reply::Sites { .. }), "client {i}");
                    let status = client.call(&Call::Status { service: None }).unwrap();
                    assert!(matches!(status, Reply::Status(_)), "client {i}");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    daemon.shut_down();
}

#[test]
fn a_second_daemon_for_the_same_home_does_not_start() {
    let mut daemon = start_daemon();
    let options = Options {
        paths: daemon.paths.clone(),
        deps: deps(),
        pipe: daemon.pipe.clone(),
        dns: None,
        timing: Timing::default(),
    };
    // Not a failure: it leaves, with exit code 0, and says why in the log.
    run(options).unwrap();
    let log = fs::read_to_string(daemon.paths.runtime_log("crisold").unwrap()).unwrap();
    assert!(
        log.lines()
            .any(|l| l.ends_with("INFO another crisold is already running for this home; leaving")),
        "{log}"
    );
    // The first one is not disturbed.
    assert!(daemon.connect().call(&Call::Sites).is_ok());
    daemon.shut_down();
}

#[test]
fn subscribers_hear_what_happens_and_others_do_not() {
    let mut daemon = start_daemon();
    let mut listener = daemon.connect();
    listener.call(&Call::Subscribe).unwrap();
    let mut other = daemon.connect();
    // A change of sites with nothing running reloads nothing; shutting down is an event.
    other.call(&Call::Shutdown).unwrap();
    let event = listener.next_event().unwrap();
    assert_eq!(event, Some(Event::ShuttingDown));
    daemon.thread.take().unwrap().join().unwrap().unwrap();
}

#[test]
fn garbage_on_the_pipe_is_answered_and_does_not_hurt_the_daemon() {
    use std::io::{BufRead, BufReader, Write};

    let mut daemon = start_daemon();
    // Opened by hand, without the client's retry: the daemon may still be making the next
    // instance of the pipe (error 231, "all instances busy"), so ask again for a moment.
    let end = Instant::now() + Duration::from_secs(5);
    let raw = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&daemon.pipe)
        {
            Ok(file) => break file,
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("{e}"),
        }
    };
    let mut reader = BufReader::new(raw.try_clone().unwrap());
    let mut writer = raw;
    for junk in [
        "not json\n",
        "{\"jsonrpc\":\"1.0\",\"id\":3,\"method\":\"sites\"}\n",
        "{}\n",
    ] {
        writer.write_all(junk.as_bytes()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("\"error\""), "{line}");
    }
    // The same connection still works afterwards.
    writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"sites\"}\n")
        .unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(
        line.contains("\"result\"") && line.contains("\"id\":9"),
        "{line}"
    );
    drop((reader, writer));
    daemon.shut_down();
}

#[test]
fn clients_that_open_the_pipe_and_leave_do_not_bring_the_daemon_down() {
    let mut daemon = start_daemon();
    for _ in 0..50 {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&daemon.pipe)
            {
                Ok(file) => {
                    drop(file);
                    break;
                }
                Err(e) if e.raw_os_error() == Some(231) && Instant::now() < end => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }
    assert!(daemon
        .connect()
        .call(&Call::Status { service: None })
        .is_ok());
    daemon.shut_down();
}
