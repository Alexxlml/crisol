//! crisol-platform: per-OS implementations behind the traits defined in `crisol-core`.

#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod console;
#[cfg(windows)]
mod db_admin;
#[cfg(windows)]
mod defender;
#[cfg(windows)]
mod diagnose;
#[cfg(windows)]
mod file_owner;
#[cfg(windows)]
mod helper_install;
#[cfg(windows)]
mod helper_location;
#[cfg(windows)]
mod hosts_file;
#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod job_object;
#[cfg(windows)]
mod nrpt;
#[cfg(windows)]
mod parent_process;
#[cfg(windows)]
mod port_probe;
#[cfg(windows)]
mod private_dir;
#[cfg(windows)]
mod root_certs;
#[cfg(windows)]
mod system;
#[cfg(windows)]
mod system_paths;
#[cfg(windows)]
mod terminal;
#[cfg(windows)]
mod trust_store;
#[cfg(windows)]
mod user_path;

#[cfg(windows)]
pub use autostart::RegistryAutostart;
#[cfg(windows)]
pub use console::survive_ctrl_c;
#[cfg(windows)]
pub use db_admin::WindowsDbAdmin;
#[cfg(windows)]
pub use defender::DefenderExclusions;
#[cfg(windows)]
pub use file_owner::WindowsFileOwners;
#[cfg(windows)]
pub use helper_install::{
    install_into, install_self, remove_installation, remove_system_changes, uninstall_self,
    WindowsSetup,
};
#[cfg(windows)]
pub use helper_location::{HelperLocation, HELPER_VERSION};
#[cfg(windows)]
pub use hosts_file::{flush_dns, ElevatedHosts, FileHosts};
#[cfg(windows)]
pub use ipc::{
    connect_to_daemon, pipe_name_for_user, spawn_daemon, DaemonConnection, PipeSecurity,
};
#[cfg(windows)]
pub use job_object::JobObjectSupervisor;
#[cfg(windows)]
pub use nrpt::{ElevatedNrpt, PowerShellNrpt};
#[cfg(windows)]
pub use parent_process::parent_program;
#[cfg(windows)]
pub use port_probe::WindowsPortProbe;
#[cfg(windows)]
pub use private_dir::WindowsPrivateDirs;
#[cfg(windows)]
pub use root_certs::WindowsRootCertificates;
#[cfg(windows)]
pub use system::{default_deps, WindowsSystemChecks};
#[cfg(windows)]
pub use system_paths::{
    is_system_program, path_program_guard, restrict_dll_search, system_directory, HELPER_EXE,
    HELPER_FOLDER,
};
#[cfg(windows)]
pub use terminal::{find_terminal, run_terminal};
#[cfg(windows)]
pub use trust_store::WindowsTrustStore;
#[cfg(windows)]
pub use user_path::RegistryUserPath;

use std::path::PathBuf;

/// Keeps a shim alive through Ctrl+C so it can pass on its child's exit code. Nothing to do
/// off Windows, where the shim replaces itself with the program instead.
#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)]
pub fn survive_ctrl_c() -> std::io::Result<()> {
    Ok(())
}

/// The program that started this process. Not looked up off Windows yet.
#[cfg(not(windows))]
#[must_use]
pub fn parent_program() -> Option<String> {
    None
}

/// Whether a global program called `name` must not get a command. Off Windows nothing is
/// reserved yet.
#[cfg(not(windows))]
pub fn path_program_guard(_paths: &CrisolPaths) -> impl Fn(&str) -> bool {
    |_| false
}

/// Whether the system has a program called `name`. Off Windows nothing is reserved.
#[cfg(not(windows))]
#[must_use]
pub fn is_system_program(_name: &str) -> bool {
    false
}

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

/// The SID of the user this process runs as, as text (`S-1-5-21-…`).
#[cfg(windows)]
pub fn current_user_sid() -> std::io::Result<String> {
    private_dir::current_user_sid()
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
