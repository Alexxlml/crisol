//! One client connection: lines in, lines out.
//!
//! It is generic over the stream so it can be tested over an in-memory duplex as well as over a
//! named pipe. A connection reads requests one after the other, hands each to the actor and
//! writes the answer; after `subscribe` it also forwards the daemon's events. Whatever a client
//! sends, a failure here ends that connection and nothing else: a line that is not a request is
//! answered with an error, and one longer than the limit closes the connection.

use std::io;
use std::sync::mpsc::Sender;

use crisol_protocol::{
    from_line, to_line, Call, DoneInfo, ErrorCode, Event, Notification, Reply, Request, Response,
    RpcError, JSONRPC, MAX_LINE,
};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::actor::{Input, Msg};

/// What every connection shares.
#[derive(Debug, Clone)]
pub struct Shared {
    /// To the actor.
    pub actor: Sender<Input>,
    /// Every event, for the connections that subscribed.
    pub events: broadcast::Sender<Event>,
}

/// Serves one client until it hangs up or breaks the protocol.
pub async fn serve<S>(stream: S, shared: Shared)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let (out, mut lines) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() || write.flush().await.is_err() {
                break;
            }
        }
    });

    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let Ok(Some(line)) = read_line(&mut reader).await else {
            break;
        };
        let request: Request = match from_line::<Request>(&line) {
            Ok(request) if request.jsonrpc == JSONRPC => request,
            Ok(request) => {
                send(
                    &out,
                    &Response::failed(
                        Some(request.id),
                        RpcError::new(ErrorCode::InvalidRequest, "jsonrpc must be \"2.0\""),
                    ),
                );
                continue;
            }
            Err(error) => {
                send(
                    &out,
                    &Response::failed(
                        None,
                        RpcError::new(ErrorCode::ParseError, error.to_string()),
                    ),
                );
                continue;
            }
        };
        match request.call {
            Call::Subscribe => {
                if forwarder.is_none() {
                    forwarder = Some(forward_events(shared.events.subscribe(), out.clone()));
                }
                let done = Reply::Done(DoneInfo {
                    message: "Subscribed to events".to_owned(),
                    changed: forwarder.is_some(),
                    warnings: Vec::new(),
                });
                send(&out, &Response::ok(request.id, done));
            }
            call => {
                let response = ask(&shared, call, &out).await;
                let response = match response {
                    Ok(reply) => Response::ok(request.id, reply),
                    Err(error) => Response::failed(Some(request.id), error),
                };
                send(&out, &response);
            }
        }
    }
    if let Some(task) = forwarder {
        task.abort();
    }
    drop(out);
    let _ = writer.await;
}

/// Gives the call to the actor and waits for the answer, passing on the events of the call as
/// they come.
async fn ask(
    shared: &Shared,
    call: Call,
    out: &mpsc::UnboundedSender<String>,
) -> Result<Reply, RpcError> {
    let (reply_tx, mut reply_rx) = oneshot::channel();
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
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
                    send(out, &Notification::new(event));
                }
            }
            outcome = &mut reply_rx => {
                return outcome.unwrap_or_else(|_| Err(RpcError::new(ErrorCode::Internal, "the daemon stopped before answering")));
            }
        }
    }
}

fn forward_events(
    mut events: broadcast::Receiver<Event>,
    out: mpsc::UnboundedSender<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => send(&out, &Notification::new(event)),
                // A subscriber that is too slow loses events, not its connection.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn send<T: serde::Serialize>(out: &mpsc::UnboundedSender<String>, message: &T) {
    if let Ok(line) = to_line(message) {
        let _ = out.send(line);
    }
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
