//! The Start menu entry that opens the window (F13.9, ADR-0026 §6).
//!
//! winget installs a portable package without a shortcut, so `crisol setup` makes one for the
//! window (`crisol-gui.exe`) and `uninstall --all` takes it out. It lives in the user's own
//! Start menu folder: no administrator, and no other account sees it.
//!
//! The folder is behind [`StartMenu`]; the Windows implementation is in `crisol-platform`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The name of the shortcut, without `.lnk`.
pub const SHORTCUT_NAME: &str = "Crisol";

/// Where the shortcut is kept.
pub trait StartMenu: Send + Sync {
    /// Where the shortcut is, or would be.
    fn path(&self) -> io::Result<PathBuf>;
    /// What the shortcut opens, if there is one.
    fn target(&self) -> io::Result<Option<PathBuf>>;
    /// Makes the shortcut open `target`, replacing the one that is there.
    fn create(&self, target: &Path) -> io::Result<()>;
    /// Deletes the shortcut. Returns whether there was one.
    fn remove(&self) -> io::Result<bool>;
}

/// Whether two Windows paths are the same file name: case and the kind of slash do not matter.
#[must_use]
pub fn same_path(a: &Path, b: &Path) -> bool {
    let normal = |path: &Path| path.to_string_lossy().replace('/', "\\").to_lowercase();
    normal(a) == normal(b)
}

/// A shortcut kept in memory, for tests.
#[derive(Debug, Default)]
pub struct FakeStartMenu {
    target: Mutex<Option<PathBuf>>,
    unreadable: bool,
}

impl FakeStartMenu {
    /// With no shortcut.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// With a shortcut that opens `target`.
    #[must_use]
    pub fn with(target: impl Into<PathBuf>) -> Self {
        Self {
            target: Mutex::new(Some(target.into())),
            unreadable: false,
        }
    }

    /// With a shortcut that is there but cannot be read (a broken `.lnk`).
    #[must_use]
    pub fn unreadable() -> Self {
        Self {
            target: Mutex::new(Some(PathBuf::new())),
            unreadable: true,
        }
    }
}

impl StartMenu for FakeStartMenu {
    fn path(&self) -> io::Result<PathBuf> {
        Ok(PathBuf::from(format!(r"C:\Start Menu\{SHORTCUT_NAME}.lnk")))
    }

    fn target(&self) -> io::Result<Option<PathBuf>> {
        if self.unreadable {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a shortcut"));
        }
        Ok(self
            .target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    fn create(&self, target: &Path) -> io::Result<()> {
        *self.target.lock().unwrap_or_else(PoisonError::into_inner) = Some(target.to_path_buf());
        Ok(())
    }

    fn remove(&self) -> io::Result<bool> {
        Ok(self
            .target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_differ_only_in_case_or_slashes_are_the_same() {
        assert!(same_path(
            Path::new(r"C:\Crisol\crisol-gui.exe"),
            Path::new("c:/crisol/CRISOL-GUI.exe")
        ));
        assert!(!same_path(
            Path::new(r"C:\Crisol\crisol-gui.exe"),
            Path::new(r"C:\Other\crisol-gui.exe")
        ));
    }

    #[test]
    fn the_double_makes_replaces_and_removes() {
        let menu = FakeStartMenu::new();
        assert_eq!(menu.target().unwrap(), None);
        menu.create(Path::new(r"C:\a\crisol-gui.exe")).unwrap();
        menu.create(Path::new(r"C:\b\crisol-gui.exe")).unwrap();
        assert_eq!(
            menu.target().unwrap(),
            Some(PathBuf::from(r"C:\b\crisol-gui.exe"))
        );
        assert!(menu.remove().unwrap());
        assert!(!menu.remove().unwrap());
    }
}
