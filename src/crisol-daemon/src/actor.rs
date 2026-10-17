//! The thread that owns the [`Daemon`].
//!
//! Everything that touches the services happens here, one thing at a time, so the state needs no
//! locks. Connections send it [`Msg`]s; it answers through the channels they bring. Between
//! messages it ticks the services every [`TICK`], and it holds the answer to `start`, `stop`,
//! `restart` and `shutdown` until the daemon is settled, which is what makes `crisol start`
//! return when Nginx is really up (or has failed, and why).

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crisol_core::daemon::Daemon;
use crisol_protocol::{Call, DoneInfo, ErrorCode, Event, Reply, RpcError};
use tokio::sync::{broadcast, oneshot};

/// How often the services are advanced when nobody is asking for anything.
const TICK: Duration = Duration::from_millis(150);

/// How long a `start` or a `stop` is waited for before the client is told it is still going.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(60);

/// What a connection asks of the actor.
#[derive(Debug)]
pub struct Msg {
    /// The operation.
    pub call: Call,
    /// Where the outcome goes.
    pub reply: oneshot::Sender<Result<Reply, RpcError>>,
    /// Where the events of this call go (the progress of an installation).
    pub events: tokio::sync::mpsc::UnboundedSender<Event>,
}

/// An answer held until the daemon settles.
struct Waiter {
    call: Call,
    first: Reply,
    reply: oneshot::Sender<Result<Reply, RpcError>>,
    deadline: Instant,
}

/// Runs until `shutdown` was asked and everything is down, or until every sender is gone.
pub fn run(
    mut daemon: Daemon,
    messages: &Receiver<Msg>,
    events: &broadcast::Sender<Event>,
    log: &mut dyn FnMut(&str),
) {
    let mut waiters: Vec<Waiter> = Vec::new();
    loop {
        match messages.recv_timeout(TICK) {
            Ok(msg) => answer(&mut daemon, msg, events, &mut waiters, log),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for event in daemon.tick(Instant::now()) {
            log_event(&event, log);
            let _ = events.send(event);
        }
        release(&mut daemon, &mut waiters);
        if daemon.shutdown_requested() && daemon.settled() && waiters.is_empty() {
            break;
        }
    }
}

fn answer(
    daemon: &mut Daemon,
    msg: Msg,
    events: &broadcast::Sender<Event>,
    waiters: &mut Vec<Waiter>,
    log: &mut dyn FnMut(&str),
) {
    let Msg {
        call,
        reply,
        events: mine,
    } = msg;
    let mut emit = |event: Event| {
        log_event(&event, &mut *log);
        let _ = mine.send(event.clone());
        let _ = events.send(event);
    };
    let outcome = daemon.handle(&call, Instant::now(), &mut emit);
    let waits = matches!(
        call,
        Call::Start { .. } | Call::Stop { .. } | Call::Restart { .. } | Call::Shutdown
    );
    match outcome {
        Ok(first) if waits && !daemon.settled() => waiters.push(Waiter {
            call,
            first,
            reply,
            deadline: Instant::now() + SETTLE_TIMEOUT,
        }),
        Ok(first) => {
            let _ = reply.send(finish(daemon, &call, first));
        }
        Err(error) => {
            let _ = reply.send(Err(error));
        }
    }
}

/// Answers the waiters whose wait is over.
fn release(daemon: &mut Daemon, waiters: &mut Vec<Waiter>) {
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
                    SETTLE_TIMEOUT.as_secs()
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
        Call::Start { .. } | Call::Restart { .. } => {
            if let Some(failure) = daemon.failure() {
                return Err(RpcError::new(ErrorCode::Conflict, failure));
            }
            let warnings = match &first {
                Reply::Done(info) => info.warnings.clone(),
                _ => Vec::new(),
            };
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
                message: if matches!(call, Call::Shutdown) {
                    "Everything is stopped and the daemon is leaving".to_owned()
                } else {
                    "Everything is stopped".to_owned()
                },
                ..info
            }),
            other => other,
        }),
        _ => Ok(first),
    }
}

fn log_event(event: &Event, log: &mut dyn FnMut(&str)) {
    match event {
        Event::ServiceState {
            service,
            state,
            detail,
        } => log(&format!(
            "{service}: {state}{}",
            detail
                .as_ref()
                .map_or_else(String::new, |d| format!(" ({d})"))
        )),
        Event::ReloadFailed { reason } => log(&format!("reload failed: {reason}")),
        Event::Blocked { reason } => log(&format!("blocked: {reason}")),
        Event::Reloaded => log("nginx reloaded"),
        Event::ShuttingDown => log("shutting down"),
        _ => {}
    }
}
