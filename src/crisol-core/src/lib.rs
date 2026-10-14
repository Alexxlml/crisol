//! crisol-core: platform-independent logic of Crisol.

pub mod config;
pub mod edit;
pub mod log;
pub mod paths;
pub mod process;
pub mod project;

pub use config::{Config, ConfigError, ConfigWarning, LoadedConfig};
pub use edit::{EditError, TomlEditor};
pub use log::{LogRotation, RotatingLog};
pub use paths::{CrisolPaths, PathsError};
pub use process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
    SupervisorError,
};
pub use project::{find_project_file, load_project, LoadedProject, ProjectConfig, PROJECT_FILE};

/// Crate version, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert_ne!(VERSION, "");
    }
}
