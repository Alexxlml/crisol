//! Starting the daemon: the pipe, the actor and the DNS responder.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crisol_core::daemon::Daemon;
use crisol_core::dns::{DnsServer, Zone};
use crisol_core::engine::{Deps, Engine};
use crisol_core::log::{LogRotation, RotatingLog};
use crisol_core::paths::CrisolPaths;
use crisol_platform::PipeSecurity;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{broadcast, oneshot};

use crate::actor::{self, Input, Timing};
use crate::connection::{serve, Shared};

/// Where the DNS responder listens and for which suffix.
#[derive(Debug, Clone)]
pub struct DnsOptions {
    /// The loopback address and port (`127.77.0.53:53`).
    pub addr: SocketAddr,
    /// The suffix it answers for (`test`).
    pub suffix: String,
}

/// What the daemon is built from.
#[derive(Debug)]
pub struct Options {
    /// The Crisol home.
    pub paths: CrisolPaths,
    /// The engine's dependencies.
    pub deps: Deps,
    /// The name of the pipe (`crisol_core::ipc::pipe_name`).
    pub pipe: String,
    /// The DNS responder, if it should run.
    pub dns: Option<DnsOptions>,
    /// How long the actor waits for services and operations. `Timing::default()` in production.
    pub timing: Timing,
}

/// Runs the daemon until `shutdown` is asked. Fails at once if another daemon already has the
/// pipe of this home.
pub fn run(options: Options) -> io::Result<()> {
    let Options {
        paths,
        deps,
        pipe,
        dns,
        timing,
    } = options;
    let mut log = DaemonLog::open(&paths);
    log.line(&format!(
        "crisold {} starting in {}",
        crisol_core::VERSION,
        paths.home().display()
    ));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (events, _) = broadcast::channel(256);
    let (to_actor, from_connections) = mpsc::channel::<Input>();
    let worker_inbox = to_actor.clone();
    let (done_tx, mut done_rx) = oneshot::channel::<()>();

    // The pipe comes first: if another daemon has it, nothing else is started.
    let first = {
        let _enter = runtime.enter();
        create_instance(&pipe, true).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("another crisold may already be running ({e})"),
            )
        })?
    };

    let dns_stop = Arc::new(AtomicBool::new(false));
    let dns_thread = dns
        .as_ref()
        .map(|options| start_dns(options, &dns_stop, &mut log));

    let daemon = Daemon::new(Engine::new(paths.clone(), deps));
    let actor_events = events.clone();
    let mut actor_log = DaemonLog::open(&paths);
    let actor = std::thread::Builder::new()
        .name("crisold-actor".to_owned())
        .spawn(move || {
            actor::run(
                daemon,
                &from_connections,
                worker_inbox,
                &actor_events,
                &mut |line| actor_log.line(line),
                timing,
            );
            let _ = done_tx.send(());
        })?;

    let shared = Shared {
        actor: to_actor,
        events,
    };
    let served = runtime.block_on(async {
        let mut instance = first;
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                connected = instance.connect() => {
                    // A failed connection (a client that opened the pipe and closed it at once
                    // is `ERROR_NO_DATA`) concerns that client only: log it and listen again.
                    let next = next_instance(&pipe, &mut log).await?;
                    let connection = std::mem::replace(&mut instance, next);
                    match connected {
                        Ok(()) => {
                            connections.spawn(serve(connection, shared.clone()));
                        }
                        Err(e) => log.line(&format!("a client could not connect: {e}")),
                    }
                }
                _ = &mut done_rx => break,
            }
        }
        // The answer to `shutdown` is on its way out through a connection: give the clients a
        // moment to read it and hang up before the runtime goes away with them.
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        Ok::<(), io::Error>(())
    });

    drop(shared);
    dns_stop.store(true, Ordering::Relaxed);
    if let Some(thread) = dns_thread {
        let _ = thread.join();
    }
    let _ = actor.join();
    log.line("crisold stopped");
    served
}

/// How many times in a row making the next pipe instance may fail before the daemon gives up.
const INSTANCE_ATTEMPTS: u32 = 5;

/// The next instance of the pipe to listen on. A failure is waited out and retried: only
/// several in a row mean the system cannot make pipes and the daemon cannot serve anyone.
async fn next_instance(name: &str, log: &mut DaemonLog) -> io::Result<NamedPipeServer> {
    let mut failures = 0;
    loop {
        match create_instance(name, false) {
            Ok(instance) => return Ok(instance),
            Err(e) => {
                failures += 1;
                log.line(&format!(
                    "could not make a pipe instance ({failures}/{INSTANCE_ATTEMPTS}): {e}"
                ));
                if failures >= INSTANCE_ATTEMPTS {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_millis(200 * u64::from(failures))).await;
            }
        }
    }
}

/// One instance of the pipe, for the current user only and not for remote clients.
fn create_instance(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let mut security = PipeSecurity::current_user_only()?;
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: the pointer is to a `SECURITY_ATTRIBUTES` that `security` keeps alive until the
    // end of this function, and the call copies the descriptor into the pipe.
    unsafe { options.create_with_security_attributes_raw(name, security.as_attributes_ptr()) }
}

fn start_dns(
    options: &DnsOptions,
    stop: &Arc<AtomicBool>,
    log: &mut DaemonLog,
) -> std::thread::JoinHandle<()> {
    let stop = Arc::clone(stop);
    let server = Zone::new(&options.suffix)
        .map_err(|e| e.to_string())
        .and_then(|zone| DnsServer::bind(options.addr, zone).map_err(|e| e.to_string()));
    match server {
        Ok(server) => {
            log.line(&format!(
                "DNS: answering for .{} on {}",
                options.suffix, options.addr
            ));
            std::thread::spawn(move || {
                server.run(&stop);
            })
        }
        Err(reason) => {
            log.line(&format!(
                "DNS: not answering for .{} ({reason})",
                options.suffix
            ));
            std::thread::spawn(|| {})
        }
    }
}

/// The daemon's own log: `logs/crisold/runtime.log`. A daemon that cannot write its log still
/// runs; it just has nothing to say it with.
struct DaemonLog(Option<RotatingLog>);

impl DaemonLog {
    fn open(paths: &CrisolPaths) -> Self {
        let log = paths.runtime_log("crisold").ok().and_then(|path| {
            RotatingLog::open(
                path,
                LogRotation {
                    max_bytes: 512 * 1024,
                    keep: 3,
                },
            )
            .ok()
        });
        Self(log)
    }

    fn line(&mut self, text: &str) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Some(log) = &mut self.0 {
            let _ = log.write(format!("{secs} {text}\n").as_bytes());
        }
    }
}
