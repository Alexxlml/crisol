//! A way to make calls: the daemon, or this process when there is none.

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use crisol_core::daemon::Daemon;
use crisol_core::i18n::rpc_error;
use crisol_core::msg;
use crisol_protocol::{Call, ClientError, ErrorCode, Event, Numbered, Reply};

/// Opens a connection to the daemon: `NotFound` when there is none. Every call opens a new
/// connection, so a live view and the calls never share one (the protocol serves one request
/// at a time per connection). It may run on any thread; the link it gives stays on the thread
/// that opened it.
pub type Connector = Arc<dyn Fn() -> std::io::Result<Box<dyn Link>> + Send + Sync>;

/// A way to make calls: the daemon, or this process.
pub trait Link {
    /// Makes `call`. `on_event` receives the progress of long operations.
    fn call(&mut self, call: &Call, on_event: &mut dyn FnMut(Event)) -> Result<Reply, ClientError>;

    /// Like [`call`](Self::call), with the number of each event: what a subscriber needs to tell
    /// which ones a `status` answer already includes.
    fn call_numbered(
        &mut self,
        call: &Call,
        on_event: &mut dyn FnMut(Numbered),
    ) -> Result<Reply, ClientError>;

    /// Waits for the next event of a subscription. `None` when the daemon closes the
    /// connection.
    fn next_numbered(&mut self) -> Result<Option<Numbered>, ClientError>;
}

/// The daemon, through a protocol client.
#[derive(Debug)]
pub struct Remote<R, W>(pub crisol_protocol::Client<R, W>);

impl<R: std::io::BufRead, W: Write> Link for Remote<R, W> {
    fn call(&mut self, call: &Call, on_event: &mut dyn FnMut(Event)) -> Result<Reply, ClientError> {
        self.0.call_with_events(call, on_event)
    }

    fn call_numbered(
        &mut self,
        call: &Call,
        on_event: &mut dyn FnMut(Numbered),
    ) -> Result<Reply, ClientError> {
        self.0.call_with_numbered(call, on_event)
    }

    fn next_numbered(&mut self) -> Result<Option<Numbered>, ClientError> {
        self.0.next_numbered()
    }
}

/// This process, for when no daemon is running. It answers the file operations and says the
/// services are stopped; anything that would start something is refused (that is the daemon's).
#[derive(Debug)]
pub struct Local(Daemon);

impl Local {
    /// A link that answers with `daemon`, which lives in this process.
    #[must_use]
    pub fn new(daemon: Daemon) -> Self {
        Self(daemon)
    }
}

impl Link for Local {
    fn call(&mut self, call: &Call, on_event: &mut dyn FnMut(Event)) -> Result<Reply, ClientError> {
        // With no daemon there is nothing running to stop.
        if matches!(call, Call::Stop { .. } | Call::Shutdown) {
            return Ok(Reply::Done(crisol_protocol::DoneInfo {
                message: "Nothing was running (the daemon is not running)".to_owned(),
                changed: false,
                warnings: Vec::new(),
                message_text: None,
                warnings_text: Vec::new(),
            }));
        }
        let local_ok =
            !call.needs_daemon() || matches!(call, Call::Hello { .. } | Call::Status { .. });
        if !local_ok {
            return Err(ClientError::Rpc(rpc_error(
                ErrorCode::NotAvailable,
                &msg!("client.daemon_not_running", command = "crisol start"),
            )));
        }
        Ok(self.0.handle(call, Instant::now(), on_event)?)
    }

    fn call_numbered(
        &mut self,
        call: &Call,
        on_event: &mut dyn FnMut(Numbered),
    ) -> Result<Reply, ClientError> {
        // Nothing numbers the events of this process: they are only the progress of a call.
        self.call(call, &mut |event| on_event(Numbered { event, seq: None }))
    }

    fn next_numbered(&mut self) -> Result<Option<Numbered>, ClientError> {
        // No daemon, no subscription: there is nothing to wait for.
        Ok(None)
    }
}
