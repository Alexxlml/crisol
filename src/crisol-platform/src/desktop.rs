//! What a program with a window asks of the desktop: other terminals, the browser and the
//! folder dialog (ADR-0026). It is a trait so that `crisol-client` decides and the tests
//! replace Windows with a fake; [`WindowsDesktop`] is the real one.

use std::io;
use std::path::PathBuf;

use crisol_core::shell::{ShellLaunch, Terminal};

/// The desktop of the user.
pub trait Desktop: Send + Sync {
    /// The program of `terminal` on this computer. `parent` is the program that started
    /// `crisol`: a Git Bash it names is the one opened again.
    fn find_terminal(&self, terminal: Terminal, parent: Option<&str>) -> Option<PathBuf>;

    /// Starts the terminal in this window and waits for it. Gives its exit code. For a program
    /// that has a console (`crisol shell`).
    fn run_terminal(&self, launch: &ShellLaunch) -> io::Result<i32>;

    /// Starts the terminal in a new window and does not wait for it. For a program that has no
    /// console (the GUI).
    fn spawn_terminal(&self, launch: &ShellLaunch) -> io::Result<()>;

    /// Opens an `http://` or `https://` address in the browser of the user.
    fn open_url(&self, url: &str) -> io::Result<()>;

    /// Asks the user for a folder with the dialog of the system. `owner` is the handle of the
    /// window the dialog belongs to. `None` when the dialog is cancelled.
    fn pick_folder(&self, owner: Option<isize>) -> io::Result<Option<PathBuf>>;
}

/// Whether `url` is an address a browser should be sent to: `http` or `https`, with a host and
/// nothing a command line could take for more than one argument. Anything else (a file, a
/// program, another scheme) would not be "open in the browser".
#[must_use]
pub fn is_web_address(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && !url
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '"')
    })
}

#[cfg(windows)]
pub use real::WindowsDesktop;

#[cfg(windows)]
mod real {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;

    use crisol_core::shell::{ShellLaunch, Terminal};
    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, ShellExecuteW, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{is_web_address, Desktop};

    /// `CREATE_NEW_CONSOLE`: the child gets a console window of its own.
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    /// The desktop of this Windows session.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct WindowsDesktop;

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    impl Desktop for WindowsDesktop {
        fn find_terminal(&self, terminal: Terminal, parent: Option<&str>) -> Option<PathBuf> {
            crate::terminal::find_terminal(terminal, parent)
        }

        fn run_terminal(&self, launch: &ShellLaunch) -> io::Result<i32> {
            crate::terminal::run_terminal(launch)
        }

        fn spawn_terminal(&self, launch: &ShellLaunch) -> io::Result<()> {
            let mut command = crate::terminal::command_of(launch);
            command.creation_flags(CREATE_NEW_CONSOLE);
            // The window is the user's from here on: it is not waited for, and nothing of this
            // process's handles is its business.
            command.spawn().map(drop)
        }

        fn open_url(&self, url: &str) -> io::Result<()> {
            if !is_web_address(url) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("`{url}` is not an http or https address"),
                ));
            }
            let (verb, url) = (wide("open"), wide(url));
            // SAFETY: both texts are NUL-terminated and outlive the call.
            let result = unsafe {
                ShellExecuteW(
                    None,
                    PCWSTR(verb.as_ptr()),
                    PCWSTR(url.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                )
            };
            // Values above 32 mean it worked (the old `HINSTANCE` convention).
            if result.0 as usize > 32 {
                Ok(())
            } else {
                Err(io::Error::other("Windows could not open the browser"))
            }
        }
        fn pick_folder(&self, owner: Option<isize>) -> io::Result<Option<PathBuf>> {
            pick(owner).map_err(io::Error::other)
        }
    }

    /// The system's folder dialog. COM is initialized on this thread for the length of the call.
    fn pick(owner: Option<isize>) -> windows::core::Result<Option<PathBuf>> {
        // SAFETY: the dialog is created, used and released on this thread, between the
        // initialization of COM and its end; `owner` is only a window handle for the dialog to
        // belong to, and the name Windows allocates is freed once copied.
        unsafe {
            // `S_FALSE` (already initialized) is balanced too; a different mode is not ours to fix.
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let result = (|| {
                let dialog: IFileOpenDialog =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)?;
                let owner = owner.map(|handle| HWND(handle as *mut std::ffi::c_void));
                if let Err(error) = dialog.Show(owner) {
                    return if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                        Ok(None)
                    } else {
                        Err(error)
                    };
                }
                let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
                let path = name.to_string().map(PathBuf::from);
                CoTaskMemFree(Some(name.0.cast_const().cast()));
                Ok(path.ok())
            })();
            if initialized {
                CoUninitialize();
            }
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_are_sent_to_the_browser() {
        assert!(is_web_address("http://shop.test/"));
        assert!(is_web_address("https://shop.test:19443/"));
        for no in [
            "",
            "http://",
            "http:///etc",
            "file:///C:/Windows/notepad.exe",
            "ftp://shop.test",
            "calc.exe",
            "shop.test",
            "http://shop.test/ --flag",
            "http://shop.test/\"",
            "http://shop.test/\r\nx",
        ] {
            assert!(!is_web_address(no), "{no:?}");
        }
    }
}
