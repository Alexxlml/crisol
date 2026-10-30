//! The client side of a connection, over any pair of byte streams.
//!
//! [`Client`] sends a [`Request`] per call and reads lines until the [`Response`] with the same
//! id arrives, handing any [`Event`] pushed in between to a callback. It does not know what the
//! streams are (a named pipe, a socket, a buffer in a test) and it blocks on them.

use std::io::{self, BufRead, Write};

use crate::MAX_LINE;
use crate::{
    from_line, to_line, Call, Event, LineError, Message, Numbered, Reply, Request, RpcError,
};

/// Why a call did not give a reply.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The stream failed.
    #[error("connection to the daemon: {0}")]
    Io(#[from] io::Error),
    /// The daemon closed the connection.
    #[error("the daemon closed the connection")]
    Closed,
    /// The other side sent something that is not this protocol.
    #[error("the daemon sent something unexpected: {0}")]
    Protocol(String),
    /// The daemon answered with an error.
    #[error("{0}")]
    Rpc(#[from] RpcError),
}

/// A connection to a daemon.
#[derive(Debug)]
pub struct Client<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
}

impl<R: BufRead, W: Write> Client<R, W> {
    /// A client over these streams.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
        }
    }

    /// The stream requests are written to.
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// Makes `call` and waits for its reply. Events pushed meanwhile are dropped.
    pub fn call(&mut self, call: &Call) -> Result<Reply, ClientError> {
        self.call_with_events(call, &mut |_| {})
    }

    /// Makes `call` and waits for its reply, giving `on_event` every event that arrives first
    /// (the progress of an installation, for one).
    pub fn call_with_events(
        &mut self,
        call: &Call,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Reply, ClientError> {
        self.call_with_numbered(call, &mut |numbered| on_event(numbered.event))
    }

    /// Like [`call_with_events`](Self::call_with_events), with the number of each event: what
    /// a subscriber needs to tell which ones a `status` answer already includes.
    pub fn call_with_numbered(
        &mut self,
        call: &Call,
        on_event: &mut dyn FnMut(Numbered),
    ) -> Result<Reply, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = to_line(&Request::new(id, call.clone()))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        loop {
            match self.read_message()? {
                Message::Notification(note) => on_event(note.params),
                Message::Response(response) if response.id == Some(id) => {
                    return Ok(response.into_result()?);
                }
                Message::Response(response) if response.id.is_none() => {
                    // The daemon could not even read the request.
                    return Ok(response.into_result()?);
                }
                Message::Response(other) => {
                    return Err(ClientError::Protocol(format!(
                        "an answer to request {:?} while waiting for {id}",
                        other.id
                    )))
                }
            }
        }
    }

    /// Waits for the next event (after `subscribe`). `None` when the daemon closes the
    /// connection.
    pub fn next_event(&mut self) -> Result<Option<Event>, ClientError> {
        Ok(self.next_numbered()?.map(|numbered| numbered.event))
    }

    /// Like [`next_event`](Self::next_event), with the event's number.
    pub fn next_numbered(&mut self) -> Result<Option<Numbered>, ClientError> {
        loop {
            match self.read_message() {
                Ok(Message::Notification(note)) => return Ok(Some(note.params)),
                Ok(Message::Response(_)) => {}
                Err(ClientError::Closed) => return Ok(None),
                Err(other) => return Err(other),
            }
        }
    }

    fn read_message(&mut self) -> Result<Message, ClientError> {
        let line = read_bounded_line(&mut self.reader)?.ok_or(ClientError::Closed)?;
        from_line(&line).map_err(|e| match e {
            LineError::TooLong => ClientError::Protocol(e.to_string()),
            LineError::Invalid(e) => ClientError::Protocol(e.to_string()),
        })
    }
}

/// One line of at most [`MAX_LINE`] bytes, without its newline. `None` at the end of the
/// stream. A longer line is an error: the peer is broken or hostile, and nothing past it is
/// read.
pub fn read_bounded_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let (consumed, found) = {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                return if bytes.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the connection ended in the middle of a message",
                    ))
                };
            }
            if let Some(at) = buffer.iter().position(|b| *b == b'\n') {
                bytes.extend_from_slice(&buffer[..at]);
                (at + 1, true)
            } else {
                bytes.extend_from_slice(buffer);
                (buffer.len(), false)
            }
        };
        reader.consume(consumed);
        if bytes.len() > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a message is longer than {MAX_LINE} bytes"),
            ));
        }
        if found {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a message is not UTF-8"));
        }
    }
}
