//! The real context: this is where Windows is wired in, once, for every program.

use crisol_core::paths::PathsError;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crisol_core::appwindow::{AppWindow, NoAppWindow};
use crisol_core::programs::{is_winget_package, winget_package_folders, GUI_IDENTIFIER};
use crisol_core::trust::TrustStore;
use crisol_platform::{
    connect_to_daemon, default_deps, pipe_name_for_user, resolve_paths, spawn_daemon,
    RegistryAutostart, RegistryUserPath, WindowsAppWindow, WindowsDesktop, WindowsPrivateDirs,
    WindowsSetup, WindowsStartMenu, WindowsTrustStore,
};

use crate::context::Context;
use crate::link::{Connector, Link, Remote};

/// The programs that come together in one folder, in the order they are deleted.
const PROGRAMS: [&str; 5] = [
    "crisol.exe",
    "crisold.exe",
    "crisol-shim.exe",
    "crisol-elevate.exe",
    "crisol-gui.exe",
];

/// Why the context could not be built.
#[derive(Debug, thiserror::Error)]
pub enum WindowsError {
    /// The home could not be found.
    #[error("{0}")]
    Home(#[from] PathsError),
    /// Windows would not say which account this is.
    #[error("cannot tell which account this is: {0}")]
    Account(std::io::Error),
}

impl Context {
    /// The context of this machine: the real home, the real pipe, registry, certificate store
    /// and helper. `client` and `version` say who is asking (`"crisol"` and its version).
    pub fn windows(client: &str, version: &str) -> Result<Self, WindowsError> {
        let paths = resolve_paths()?;
        let pipe = pipe_name_for_user(paths.home()).map_err(WindowsError::Account)?;
        // The programs come together in one folder. This one may be reached through a link
        // (winget puts one in its links folder), so the folder is that of the real file.
        let folder = crisol_platform::real_program_folder();
        let daemon = folder.as_ref().map(|dir| dir.join("crisold.exe"));
        let winget = folder.as_deref().is_some_and(|dir| {
            is_winget_package(
                dir,
                &winget_package_folders(&|name| std::env::var(name).ok()),
            )
        });
        let reserved_name = crisol_platform::path_program_guard(&paths);
        let gui_exe = folder
            .as_ref()
            .map(|dir| dir.join("crisol-gui.exe"))
            .filter(|path| path.is_file());
        let connect: Connector = Arc::new(move || {
            let client = connect_to_daemon(&pipe)?;
            Ok(Box::new(Remote(client)) as Box<dyn Link>)
        });
        let language = crisol_core::i18n::choose(
            std::env::var(crisol_core::i18n::LANGUAGE_VARIABLE)
                .ok()
                .as_deref(),
            crisol_platform::ui_languages(),
        );
        Ok(Self {
            language,
            client: client.to_owned(),
            version: version.to_owned(),
            paths,
            deps: Box::new(default_deps),
            connect,
            setup: Box::new(WindowsSetup::new(
                daemon
                    .as_ref()
                    .and_then(|d| d.parent())
                    .map(|dir| dir.join("crisol-elevate.exe"))
                    .unwrap_or_default(),
            )),
            autostart: Box::new(RegistryAutostart::new()),
            daemon_exe: daemon.clone(),
            user_path: Box::new(RegistryUserPath::new()),
            trust_store: Box::new(|| {
                WindowsTrustStore::current_user_root()
                    .map(|store| Box::new(store) as Box<dyn TrustStore>)
            }),
            shim_exe: folder.as_ref().map(|dir| dir.join("crisol-shim.exe")),
            reserved_name: Box::new(reserved_name),
            // The programs that came together; `uninstall --all` deletes the ones that are
            // there, unless winget installed them: then winget owns them.
            programs: folder
                .as_ref()
                .filter(|_| !winget)
                .map(|dir| {
                    PROGRAMS
                        .iter()
                        .map(|name| dir.join(name))
                        .filter(|path| path.is_file())
                        .collect()
                })
                .unwrap_or_default(),
            winget,
            remove_programs: Box::new(crisol_platform::schedule_delete),
            start_menu: Box::new(WindowsStartMenu::current_user()),
            gui_exe: gui_exe.clone(),
            app_window: match gui_exe {
                Some(exe) => Box::new(WindowsAppWindow::new(exe)),
                None => Box::new(NoAppWindow) as Box<dyn AppWindow>,
            },
            // The profile of the window's web view: Crisol's even when winget owns the programs.
            gui_data: std::env::var_os("LOCALAPPDATA")
                .filter(|base| !base.is_empty())
                .map(|base| PathBuf::from(base).join(GUI_IDENTIFIER)),
            dirs: Arc::new(WindowsPrivateDirs::new()),
            spawn_daemon: Box::new(move || match &daemon {
                Some(path) if path.is_file() => spawn_daemon(path),
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "crisold.exe is not next to crisol.exe",
                )),
            }),
            daemon_wait: Duration::from_secs(15),
            desktop: Arc::new(WindowsDesktop),
        })
    }
}
