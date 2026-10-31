//! Where the running `crisol.exe` really is.
//!
//! winget puts `crisol` in its links folder as a symbolic link to the program in its package
//! folder. `current_exe()` can give the path of the link, and the programs that belong with it
//! (`crisold.exe`, `crisol-shim.exe`, `crisol-elevate.exe`) are next to the target, not next to
//! the link (ADR-0024, section 4).

use std::io;
use std::path::{Path, PathBuf};

/// The path of the running program with links resolved, in the plain form (`C:\…`) that the
/// registry and the command lines of Windows expect.
pub fn real_current_exe() -> io::Result<PathBuf> {
    std::env::current_exe()?
        .canonicalize()
        .map(without_verbatim)
}

/// `\\?\C:\x` as `C:\x`. `canonicalize` gives the first form on Windows; a path that has no
/// plain form (a network path, or one that is too long) is left as it is.
fn without_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if is_drive_path(rest) => PathBuf::from(rest),
        _ => path,
    }
}

fn is_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

/// The folder of the running program, with links resolved.
pub fn real_program_folder() -> Option<PathBuf> {
    real_current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verbatim_prefix_goes_only_when_a_plain_path_remains() {
        assert_eq!(
            without_verbatim(PathBuf::from(r"\\?\C:\Users\Ana\crisol.exe")),
            PathBuf::from(r"C:\Users\Ana\crisol.exe")
        );
        for kept in [
            r"\\?\UNC\server\share\crisol.exe",
            r"C:\plain\crisol.exe",
            r"\\?\Volume{1}\x",
        ] {
            assert_eq!(without_verbatim(PathBuf::from(kept)), PathBuf::from(kept));
        }
    }

    #[test]
    fn the_running_program_is_found_by_a_plain_existing_path() {
        let exe = real_current_exe().unwrap();
        assert!(exe.is_file(), "{}", exe.display());
        assert!(
            !exe.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            exe.display()
        );
        assert_eq!(real_program_folder().as_deref(), exe.parent());
    }

    /// What winget does: the program is reached through a link, and its neighbours are next to
    /// the target. Making a link needs a privilege some accounts do not have: then it is skipped.
    #[test]
    fn a_path_through_a_symbolic_link_is_resolved_to_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let package = tmp.path().join("package");
        std::fs::create_dir(&package).unwrap();
        let target = package.join("crisol.exe");
        std::fs::write(&target, b"MZ").unwrap();
        let link = tmp.path().join("crisol.exe");
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            eprintln!("no privilege to make symbolic links: skipped");
            return;
        }
        let resolved = without_verbatim(link.canonicalize().unwrap());
        let folder = without_verbatim(package.canonicalize().unwrap());
        assert_eq!(resolved.parent(), Some(folder.as_path()));
    }
}
