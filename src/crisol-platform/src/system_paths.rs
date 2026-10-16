//! Where Windows keeps the things Crisol runs with administrator rights or reads on behalf of
//! one (ADR-0014).
//!
//! Everything the elevated helper launches or opens is named by an **absolute path built from
//! the system directory** (`GetSystemDirectoryW`) and never by a bare name or by an
//! environment variable: a bare name is searched for in the folder of the calling program
//! first, and `%SystemRoot%` belongs to the user, who is not who the helper answers to. The
//! builders are plain functions of the directory they are given, so they are tested as text;
//! only the calls that ask Windows for the directories use `unsafe`.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// The folder of Crisol's privileged program inside `Program Files`.
pub const HELPER_FOLDER: &str = "Crisol";
/// The privileged program.
pub const HELPER_EXE: &str = "crisol-elevate.exe";

/// `C:\Windows\System32`, as the operating system reports it.
pub fn system_directory() -> io::Result<PathBuf> {
    let mut buffer = [0_u16; 260];
    // SAFETY: `buffer` is valid for writes of its own length.
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io::Error::other("the system directory could not be read"));
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
}

/// `C:\Program Files`, from the known-folder API (not from `%ProgramFiles%`).
pub fn program_files_directory() -> io::Result<PathBuf> {
    // SAFETY: `FOLDERID_ProgramFiles` is a valid folder id; no token and no flags.
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }
        .map_err(io::Error::from)?;
    // SAFETY: on success `path` is a valid NUL-terminated UTF-16 string.
    let text = unsafe { path.to_string() };
    // SAFETY: the string was allocated for us by the shell and is not used afterwards.
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    text.map(PathBuf::from)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Limits the places this process loads DLLs from, from now on, to `System32`.
pub fn restrict_dll_search() -> io::Result<()> {
    // SAFETY: a plain call with a valid flag.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(io::Error::from)
}

/// Windows PowerShell inside the system directory `system`.
#[must_use]
pub fn powershell_exe_in(system: &Path) -> PathBuf {
    system
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

/// `ipconfig.exe` inside the system directory `system`.
#[must_use]
pub fn ipconfig_exe_in(system: &Path) -> PathBuf {
    system.join("ipconfig.exe")
}

/// The `hosts` file inside the system directory `system`.
#[must_use]
pub fn hosts_file_in(system: &Path) -> PathBuf {
    system.join("drivers").join("etc").join("hosts")
}

/// Where the privileged helper lives, given `Program Files`.
#[must_use]
pub fn helper_exe_in(program_files: &Path) -> PathBuf {
    program_files.join(HELPER_FOLDER).join(HELPER_EXE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paths_are_built_from_the_directory_they_are_given() {
        let system = Path::new(r"C:\Windows\System32");
        assert_eq!(
            powershell_exe_in(system),
            Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
        );
        assert_eq!(
            ipconfig_exe_in(system),
            Path::new(r"C:\Windows\System32\ipconfig.exe")
        );
        assert_eq!(
            hosts_file_in(system),
            Path::new(r"C:\Windows\System32\drivers\etc\hosts")
        );
        assert_eq!(
            helper_exe_in(Path::new(r"C:\Program Files")),
            Path::new(r"C:\Program Files\Crisol\crisol-elevate.exe")
        );
    }

    #[test]
    fn the_system_directories_are_absolute_and_exist() {
        let system = system_directory().unwrap();
        assert!(system.is_absolute() && system.is_dir(), "{system:?}");
        assert!(powershell_exe_in(&system).is_file());
        assert!(ipconfig_exe_in(&system).is_file());
        assert!(hosts_file_in(&system).parent().unwrap().is_dir());
        let files = program_files_directory().unwrap();
        assert!(files.is_absolute() && files.is_dir(), "{files:?}");
    }
}
