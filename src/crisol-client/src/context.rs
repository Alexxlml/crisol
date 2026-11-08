//! Everything a program that talks to Crisol touches besides its own interface.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crisol_core::autostart::Autostart;
use crisol_core::engine::Deps;
use crisol_core::paths::{CrisolPaths, PrivateDirs};
use crisol_core::setup::SystemSetup;
use crisol_core::startmenu::StartMenu;
use crisol_core::trust::TrustStore;
use crisol_core::userpath::UserPath;
use crisol_platform::Desktop;

use crate::link::Connector;

/// Deletes files once this program has ended.
pub type RemovePrograms = Box<dyn Fn(&[PathBuf]) -> std::io::Result<()>>;

/// The parts of Windows and of the install that the command line and the GUI share. The tests
/// of each build it with fakes; [`Context::windows`] builds the real one.
pub struct Context {
    /// Who is talking, as the daemon's `hello` hears it (`crisol`, `crisol-gui`).
    pub client: String,
    /// The version of this program, which `doctor` compares with the daemon's.
    pub version: String,
    /// The Crisol home.
    pub paths: CrisolPaths,
    /// Builds the dependencies of the engine (only needed when there is no daemon).
    pub deps: Box<dyn Fn() -> std::io::Result<Deps>>,
    /// Connects to the daemon: `NotFound` when there is none.
    pub connect: Connector,
    /// Starts the daemon without waiting for it.
    pub spawn_daemon: Box<dyn Fn() -> std::io::Result<()>>,
    /// The one-time setup that needs an administrator (the helper and the DNS rule).
    pub setup: Box<dyn SystemSetup>,
    /// The sign-in entry that starts the daemon.
    pub autostart: Box<dyn Autostart>,
    /// The `crisold` that entry would start (the one next to this program).
    pub daemon_exe: Option<PathBuf>,
    /// The user PATH (the registry; a double in the tests).
    pub user_path: Box<dyn UserPath>,
    /// Opens the certificates the user trusts (the current user's root store; a double in the
    /// tests).
    pub trust_store: Box<dyn Fn() -> std::io::Result<Box<dyn TrustStore>>>,
    /// The `crisol-shim` that `shims install` copies (the one next to this program).
    pub shim_exe: Option<PathBuf>,
    /// Whether a program with that name is already in the `PATH` (a command of the shims would
    /// hide it).
    pub reserved_name: Box<dyn Fn(&str) -> bool>,
    /// Crisol's programs (`crisol.exe` and the ones next to it), which `uninstall --all` deletes.
    pub programs: Vec<PathBuf>,
    /// winget installed those programs, so winget removes them and `uninstall --all` does not.
    pub winget: bool,
    /// Deletes them once this program has ended.
    pub remove_programs: RemovePrograms,
    /// The Start menu of the user, where the entry that opens the window goes.
    pub start_menu: Box<dyn StartMenu>,
    /// The `crisol-gui` next to this program, if there is one.
    pub gui_exe: Option<PathBuf>,
    /// Where the window keeps its web view profile, which `uninstall --all` deletes.
    pub gui_data: Option<PathBuf>,
    /// Judges who can write to the programs before they are registered or launched.
    pub dirs: Arc<dyn PrivateDirs>,
    /// How long to wait for a daemon that was just started.
    pub daemon_wait: Duration,
    /// The desktop of the user: terminals, the browser, the folder dialog.
    pub desktop: Arc<dyn Desktop>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("client", &self.client)
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}
