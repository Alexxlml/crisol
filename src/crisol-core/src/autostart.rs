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

/// Makes the daemon start at sign-in.
pub fn enable(store: &dyn Autostart, daemon: &Path) -> io::Result<AutostartChange> {
    let command = command_for(daemon)?;
    match store.current()? {
        Some(current) if current.eq_ignore_ascii_case(&command) => Ok(AutostartChange::Unchanged),
        Some(_) => {
            store.set(&command)?;
            Ok(AutostartChange::Replaced)
        }
        None => {
            store.set(&command)?;
            Ok(AutostartChange::Enabled)
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
        assert_eq!(enable(&store, &exe).unwrap(), AutostartChange::Enabled);
        assert_eq!(state(&store, &exe).unwrap(), AutostartState::On);
        assert_eq!(enable(&store, &exe).unwrap(), AutostartChange::Unchanged);

        let stale = FakeAutostart::with("\"D:\\old\\crisold.exe\"");
        assert_eq!(
            state(&stale, &exe).unwrap(),
            AutostartState::Other("\"D:\\old\\crisold.exe\"".to_owned())
        );
        assert_eq!(enable(&stale, &exe).unwrap(), AutostartChange::Replaced);
        assert_eq!(state(&stale, &exe).unwrap(), AutostartState::On);
    }

    #[test]
    fn disabling_removes_only_the_entry_and_says_whether_there_was_one() {
        let (_dir, exe) = daemon();
        let store = FakeAutostart::new();
        assert!(!disable(&store).unwrap());
        enable(&store, &exe).unwrap();
        assert!(disable(&store).unwrap());
        assert_eq!(state(&store, &exe).unwrap(), AutostartState::Off);
    }
}
