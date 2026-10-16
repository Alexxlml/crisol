//! crisol-core: platform-independent logic of Crisol.

pub mod ca;
pub mod cacert;
pub mod catalog;
pub mod config;
pub mod constraint;
pub mod edit;
pub mod fetch;
pub mod install;
pub mod log;
pub mod manage;
pub mod nginx;
pub mod paths;
pub mod phpini;
pub mod phppool;
pub mod pin;
pub mod ports;
pub mod process;
pub mod project;
pub mod resolve;
pub mod service;
pub mod shims;
pub mod sites;
pub mod tls;
pub mod userpath;
pub mod version;
pub mod web;

pub use catalog::{Catalog, CatalogError, Package, Target};
pub use config::{Config, ConfigError, ConfigWarning, LoadedConfig};
pub use edit::{EditError, TomlEditor};
pub use log::{LogRotation, RotatingLog};
pub use paths::{CrisolPaths, HomeSecurity, PathsError, PrivateDirs};
pub use phpini::{write_php_ini, PhpIniError, PhpIniSettings, WrittenPhpIni};
pub use ports::{PortError, PortOwner, PortProbe, PortStatus};
pub use process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
    SupervisorError,
};
pub use project::{
    find_project_file, load_project, FileOwner, FileOwners, IgnoredProject, LoadedProject,
    ProjectConfig, ProjectLookup, ProjectSearch, TrustAllOwners, PROJECT_FILE,
};
pub use version::{Version, VersionError, VersionReq};

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
