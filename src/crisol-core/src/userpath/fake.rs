//! An in-memory [`UserPath`] for tests: the value, the machine value, and a count of writes
//! and notifications.

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{PathValue, UserPath};

#[derive(Debug, Default)]
struct State {
    user: Option<PathValue>,
    system: Option<String>,
    writes: usize,
    notifications: usize,
    fail_writes: bool,
}

/// Scriptable fake of [`UserPath`]. `%NAME%` is expanded from the variables given with
/// [`FakeUserPath::variable`].
#[derive(Debug, Default)]
pub struct FakeUserPath {
    state: Mutex<State>,
    variables: Mutex<Vec<(String, String)>>,
}

impl FakeUserPath {
    /// A user with no `PATH` and an empty machine `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A user `PATH` that already has this value (expandable, as Windows creates it).
    #[must_use]
    pub fn with_user(text: &str) -> Self {
        let fake = Self::new();
        fake.set_user(Some(PathValue {
            text: text.to_owned(),
            expandable: true,
        }));
        fake
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the stored user value directly (not counted as a write).
    pub fn set_user(&self, value: Option<PathValue>) {
        self.state().user = value;
    }

    /// Sets the machine `PATH`.
    pub fn set_system(&self, text: &str) {
        self.state().system = Some(text.to_owned());
    }

    /// Defines `%name%`.
    pub fn variable(&self, name: &str, value: &str) {
        self.variables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((name.to_owned(), value.to_owned()));
    }

    /// The stored user value.
    #[must_use]
    pub fn user(&self) -> Option<PathValue> {
        self.state().user.clone()
    }

    /// How many times the user value was written.
    #[must_use]
    pub fn writes(&self) -> usize {
        self.state().writes
    }

    /// How many times a change was announced.
    #[must_use]
    pub fn notifications(&self) -> usize {
        self.state().notifications
    }

    /// Makes every following write fail.
    pub fn fail_writes(&self) {
        self.state().fail_writes = true;
    }
}

impl UserPath for FakeUserPath {
    fn read(&self) -> io::Result<Option<PathValue>> {
        Ok(self.state().user.clone())
    }

    fn write(&self, value: Option<&PathValue>) -> io::Result<()> {
        let mut state = self.state();
        if state.fail_writes {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read only"));
        }
        state.writes += 1;
        state.user = value.cloned();
        Ok(())
    }

    fn system_path(&self) -> io::Result<Option<String>> {
        Ok(self.state().system.clone())
    }

    fn expand(&self, text: &str) -> String {
        let variables = self
            .variables
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut out = text.to_owned();
        for (name, value) in variables.iter() {
            out = out.replace(&format!("%{name}%"), value);
        }
        out
    }

    fn notify_changed(&self) -> io::Result<()> {
        self.state().notifications += 1;
        Ok(())
    }
}
