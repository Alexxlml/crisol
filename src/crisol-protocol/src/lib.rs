//! crisol-protocol: the contract between the daemon (`crisold`) and everything that talks to it
//! (the CLI today, a GUI or a web panel later).
//!
//! It is JSON-RPC 2.0, one JSON object per line (`\n`-terminated, no embedded newlines), over a
//! local pipe or socket. This crate has no I/O: it defines the messages, how a line is read and
//! written, and the error codes. Everything is versioned by [`PROTOCOL_VERSION`]; a client says
//! which one it speaks with [`Call::Hello`] and the daemon refuses one it does not understand.
//!
//! - A client sends a [`Request`] (`id` + a [`Call`]) and gets a [`Response`] with the same `id`:
//!   a [`Reply`] or an [`RpcError`].
//! - After [`Call::Subscribe`] the daemon also pushes [`Notification`]s (`"method": "event"`,
//!   no `id`) carrying an [`Event`] on that connection.
//! - Nothing here knows about Windows: paths travel as text.
//!
//! Adding a field to a reply or an event is compatible (unknown fields are ignored); anything
//! else raises [`PROTOCOL_VERSION`].

mod call;
mod client;
mod error;
mod event;
mod reply;

pub use call::{Call, Tool};
pub use client::{read_bounded_line, Client, ClientError};
pub use error::{ErrorCode, RpcError};
pub use event::{Event, Numbered};
pub use reply::{
    AdvisoryInfo, Check, CheckStatus, CredentialsInfo, DoneInfo, EnvLine, HelloInfo, InstalledInfo,
    LogChunk, RemovedInfo, Reply, ServiceInfo, SiteInfo, StatusInfo, UsedInfo, VersionInfo,
    VersionList,
};

use serde::{Deserialize, Serialize};

/// Crate version, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of this protocol. Raised for any change that is not an added field.
pub const PROTOCOL_VERSION: u32 = 3;

/// The `jsonrpc` member of every message.
pub const JSONRPC: &str = "2.0";

/// Longest line either side accepts, in bytes. A longer one is a broken or hostile peer.
pub const MAX_LINE: usize = 1 << 20;

/// What a client sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always [`JSONRPC`].
    pub jsonrpc: String,
    /// Chosen by the client; the response carries it back.
    pub id: u64,
    /// The operation and its parameters (`method` and `params` in the JSON).
    #[serde(flatten)]
    pub call: Call,
}

impl Request {
    /// A request with this id.
    #[must_use]
    pub fn new(id: u64, call: Call) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id,
            call,
        }
    }
}

/// What the daemon answers: exactly one of `result` and `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always [`JSONRPC`].
    pub jsonrpc: String,
    /// The `id` of the request; `None` if the request could not even be read.
    pub id: Option<u64>,
    /// The outcome, when it worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Reply>,
    /// Why it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// A successful response.
    #[must_use]
    pub fn ok(id: u64, reply: Reply) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id: Some(id),
            result: Some(reply),
            error: None,
        }
    }

    /// A failed response.
    #[must_use]
    pub fn failed(id: Option<u64>, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// The reply, or the error as `Err`. A message with neither is itself an error.
    pub fn into_result(self) -> Result<Reply, RpcError> {
        match (self.result, self.error) {
            (Some(reply), None) => Ok(reply),
            (_, Some(error)) => Err(error),
            (None, None) => Err(RpcError::new(
                ErrorCode::InvalidRequest,
                "the response has neither a result nor an error",
            )),
        }
    }
}

/// A message the daemon pushes without being asked (after [`Call::Subscribe`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always [`JSONRPC`].
    pub jsonrpc: String,
    /// Always `"event"`.
    pub method: String,
    /// What happened, with its number.
    pub params: Numbered,
}

impl Notification {
    /// A notification carrying `event`, with no number (the progress of a call).
    #[must_use]
    pub fn new(event: Event) -> Self {
        Self::numbered(Numbered { event, seq: None })
    }

    /// A notification carrying an event of the daemon's stream, with its number.
    #[must_use]
    pub fn numbered(params: Numbered) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            method: "event".to_owned(),
            params,
        }
    }
}

/// Anything the daemon can write on a connection: a response to a request or an event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    /// Pushed event. Tried first: it needs `method` and `params`, which a response never has,
    /// while a response has no required field a notification would lack.
    Notification(Notification),
    /// Answer to a request.
    Response(Response),
}

/// Why a line could not be turned into a message.
#[derive(Debug, thiserror::Error)]
pub enum LineError {
    /// The line is longer than [`MAX_LINE`].
    #[error("a message is longer than {MAX_LINE} bytes")]
    TooLong,
    /// The line is not a message of this protocol.
    #[error("not a valid message: {0}")]
    Invalid(#[from] serde_json::Error),
}

/// A message as one line, `\n` included.
pub fn to_line<T: Serialize>(message: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    Ok(line)
}

/// Reads one message from a line (with or without its `\n`).
pub fn from_line<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, LineError> {
    if line.len() > MAX_LINE {
        return Err(LineError::TooLong);
    }
    Ok(serde_json::from_str(line.trim_end_matches(['\r', '\n']))?)
}

#[cfg(test)]
mod tests;
