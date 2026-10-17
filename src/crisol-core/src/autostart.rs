//! Starting the daemon when the user signs in (F8.4).
//!
//! It is optional and it is the user's: a value under the **current user's** `Run` key, with the
//! quoted path of `crisold` and nothing else. No Windows service, no scheduled task, no
//! administrator, and nothing runs with more rights than the user has.
//!
//! The store is behind [`Autostart`]; the Windows implementation is in `crisol-platform`.

use std::io;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use crisol_protocol::{Check, CheckStatus};

use crate::paths::PrivateDirs;
use crate::programs::{require_trusted_program, ProgramError};

/// Where the sign-in entry is kept.
pub trait Autostart: Send + Sync {
    /// The command line that is set to run at sign-in, if there is an entry.
    fn current(&self) -> io::Result<Option<String>>;
    /// Makes `command` the entry, replacing the one that is there.
    fn set(&self, command: &str) -> io::Result<()>;
    /// Removes the entry. Returns whether there was one.
    fn remove(&self) -> io::Result<bool>;
}

/// What the entry says about this installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartState {
    /// No entry.
    Off,
    /// An entry that starts this `crisold`.
    On,
    /// An entry with this other command (another install of Crisol, or an old path).
    Other(String),
}

/// What [`enable`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartChange {
    /// The entry was made.
    Enabled,
    /// An entry for another command was replaced.
    Replaced,
    /// It already was exactly this.
    Unchanged,
}

/// The command line for the entry: the path in quotes, no arguments.
pub fn command_for(daemon: &Path) -> io::Result<String> {
    if !daemon.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path of crisold must be absolute",
        ));
    }
    if !daemon.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", daemon.display()),
        ));
    }
    let text = daemon.to_string_lossy();
    if text.contains('"') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path of crisold has a quotation mark",
        ));
    }
    Ok(format!("\"{text}\""))
}

/// Whether the entry starts `daemon`.
pub fn state(store: &dyn Autostart, daemon: &Path) -> io::Result<AutostartState> {
    let wanted = command_for(daemon).ok();
    Ok(match store.current()? {
        None => AutostartState::Off,
        Some(current)
            if wanted
                .as_deref()
                .is_some_and(|w| w.eq_ignore_ascii_case(&current)) =>
        {
            AutostartState::On
        }
        Some(current) => AutostartState::Other(current),
    })
}

/// Why the entry could not be made.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// `crisold`, or its folder, can be replaced by other accounts: it would run at every
    /// sign-in with the user's rights.
    #[error("{0}")]
    Program(#[from] ProgramError),
    /// The store or the path failed.
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Makes the daemon start at sign-in. Refuses if `daemon` or its folder can be written by other
/// accounts (`dirs` says), since whoever can replace it runs code as this user at every sign-in.
pub fn enable(
    store: &dyn Autostart,
    daemon: &Path,
    dirs: &dyn PrivateDirs,
) -> Result<AutostartChange, AutostartError> {
    let command = command_for(daemon)?;
    require_trusted_program(daemon, dirs)?;
    enable_command(store, &command).map_err(AutostartError::Io)
}

fn enable_command(store: &dyn Autostart, command: &str) -> io::Result<AutostartChange> {
    match store.current()? {
        Some(current) if current.eq_ignore_ascii_case(command) => Ok(AutostartChange::Unchanged),
        Some(_) => {
            store.set(command)?;
            Ok(AutostartChange::Replaced)
        }
        None => {
            store.set(command)?;
            Ok(AutostartChange::Enabled)
        }
    }
}

/// The program an entry runs: its quoted path, or its first word when it is not quoted.
fn program_of(command: &str) -> Option<&str> {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix('"') {
        return rest.split_once('"').map(|(path, _)| path);
    }
    command.split_whitespace().next()
}

/// The `doctor` check of the sign-in entry: `None` when there is none; a problem when it runs a
/// `crisold.exe` that other accounts can replace (the entry runs at every sign-in), with
/// `crisol autostart off` as the way out. With the entry in order but no daemon running, it says
/// why that can be: Windows starts the programs of the sign-in entry some seconds after the
/// desktop appears (about 20 s were measured), so the daemon may simply not be there yet.
pub fn check(store: &dyn Autostart, dirs: &dyn PrivateDirs, daemon_running: bool) -> Option<Check> {
    let name = "autostart";
    let mk = |status, detail: String, fix: Option<&str>| Check {
        name: name.to_owned(),
        status,
        detail,
        fix: fix.map(str::to_owned),
    };
    let command = match store.current() {
        Ok(Some(command)) => command,
        Ok(None) => return None,
        Err(error) => {
            return Some(mk(
                CheckStatus::Warning,
                format!("cannot read the sign-in entry: {error}"),
                None,
            ))
        }
    };
    let Some(program) = program_of(&command).map(Path::new) else {
        return Some(mk(
            CheckStatus::Warning,
            format!("the sign-in entry is not a program: {command}"),
            Some("crisol autostart off"),
        ));
    };
    match require_trusted_program(program, dirs) {
        Ok(()) if daemon_running => Some(mk(
            CheckStatus::Ok,
            format!("{} starts when you sign in", program.display()),
            None,
        )),
        Ok(()) => Some(mk(
            CheckStatus::Warning,
            format!(
                "{} starts when you sign in, but the daemon is not running now. Windows delays the programs that start at sign-in by some seconds (about 20 were measured), so if you have just signed in, wait a moment; if it was closed on purpose, `crisol start` brings it back",
                program.display()
            ),
            Some("crisol start"),
        )),
        Err(ProgramError::Missing { .. }) => Some(mk(
            CheckStatus::Warning,
            format!(
                "the sign-in entry runs {}, which is not there any more",
                program.display()
            ),
            Some("crisol autostart off"),
        )),
        Err(ProgramError::Exposed { path, .. }) => Some(mk(
            CheckStatus::Problem,
            format!(
                "the sign-in entry runs {} and {} can be modified by other accounts, who would run code as you at every sign-in; close it, or turn the entry off",
                program.display(),
                path.display()
            ),
            Some("crisol autostart off"),
        )),
        Err(error @ ProgramError::Check { .. }) => {
            Some(mk(CheckStatus::Warning, error.to_string(), None))
        }
    }
}

/// Stops the daemon from starting at sign-in. Returns whether there was an entry.
pub fn disable(store: &dyn Autostart) -> io::Result<bool> {
    store.remove()
}

/// An entry kept in memory, for tests.
#[derive(Debug, Default)]
pub struct FakeAutostart {
    value: Mutex<Option<String>>,
}

impl FakeAutostart {
    /// With no entry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// With this entry.
    #[must_use]
    pub fn with(command: &str) -> Self {
        Self {
            value: Mutex::new(Some(command.to_owned())),
        }
    }
}

impl Autostart for FakeAutostart {
    fn current(&self) -> io::Result<Option<String>> {
        Ok(self
            .value
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    fn set(&self, command: &str) -> io::Result<()> {
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(command.to_owned());
        Ok(())
    }

    fn remove(&self) -> io::Result<bool> {
        Ok(self
            .value
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::fake::FakePrivateDirs;

    fn daemon() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("crisold.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        (dir, exe)
    }

    #[test]
    fn the_entry_is_the_quoted_path_and_nothing_else() {
        let (_dir, exe) = daemon();
        let command = command_for(&exe).unwrap();
        assert_eq!(command, format!("\"{}\"", exe.display()));
        assert!(!command.contains(" --"), "no arguments");
    }

    #[test]
    fn a_relative_missing_or_odd_path_is_refused() {
        assert_eq!(
            command_for(Path::new("crisold.exe")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let (dir, _) = daemon();
        assert_eq!(
            command_for(&dir.path().join("nope.exe"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn enabling_twice_changes_nothing_and_another_entry_is_replaced() {
        let (_dir, exe) = daemon();
        let store = FakeAutostart::new();
        assert_eq!(state(&store, &exe).unwrap(), AutostartState::Off);
        assert_eq!(
            enable(&store, &exe, &FakePrivateDirs::new()).unwrap(),
            AutostartChange::Enabled
        );
        assert_eq!(state(&store, &exe).unwrap(), AutostartState::On);
        assert_eq!(
            enable(&store, &exe, &FakePrivateDirs::new()).unwrap(),
            AutostartChange::Unchanged
        );

        let stale = FakeAutostart::with("\"D:\\old\\crisold.exe\"");
        assert_eq!(
            state(&stale, &exe).unwrap(),
            AutostartState::Other("\"D:\\old\\crisold.exe\"".to_owned())
        );
        assert_eq!(
            enable(&stale, &exe, &FakePrivateDirs::new()).unwrap(),
            AutostartChange::Replaced
        );
        assert_eq!(state(&stale, &exe).unwrap(), AutostartState::On);
    }

    #[test]
    fn disabling_removes_only_the_entry_and_says_whether_there_was_one() {
        let (_dir, exe) = daemon();
        let store = FakeAutostart::new();
        assert!(!disable(&store).unwrap());
        enable(&store, &exe, &FakePrivateDirs::new()).unwrap();
        assert!(disable(&store).unwrap());
        assert_eq!(state(&store, &exe).unwrap(), AutostartState::Off);
    }

    #[test]
    fn an_open_crisold_is_not_registered() {
        let (_dir, exe) = daemon();
        let dirs = FakePrivateDirs::new();
        dirs.expose(exe.parent().unwrap());
        let store = FakeAutostart::new();
        assert!(matches!(
            enable(&store, &exe, &dirs),
            Err(AutostartError::Program(ProgramError::Exposed { .. }))
        ));
        assert_eq!(store.current().unwrap(), None);
    }

    #[test]
    fn doctor_flags_an_entry_that_runs_a_program_others_can_replace() {
        let (_dir, exe) = daemon();
        let dirs = FakePrivateDirs::new();
        let store = FakeAutostart::new();
        assert!(
            check(&store, &dirs, true).is_none(),
            "no entry, nothing to say"
        );

        enable(&store, &exe, &dirs).unwrap();
        assert_eq!(check(&store, &dirs, true).unwrap().status, CheckStatus::Ok);

        // Entry in order, daemon not there: a warning that explains the delay of Windows.
        let waiting = check(&store, &dirs, false).unwrap();
        assert_eq!(waiting.status, CheckStatus::Warning);
        assert!(
            waiting.detail.contains("Windows delays"),
            "{}",
            waiting.detail
        );
        assert!(!waiting.detail.contains("  "), "{}", waiting.detail);

        dirs.expose(exe.parent().unwrap());
        let flagged = check(&store, &dirs, true).unwrap();
        assert_eq!(flagged.status, CheckStatus::Problem);
        assert_eq!(flagged.fix.as_deref(), Some("crisol autostart off"));

        let gone = FakeAutostart::with("\"D:\\nowhere\\crisold.exe\"");
        assert_eq!(
            check(&gone, &dirs, true).unwrap().status,
            CheckStatus::Warning
        );
    }
}
