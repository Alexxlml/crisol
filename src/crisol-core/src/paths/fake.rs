//! A [`PrivateDirs`] for tests: nothing is ever really private or open until the test says so.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::PrivateDirs;

#[derive(Debug, Default)]
struct State {
    created: Vec<PathBuf>,
    exposed: BTreeSet<PathBuf>,
    user_writable: BTreeSet<PathBuf>,
    foreign: BTreeSet<PathBuf>,
}

/// Scriptable fake of [`PrivateDirs`]. A folder it creates is private; a path is open to other
/// accounts only after [`FakePrivateDirs::expose`], and so is everything inside it.
#[derive(Debug, Default)]
pub struct FakePrivateDirs {
    state: Mutex<State>,
}

impl FakePrivateDirs {
    /// Nothing exposed, nothing created.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `path` (and what is inside it) writable by other accounts.
    pub fn expose(&self, path: &Path) {
        self.state().exposed.insert(path.to_path_buf());
    }

    /// Makes it private again.
    pub fn close(&self, path: &Path) {
        self.state().exposed.remove(path);
        self.state().user_writable.remove(path);
    }

    /// Makes `path` (and what is inside it) writable by the current user only: private for
    /// [`PrivateDirs::is_writable_by_others`], but not for the stricter
    /// [`PrivateDirs::is_writable_by_non_administrators`].
    pub fn make_user_writable(&self, path: &Path) {
        self.state().user_writable.insert(path.to_path_buf());
    }

    /// Makes `path` (and what is inside it) belong to another account.
    pub fn make_foreign(&self, path: &Path) {
        self.state().foreign.insert(path.to_path_buf());
    }

    /// The folders created through [`PrivateDirs::create_private_dir`], in order.
    #[must_use]
    pub fn created(&self) -> Vec<PathBuf> {
        self.state().created.clone()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PrivateDirs for FakePrivateDirs {
    /// Like the real one, the parent must exist; a folder that exists is left alone.
    fn create_private_dir(&self, dir: &Path) -> io::Result<bool> {
        if dir.exists() {
            return Ok(false);
        }
        fs::create_dir(dir)?;
        self.state().created.push(dir.to_path_buf());
        Ok(true)
    }

    fn is_writable_by_others(&self, dir: &Path) -> io::Result<bool> {
        let state = self.state();
        Ok(dir.ancestors().any(|a| state.exposed.contains(a)))
    }

    fn fix_command(&self, path: &Path) -> Option<String> {
        Some(super::private_acl_fix_command(
            path,
            path.is_dir(),
            "S-1-5-21-1-2-3-1001",
        ))
    }

    fn owned_by_another_account(&self, path: &Path) -> io::Result<bool> {
        let state = self.state();
        Ok(path.ancestors().any(|a| state.foreign.contains(a)))
    }

    fn is_writable_by_non_administrators(&self, path: &Path) -> io::Result<bool> {
        let state = self.state();
        Ok(path
            .ancestors()
            .any(|a| state.exposed.contains(a) || state.user_writable.contains(a)))
    }
}
