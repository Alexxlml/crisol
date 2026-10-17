//! The `crisol` command line (F8.3).
//!
//! It parses the arguments ([`args`]), turns them into protocol calls, sends them to the daemon
//! (or, for the operations that only change files and when no daemon is running, to the very
//! same core in this process) and shows the answer ([`render`]). There is nothing else in it.
//!
//! [`run`] takes everything it touches through [`Context`], so the tests run whole commands in
//! this process against a daemon in another thread, a temporary home and fakes behind it.

pub mod args;
pub mod render;

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crisol_core::autostart::{Autostart, AutostartChange, AutostartState};
use crisol_core::daemon::Daemon;
use crisol_core::engine::{Deps, Engine};
use crisol_core::paths::{CrisolPaths, PrivateDirs};
use crisol_core::programs::{program_error, require_trusted_program};
use crisol_core::setup::SystemSetup;
use crisol_protocol::{
    Call, CheckStatus, ClientError, ErrorCode, Event, Reply, RpcError, PROTOCOL_VERSION,
};

use crate::args::{Args, AutostartAction, Plan, SystemAction};

/// Exit code: it worked.
pub const EXIT_OK: u8 = 0;
/// Exit code: the command failed, or `doctor` found a problem.
pub const EXIT_FAILED: u8 = 1;
/// Exit code: the arguments are wrong (the same as `clap` uses).
pub const EXIT_USAGE: u8 = 2;

/// A way to make calls: the daemon, or this process.
pub trait Link {
    /// Makes `call`. `on_event` receives the progress of long operations.
    fn call(&mut self, call: &Call, on_event: &mut dyn FnMut(Event)) -> Result<Reply, ClientError>;
}

/// The daemon, through a protocol client.
#[derive(Debug)]
pub struct Remote<R, W>(pub crisol_protocol::Client<R, W>);

impl<R: std::io::BufRead, W: Write> Link for Remote<R, W> {
    fn call(&mut self, call: &Call, on_event: &mut dyn FnMut(Event)) -> Result<Reply, ClientError> {
        self.0.call_with_events(call, on_event)
    }
}

/// This process, for when no daemon is running. It answers the file operations and says the
/// services are stopped; anything that would start something is refused (that is the daemon's).
struct Local(Daemon);

impl Link for Local {
    fn call(&mut self, call: &Call, on_event: &mut dyn FnMut(Event)) -> Result<Reply, ClientError> {
        // With no daemon there is nothing running to stop.
        if matches!(call, Call::Stop { .. } | Call::Shutdown) {
            return Ok(Reply::Done(crisol_protocol::DoneInfo {
                message: "Nothing was running (the daemon is not running)".to_owned(),
                changed: false,
                warnings: Vec::new(),
            }));
        }
        let local_ok =
            !call.needs_daemon() || matches!(call, Call::Hello { .. } | Call::Status { .. });
        if !local_ok {
            return Err(ClientError::Rpc(RpcError::new(
                ErrorCode::NotAvailable,
                "the daemon is not running; `crisol start` starts it",
            )));
        }
        Ok(self.0.handle(call, Instant::now(), on_event)?)
    }
}

/// Everything the command line touches besides its own arguments.
pub struct Context {
    /// The Crisol home.
    pub paths: CrisolPaths,
    /// The folder the command ran in.
    pub cwd: PathBuf,
    /// Builds the dependencies of the engine (only needed when there is no daemon).
    pub deps: Box<dyn Fn() -> std::io::Result<Deps>>,
    /// Connects to the daemon: `NotFound` when there is none.
    pub connect: Box<dyn Fn() -> std::io::Result<Box<dyn Link>>>,
    /// Starts the daemon without waiting for it.
    pub spawn_daemon: Box<dyn Fn() -> std::io::Result<()>>,
    /// The one-time setup that needs an administrator (the helper and the DNS rule).
    pub setup: Box<dyn SystemSetup>,
    /// The sign-in entry that starts the daemon.
    pub autostart: Box<dyn Autostart>,
    /// The `crisold` that entry would start (the one next to this program).
    pub daemon_exe: Option<PathBuf>,
    /// Judges who can write to the programs before they are registered or launched.
    pub dirs: std::sync::Arc<dyn PrivateDirs>,
    /// How long to wait for a daemon that was just started.
    pub daemon_wait: Duration,
    /// Time between asking again for the log of `logs -f`.
    pub follow_every: Duration,
    /// Stops `logs -f` after this many rounds (`None`: until the process is killed).
    pub follow_rounds: Option<u32>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

/// Runs the command line `arguments` (the first is the program name). Output goes to `out`,
/// errors and progress to `err`. Returns the exit code.
pub fn run(
    arguments: &[String],
    context: &Context,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let args = match args::parse(arguments) {
        Ok(args) => args,
        Err(error) => {
            let code = error.exit_code();
            let _ = if error.use_stderr() {
                write!(err, "{error}")
            } else {
                write!(out, "{error}")
            };
            return if code == 0 { EXIT_OK } else { EXIT_USAGE };
        }
    };
    let json = args.json;
    let mut session = Session {
        context,
        json,
        out,
        err,
    };
    session.execute(args)
}

struct Session<'a> {
    context: &'a Context,
    json: bool,
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
}

impl Session<'_> {
    fn execute(&mut self, args: Args) -> u8 {
        let plan = args.command.plan(&self.context.cwd);
        match plan {
            Plan::Call(call) => self.one(&call),
            Plan::Follow { service, lines } => self.follow(&service, lines),
            Plan::Autostart(action) => self.autostart(action),
            Plan::System(action) => self.system(action),
        }
    }

    fn system(&mut self, action: SystemAction) -> u8 {
        let setup = self.context.setup.as_ref();
        let suffix = &crisol_core::setup::suffix_of(&self.context.paths);
        let outcome = match action {
            SystemAction::Install => setup.install(suffix),
            SystemAction::Uninstall => setup.uninstall(suffix),
        };
        match outcome {
            Ok(report) => {
                if self.json {
                    let steps = serde_json::to_string(&report.steps).unwrap_or_default();
                    let _ = writeln!(self.out, "{{\"steps\":{steps}}}");
                } else {
                    for step in &report.steps {
                        let _ = writeln!(self.out, "{step}");
                    }
                }
                EXIT_OK
            }
            Err(error) => {
                let code = if error.kind() == std::io::ErrorKind::PermissionDenied {
                    ErrorCode::NotAvailable
                } else {
                    ErrorCode::Io
                };
                self.fail(&RpcError::new(code, error.to_string()))
            }
        }
    }

    fn autostart(&mut self, action: AutostartAction) -> u8 {
        let Some(daemon) = self.context.daemon_exe.clone() else {
            return self.fail(&RpcError::new(
                ErrorCode::NotAvailable,
                "crisold.exe was not found next to this program",
            ));
        };
        let store = self.context.autostart.as_ref();
        let io_error = |e: std::io::Error| RpcError::new(ErrorCode::Io, e.to_string());
        let (word, message) = match action {
            AutostartAction::On => match crisol_core::autostart::enable(store, &daemon, self.context.dirs.as_ref()) {
                Ok(AutostartChange::Enabled) => ("on", "Crisol will start when you sign in".to_owned()),
                Ok(AutostartChange::Replaced) => ("on", "Crisol will start when you sign in (the entry of another crisold was replaced)".to_owned()),
                Ok(AutostartChange::Unchanged) => ("on", "Crisol already starts when you sign in".to_owned()),
                Err(crisol_core::autostart::AutostartError::Program(e)) => return self.fail(&program_error(&e)),
                Err(crisol_core::autostart::AutostartError::Io(e)) => return self.fail(&io_error(e)),
            },
            AutostartAction::Off => match crisol_core::autostart::disable(store) {
                Ok(true) => ("off", "Crisol will not start when you sign in".to_owned()),
                Ok(false) => ("off", "Crisol did not start when you signed in".to_owned()),
                Err(e) => return self.fail(&io_error(e)),
            },
            AutostartAction::Status => match crisol_core::autostart::state(store, &daemon) {
                Ok(AutostartState::On) => ("on", "Crisol starts when you sign in".to_owned()),
                Ok(AutostartState::Off) => ("off", "Crisol does not start when you sign in; `crisol autostart on` makes it".to_owned()),
                Ok(AutostartState::Other(other)) => ("other", format!("Crisol starts when you sign in, but with another crisold: {other}")),
                Err(e) => return self.fail(&io_error(e)),
            },
        };
        if self.json {
            let _ = writeln!(self.out, "{{\"autostart\":\"{word}\"}}");
        } else {
            let _ = writeln!(self.out, "{message}");
        }
        EXIT_OK
    }

    /// Connects to the daemon, starting it if the call is one that should.
    fn link(&mut self, call: &Call) -> Result<Box<dyn Link>, RpcError> {
        let mut link = match (self.context.connect)() {
            Ok(link) => Some(link),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(RpcError::new(ErrorCode::Io, error.to_string())),
        };
        if link.is_none() && matches!(call, Call::Start { .. } | Call::Restart { .. }) {
            link = Some(self.start_daemon()?);
        }
        let mut link = match link {
            Some(link) => link,
            None => self.local()?,
        };
        // The handshake: same protocol on both sides, or nothing is sent.
        let hello = Call::Hello {
            client: format!("crisol {}", env!("CARGO_PKG_VERSION")),
            protocol: PROTOCOL_VERSION,
        };
        match link.call(&hello, &mut |_| {}) {
            Ok(_) => Ok(link),
            Err(error) => Err(client_error(error)),
        }
    }

    fn local(&self) -> Result<Box<dyn Link>, RpcError> {
        let deps =
            (self.context.deps)().map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?;
        let engine = Engine::new(self.context.paths.clone(), deps);
        // The first command of a new user may be this one: the home must be born private.
        engine.prepare_home()?;
        Ok(Box::new(Local(Daemon::new(engine))))
    }

    fn start_daemon(&mut self) -> Result<Box<dyn Link>, RpcError> {
        // The daemon runs with the user's rights: not if someone else could have replaced it.
        if let Some(exe) = &self.context.daemon_exe {
            require_trusted_program(exe, self.context.dirs.as_ref())
                .map_err(|e| program_error(&e))?;
        }
        (self.context.spawn_daemon)().map_err(|e| {
            RpcError::new(
                ErrorCode::Io,
                format!("could not start the daemon (crisold): {e}"),
            )
        })?;
        let _ = writeln!(self.err, "Starting the daemon...");
        let end = Instant::now() + self.context.daemon_wait;
        loop {
            match (self.context.connect)() {
                Ok(link) => return Ok(link),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if Instant::now() >= end {
                        return Err(RpcError::new(
                            ErrorCode::NotAvailable,
                            format!(
                                "the daemon did not open its pipe in {} seconds; its log is {}",
                                self.context.daemon_wait.as_secs(),
                                self.context.paths.runtime_log("crisold").map_or_else(
                                    |_| "logs/crisold/runtime.log".to_owned(),
                                    |p| p.display().to_string()
                                )
                            ),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(RpcError::new(ErrorCode::Io, error.to_string())),
            }
        }
    }

    fn one(&mut self, call: &Call) -> u8 {
        let mut link = match self.link(call) {
            Ok(link) => link,
            Err(error) => return self.fail(&error),
        };
        let mut last = None;
        let json = self.json;
        let err = &mut *self.err;
        let outcome = link.call(call, &mut |event| {
            if !json {
                if let Some(line) = render::progress(&event, &mut last) {
                    let _ = writeln!(err, "{line}");
                }
            }
        });
        match outcome {
            Ok(reply) => self.show(&reply),
            Err(error) => self.fail(&client_error(error)),
        }
    }

    fn show(&mut self, reply: &Reply) -> u8 {
        if self.json {
            let _ = writeln!(
                self.out,
                "{}",
                serde_json::to_string(reply).unwrap_or_default()
            );
        } else {
            let _ = write!(self.out, "{}", render::reply(reply));
        }
        match reply {
            Reply::Doctor { checks } if checks.iter().any(|c| c.status == CheckStatus::Problem) => {
                EXIT_FAILED
            }
            _ => EXIT_OK,
        }
    }

    fn fail(&mut self, error: &RpcError) -> u8 {
        if self.json {
            let _ = writeln!(
                self.out,
                "{}",
                serde_json::to_string(error).unwrap_or_default()
            );
        } else {
            let _ = write!(self.err, "{}", render::error(error));
        }
        EXIT_FAILED
    }

    fn follow(&mut self, service: &str, lines: usize) -> u8 {
        let first = Call::Logs {
            service: service.to_owned(),
            lines,
            from: None,
        };
        let mut link = match self.link(&first) {
            Ok(link) => link,
            Err(error) => return self.fail(&error),
        };
        let mut next = match link.call(&first, &mut |_| {}) {
            Ok(Reply::Logs(chunk)) => {
                self.show(&Reply::Logs(chunk.clone()));
                chunk.next
            }
            Ok(_) => return EXIT_FAILED,
            Err(error) => return self.fail(&client_error(error)),
        };
        let mut rounds = 0_u32;
        loop {
            if self.context.follow_rounds.is_some_and(|max| rounds >= max) {
                return EXIT_OK;
            }
            rounds += 1;
            std::thread::sleep(self.context.follow_every);
            let ask = Call::Logs {
                service: service.to_owned(),
                lines,
                from: Some(next),
            };
            match link.call(&ask, &mut |_| {}) {
                Ok(Reply::Logs(chunk)) => {
                    next = chunk.next;
                    if !chunk.lines.is_empty() {
                        self.show(&Reply::Logs(chunk));
                        let _ = self.out.flush();
                    }
                }
                Ok(_) => return EXIT_FAILED,
                Err(error) => return self.fail(&client_error(error)),
            }
        }
    }
}

fn client_error(error: ClientError) -> RpcError {
    match error {
        ClientError::Rpc(error) => error,
        other => RpcError::new(ErrorCode::Io, other.to_string()),
    }
}
