//! The log viewer (F13.6): which logs there are to read, and how one is followed.
//!
//! The daemon reads a log through a bounded window (it never loads a whole file) and answers
//! with the lines and the byte offset to continue from (`next`). Following is asking again with
//! that offset, which is what `crisol logs -f` does. This decides what the window shows after
//! each answer: the lines to add, or the lines to start over with when the file is new (the
//! first read) or was rotated underneath the viewer. The page only draws it.

use crisol_protocol::{Call, ErrorCode, LogChunk, Reply, RpcError};
use serde::Serialize;

use crate::connect;
use crate::context::Context;
use crate::live::LiveState;

/// How many lines the first read asks for: enough to see what just happened.
pub const FIRST_LINES: usize = 200;

/// How many lines the page keeps. A service that writes a lot must not grow the window forever.
pub const KEEP_LINES: usize = 2000;

/// The name of the daemon's own log.
pub const DAEMON_LOG: &str = "crisold";

/// What the viewer lets a person choose from: every service the daemon knows, in its order, and
/// the daemon's own log last. A service that never wrote anything has no log yet; asking for it
/// says so ([`fetch_log`]).
#[must_use]
pub fn log_services(state: &LiveState) -> Vec<String> {
    let mut names: Vec<String> = state.services.iter().map(|s| s.name.clone()).collect();
    if !names.iter().any(|name| name == DAEMON_LOG) {
        names.push(DAEMON_LOG.to_owned());
    }
    names
}

/// The call for the next read of `service`: its end the first time, what came after `from`
/// afterwards.
#[must_use]
pub fn log_call(service: &str, from: Option<u64>) -> Call {
    Call::Logs {
        service: service.to_owned(),
        lines: FIRST_LINES,
        from,
    }
}

/// What the window does after a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogUpdate {
    /// The service.
    pub service: String,
    /// The lines the read brought, without newlines.
    pub lines: Vec<String>,
    /// Where the next read starts (`from`).
    pub next: u64,
    /// The file they come from.
    pub path: String,
    /// Throw away what the window shows and start with `lines`: it is the first read, or the
    /// file is shorter than where the last read ended, which means it was rotated.
    pub replace: bool,
    /// How many lines the window keeps; the oldest go first.
    pub keep: usize,
}

/// Turns the daemon's answer into what the window does. `from` is what the read asked for.
#[must_use]
pub fn log_update(service: &str, from: Option<u64>, chunk: LogChunk) -> LogUpdate {
    let rotated = from.is_some_and(|from| chunk.next < from);
    LogUpdate {
        service: service.to_owned(),
        replace: from.is_none() || rotated,
        lines: chunk.lines,
        next: chunk.next,
        path: chunk.path,
        keep: KEEP_LINES,
    }
}

/// Reads the log of `service`: its last lines when `from` is `None`, what came after `from`
/// otherwise. A service with no log yet is an error with the daemon's words.
pub fn fetch_log(
    context: &Context,
    service: &str,
    from: Option<u64>,
) -> Result<LogUpdate, RpcError> {
    match connect::call(context, &log_call(service, from), &mut |_| {})? {
        Reply::Logs(chunk) => Ok(log_update(service, from, chunk)),
        other => Err(RpcError::new(
            ErrorCode::Internal,
            format!("unexpected answer to `logs`: {other:?}"),
        )),
    }
}

#[cfg(test)]
mod tests;
