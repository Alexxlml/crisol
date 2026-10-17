//! Starting the daemon: the pipe, the actor and the DNS responder.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crisol_core::daemon::Daemon;
use crisol_core::dns::{DnsServer, Zone};
use crisol_core::engine::{Deps, Engine};
use crisol_core::paths::CrisolPaths;
use crisol_platform::PipeSecurity;
use crisol_protocol::Call;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{broadcast, oneshot, watch};
use tracing::{error, info, warn};

use crate::actor::{self, Input, Msg, Timing};
use crate::connection::{serve, Shared};
use crate::logging;

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
    let dispatch = logging::dispatch(&options.paths);
    tracing::dispatcher::with_default(&dispatch.clone(), || {
        serve_until_shutdown(options, &dispatch)
    })
}

fn serve_until_shutdown(options: Options, dispatch: &tracing::Dispatch) -> io::Result<()> {
    let Options {
        paths,
        deps,
        pipe,
        dns,
        timing,
    } = options;
    info!(
        "crisold {} starting in {}",
        crisol_core::VERSION,
        paths.home().display()
    );

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
    let dns_thread = dns.as_ref().map(|options| start_dns(options, &dns_stop));

    let daemon = Daemon::new(Engine::new(paths.clone(), deps));
    let actor_events = events.clone();
    let actor_dispatch = dispatch.clone();
    let actor = std::thread::Builder::new()
        .name("crisold-actor".to_owned())
        .spawn(move || {
            tracing::dispatcher::with_default(&actor_dispatch, || {
                actor::run(
                    daemon,
                    &from_connections,
                    worker_inbox,
                    &actor_events,
                    timing,
                );
            });
            let _ = done_tx.send(());
        })?;

    let (closing_tx, closing) = watch::channel(false);
    let shared = Shared {
        actor: to_actor,
        events,
        closing,
    };
    let served = runtime.block_on(async {
        let mut instance = first;
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                connected = instance.connect() => {
                    // A failed connection (a client that opened the pipe and closed it at once
                    // is `ERROR_NO_DATA`) concerns that client only: log it and listen again.
                    let next = next_instance(&pipe).await?;
                    let connection = std::mem::replace(&mut instance, next);
                    match connected {
                        Ok(()) => {
                            connections.spawn(serve(connection, shared.clone()));
                        }
                        Err(e) => warn!("a client could not connect: {e}"),
                    }
                }
                _ = &mut done_rx => break,
            }
        }
        // The answer to `shutdown` is on its way out through a connection. Tell the connections
        // the daemon is leaving: each delivers what is queued for it and ends, and this waits
        // for them (not a fixed time; the cap is only for a client that takes nothing).
        let _ = closing_tx.send(true);
        let _ = tokio::time::timeout(CLOSE_CAP, async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        Ok::<(), io::Error>(())
    });

    if served.is_err() {
        // The pipe is lost for good: have the actor stop the services and leave, or `join`
        // below would wait for ever.
        let (reply, _) = oneshot::channel();
        let (events, _) = tokio::sync::mpsc::channel(1);
        let _ = shared.actor.send(Input::Request(Msg {
            call: Call::Shutdown,
            reply,
            events,
        }));
    }
    drop(shared);
    dns_stop.store(true, Ordering::Relaxed);
    if let Some(thread) = dns_thread {
        let _ = thread.join();
    }
    let _ = actor.join();
    info!("crisold stopped");
    served
}

/// The longest the daemon waits, once it is leaving, for its connections to deliver what is
/// queued for them and end.
const CLOSE_CAP: Duration = Duration::from_secs(5);

/// How many times in a row making the next pipe instance may fail before the daemon gives up.
const INSTANCE_ATTEMPTS: u32 = 5;

/// The next instance of the pipe to listen on. A failure is waited out and retried: only
/// several in a row mean the system cannot make pipes and the daemon cannot serve anyone.
async fn next_instance(name: &str) -> io::Result<NamedPipeServer> {
    let mut failures = 0;
    loop {
        match create_instance(name, false) {
            Ok(instance) => return Ok(instance),
            Err(e) => {
                failures += 1;
                warn!("could not make a pipe instance ({failures}/{INSTANCE_ATTEMPTS}): {e}");
                if failures >= INSTANCE_ATTEMPTS {
                    error!("giving up on the pipe");
                }
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

fn start_dns(options: &DnsOptions, stop: &Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let stop = Arc::clone(stop);
    let server = Zone::new(&options.suffix)
        .map_err(|e| e.to_string())
        .and_then(|zone| DnsServer::bind(options.addr, zone).map_err(|e| e.to_string()));
    match server {
        Ok(server) => {
            info!("DNS: answering for .{} on {}", options.suffix, options.addr);
            std::thread::spawn(move || {
                server.run(&stop);
            })
        }
        Err(reason) => {
            warn!("DNS: not answering for .{} ({reason})", options.suffix);
            std::thread::spawn(|| {})
        }
    }
}
