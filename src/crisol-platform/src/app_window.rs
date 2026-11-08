//! The window of Crisol in another process: found by the path of its program, closed by asking
//! it (`crisol-gui.exe --quit`, which its single-instance plugin passes to the one running).

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::PWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, CREATE_NO_WINDOW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use crisol_core::appwindow::{AppWindow, QUIT_ARG};
use crisol_core::startmenu::same_path;

/// How long the window has to end once asked.
const CLOSE_WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(200);

/// The window whose program is `exe` (the `crisol-gui.exe` next to `crisol.exe`).
#[derive(Debug, Clone)]
pub struct WindowsAppWindow {
    exe: PathBuf,
}

impl WindowsAppWindow {
    /// The window run from `exe`.
    #[must_use]
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self { exe: exe.into() }
    }
}

impl AppWindow for WindowsAppWindow {
    fn running(&self) -> io::Result<bool> {
        Ok(!others_running(&self.exe)?.is_empty())
    }

    fn close(&self) -> io::Result<bool> {
        if !self.running()? {
            return Ok(false);
        }
        // A second instance hands its arguments to the first one and ends at once.
        let mut asked = std::process::Command::new(&self.exe)
            .arg(QUIT_ARG)
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()?;
        let start = Instant::now();
        loop {
            // Collected so that it is not left as a zombie handle; it ends by itself.
            let _ = asked.try_wait();
            if !self.running()? {
                return Ok(true);
            }
            if start.elapsed() > CLOSE_WAIT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "it was still running after {} seconds",
                        CLOSE_WAIT.as_secs()
                    ),
                ));
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The processes of this session, other than this one, whose program is `exe`.
fn others_running(exe: &Path) -> io::Result<Vec<u32>> {
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return Ok(Vec::new());
    };
    let me = std::process::id();
    let session = session_of(me).ok_or_else(io::Error::last_os_error)?;
    Ok(processes()?
        .into_iter()
        .filter(|(pid, file)| *pid != me && file.to_lowercase() == name)
        .filter(|(pid, _)| session_of(*pid) == Some(session))
        .filter(|(pid, _)| image_path(*pid).is_some_and(|path| same_path(&path, exe)))
        .map(|(pid, _)| pid)
        .collect())
}

/// Every process: its id and the file name of its program.
fn processes() -> io::Result<Vec<(u32, String)>> {
    // SAFETY: plain snapshot request; the returned handle is owned right below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| io::Error::other(format!("cannot list the processes: {e}")))?;
    // SAFETY: `snapshot` is a valid handle that nobody else owns.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let handle = HANDLE(snapshot.as_raw_handle());
    let mut found = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..Default::default()
    };
    // SAFETY: `entry.dwSize` is set and `entry` outlives the call.
    let mut more = unsafe { Process32FirstW(handle, &raw mut entry) }.is_ok();
    while more {
        let length = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        found.push((
            entry.th32ProcessID,
            String::from_utf16_lossy(&entry.szExeFile[..length]),
        ));
        // SAFETY: same as for `Process32FirstW`.
        more = unsafe { Process32NextW(handle, &raw mut entry) }.is_ok();
    }
    Ok(found)
}

/// The Windows session of process `pid`: the window of another account is not this one's.
fn session_of(pid: u32) -> Option<u32> {
    let mut session = 0_u32;
    // SAFETY: `session` outlives the call.
    unsafe { ProcessIdToSessionId(pid, &raw mut session) }.ok()?;
    Some(session)
}

/// The full path of the program of process `pid`.
fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain call; the returned handle is owned right below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    // SAFETY: `handle` is a valid handle that nobody else owns.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    let mut buffer = vec![0_u16; 1024];
    let mut length = u32::try_from(buffer.len()).ok()?;
    // SAFETY: `buffer` has `length` UTF-16 units and both outlive the call.
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(handle.as_raw_handle()),
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &raw mut length,
        )
    }
    .ok()?;
    Some(PathBuf::from(String::from_utf16_lossy(
        buffer.get(..length as usize)?,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_never_the_window_and_a_program_that_is_not_running_is_not_found() {
        let me = std::env::current_exe().unwrap();
        assert!(!WindowsAppWindow::new(&me).running().unwrap());
        let nowhere = std::env::temp_dir().join("crisol-no-such-window.exe");
        let window = WindowsAppWindow::new(&nowhere);
        assert!(!window.running().unwrap());
        // Nothing to ask: the program is not even started.
        assert!(!window.close().unwrap());
    }

    #[test]
    fn another_process_of_the_same_program_is_found() {
        // `cmd.exe` stands for the window: another process of a known program, this session.
        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe");
        let mut child = std::process::Command::new(&cmd)
            .args(["/C", "ping -n 3 127.0.0.1 >NUL"])
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .unwrap();
        let found = WindowsAppWindow::new(&cmd).running();
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.unwrap());
    }
}
