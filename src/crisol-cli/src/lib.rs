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
use std::time::{Duration, SystemTime};

use crisol_client::connect::{self, client_error};
use crisol_client::Link;
use crisol_client::{localize_error, localize_reply};
use crisol_core::autostart::{AutostartChange, AutostartState};
use crisol_core::config::Config;
use crisol_core::onboarding;
use crisol_core::programs::program_error;
use crisol_core::shell::{self, Terminal};
use crisol_core::shimsetup::{self, Shell, ShimsError};
use crisol_core::teardown;
use crisol_core::trust::{
    prepare_trust, trust_ca, untrust_local, AddOutcome, TrustError, TrustStore,
};
use crisol_core::userpath::PathChange;
use crisol_protocol::{Call, CheckStatus, ErrorCode, Reply, RpcError};

use crate::args::{
    Args, AutostartAction, PathAction, Plan, ShellArg, ShimsAction, SystemAction, TerminalArg,
};

/// Exit code: it worked.
pub const EXIT_OK: u8 = 0;
/// Exit code: the command failed, or `doctor` found a problem.
pub const EXIT_FAILED: u8 = 1;
/// Exit code: the arguments are wrong (the same as `clap` uses).
pub const EXIT_USAGE: u8 = 2;

/// Reads a variable of the environment of the process.
pub type EnvLookup = Box<dyn Fn(&str) -> Option<String>>;

/// Shows the user the plan of `uninstall --all` and asks whether to go on; says whether they
/// agreed. The terminal asks with [`render::teardown_question`]; a GUI would show the plan.
pub type Confirm = Box<dyn Fn(&teardown::Plan) -> bool>;

/// Shows the user the plan of `setup` and asks whether to go on.
pub type ConfirmSetup = Box<dyn Fn(&onboarding::Plan) -> bool>;

/// Asks about one step of `setup` that is asked alone (the CA, the helper, the sign-in entry).
pub type ConfirmStep = Box<dyn Fn(&onboarding::Step) -> bool>;

/// Everything the command line touches besides its own arguments.
pub struct Context {
    /// What the command line shares with the GUI: the home, the daemon, Windows.
    pub client: crisol_client::Context,
    /// The folder the command ran in.
    pub cwd: PathBuf,
    /// The environment of this process, to tell which shell this is.
    pub env: EnvLookup,
    /// The program that started this one (`pwsh.exe`), which tells the shell better.
    pub parent_program: Option<String>,
    /// All the variables of the environment of this process, for `crisol shell`.
    pub env_vars: Box<dyn Fn() -> Vec<(OsString, OsString)>>,
    /// Asks before something that cannot be undone (`uninstall --all`).
    pub confirm: Confirm,
    /// Asks before `setup` changes anything.
    pub confirm_setup: ConfirmSetup,
    /// Asks about each step of `setup` that is asked alone.
    pub confirm_step: ConfirmStep,
    /// Time between asking again for the log of `logs -f`.
    pub follow_every: Duration,
    /// Stops `logs -f` after this many rounds (`None`: until the process is killed).
    pub follow_rounds: Option<u32>,
    /// How standard output looks (tables and colour only in a terminal; `--plain` turns it off).
    pub style: render::Style,
    /// How standard error looks (the messages of errors and the progress line).
    pub err_style: render::Style,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("client", &self.client)
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
    let plain = args.plain || json;
    let mut session = Session {
        context,
        json,
        style: if plain {
            render::Style::plain()
        } else {
            context.style
        },
        err_style: if plain {
            render::Style::plain()
        } else {
            context.err_style
        },
        out,
        err,
        daemon_version: None,
    };
    session.execute(args)
}

struct Session<'a> {
    context: &'a Context,
    json: bool,
    style: render::Style,
    err_style: render::Style,
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
    /// The version of the daemon the last connection spoke with.
    daemon_version: Option<String>,
}

impl Session<'_> {
    fn execute(&mut self, args: Args) -> u8 {
        let plan = args.command.plan(&self.context.cwd);
        match plan {
            Plan::Call(call) => self.one(&call),
            Plan::Follow { service, lines } => self.follow(&service, lines),
            Plan::Autostart(action) => self.autostart(action),
            Plan::System(action) => self.system(action),
            Plan::UninstallAll { purge, yes } => self.uninstall_all(purge, yes),
            Plan::Setup { yes } => self.setup(yes),
            Plan::Trust => self.trust(),
            Plan::Untrust { stale } => self.untrust(stale),
            Plan::Shims(action) => self.shims(action),
            Plan::Path(action) => self.path(action),
            Plan::Env(shell) => self.env(shell),
            Plan::Shell { shell, clean } => self.shell(shell, clean),
        }
    }

    /// Prints the lines of a message, each with its own mark in a terminal (ADR-0028), or `json`
    /// when `--json` was given.
    fn say_lines(&mut self, lines: &[(String, render::Outcome)], json: &serde_json::Value) {
        if self.json {
            let _ = writeln!(self.out, "{json}");
        } else {
            for (line, outcome) in lines {
                let shown = render::message_styled(line, *outcome, &self.style);
                let _ = write!(self.out, "{shown}");
            }
        }
    }

    /// Prints a text with the mark of `outcome` on its first line in a terminal (ADR-0028), or
    /// `json` when `--json` was given.
    fn say_marked(&mut self, text: &str, outcome: render::Outcome, json: &serde_json::Value) {
        if self.json {
            let _ = writeln!(self.out, "{json}");
        } else {
            let shown = render::message_styled(text, outcome, &self.style);
            let _ = write!(self.out, "{shown}");
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
        match self.context.client.shim_exe.clone() {
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
        let folder = self.context.client.paths.shims_dir().display().to_string();
        let done = match shimsetup::install_shims(
            &self.context.client.paths,
            &exe,
            self.context.client.dirs.as_ref(),
            self.context.client.reserved_name.as_ref(),
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
        let outcome = if !done.failed.is_empty() {
            render::Outcome::Bad
        } else if done.globals_skipped.is_empty() {
            render::Outcome::Good
        } else {
            render::Outcome::Warn
        };
        self.say_marked(&text, outcome, &json);
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
            &self.context.client.paths,
            &exe,
            self.context.client.dirs.as_ref(),
            self.context.client.reserved_name.as_ref(),
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
        let outcome = if done.skipped.is_empty() {
            render::Outcome::Good
        } else {
            render::Outcome::Warn
        };
        self.say_marked(&text, outcome, &json);
        EXIT_OK
    }

    fn shims_remove(&mut self) -> u8 {
        let folder = self.context.client.paths.shims_dir().display().to_string();
        match shimsetup::remove_shims_files(&self.context.client.paths) {
            Ok(removed) => {
                let text = if removed.is_empty() {
                    format!("There were no shims in {folder}")
                } else {
                    format!("{} shim(s) removed from {folder}", removed.len())
                };
                self.say_marked(
                    &text,
                    render::Outcome::Good,
                    &serde_json::json!({ "removed": removed }),
                );
                EXIT_OK
            }
            Err(e) => self.fail(&RpcError::new(ErrorCode::Io, e.to_string())),
        }
    }

    fn shims_status(&mut self) -> u8 {
        let paths = &self.context.client.paths;
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
        let outcome = if missing.is_empty() {
            render::Outcome::Good
        } else {
            render::Outcome::Warn
        };
        self.say_marked(&text, outcome, &json);
        EXIT_OK
    }

    fn path(&mut self, action: PathAction) -> u8 {
        let paths = &self.context.client.paths;
        let store = self.context.client.user_path.as_ref();
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
        let shows_shadows = word != "removed" && word != "absent";
        // Not in the PATH is only worth a `!` when it was asked about, not after `path remove`.
        let outcome = if (shows_shadows && !status.shadows.is_empty())
            || (word == "status" && !status.in_path)
        {
            render::Outcome::Warn
        } else {
            render::Outcome::Good
        };
        if shows_shadows {
            for line in shimsetup::describe_shadows(&status.shadows) {
                text.push_str("\n  ");
                text.push_str(&line.to_string());
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
        self.say_marked(&text, outcome, &json);
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
        let script = shimsetup::env_script(&self.context.client.paths.shims_dir(), shell);
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
        let config = match Config::load_from(&self.context.client.paths) {
            Ok(loaded) => loaded.config,
            Err(error) => {
                return self.fail(&RpcError::new(ErrorCode::InvalidParams, error.to_string()))
            }
        };
        let parent = self.context.parent_program.as_deref();
        let desktop = std::sync::Arc::clone(&self.context.client.desktop);
        let find = |t: Terminal| desktop.find_terminal(t, parent);
        // What was typed, then the terminal this was typed in, then `[shell] terminal`.
        let chosen = shell::choose_terminal(asked, config.terminal_preference(), parent, &find);
        let (terminal, program, note) = match chosen {
            Ok(chosen) => chosen,
            Err(error) => return self.shell_error(&error),
        };
        let front = shell::front_folders(&self.context.client.paths, &config);
        let env = (self.context.env_vars)();
        let launch = match shell::plan_shell(&shell::ShellInput {
            paths: &self.context.client.paths,
            terminal,
            program: &program,
            clean,
            front: &front,
            env: &env,
            cwd: None,
        }) {
            Ok(launch) => launch,
            Err(error) => return self.shell_error(&error),
        };
        if let Some(note) = note {
            let _ = writeln!(self.err, "crisol: {note}");
        }
        match desktop.run_terminal(&launch) {
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
        (self.context.client.trust_store)().map_err(|e| {
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
        let paths = &self.context.client.paths;
        let dirs = self.context.client.dirs.as_ref();
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
            for line in lines {
                let _ = writeln!(self.out, "{line}");
            }
            if !plan.already {
                // A thing to look at before saying yes.
                let ask = render::message_styled(
                    "Windows will ask you to confirm: check that the thumbprint (sha1) it shows is the SHA-1 above",
                    render::Outcome::Warn,
                    &self.style,
                );
                let _ = write!(self.out, "{ask}");
            }
            let _ = self.out.flush();
        }
        let outcome = match trust_ca(store.as_ref(), &plan.ca, paths, dirs) {
            Ok(outcome) => outcome,
            Err(error) => return self.trust_error(&error),
        };
        let (word, said, mark, code) = match outcome {
            AddOutcome::Added => (
                "added",
                "Windows trusts it now; a browser that was open may need a restart",
                render::Outcome::Good,
                EXIT_OK,
            ),
            AddOutcome::AlreadyThere => (
                "already",
                "Windows already trusts it; nothing was changed",
                render::Outcome::Good,
                EXIT_OK,
            ),
            AddOutcome::Declined => (
                "declined",
                "Windows was told no; nothing was changed",
                render::Outcome::Bad,
                EXIT_FAILED,
            ),
        };
        let mut lines = vec![(said.to_owned(), mark)];
        if !plan.stale.is_empty() {
            lines.push((
                format!(
                    "Older Crisol CAs are still trusted: {}; `crisol untrust --stale` takes them out",
                    plan.stale.join(", ")
                ),
                render::Outcome::Warn,
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
        self.say_lines(&lines, &json);
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
        let paths = &self.context.client.paths;
        let report = match untrust_local(
            store.as_ref(),
            paths,
            self.context.client.dirs.as_ref(),
            stale,
            SystemTime::now(),
        ) {
            Ok(report) => report,
            Err(error) => return self.trust_error(&error),
        };
        let tld = crisol_core::setup::suffix_of(paths);
        // Nothing changed only because Windows was told no: the line under it carries the cross.
        let first_mark = if report.removed.is_empty() && !report.declined.is_empty() {
            render::Outcome::Info
        } else {
            render::Outcome::Good
        };
        let first = match (report.removed.is_empty(), stale, &report.current) {
            _ if report.removed.is_empty() && !report.declined.is_empty() => {
                "Nothing was changed".to_owned()
            }
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
        };
        let mut lines = vec![(first, first_mark)];
        if !report.declined.is_empty() {
            lines.push((
                format!(
                    "Windows was told no for {}; it is still trusted",
                    report.declined.join(", ")
                ),
                render::Outcome::Bad,
            ));
        }
        if !stale && report.stale_left > 0 {
            lines.push((
                format!(
                    "{} older Crisol CA(s) are still trusted; `crisol untrust --stale` takes them out",
                    report.stale_left
                ),
                render::Outcome::Warn,
            ));
        }
        let json = serde_json::json!({
            "removed": report.removed,
            "declined": report.declined,
            "current": report.current,
            "stale_left": report.stale_left,
        });
        self.say_lines(&lines, &json);
        if report.declined.is_empty() {
            EXIT_OK
        } else {
            EXIT_FAILED
        }
    }

    fn system(&mut self, action: SystemAction) -> u8 {
        let setup = self.context.client.setup.as_ref();
        let suffix = &crisol_core::setup::suffix_of(&self.context.client.paths);
        let outcome = match action {
            SystemAction::Install => setup.install(suffix),
            SystemAction::Uninstall => setup.uninstall(suffix, self.context.client.paths.home()),
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

    fn setup(&mut self, yes: bool) -> u8 {
        let context = self.context;
        crisol_client::with_setup_system(&context.client, |system| self.setup_on(system, yes))
    }

    fn setup_on(&mut self, system: &onboarding::System<'_>, yes: bool) -> u8 {
        let context = self.context;
        let now = SystemTime::now();
        let plan = onboarding::plan(system, now);
        let mut json = serde_json::to_value(&plan).unwrap_or_default();
        if !self.json {
            let _ = write!(self.out, "{}", render::setup_plan(&plan));
            let _ = self.out.flush();
        }
        if !yes && self.json && !plan.is_empty() && !plan.is_blocked() {
            // Without `--yes` the JSON is only the list: nothing can be asked.
            let _ = writeln!(self.out, "{json}");
            return EXIT_OK;
        }
        let agreed = onboarding::run_if_agreed(
            system,
            &plan,
            // What is asked alone is asked when its turn comes; if that is all there is, one
            // more question about the whole list would only repeat them.
            &mut |plan| {
                yes || plan.steps.iter().all(onboarding::Step::optional)
                    || (context.confirm_setup)(plan)
            },
            &mut |step| yes || (context.confirm_step)(step),
            now,
        );
        let outcomes = match agreed {
            onboarding::Agreed::Blocked => {
                if self.json {
                    let _ = writeln!(self.out, "{json}");
                }
                return EXIT_FAILED;
            }
            onboarding::Agreed::NothingToDo => {
                if self.json {
                    let _ = writeln!(self.out, "{json}");
                }
                return EXIT_OK;
            }
            onboarding::Agreed::No => {
                let _ = writeln!(self.out, "Nothing was changed");
                return EXIT_FAILED;
            }
            onboarding::Agreed::Ran(outcomes) => outcomes,
        };
        json["results"] = serde_json::to_value(&outcomes).unwrap_or_default();
        let failed = outcomes
            .iter()
            .any(|o| o.kind == teardown::OutcomeKind::Failed);
        if self.json {
            let _ = writeln!(self.out, "{json}");
        } else {
            let _ = write!(self.out, "\n{}", render::teardown_outcomes(&outcomes));
            let _ = writeln!(
                self.out,
                "{}",
                if failed {
                    "Something failed; solve what is said above and run `crisol setup` again: it only does what is left."
                } else if onboarding::needs_new_terminal(&outcomes) {
                    "Done. Open a new terminal for the commands to be in your PATH; `crisol start` starts the sites."
                } else {
                    "Done. `crisol start` starts the sites."
                }
            );
        }
        if failed {
            EXIT_FAILED
        } else {
            EXIT_OK
        }
    }

    fn uninstall_all(&mut self, purge: bool, yes: bool) -> u8 {
        let context = self.context;
        crisol_client::with_teardown_system(&context.client, |system| {
            self.uninstall_on(system, purge, yes)
        })
    }

    fn uninstall_on(&mut self, system: &teardown::System<'_>, purge: bool, yes: bool) -> u8 {
        let context = self.context;
        let plan = teardown::plan(system, purge);
        let mut json = serde_json::to_value(&plan).unwrap_or_default();
        if !self.json {
            let _ = write!(self.out, "{}", render::teardown_plan(&plan));
            let _ = self.out.flush();
        }
        if !yes && self.json && !plan.is_empty() {
            // Without `--yes` the JSON is only the list: nothing can be asked.
            let _ = writeln!(self.out, "{json}");
            return EXIT_OK;
        }
        let agreed = teardown::run_if_agreed(
            system,
            &plan,
            &mut |plan| yes || (context.confirm)(plan),
            &mut || self.stop_daemon(),
        );
        let outcomes = match agreed {
            teardown::Agreed::NothingToDo => {
                if self.json {
                    let _ = writeln!(self.out, "{json}");
                }
                return if plan.problems.is_empty() {
                    EXIT_OK
                } else {
                    EXIT_FAILED
                };
            }
            teardown::Agreed::No => {
                let _ = writeln!(self.out, "Nothing was changed");
                return EXIT_FAILED;
            }
            teardown::Agreed::Ran(outcomes) => outcomes,
        };
        json["results"] = serde_json::to_value(&outcomes).unwrap_or_default();
        let done = teardown::all_done(&outcomes);
        if self.json {
            let _ = writeln!(self.out, "{json}");
        } else {
            let _ = write!(self.out, "\n{}", render::teardown_outcomes(&outcomes));
            let _ = writeln!(
                self.out,
                "{}",
                if done {
                    "Crisol is out of Windows."
                } else {
                    "Some of it is still in place; solve what is said above and run `crisol uninstall --all` again to finish."
                }
            );
        }
        if done && plan.problems.is_empty() {
            EXIT_OK
        } else {
            EXIT_FAILED
        }
    }

    /// Asks the daemon to stop and waits until its pipe is gone, so its files are free.
    fn stop_daemon(&mut self) -> Result<(), String> {
        let err = &mut *self.err;
        connect::stop_daemon(&self.context.client, &mut |line| {
            let _ = writeln!(err, "{line}");
        })
    }

    fn autostart(&mut self, action: AutostartAction) -> u8 {
        let Some(daemon) = self.context.client.daemon_exe.clone() else {
            return self.fail(&RpcError::new(
                ErrorCode::NotAvailable,
                "crisold.exe was not found next to this program",
            ));
        };
        let store = self.context.client.autostart.as_ref();
        let io_error = |e: std::io::Error| RpcError::new(ErrorCode::Io, e.to_string());
        // Off is a choice, not a fault: as a plain answer to `status` it carries no mark.
        let (good, info, warn) = (
            render::Outcome::Good,
            render::Outcome::Info,
            render::Outcome::Warn,
        );
        let (word, outcome, message) = match action {
            AutostartAction::On => match crisol_core::autostart::enable(store, &daemon, self.context.client.dirs.as_ref()) {
                Ok(AutostartChange::Enabled) => ("on", good, "Crisol will start when you sign in".to_owned()),
                Ok(AutostartChange::Replaced) => ("on", good, "Crisol will start when you sign in (the entry of another crisold was replaced)".to_owned()),
                Ok(AutostartChange::Unchanged) => ("on", good, "Crisol already starts when you sign in".to_owned()),
                Err(crisol_core::autostart::AutostartError::Program(e)) => return self.fail(&program_error(&e)),
                Err(crisol_core::autostart::AutostartError::Io(e)) => return self.fail(&io_error(e)),
            },
            AutostartAction::Off => match crisol_core::autostart::disable(store) {
                Ok(true) => ("off", good, "Crisol will not start when you sign in".to_owned()),
                Ok(false) => ("off", good, "Crisol did not start when you signed in".to_owned()),
                Err(e) => return self.fail(&io_error(e)),
            },
            AutostartAction::Status => match crisol_core::autostart::state(store, &daemon) {
                Ok(AutostartState::On) => ("on", good, "Crisol starts when you sign in".to_owned()),
                Ok(AutostartState::Off) => ("off", info, "Crisol does not start when you sign in; `crisol autostart on` makes it".to_owned()),
                Ok(AutostartState::Other(other)) => ("other", warn, format!("Crisol starts when you sign in, but with another crisold: {other}")),
                Err(e) => return self.fail(&io_error(e)),
            },
        };
        if self.json {
            let _ = writeln!(self.out, "{{\"autostart\":\"{word}\"}}");
        } else {
            let shown = render::message_styled(&message, outcome, &self.style);
            let _ = write!(self.out, "{shown}");
        }
        EXIT_OK
    }

    /// Connects to the daemon, starting it if the call is one that should.
    fn link(&mut self, call: &Call) -> Result<Box<dyn Link>, RpcError> {
        let err = &mut *self.err;
        let opened = connect::open(&self.context.client, call, &mut |line| {
            let _ = writeln!(err, "{line}");
        })?;
        self.daemon_version = opened.daemon_version;
        Ok(opened.link)
    }

    fn one(&mut self, call: &Call) -> u8 {
        let mut link = match self.link(call) {
            Ok(link) => link,
            Err(error) => return self.fail(&error),
        };
        let mut last = None;
        let json = self.json;
        let in_place = self.err_style.formatted;
        let err_width = self.err_style.width;
        let mut drawn = false;
        let err = &mut *self.err;
        let outcome = link.call(call, &mut |event| {
            if json {
                return;
            }
            if in_place {
                // One line that is drawn again over itself (ADR-0028).
                if let Some(line) = render::progress_in_place(&event, &mut last, err_width) {
                    drawn = true;
                    let _ = write!(err, "{line}");
                    let _ = err.flush();
                }
            } else if let Some(line) = render::progress(&event, &mut last) {
                let _ = writeln!(err, "{line}");
            }
        });
        if drawn {
            let _ = writeln!(self.err);
        }
        match outcome {
            Ok(mut reply) => {
                self.note_daemon_version(&mut reply);
                self.show(&reply)
            }
            Err(error) => self.fail(&client_error(error)),
        }
    }

    /// `doctor` says when the daemon that answers is not of the version of this program: after
    /// an update the old one keeps running until it is stopped (Windows does not let the
    /// program be replaced while it runs, so the update itself needs it stopped).
    fn note_daemon_version(&self, reply: &mut Reply) {
        let (Reply::Doctor { checks }, Some(daemon)) = (reply, &self.daemon_version) else {
            return;
        };
        checks.extend(connect::daemon_version_check(
            daemon,
            &self.context.client.version,
        ));
    }

    fn show(&mut self, reply: &Reply) -> u8 {
        let mut shown = reply.clone();
        if self.json {
            // `--json` is what it always was: English, and without the keys of the texts.
            shown.strip_texts();
            let _ = writeln!(
                self.out,
                "{}",
                serde_json::to_string(&shown).unwrap_or_default()
            );
        } else {
            localize_reply(&mut shown, self.context.client.language);
            let _ = write!(self.out, "{}", render::reply_styled(&shown, &self.style));
        }
        match reply {
            Reply::Doctor { checks } if checks.iter().any(|c| c.status == CheckStatus::Problem) => {
                EXIT_FAILED
            }
            _ => EXIT_OK,
        }
    }

    fn fail(&mut self, error: &RpcError) -> u8 {
        let mut shown = error.clone();
        if self.json {
            shown.strip_texts();
            let _ = writeln!(
                self.out,
                "{}",
                serde_json::to_string(&shown).unwrap_or_default()
            );
        } else {
            localize_error(&mut shown, self.context.client.language);
            let _ = write!(
                self.err,
                "{}",
                render::error_styled(&shown, &self.err_style)
            );
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
