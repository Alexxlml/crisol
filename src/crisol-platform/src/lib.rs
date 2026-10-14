//! crisol-platform: per-OS implementations behind the traits defined in `crisol-core`.

#[cfg(windows)]
mod job_object;
#[cfg(windows)]
mod port_probe;

#[cfg(windows)]
pub use job_object::JobObjectSupervisor;
#[cfg(windows)]
pub use port_probe::WindowsPortProbe;

use std::path::PathBuf;

use crisol_core::{CrisolPaths, PathsError};

/// Default Crisol home when `CRISOL_HOME` is not set: `C:\Crisol` on Windows, `~/.crisol` elsewhere.
#[must_use]
pub fn default_home() -> Option<PathBuf> {
    if cfg!(windows) {
        Some(PathBuf::from(r"C:\Crisol"))
    } else {
        std::env::home_dir().map(|home| home.join(".crisol"))
    }
}

/// Resolves the Crisol paths from `CRISOL_HOME`, falling back to [`default_home`].
pub fn resolve_paths() -> Result<CrisolPaths, PathsError> {
    CrisolPaths::from_env(default_home)
}

/// Crate version, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert_ne!(VERSION, "");
    }

    #[test]
    fn default_home_is_absolute() {
        let home = default_home().expect("tests run with a home directory");
        assert!(home.is_absolute());
        assert!(home.ends_with(if cfg!(windows) { "Crisol" } else { ".crisol" }));
    }
}
