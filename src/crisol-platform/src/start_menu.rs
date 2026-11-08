//! The Start menu entry of the window, as a `.lnk` in the user's own Programs folder
//! (F13.9, ADR-0026 §6).

use std::io;
use std::path::{Path, PathBuf};

use crisol_core::startmenu::{StartMenu, SHORTCUT_NAME};

/// The Start menu of the current user: `%APPDATA%\Microsoft\Windows\Start Menu\Programs`.
#[derive(Debug, Clone)]
pub struct WindowsStartMenu {
    folder: Option<PathBuf>,
}

impl WindowsStartMenu {
    /// The folder of the current user, from `%APPDATA%`. Without it every call fails.
    #[must_use]
    pub fn current_user() -> Self {
        Self {
            folder: std::env::var_os("APPDATA")
                .filter(|v| !v.is_empty())
                .map(|appdata| {
                    PathBuf::from(appdata)
                        .join("Microsoft")
                        .join("Windows")
                        .join("Start Menu")
                        .join("Programs")
                }),
        }
    }

    /// A Start menu in `folder` (for tests).
    #[must_use]
    pub fn in_folder(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: Some(folder.into()),
        }
    }

    fn folder(&self) -> io::Result<&Path> {
        self.folder
            .as_deref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is not set"))
    }

    fn shortcut(&self) -> io::Result<PathBuf> {
        Ok(self.folder()?.join(format!("{SHORTCUT_NAME}.lnk")))
    }
}

impl StartMenu for WindowsStartMenu {
    fn path(&self) -> io::Result<PathBuf> {
        self.shortcut()
    }

    fn target(&self) -> io::Result<Option<PathBuf>> {
        let path = self.shortcut()?;
        if !path.is_file() {
            return Ok(None);
        }
        link::read(&path).map(Some)
    }

    fn create(&self, target: &Path) -> io::Result<()> {
        if !target.is_absolute() || !target.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a program that exists", target.display()),
            ));
        }
        std::fs::create_dir_all(self.folder()?)?;
        link::write(&self.shortcut()?, target)
    }

    fn remove(&self) -> io::Result<bool> {
        match std::fs::remove_file(self.shortcut()?) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Reading and writing a `.lnk` is the shell's `IShellLinkW`, which needs COM on the thread.
mod link {
    use std::io;
    use std::path::{Path, PathBuf};

    use windows::core::{Interface, HSTRING};
    use windows::Win32::Foundation::MAX_PATH;
    use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    /// Runs `work` with COM initialized on this thread (balanced when it was ours to start).
    fn with_com<R>(work: impl FnOnce() -> windows::core::Result<R>) -> io::Result<R> {
        // SAFETY: COM is started and ended on this thread around `work`, which only uses
        // objects it creates itself and drops before the end.
        unsafe {
            // `S_FALSE` (already initialized) is balanced too; a different mode is not ours to fix.
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let result = work();
            if initialized {
                CoUninitialize();
            }
            result.map_err(io::Error::other)
        }
    }

    pub(super) fn write(path: &Path, target: &Path) -> io::Result<()> {
        with_com(|| {
            // SAFETY: the shell objects are created, used and released inside this call.
            unsafe {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&HSTRING::from(target.as_os_str()))?;
                if let Some(folder) = target.parent() {
                    link.SetWorkingDirectory(&HSTRING::from(folder.as_os_str()))?;
                }
                link.SetDescription(&HSTRING::from("Crisol - local web development"))?;
                let file: IPersistFile = link.cast()?;
                file.Save(&HSTRING::from(path.as_os_str()), true)
            }
        })
    }

    pub(super) fn read(path: &Path) -> io::Result<PathBuf> {
        with_com(|| {
            // SAFETY: the shell objects are created, used and released inside this call, and
            // the buffer handed to `GetPath` is as long as the length it is told.
            unsafe {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                let file: IPersistFile = link.cast()?;
                file.Load(&HSTRING::from(path.as_os_str()), STGM_READ)?;
                let mut buffer = [0u16; MAX_PATH as usize];
                let mut data = WIN32_FIND_DATAW::default();
                link.GetPath(&mut buffer, std::ptr::addr_of_mut!(data), 0)?;
                let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
                Ok(PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::startmenu::same_path;

    #[test]
    fn the_shortcut_is_made_read_replaced_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let menu = WindowsStartMenu::in_folder(tmp.path().join("Programs"));
        let first = tmp.path().join("one").join("crisol-gui.exe");
        let second = tmp.path().join("two").join("crisol-gui.exe");
        for exe in [&first, &second] {
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(exe, "MZ").unwrap();
        }

        assert_eq!(menu.target().unwrap(), None);
        assert!(!menu.remove().unwrap());
        menu.create(&first).unwrap();
        assert!(menu.path().unwrap().is_file());
        assert!(same_path(&menu.target().unwrap().unwrap(), &first));
        menu.create(&second).unwrap();
        assert!(same_path(&menu.target().unwrap().unwrap(), &second));
        assert!(menu.remove().unwrap());
        assert_eq!(menu.target().unwrap(), None);
    }

    #[test]
    fn a_shortcut_to_a_program_that_is_not_there_is_not_made() {
        let tmp = tempfile::tempdir().unwrap();
        let menu = WindowsStartMenu::in_folder(tmp.path().join("Programs"));
        let error = menu.create(&tmp.path().join("missing.exe")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!menu.path().unwrap().exists());
        let relative = menu.create(Path::new("crisol-gui.exe")).unwrap_err();
        assert_eq!(relative.kind(), io::ErrorKind::InvalidInput);
    }
}
