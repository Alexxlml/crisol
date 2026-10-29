//! The program that started this process (`pwsh.exe`, `cmd.exe`, `bash.exe`…), so that
//! `crisol env` can tell which shell it is printing for. Variables such as `PROMPT` and `SHELL`
//! are inherited by every shell started from another one, so they cannot tell it on their own
//! (review of phase 10, finding 1).

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::core::PWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The program that started this process: its full path when Windows gives it (`crisol shell`
/// opens the same `bash.exe`), else its file name. `None` if Windows no longer knows it.
///
/// A parent that already ended (and whose id was given to another process) can give a wrong
/// name; that is not a concern for the shell a command was typed in, which waits for it.
#[must_use]
pub fn parent_program() -> Option<String> {
    // SAFETY: plain snapshot request; the returned handle is owned right below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
    // SAFETY: `snapshot` is a valid handle that nobody else owns.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let handle = HANDLE(snapshot.as_raw_handle());

    let mut entries = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).ok()?,
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
        entries.push((
            entry.th32ProcessID,
            entry.th32ParentProcessID,
            String::from_utf16_lossy(&entry.szExeFile[..length]),
        ));
        // SAFETY: same as for `Process32FirstW`.
        more = unsafe { Process32NextW(handle, &raw mut entry) }.is_ok();
    }
    let me = std::process::id();
    let parent = entries.iter().find(|(id, _, _)| *id == me)?.1;
    let name = entries
        .into_iter()
        .find(|(id, _, _)| *id == parent)
        .map(|(_, _, name)| name)?;
    Some(image_path(parent).unwrap_or(name))
}

/// The full path of the program of process `pid`.
fn image_path(pid: u32) -> Option<String> {
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
    Some(String::from_utf16_lossy(buffer.get(..length as usize)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_of_a_test_is_a_full_path_to_an_exe() {
        // `cargo test` starts the test binary; under another runner the name differs, but
        // there is always a parent, and Windows gives its path.
        let name = parent_program().expect("the parent of this process");
        assert!(std::path::Path::new(&name).is_absolute(), "{name}");
        assert!(
            std::path::Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("exe")),
            "{name}"
        );
    }
}
