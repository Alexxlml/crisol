//! The live view of the daemon (ADR-0026 §5): what a window or a tray icon draws.
//!
//! [`LiveView`] opens its own connection, follows the order of `docs/protocolo.md` (`hello`,
//! `subscribe` **before** `status`, then the events) with the rule of `seq` of the ADR-0025, and
//! keeps a [`LiveState`] up to date for as long as it lives. When the daemon goes away
//! (`shutting_down` or a closed pipe) the state says so and the view keeps looking for it, and
//! when it comes back the picture is built again. It never starts the daemon: that is a call,
//! made through another connection, because a connection serves one request at a time.
//!
//! The rules that turn events into state live in [`follow`], which works on any [`Link`], so
//! the tests run it against scripted daemons with no thread and no clock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crisol_core::i18n::Language;
use crisol_core::msg;
use crisol_core::trouble::WebTrouble;
use crisol_protocol::{
    Call, Check, ClientError, Event, HelloInfo, Numbered, Reply, ServiceInfo, StatusInfo,
};
use serde::Serialize;

use crate::connect::{client_error, daemon_version_check, hello};
use crate::context::Context;
use crate::link::{Connector, Link};
use crate::text::{localize_check, localize_error};

/// Whether there is a daemon on the other side.
///
/// It is serialized as `{"kind":"connected"}` and `{"kind":"refused","message":"..."}`: a page
/// reads it by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum Connection {
    /// No daemon is answering (not started, going away, or gone): the view keeps looking.
    #[default]
    Disconnected,
    /// The daemon said hello and the picture is up to date.
    Connected,
    /// Something answers but cannot be used: another protocol, or a pipe that is not this
    /// user's. The sentence is meant to be shown as it is.
    Refused(String),
}

/// What the view knows about the daemon. It can be copied and compared, so a screen redraws
/// when it is not equal to the one it drew.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct LiveState {
    /// Whether there is a daemon.
    pub connection: Connection,
    /// What the daemon said about itself in its `hello`.
    pub daemon: Option<HelloInfo>,
    /// The warning for a daemon that is not of this program's version, with the way out.
    pub version_warning: Option<Check>,
    /// The services, in the order the daemon gives them (new ones at the end).
    pub services: Vec<ServiceInfo>,
    /// What the daemon has to say about them.
    pub warnings: Vec<String>,
    /// A new web configuration is being tested: between `reload_started` and its closing.
    pub reloading: bool,
    /// Why the last web configuration was not applied; it stays until one is, or Nginx starts
    /// again ([`WebTrouble`]). The `status` picture carries it too.
    pub reload_error: Option<String>,
    /// Crisol will not change the running web stack until a folder is closed; shown as it is.
    /// It stays until a configuration is applied, or Nginx starts again ([`WebTrouble`]). The
    /// `status` picture carries it too, so a view that connects late sees it.
    pub blocked: Option<String>,
}

impl LiveState {
    /// Takes `event` into account. `Log` and `Progress` are not state, and `ShuttingDown` is
    /// the end of the connection: [`follow`] handles it.
    fn apply(&mut self, event: &Event) {
        // What still holds against the web stack follows the daemon's own rule, so the events
        // and the next picture agree.
        let mut trouble = WebTrouble {
            blocked: self.blocked.take(),
            reload_error: self.reload_error.take(),
        };
        trouble.note(event);
        self.blocked = trouble.blocked;
        self.reload_error = trouble.reload_error;
        match event {
            Event::ServiceState {
                service,
                state,
                detail,
            } => match self.services.iter_mut().find(|s| s.name == *service) {
                Some(known) => {
                    known.state.clone_from(state);
                    known.detail.clone_from(detail);
                }
                None => self.services.push(ServiceInfo {
                    name: service.clone(),
                    state: state.clone(),
                    detail: detail.clone(),
                }),
            },
            Event::ReloadStarted => self.reloading = true,
            Event::Reloaded | Event::ReloadFailed { .. } => self.reloading = false,
            // The whole list as it is now, not something to add.
            Event::Warnings { warnings } => self.warnings.clone_from(warnings),
            // An event of a newer daemon: its number was counted, there is nothing to draw.
            Event::Blocked { .. }
            | Event::Log { .. }
            | Event::Progress { .. }
            | Event::ShuttingDown
            | Event::Unknown => {}
        }
    }

    /// The daemon's own picture replaces what was known, including what still holds against
    /// the web stack. Whether a reload is under way is not in it, so after a gap in the events
    /// it is taken as not: a spinner that never ends is worse than one that was missed.
    fn replace(&mut self, status: StatusInfo) {
        self.services = status.services;
        self.warnings = status.warnings;
        self.blocked = status.blocked;
        self.reload_error = status.reload_error;
        self.reloading = false;
    }
}

/// How one connection ended.
#[derive(Debug)]
enum Ended {
    /// The daemon said `shutting_down`.
    DaemonLeft,
    /// The daemon closed the connection, or the stream failed.
    Lost,
    /// The daemon did not accept the `hello`.
    Refused(String),
    /// The view was dropped: nobody reads the state any more.
    Stopped,
}

/// Follows the daemon on `link` until the connection ends, keeping `state` up to date and
/// calling `publish` with it whenever it changed. `hello` is the call that opens the
/// conversation; `mine` is the version of this program.
///
/// 1. `hello`, to know the daemon speaks this protocol;
/// 2. `subscribe`, **before** asking for the state, so that no change falls between the two;
/// 3. `status`, whose answer replaces the picture; the events that arrive during the call are
///    kept, and the ones numbered after the answer's `seq` are applied on top;
/// 4. every event after that updates the picture. One numbered up to the last `seq` is already
///    in it; a skipped number means events were lost, and the state is asked for again.
///
/// `stop` is looked at after each event: once it is set, nothing more is published.
fn follow(
    link: &mut dyn Link,
    hello: &Call,
    mine: &str,
    language: Language,
    stop: &AtomicBool,
    state: &mut LiveState,
    publish: &mut dyn FnMut(&LiveState),
) -> Ended {
    let info = match link.call(hello, &mut |_| {}) {
        Ok(Reply::Hello(info)) => info,
        Ok(other) => {
            return Ended::Refused(
                msg!(
                    "client.unexpected_answer",
                    call = "hello",
                    answer = format!("{other:?}")
                )
                .render(language),
            )
        }
        Err(ClientError::Rpc(mut error)) => {
            localize_error(&mut error, language);
            return Ended::Refused(error.message);
        }
        Err(_) => return Ended::Lost,
    };
    state.version_warning = daemon_version_check(&info.daemon, mine).map(|mut check| {
        localize_check(&mut check, language);
        check
    });
    state.daemon = Some(info);
    if link.call(&Call::Subscribe, &mut |_| {}).is_err() {
        return Ended::Lost;
    }
    let Ok(mut last) = refresh(link, state) else {
        return Ended::Lost;
    };
    state.connection = Connection::Connected;
    publish(state);
    loop {
        let Ok(Some(numbered)) = link.next_numbered() else {
            return Ended::Lost;
        };
        if stop.load(Ordering::Relaxed) {
            return Ended::Stopped;
        }
        // Only the daemon's stream is numbered; the progress of a call is not.
        if let (Some(seq), Some(previous)) = (numbered.seq, last) {
            if seq <= previous {
                // Already in the last picture.
                continue;
            }
            if seq > previous + 1 {
                // Events were lost: the picture is asked for again, and it may already include
                // this one.
                match refresh(link, state) {
                    Ok(picture) => last = picture,
                    Err(_) => return Ended::Lost,
                }
                publish(state);
                if last.is_some_and(|picture| seq <= picture) {
                    continue;
                }
            }
            last = Some(seq);
        }
        if numbered.event == Event::ShuttingDown {
            return Ended::DaemonLeft;
        }
        state.apply(&numbered.event);
        publish(state);
    }
}

/// Asks for `status` and puts it in `state`, with the events that arrived meanwhile and are
/// newer than it on top. Returns the number of the last event the state includes, if the
/// daemon numbers them.
fn refresh(link: &mut dyn Link, state: &mut LiveState) -> Result<Option<u64>, ClientError> {
    let mut early = Vec::new();
    let reply = link.call_numbered(&Call::Status { service: None }, &mut |numbered| {
        early.push(numbered);
    })?;
    let Reply::Status(status) = reply else {
        return Err(ClientError::Protocol(format!(
            "`status` answered {reply:?}"
        )));
    };
    let mut last = status.seq;
    state.replace(status);
    for Numbered { event, seq } in early {
        if let (Some(seq), Some(picture)) = (seq, last) {
            if seq > picture {
                state.apply(&event);
                last = Some(seq);
            }
        }
    }
    Ok(last)
}

/// What [`LiveView::start`] needs.
#[allow(missing_debug_implementations)] // holds a closure
pub struct LiveConfig {
    /// Opens a connection to the daemon (a new one every time).
    pub connect: Connector,
    /// The `hello` that opens each conversation.
    pub hello: Call,
    /// The version of this program, which the daemon's is compared with.
    pub version: String,
    /// The language the errors and warnings are said in.
    pub language: Language,
    /// How long to wait before looking for the daemon again.
    pub retry: Duration,
}

impl LiveConfig {
    /// The configuration for the program `context` describes, looking for the daemon every
    /// second.
    #[must_use]
    pub fn for_context(context: &Context) -> Self {
        Self {
            connect: Arc::clone(&context.connect),
            hello: hello(context),
            version: context.version.clone(),
            language: context.language,
            retry: Duration::from_secs(1),
        }
    }
}

/// The state, and the signal that it changed.
#[derive(Debug, Default)]
struct Cell {
    state: Mutex<LiveState>,
    changed: Condvar,
}

impl Cell {
    fn lock(&self) -> MutexGuard<'_, LiveState> {
        // A panic in `on_change` must not take the whole view down with it.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The live view: a thread that follows the daemon and a state to read.
///
/// Dropping it tells the thread to stop. The thread may be waiting for an event of the daemon
/// at that moment, so it ends at the next one (or with the process), without calling
/// `on_change` again; it is not waited for.
#[derive(Debug)]
pub struct LiveView {
    cell: Arc<Cell>,
    stop: Arc<AtomicBool>,
    _thread: JoinHandle<()>,
}

impl LiveView {
    /// Starts following the daemon. `on_change` is called, from the thread of the view, each
    /// time the state is not what it was; it must not block (a GUI wakes its own thread there).
    #[must_use]
    pub fn start(config: LiveConfig, on_change: impl Fn(&LiveState) + Send + 'static) -> Self {
        let cell = Arc::new(Cell::default());
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (cell, stop) = (Arc::clone(&cell), Arc::clone(&stop));
            std::thread::spawn(move || run(&config, &cell, &stop, &on_change))
        };
        Self {
            cell,
            stop,
            _thread: thread,
        }
    }

    /// A copy of the state as it is now.
    #[must_use]
    pub fn state(&self) -> LiveState {
        self.cell.lock().clone()
    }

    /// Waits until the state satisfies `ready` and gives it, or `None` after `timeout`.
    #[must_use]
    pub fn wait_for(
        &self,
        timeout: Duration,
        ready: impl Fn(&LiveState) -> bool,
    ) -> Option<LiveState> {
        let end = Instant::now() + timeout;
        let mut guard = self.cell.lock();
        loop {
            if ready(&guard) {
                return Some(guard.clone());
            }
            let left = end.checked_duration_since(Instant::now())?;
            guard = self
                .cell
                .changed
                .wait_timeout(guard, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

impl Drop for LiveView {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The thread of the view: connect, follow, and when the connection ends, look again.
fn run(config: &LiveConfig, cell: &Cell, stop: &AtomicBool, on_change: &dyn Fn(&LiveState)) {
    let publish = |state: &LiveState| {
        let mut current = cell.lock();
        if *current != *state {
            current.clone_from(state);
            cell.changed.notify_all();
            drop(current);
            on_change(state);
        }
    };
    while !stop.load(Ordering::Relaxed) {
        // What a past connection knew says nothing about this one.
        let mut state = LiveState::default();
        match (config.connect)() {
            Ok(mut link) => {
                let ended = follow(
                    link.as_mut(),
                    &config.hello,
                    &config.version,
                    config.language,
                    stop,
                    &mut state,
                    &mut |state| publish(state),
                );
                state = match ended {
                    Ended::Stopped => return,
                    // What answered is not a daemon to follow: only its words are kept.
                    Ended::Refused(message) => LiveState {
                        connection: Connection::Refused(message),
                        ..LiveState::default()
                    },
                    // `DaemonLeft` and `Lost` both end with nobody on the other side.
                    Ended::DaemonLeft | Ended::Lost => LiveState::default(),
                };
            }
            // No pipe: the daemon is not there (yet).
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            // Something is there that cannot be used (a pipe of another account, say).
            Err(error) => {
                let mut error = client_error(error.into());
                localize_error(&mut error, config.language);
                state.connection = Connection::Refused(error.message);
            }
        }
        publish(&state);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(config.retry);
    }
}

#[cfg(test)]
mod tests;
