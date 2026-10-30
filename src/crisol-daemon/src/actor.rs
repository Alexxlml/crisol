//! The thread that owns the [`Daemon`].
//!
//! Everything that touches the services happens here, one thing at a time, so the state needs no
//! locks. Connections send it [`Input`]s; it answers through the channels they bring. Between
//! messages it ticks the services every [`TICK`], and it holds the answer to `start`, `stop`,
//! `restart` and `shutdown` until the daemon is settled, which is what makes `crisol start`
//! return when Nginx is really up (or has failed, and why).
//!
//! The operations that can take long (`install`, `uninstall`, `doctor`) do not run here: one
//! worker thread runs them in order and leaves each result in the actor's inbox like any other
//! message. While a download goes on the services keep being ticked (a crashed worker is
//! restarted) and `status` answers. A `start` or `restart` during an installation is refused with
//! `Conflict` and a message that says what is going on; `stop` and `shutdown` are not.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crisol_core::daemon::Daemon;
use crisol_core::engine::Engine;
use crisol_protocol::{Call, DoneInfo, ErrorCode, Event, Reply, RpcError};
use tokio::sync::{broadcast, oneshot};
use tracing::{info, warn};

/// How often the services are advanced when nobody is asking for anything.
const TICK: Duration = Duration::from_millis(150);

/// The waits of the actor. The defaults are the real ones; tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How long a `start` or a `stop` is waited for before the client is told it is still going.
    pub settle: Duration,
    /// After the answer to `shutdown` is due, how much longer the services get to stop before
    /// the daemon kills their groups and leaves anyway.
    pub exit_margin: Duration,
    /// Once everything is down, how long a long operation (a download) still running gets to
    /// end before the daemon leaves without it.
    pub job_grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            settle: Duration::from_secs(60),
            exit_margin: Duration::from_secs(10),
            job_grace: Duration::from_secs(10),
        }
    }
}

/// What reaches the actor: a request from a connection, or the end of a long operation from
/// the worker. Both are messages like any other, so the actor never waits for the second.
#[derive(Debug)]
pub enum Input {
    /// A client asks.
    Request(Msg),
    /// The worker finished a long operation.
    Finished(Finished),
}

/// What a connection asks of the actor.
#[derive(Debug)]
pub struct Msg {
    /// The operation.
    pub call: Call,
    /// Where the outcome goes.
    pub reply: oneshot::Sender<Result<Reply, RpcError>>,
    /// Where the events of this call go (the progress of an installation).
    pub events: tokio::sync::mpsc::Sender<Event>,
}

/// A long operation, queued for the worker.
struct Job {
    call: Call,
    label: String,
    reply: oneshot::Sender<Result<Reply, RpcError>>,
    events: tokio::sync::mpsc::Sender<Event>,
}

/// A long operation that ended.
#[derive(Debug)]
pub struct Finished {
    label: String,
    reply: oneshot::Sender<Result<Reply, RpcError>>,
    outcome: Result<Reply, RpcError>,
}

/// An answer held until the daemon settles.
struct Waiter {
    call: Call,
    first: Reply,
    reply: oneshot::Sender<Result<Reply, RpcError>>,
    deadline: Instant,
}

/// Runs until `shutdown` was asked and everything is down (or the time to get there ran out),
/// or until every sender is gone.
///
/// `inbox` is a sender to the channel `messages` reads: it is where the worker leaves its
/// results.
pub fn run(
    mut daemon: Daemon,
    messages: &Receiver<Input>,
    inbox: Sender<Input>,
    events: &broadcast::Sender<Event>,
    timing: Timing,
) {
    let jobs = spawn_worker(daemon.engine().clone(), events.clone(), inbox);
    let mut waiters: Vec<Waiter> = Vec::new();
    // The label of every long operation queued or running, oldest first.
    let mut busy: Vec<String> = Vec::new();
    let mut shutdown_at: Option<Instant> = None;
    let mut quiet_since: Option<Instant> = None;
    loop {
        match messages.recv_timeout(TICK) {
            Ok(Input::Request(msg)) => {
                if matches!(msg.call, Call::Shutdown) {
                    shutdown_at.get_or_insert_with(Instant::now);
                }
                let mut ctx = Ctx {
                    daemon: &mut daemon,
                    events,
                    waiters: &mut waiters,
                    jobs: jobs.as_ref(),
                    busy: &mut busy,
                    timing,
                };
                ctx.answer(msg);
            }
            Ok(Input::Finished(done)) => {
                if let Some(at) = busy.iter().position(|label| *label == done.label) {
                    busy.remove(at);
                }
                info!("{}: finished", done.label);
                // The worker sees the files; what the services are doing is the actor's to add.
                let outcome = if done.label == "doctor" {
                    done.outcome.map(|reply| daemon.complete_doctor(reply))
                } else {
                    done.outcome
                };
                let _ = done.reply.send(outcome);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for event in daemon.tick(Instant::now()) {
            log_event(&event);
            let _ = events.send(event);
        }
        for note in daemon.take_notes() {
            info!("{note}");
        }
        release(&mut daemon, &mut waiters, timing.settle);
        if !daemon.shutdown_requested() {
            continue;
        }
        let now = Instant::now();
        if daemon.settled() && waiters.is_empty() {
            let since = *quiet_since.get_or_insert(now);
            if busy.is_empty() || now >= since + timing.job_grace {
                if !busy.is_empty() {
                    warn!("leaving with a long operation still running");
                }
                say_goodbye(events);
                break;
            }
        }
        // The services did not stop in time: kill them rather than leave a daemon that no
        // longer answers anybody and never ends.
        if shutdown_at.is_some_and(|at| now >= at + timing.settle + timing.exit_margin) {
            warn!("the services did not stop in time; killing them and leaving");
            daemon.abort();
            for waiter in waiters.drain(..) {
                let _ = waiter.reply.send(Ok(Reply::Done(DoneInfo {
                    message: "The daemon is leaving; some services were killed".to_owned(),
                    changed: true,
                    warnings: Vec::new(),
                })));
            }
            say_goodbye(events);
            break;
        }
    }
}

/// The last event: every service is down (or was killed) and the daemon is about to leave, so
/// a subscriber that stops reading here has missed nothing.
fn say_goodbye(events: &broadcast::Sender<Event>) {
    log_event(&Event::ShuttingDown);
    let _ = events.send(Event::ShuttingDown);
}

/// The worker thread: runs the long operations one after another and sends each result to the
/// actor. It ends when the actor drops its end. `None` if the thread could not be made; every
/// long operation is then refused.
fn spawn_worker(
    engine: Engine,
    events: broadcast::Sender<Event>,
    inbox: Sender<Input>,
) -> Option<Sender<Job>> {
    let (jobs, queue) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("crisold-worker".to_owned())
        .spawn(move || {
            for job in queue {
                let Job {
                    call,
                    label,
                    reply,
                    events: mine,
                } = job;
                let mut emit = |event: Event| {
                    // A client that does not read loses progress, not the daemon its memory.
                    let _ = mine.try_send(event.clone());
                    let _ = events.send(event);
                };
                let outcome = catch_unwind(AssertUnwindSafe(|| engine.handle(&call, &mut emit)))
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| {
                        Err(RpcError::new(
                            ErrorCode::Internal,
                            format!("`{}` stopped unexpectedly", call.method()),
                        ))
                    });
                let finished = Finished {
                    label,
                    reply,
                    outcome,
                };
                if inbox.send(Input::Finished(finished)).is_err() {
                    break;
                }
            }
        })
        .ok()
        .map(|_| jobs)
}

/// What `answer` works on.
struct Ctx<'a> {
    daemon: &'a mut Daemon,
    events: &'a broadcast::Sender<Event>,
    waiters: &'a mut Vec<Waiter>,
    jobs: Option<&'a Sender<Job>>,
    busy: &'a mut Vec<String>,
    timing: Timing,
}

impl Ctx<'_> {
    fn answer(&mut self, msg: Msg) {
        let Msg {
            call,
            reply,
            events: mine,
        } = msg;
        if Daemon::is_long(&call) {
            self.queue(call, reply, mine);
            return;
        }
        // A start while the files of a tool are being changed would run half an installation.
        if matches!(call, Call::Start { .. } | Call::Restart { .. }) {
            if let Some(label) = self.busy.iter().find(|label| *label != "doctor") {
                let _ = reply.send(Err(RpcError::new(
                    ErrorCode::Conflict,
                    format!(
                        "{label} is in progress; run `crisol {}` again when it ends",
                        call.method()
                    ),
                )));
                return;
            }
        }
        let mut emit = |event: Event| {
            log_event(&event);
            // A client that does not read loses progress, not the daemon its memory.
            let _ = mine.try_send(event.clone());
            let _ = self.events.send(event);
        };
        let outcome = self.daemon.handle(&call, Instant::now(), &mut emit);
        let waits = matches!(
            call,
            Call::Start { .. } | Call::Stop { .. } | Call::Restart { .. } | Call::Shutdown
        );
        match outcome {
            Ok(first) if waits && !self.daemon.settled() => self.waiters.push(Waiter {
                call,
                first,
                reply,
                deadline: Instant::now() + self.timing.settle,
            }),
            Ok(first) => {
                let _ = reply.send(finish(self.daemon, &call, first));
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn queue(
        &mut self,
        call: Call,
        reply: oneshot::Sender<Result<Reply, RpcError>>,
        events: tokio::sync::mpsc::Sender<Event>,
    ) {
        let label = match &call {
            Call::Install {
                service, version, ..
            } => match version {
                Some(version) => format!("installing {service} {version}"),
                None => format!("installing {service}"),
            },
            Call::Uninstall { service, version } => format!("removing {service} {version}"),
            _ => "doctor".to_owned(),
        };
        info!("{label}: queued");
        let job = Job {
            call,
            label: label.clone(),
            reply,
            events,
        };
        match self.jobs {
            Some(jobs) => match jobs.send(job) {
                Ok(()) => self.busy.push(label),
                Err(mpsc::SendError(job)) => refuse(job.reply),
            },
            None => refuse(job.reply),
        }
    }
}

fn refuse(reply: oneshot::Sender<Result<Reply, RpcError>>) {
    let _ = reply.send(Err(RpcError::new(
        ErrorCode::Internal,
        "the daemon cannot run this operation right now",
    )));
}

/// Answers the waiters whose wait is over.
fn release(daemon: &mut Daemon, waiters: &mut Vec<Waiter>, settle: Duration) {
    if waiters.is_empty() {
        return;
    }
    let settled = daemon.settled();
    let now = Instant::now();
    let mut still = Vec::new();
    for waiter in waiters.drain(..) {
        if settled {
            let outcome = finish(daemon, &waiter.call, waiter.first);
            let _ = waiter.reply.send(outcome);
        } else if now >= waiter.deadline {
            let _ = waiter.reply.send(Err(RpcError::new(
                ErrorCode::Conflict,
                format!(
                    "`{}` is still in progress after {} seconds; `crisol status` shows where it is",
                    waiter.call.method(),
                    settle.as_secs()
                ),
            )));
        } else {
            still.push(waiter);
        }
    }
    *waiters = still;
}

/// The final answer of a call that waited: the services as they ended up, or why they did not
/// come up.
fn finish(daemon: &mut Daemon, call: &Call, first: Reply) -> Result<Reply, RpcError> {
    match call {
        Call::Start { service } | Call::Restart { service } => {
            if let Some(failure) = daemon.failure_of(service.as_deref()) {
                return Err(RpcError::new(ErrorCode::Conflict, failure));
            }
            let mut warnings = match &first {
                Reply::Done(info) => info.warnings.clone(),
                _ => Vec::new(),
            };
            // A database that failed is a warning on the start of everything, not its failure.
            if service.is_none() {
                warnings.extend(daemon.database_warnings());
            }
            let status =
                daemon.handle(&Call::Status { service: None }, Instant::now(), &mut |_| {})?;
            // The warnings of the start go with the final answer.
            Ok(match (status, warnings.is_empty()) {
                (Reply::Status(mut info), false) => {
                    info.warnings = warnings;
                    Reply::Status(info)
                }
                (other, _) => other,
            })
        }
        Call::Stop { .. } | Call::Shutdown => Ok(match first {
            Reply::Done(info) => Reply::Done(DoneInfo {
                message: match call {
                    Call::Shutdown => "Everything is stopped and the daemon is leaving".to_owned(),
                    Call::Stop {
                        service: Some(name),
                    } => format!("{name} is stopped"),
                    _ => "Everything is stopped".to_owned(),
                },
                ..info
            }),
            other => other,
        }),
        _ => Ok(first),
    }
}

fn log_event(event: &Event) {
    match event {
        Event::ServiceState {
            service,
            state,
            detail,
        } => info!(
            "{service}: {state}{}",
            detail
                .as_ref()
                .map_or_else(String::new, |d| format!(" ({d})"))
        ),
        Event::ReloadFailed { reason } => warn!("reload failed: {reason}"),
        Event::Blocked { reason } => warn!("blocked: {reason}"),
        Event::Reloaded => info!("nginx reloaded"),
        Event::ShuttingDown => info!("shutting down"),
        _ => {}
    }
}
