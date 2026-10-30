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

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crisol_core::autostart::{Autostart, AutostartChange, AutostartState};
use crisol_core::config::Config;
use crisol_core::daemon::Daemon;
use crisol_core::engine::{Deps, Engine};
use crisol_core::paths::{CrisolPaths, PrivateDirs};
use crisol_core::programs::{program_error, require_trusted_program};
use crisol_core::setup::SystemSetup;
use crisol_core::shell::{self, ShellLaunch, Terminal};
use crisol_core::shimsetup::{self, Shell, ShimsError};
use crisol_core::trust::{
    prepare_trust, trust_ca, untrust_local, AddOutcome, TrustError, TrustStore,
};
use crisol_core::userpath::{PathChange, UserPath};
use crisol_protocol::{
    Call, CheckStatus, ClientError, ErrorCode, Event, Reply, RpcError, PROTOCOL_VERSION,
};

use crate::args::{
    Args, AutostartAction, PathAction, Plan, ShellArg, ShimsAction, SystemAction, TerminalArg,
};

/// Exit code: it worked.
pub const EXIT_OK: u8 = 0;
/// Exit code: the command failed, or `doctor` found a problem.
pub const EXIT_FAILED: u8 = 1;
/// Exit code: the arguments are wrong (the same as `clap` uses).
pub const EXIT_USAGE: u8 = 2;

/// Where a terminal's program is, given the program that started `crisol`.
pub type FindTerminal = Box<dyn Fn(Terminal, Option<&str>) -> Option<PathBuf>>;
/// Starts a terminal and gives its exit code.
pub type RunTerminal = Box<dyn Fn(&ShellLaunch) -> std::io::Result<i32>>;

/// Reads a variable of the environment of the process.
pub type EnvLookup = Box<dyn Fn(&str) -> Option<String>>;

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
    /// The user PATH (the registry; a double in the tests).
    pub user_path: Box<dyn UserPath>,
    /// Opens the certificates the user trusts (the current user's root store; a double in the
    /// tests).
    pub trust_store: Box<dyn Fn() -> std::io::Result<Box<dyn TrustStore>>>,
    /// The `crisol-shim` that `shims install` copies (the one next to this program).
    pub shim_exe: Option<PathBuf>,
    /// Whether a program with that name is already in the `PATH` (a command of the shims would
    /// hide it).
    pub reserved_name: Box<dyn Fn(&str) -> bool>,
    /// The environment of this process, to tell which shell this is.
    pub env: EnvLookup,
    /// The program that started this one (`pwsh.exe`), which tells the shell better.
    pub parent_program: Option<String>,
    /// All the variables of the environment of this process, for `crisol shell`.
    pub env_vars: Box<dyn Fn() -> Vec<(OsString, OsString)>>,
    /// Where a terminal's program is (given the program that started this one).
    pub find_terminal: FindTerminal,
    /// Starts a terminal in this window, waits and gives its exit code.
    pub run_terminal: RunTerminal,
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
            Plan::Trust => self.trust(),
            Plan::Untrust { stale } => self.untrust(stale),
            Plan::Shims(action) => self.shims(action),
            Plan::Path(action) => self.path(action),
            Plan::Env(shell) => self.env(shell),
            Plan::Shell { shell, clean } => self.shell(shell, clean),
        }
    }

    /// Prints a text, or `json` when `--json` was given.
    fn say(&mut self, text: &str, json: &serde_json::Value) {
        if self.json {
            let _ = writeln!(self.out, "{json}");
        } else {
            let _ = writeln!(self.out, "{text}");
        }
    }

    fn shims(&mut self, action: ShimsAction) -> u8 {
        match action {
            ShimsAction::Install => self.shims_install(),
            ShimsAction::Sync => self.shims_sync(),
            ShimsAction::Remove => self.shims_remove(),
            ShimsAction::Status => self.shims_status(),
        }
    }

    /// The `crisol-shim` next to this program, or the failure that says it is not there.
    fn shim_exe(&mut self) -> Result<PathBuf, u8> {
        match self.context.shim_exe.clone() {
            Some(exe) => Ok(exe),
            None => Err(self.fail(&RpcError::new(
                ErrorCode::NotAvailable,
                "crisol-shim.exe was not found next to this program",
            ))),
        }
    }

    fn shims_error(&mut self, error: &ShimsError) -> u8 {
        match error {
            ShimsError::Program(e) => self.fail(&program_error(e)),
            ShimsError::Io(e) => self.fail(&RpcError::new(ErrorCode::Io, e.to_string())),
        }
    }

    fn shims_install(&mut self) -> u8 {
        let exe = match self.shim_exe() {
            Ok(exe) => exe,
            Err(code) => return code,
        };
        let folder = self.context.paths.shims_dir().display().to_string();
        let done = match shimsetup::install_shims(
            &self.context.paths,
            &exe,
            self.context.dirs.as_ref(),
            self.context.reserved_name.as_ref(),
        ) {
            Ok(done) => done,
            Err(error) => return self.shims_error(&error),
        };
        let mut text = format!(
            "{} shim(s) written and {} already up to date in {folder}",
            done.written.len(),
            done.unchanged.len()
        );
        for (name, why) in &done.failed {
            text.push_str(&["\n  could not write ", name, ": ", why].concat());
        }
        text.push_str(&globals_text(
            &done.globals_added,
            &done.globals_removed,
            &done.globals_skipped,
        ));
        text.push_str(
            "\nThey run in a terminal once the folder is in your PATH: `crisol path add`",
        );
        let failed: Vec<_> = done
            .failed
            .iter()
            .map(|(n, w)| serde_json::json!({"shim": n, "why": w}))
            .collect();
        let globals_skipped: Vec<_> = done
            .globals_skipped
            .iter()
            .map(|(n, w)| serde_json::json!({"command": n, "why": w}))
            .collect();
        let json = serde_json::json!({
            "written": done.written,
            "unchanged": done.unchanged,
            "failed": failed,
            "globals_added": done.globals_added,
            "globals_removed": done.globals_removed,
            "globals_skipped": globals_skipped,
        });
        self.say(&text, &json);
        if done.failed.is_empty() {
            EXIT_OK
        } else {
            EXIT_FAILED
        }
    }

    fn shims_sync(&mut self) -> u8 {
        let exe = match self.shim_exe() {
            Ok(exe) => exe,
            Err(code) => return code,
        };
        let done = match shimsetup::sync_globals(
            &self.context.paths,
            &exe,
            self.context.dirs.as_ref(),
            self.context.reserved_name.as_ref(),
        ) {
            Ok(done) => done,
            Err(error) => return self.shims_error(&error),
        };
        let mut text = String::from("The commands of the global programs are up to date");
        text.push_str(&globals_text(&done.added, &done.removed, &done.skipped));
        let skipped: Vec<_> = done
            .skipped
            .iter()
            .map(|(n, w)| serde_json::json!({"command": n, "why": w}))
            .collect();
        let json = serde_json::json!({
            "added": done.added, "removed": done.removed, "skipped": skipped,
        });
        self.say(&text, &json);
        EXIT_OK
    }

    fn shims_remove(&mut self) -> u8 {
        let folder = self.context.paths.shims_dir().display().to_string();
        match shimsetup::remove_shims_files(&self.context.paths) {
            Ok(removed) => {
                let text = if removed.is_empty() {
                    format!("There were no shims in {folder}")
                } else {
                    format!("{} shim(s) removed from {folder}", removed.len())
                };
                self.say(&text, &serde_json::json!({ "removed": removed }));
                EXIT_OK
            }
            Err(e) => self.fail(&RpcError::new(ErrorCode::Io, e.to_string())),
        }
    }

    fn shims_status(&mut self) -> u8 {
        let paths = &self.context.paths;
        let folder = paths.shims_dir().display().to_string();
        let present = shimsetup::shims_present(paths);
        let missing: Vec<&str> = present
            .iter()
            .filter(|(_, p)| !p)
            .map(|(c, _)| *c)
            .collect();
        let mut text = if missing.is_empty() {
            format!("All the shims are in {folder}")
        } else if missing.len() == present.len() {
            format!("There are no shims in {folder}; `crisol shims install` makes them")
        } else {
            format!(
                "Missing in {folder}: {}; `crisol shims install` makes them",
                missing.join(", ")
            )
        };
        let globals = shimsetup::global_shims(paths);
        if !globals.is_empty() {
            text.push_str("\nCommands of global programs: ");
            text.push_str(&globals.join(", "));
        }
        let list: Vec<_> = present
            .iter()
            .map(|(c, p)| serde_json::json!({"command": c, "present": p}))
            .collect();
        let json = serde_json::json!({"folder": folder, "shims": list, "globals": globals});
        self.say(&text, &json);
        EXIT_OK
    }

    fn path(&mut self, action: PathAction) -> u8 {
        let paths = &self.context.paths;
        let store = self.context.user_path.as_ref();
        let io_error = |e: std::io::Error| RpcError::new(ErrorCode::Io, e.to_string());
        let folder = paths.shims_dir().display().to_string();
        let word = match action {
            PathAction::Add => match shimsetup::path_add(store, paths) {
                Ok(PathChange::Added) => "added",
                Ok(_) => "already",
                Err(e) => return self.fail(&io_error(e)),
            },
            PathAction::Remove => match shimsetup::path_remove(store, paths) {
                Ok(PathChange::Removed) => "removed",
                Ok(_) => "absent",
                Err(e) => return self.fail(&io_error(e)),
            },
            PathAction::Status => "status",
        };
        let status = match shimsetup::path_status(store, paths, &|p| p.is_file()) {
            Ok(status) => status,
            Err(e) => return self.fail(&io_error(e)),
        };
        let mut text = match word {
            "added" => format!(
                "{folder} is now first in your user PATH. Open a new terminal to use it"
            ),
            "already" => format!("{folder} is already in your user PATH"),
            "removed" => format!(
                "{folder} is no longer in your user PATH. Terminals that are open keep the old PATH until they are closed"
            ),
            "absent" => format!("{folder} was not in your user PATH"),
            _ if status.in_path => format!("{folder} is in your user PATH"),
            _ => format!("{folder} is not in your user PATH; `crisol path add` puts it there"),
        };
        // What would win over the shims matters whenever they are (or are about to be) in use.
        if word != "removed" && word != "absent" {
            for line in shimsetup::describe_shadows(&status.shadows) {
                text.push_str("\n  ");
                text.push_str(&line);
            }
        }
        let shadowed: Vec<_> = status
            .shadows
            .iter()
            .map(|s| {
                serde_json::json!({
                    "command": s.program,
                    "found": s.found.display().to_string(),
                    "machine_path": s.in_system_path,
                })
            })
            .collect();
        let json = serde_json::json!({
            "path": word,
            "in_path": status.in_path,
            "shadowed_by": shadowed,
        });
        self.say(&text, &json);
        EXIT_OK
    }

    fn env(&mut self, shell: Option<ShellArg>) -> u8 {
        let shell = match shell {
            Some(ShellArg::Powershell) => Shell::PowerShell,
            Some(ShellArg::Cmd) => Shell::Cmd,
            Some(ShellArg::Bash) => Shell::Bash,
            None => shimsetup::detect_shell(
                self.context.parent_program.as_deref(),
                self.context.env.as_ref(),
            ),
        };
        // The script itself, nothing else, so that it can be piped into the shell.
        let script = shimsetup::env_script(&self.context.paths.shims_dir(), shell);
        if self.json {
            let _ = writeln!(self.out, "{}", serde_json::json!({ "script": script }));
        } else {
            let _ = write!(self.out, "{script}");
        }
        EXIT_OK
    }

    fn shell(&mut self, asked: Option<TerminalArg>, clean: bool) -> u8 {
        let asked = asked.map(|arg| match arg {
            TerminalArg::Pwsh => Terminal::Pwsh,
            TerminalArg::Powershell => Terminal::WindowsPowerShell,
            TerminalArg::Cmd => Terminal::Cmd,
            TerminalArg::Bash => Terminal::Bash,
        });
        let parent = self.context.parent_program.as_deref();
        let find = |t: Terminal| (self.context.find_terminal)(t, parent);
        let (terminal, program, note) = match shell::choose_terminal(asked, parent, &find) {
            Ok(chosen) => chosen,
            Err(error) => return self.shell_error(&error),
        };
        let config = match Config::load_from(&self.context.paths) {
            Ok(loaded) => loaded.config,
            Err(error) => {
                return self.fail(&RpcError::new(ErrorCode::InvalidParams, error.to_string()))
            }
        };
        let front = shell::front_folders(&self.context.paths, &config);
        let env = (self.context.env_vars)();
        let launch = match shell::plan_shell(&shell::ShellInput {
            paths: &self.context.paths,
            terminal,
            program: &program,
            clean,
            front: &front,
            env: &env,
        }) {
            Ok(launch) => launch,
            Err(error) => return self.shell_error(&error),
        };
        if let Some(note) = note {
            let _ = writeln!(self.err, "crisol: {note}");
        }
        match (self.context.run_terminal)(&launch) {
            Ok(code) => u8::try_from(code).unwrap_or(EXIT_FAILED),
            Err(error) => self.fail(&RpcError::new(
                ErrorCode::Io,
                format!("cannot start {}: {error}", program.display()),
            )),
        }
    }

    fn shell_error(&mut self, error: &shell::ShellError) -> u8 {
        let code = match error {
            shell::ShellError::AlreadyInside => ErrorCode::Conflict,
            shell::ShellError::NoShims
            | shell::ShellError::NotFound(_)
            | shell::ShellError::NoTerminal => ErrorCode::NotAvailable,
            shell::ShellError::Unusable(_) => ErrorCode::InvalidParams,
            shell::ShellError::Io(_) => ErrorCode::Io,
        };
        self.fail(&RpcError::new(code, error.to_string()))
    }

    fn trust_error(&mut self, error: &TrustError) -> u8 {
        let rpc = match error {
            TrustError::Home(home) => crisol_core::engine::home_error(home),
            TrustError::Ca(ca) => RpcError::new(ErrorCode::InvalidParams, ca.to_string()),
            TrustError::Store(io) => RpcError::new(ErrorCode::Io, format!("trust store: {io}")),
        };
        self.fail(&rpc)
    }

    fn open_trust_store(&mut self) -> Result<Box<dyn TrustStore>, u8> {
        (self.context.trust_store)().map_err(|e| {
            self.fail(&RpcError::new(
                ErrorCode::Io,
                format!("cannot open the certificates you trust: {e}"),
            ))
        })
    }

    fn trust(&mut self) -> u8 {
        let store = match self.open_trust_store() {
            Ok(store) => store,
            Err(code) => return code,
        };
        let paths = &self.context.paths;
        let dirs = self.context.dirs.as_ref();
        let tld = crisol_core::setup::suffix_of(paths);
        let plan = match prepare_trust(store.as_ref(), paths, dirs, &tld, SystemTime::now()) {
            Ok(plan) => plan,
            Err(error) => return self.trust_error(&error),
        };
        let info = plan.ca.info().clone();
        // What the CA is, before Windows asks about it.
        if !self.json {
            let mut lines = vec![
                format!(
                    "{}{}",
                    info.common_name,
                    if plan.created { " (made now)" } else { "" }
                ),
                format!(
                    "  vouches only for: {} and their subdomains, and {}",
                    info.permitted_dns.join(", "),
                    info.permitted_ips.join(", ")
                ),
                format!("  valid until {}", info.valid_until()),
                format!("  SHA-256 {}", info.fingerprint),
            ];
            if let Some(thumbprint) = &plan.thumbprint {
                lines.push(format!("  SHA-1   {thumbprint}"));
            }
            if !plan.already {
                lines.push(
                    "Windows will ask you to confirm: check that the thumbprint (sha1) it shows is the SHA-1 above"
                        .to_owned(),
                );
            }
            for line in lines {
                let _ = writeln!(self.out, "{line}");
            }
            let _ = self.out.flush();
        }
        let outcome = match trust_ca(store.as_ref(), &plan.ca, paths, dirs) {
            Ok(outcome) => outcome,
            Err(error) => return self.trust_error(&error),
        };
        let (word, mut lines, code) = match outcome {
            AddOutcome::Added => (
                "added",
                vec![
                    "Windows trusts it now; a browser that was open may need a restart".to_owned(),
                ],
                EXIT_OK,
            ),
            AddOutcome::AlreadyThere => (
                "already",
                vec!["Windows already trusts it; nothing was changed".to_owned()],
                EXIT_OK,
            ),
            AddOutcome::Declined => (
                "declined",
                vec!["Windows was told no; nothing was changed".to_owned()],
                EXIT_FAILED,
            ),
        };
        if !plan.stale.is_empty() {
            lines.push(format!(
                "Older Crisol CAs are still trusted: {}; `crisol untrust --stale` takes them out",
                plan.stale.join(", ")
            ));
        }
        let json = serde_json::json!({
            "trust": word,
            "ca": info.common_name,
            "created": plan.created,
            "sha256": info.fingerprint,
            "thumbprint": plan.thumbprint,
            "valid_until": info.valid_until(),
            "stale": plan.stale,
        });
        self.say(&lines.join("\n"), &json);
        code
    }

    fn untrust(&mut self, stale: bool) -> u8 {
        let store = match self.open_trust_store() {
            Ok(store) => store,
            Err(code) => return code,
        };
        if !self.json {
            let _ = writeln!(
                self.err,
                "crisol: Windows may ask you to confirm each removal"
            );
        }
        let paths = &self.context.paths;
        let report = match untrust_local(
            store.as_ref(),
            paths,
            self.context.dirs.as_ref(),
            stale,
            SystemTime::now(),
        ) {
            Ok(report) => report,
            Err(error) => return self.trust_error(&error),
        };
        let tld = crisol_core::setup::suffix_of(paths);
        let mut lines = vec![match (report.removed.is_empty(), stale, &report.current) {
            (true, true, _) => "No older Crisol CA is trusted; nothing was changed".to_owned(),
            (true, false, None) => format!(
                "There is no local CA in {}; nothing was changed",
                paths.certs_dir().display()
            ),
            (true, false, Some(name)) => {
                format!("Windows did not trust {name}; nothing was changed")
            }
            (false, true, _) => format!("Windows no longer trusts: {}", report.removed.join(", ")),
            (false, false, _) => format!(
                "Windows no longer trusts {}: browsers warn about the secured sites under .{tld} until `crisol trust`",
                report.removed.join(", ")
            ),
        }];
        if !stale && report.stale_left > 0 {
            lines.push(format!(
                "{} older Crisol CA(s) are still trusted; `crisol untrust --stale` takes them out",
                report.stale_left
            ));
        }
        let json = serde_json::json!({
            "removed": report.removed,
            "current": report.current,
            "stale_left": report.stale_left,
        });
        self.say(&lines.join("\n"), &json);
        EXIT_OK
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
                // A program that others could replace is told like in `autostart on`: the
                // sentence, and the command that closes the folder apart.
                if let Some(program) = error
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<crisol_core::programs::ProgramError>())
                {
                    return self.fail(&program_error(program));
                }
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
            None => self.local(call)?,
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

    fn local(&self, call: &Call) -> Result<Box<dyn Link>, RpcError> {
        let deps =
            (self.context.deps)().map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?;
        let engine = Engine::new(self.context.paths.clone(), deps);
        // The first command of a new user may be this one: the home must be born private. The
        // commands that only look do not make it.
        if call.writes() {
            engine.prepare_home()?;
        }
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

/// The lines about the commands of global programs (`tsc`), for what `shims install|sync` did.
fn globals_text(added: &[String], removed: &[String], skipped: &[(String, String)]) -> String {
    let mut lines = Vec::new();
    if !added.is_empty() {
        lines.push(["commands for global programs added: ", &added.join(", ")].concat());
    }
    if !removed.is_empty() {
        lines.push(
            [
                "commands for programs that are gone removed: ",
                &removed.join(", "),
            ]
            .concat(),
        );
    }
    for (name, why) in skipped {
        lines.push(["no command `", name, "`: ", why].concat());
    }
    lines.iter().flat_map(|l| ["\n  ", l.as_str()]).collect()
}

fn client_error(error: ClientError) -> RpcError {
    match error {
        ClientError::Rpc(error) => error,
        other => RpcError::new(ErrorCode::Io, other.to_string()),
    }
}
