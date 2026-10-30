//! One client connection: lines in, lines out.
//!
//! It is generic over the stream so it can be tested over an in-memory duplex as well as over a
//! named pipe. A connection reads requests one after the other, hands each to the actor and
//! writes the answer; after `subscribe` it also forwards the daemon's events. Whatever a client
//! sends, a failure here ends that connection and nothing else: a line that is not a request is
//! answered with an error, and one longer than the limit closes the connection.
//!
//! **A client that does not read** costs a bounded amount of memory. Everything a connection
//! writes waits in a queue of [`OUT_CAPACITY`] lines (and the progress of one call in one of
//! [`EVENT_CAPACITY`]). When it is full, an event is dropped (the next one tells the same story,
//! and a subscriber that cannot keep up loses events, not its connection), but an *answer* that
//! does not fit means the client is not listening at all: the connection is closed.
//!
//! **Leaving.** When the daemon is about to exit it raises `closing`. Each connection then stops
//! reading, delivers what is already queued for it (the answer to `shutdown`, the last events)
//! and ends, and the daemon waits for the connections instead of for a fixed time.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crisol_protocol::{
    from_line, to_line, Call, DoneInfo, ErrorCode, Event, Notification, Numbered, Reply, Request,
    Response, RpcError, JSONRPC, MAX_LINE,
};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Notify};
use tracing::{debug, warn};

use crate::actor::{Input, Msg};
use crate::bus::Bus;

/// Lines waiting to be written to one client.
pub const OUT_CAPACITY: usize = 1024;

/// Events of one call waiting to be passed on (the progress of a download).
pub const EVENT_CAPACITY: usize = 256;

/// How long a connection that is ending waits for its client to take what is queued for it.
const WRITE_GRACE: Duration = Duration::from_secs(3);

/// What every connection shares.
#[derive(Debug, Clone)]
pub struct Shared {
    /// To the actor.
    pub actor: std::sync::mpsc::Sender<Input>,
    /// Every event, for the connections that subscribed.
    pub events: Bus,
    /// Becomes `true` when the daemon is leaving.
    pub closing: watch::Receiver<bool>,
}

/// The way into the queue of one connection.
#[derive(Debug, Clone)]
struct Out {
    lines: mpsc::Sender<String>,
    /// Raised when an answer did not fit.
    overflow: Arc<Notify>,
}

impl Out {
    /// Queues an event notification; if the queue is full it is dropped.
    fn notify<T: serde::Serialize>(&self, message: &T) {
        let Ok(line) = to_line(message) else { return };
        if let Err(TrySendError::Full(_)) = self.lines.try_send(line) {
            debug!("a client is not reading: an event was dropped");
        }
    }

    /// Queues the answer to a request; if the queue is full the client is not listening and the
    /// connection ends.
    fn respond<T: serde::Serialize>(&self, message: &T) {
        let Ok(line) = to_line(message) else { return };
        if let Err(TrySendError::Full(_)) = self.lines.try_send(line) {
            warn!("a client does not read its answers: closing its connection");
            self.overflow.notify_one();
        }
    }
}

/// Serves one client until it hangs up, breaks the protocol, stops reading, or the daemon leaves.
pub async fn serve<S>(stream: S, shared: Shared)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let (lines, mut queue) = mpsc::channel::<String>(OUT_CAPACITY);
    let out = Out {
        lines,
        overflow: Arc::new(Notify::new()),
    };
    let mut writer = tokio::spawn(async move {
        while let Some(line) = queue.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() || write.flush().await.is_err() {
                break;
            }
        }
    });

    let mut closing = shared.closing.clone();
    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    let mut overflowed = false;
    loop {
        let line = tokio::select! {
            line = read_line(&mut reader) => line,
            () = out.overflow.notified() => { overflowed = true; break; }
            _ = closing.wait_for(|leaving| *leaving) => break,
        };
        let Ok(Some(line)) = line else {
            break;
        };
        let request: Request = match from_line::<Request>(&line) {
            Ok(request) if request.jsonrpc == JSONRPC => request,
            Ok(request) => {
                out.respond(&Response::failed(
                    Some(request.id),
                    RpcError::new(ErrorCode::InvalidRequest, "jsonrpc must be \"2.0\""),
                ));
                continue;
            }
            Err(error) => {
                out.respond(&Response::failed(
                    None,
                    RpcError::new(ErrorCode::ParseError, error.to_string()),
                ));
                continue;
            }
        };
        match request.call {
            Call::Subscribe => {
                if forwarder.is_none() {
                    forwarder = Some(forward_events(
                        shared.events.subscribe(),
                        out.clone(),
                        shared.closing.clone(),
                    ));
                }
                let done = Reply::Done(DoneInfo {
                    message: "Subscribed to events".to_owned(),
                    changed: forwarder.is_some(),
                    warnings: Vec::new(),
                });
                out.respond(&Response::ok(request.id, done));
            }
            call => {
                let response = ask(&shared, call, &out).await;
                let response = match response {
                    Ok(reply) => Response::ok(request.id, reply),
                    Err(error) => Response::failed(Some(request.id), error),
                };
                out.respond(&response);
            }
        }
    }
    if overflowed {
        // The client is not reading, so nothing more is worth writing to it.
        if let Some(task) = forwarder {
            task.abort();
        }
        writer.abort();
        return;
    }
    // Let the events already queued reach the client before the connection goes: the forwarder
    // hands them over when the daemon is leaving; for a client that simply hung up it is stopped.
    if let Some(task) = forwarder {
        if *shared.closing.borrow() {
            let _ = task.await;
        } else {
            task.abort();
        }
    }
    drop(out);
    // The writer ends when its queue is empty; a client that does not take what is left is not
    // waited for.
    if tokio::time::timeout(WRITE_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

/// Gives the call to the actor and waits for the answer, passing on the events of the call as
/// they come.
async fn ask(shared: &Shared, call: Call, out: &Out) -> Result<Reply, RpcError> {
    let (reply_tx, mut reply_rx) = oneshot::channel();
    let (events_tx, mut events_rx) = mpsc::channel(EVENT_CAPACITY);
    let msg = Msg {
        call,
        reply: reply_tx,
        events: events_tx,
    };
    if shared.actor.send(Input::Request(msg)).is_err() {
        return Err(RpcError::new(
            ErrorCode::Internal,
            "the daemon is shutting down",
        ));
    }
    loop {
        tokio::select! {
            Some(event) = events_rx.recv() => {
                if matches!(event, Event::Progress { .. }) {
                    out.notify(&Notification::new(event));
                }
            }
            outcome = &mut reply_rx => {
                return outcome.unwrap_or_else(|_| Err(RpcError::new(ErrorCode::Internal, "the daemon stopped before answering")));
            }
        }
    }
}

fn forward_events(
    mut events: broadcast::Receiver<Numbered>,
    out: Out,
    mut closing: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                received = events.recv() => match received {
                    Ok(event) => out.notify(&Notification::numbered(event)),
                    // A subscriber that is too slow loses events, not its connection.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = closing.wait_for(|leaving| *leaving) => {
                    // The daemon is leaving: pass on what was said before it did.
                    while let Ok(event) = events.try_recv() {
                        out.notify(&Notification::numbered(event));
                    }
                    break;
                }
            }
        }
    })
}

/// One line of at most [`MAX_LINE`] bytes; `None` at the end of the stream.
async fn read_line<R: AsyncBufRead + AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let read = (&mut *reader)
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    } else if bytes.len() > MAX_LINE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a message is too long",
        ));
    } else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the connection ended in the middle of a message",
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a message is not UTF-8"))
}

#[cfg(test)]
mod tests;
