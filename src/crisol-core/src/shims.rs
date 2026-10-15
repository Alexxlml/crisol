//! The files in `CRISOL_HOME\shims` (F4.4).
//!
//! A shim is a copy of the `crisol-shim` executable under the name of the command it stands for
//! (`php.exe`, `node.exe`, …): the program finds out which command it is from its own file name
//! (F4.2). Copies, not links, so that a shim keeps working if the original moves, and so that
//! Windows needs no special permission to make them.
//!
//! Only these five names are ever created or removed here. The folder is Crisol's, but nothing
//! else in it is touched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::CrisolPaths;

/// The commands that have a shim.
pub const SHIM_COMMANDS: [&str; 5] = ["php", "node", "npm", "npx", "composer"];

/// What [`install_shim_files`] did.
#[derive(Debug)]
pub struct ShimFiles {
    /// Shims created or brought up to date.
    pub written: Vec<PathBuf>,
    /// Shims that already were exactly this program.
    pub unchanged: Vec<PathBuf>,
    /// Shims that could not be written (a shim that is running cannot be replaced on
    /// Windows), with why. The others are done regardless.
    pub failed: Vec<(PathBuf, io::Error)>,
}

/// The file name of the shim for `command` on this platform (`php.exe` on Windows).
#[must_use]
pub fn shim_file_name(command: &str) -> String {
    format!("{command}{}", std::env::consts::EXE_SUFFIX)
}

/// Makes `shims/<command>` a copy of `shim_exe` for every command. A shim that is already
/// identical is left alone; one that differs (an older Crisol) is replaced atomically.
pub fn install_shim_files(paths: &CrisolPaths, shim_exe: &Path) -> io::Result<ShimFiles> {
    let program = fs::read(shim_exe)?;
    let dir = paths.shims_dir();
    fs::create_dir_all(&dir)?;
    let mut result = ShimFiles {
        written: Vec::new(),
        unchanged: Vec::new(),
        failed: Vec::new(),
    };
    for command in SHIM_COMMANDS {
        let target = dir.join(shim_file_name(command));
        if fs::read(&target).ok().as_deref() == Some(program.as_slice()) {
            result.unchanged.push(target);
            continue;
        }
        match write_atomically(&dir, &target, &program) {
            Ok(()) => result.written.push(target),
            Err(error) => result.failed.push((target, error)),
        }
    }
    Ok(result)
}

fn write_atomically(dir: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temp = tempfile::Builder::new()
        .prefix(".crisol-shim-")
        .tempfile_in(dir)?;
    io::Write::write_all(&mut temp, bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

/// Removes the shims, and the folder too if that leaves it empty. Returns the files removed.
/// Anything else in the folder stays.
pub fn remove_shim_files(paths: &CrisolPaths) -> io::Result<Vec<PathBuf>> {
    let dir = paths.shims_dir();
    let mut removed = Vec::new();
    for command in SHIM_COMMANDS {
        let target = dir.join(shim_file_name(command));
        match fs::remove_file(&target) {
            Ok(()) => removed.push(target),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // `remove_dir` only removes an empty folder.
    let _ = fs::remove_dir(&dir);
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, CrisolPaths, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        let exe = tmp.path().join("crisol-shim-build.exe");
        fs::write(&exe, b"MZ the shim, version one").unwrap();
        (tmp, paths, exe)
    }

    fn names(files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_command_gets_a_copy_in_the_shims_folder() {
        let (_tmp, paths, exe) = setup();
        let done = install_shim_files(&paths, &exe).unwrap();
        assert_eq!(done.written.len(), 5);
        assert!(done.unchanged.is_empty() && done.failed.is_empty());
        let expected: Vec<String> = SHIM_COMMANDS.iter().map(|c| shim_file_name(c)).collect();
        assert_eq!(names(&done.written), expected);
        for file in &done.written {
            assert!(file.starts_with(paths.shims_dir()));
            assert_eq!(fs::read(file).unwrap(), b"MZ the shim, version one");
        }
    }

    #[test]
    fn running_it_again_changes_nothing() {
        let (_tmp, paths, exe) = setup();
        install_shim_files(&paths, &exe).unwrap();
        let again = install_shim_files(&paths, &exe).unwrap();
        assert_eq!((again.written.len(), again.unchanged.len()), (0, 5));
    }

    #[test]
    fn a_new_version_replaces_the_old_shims() {
        let (_tmp, paths, exe) = setup();
        install_shim_files(&paths, &exe).unwrap();
        fs::write(&exe, b"MZ the shim, version two").unwrap();
        let again = install_shim_files(&paths, &exe).unwrap();
        assert_eq!(again.written.len(), 5);
        let php = paths.shims_dir().join(shim_file_name("php"));
        assert_eq!(fs::read(php).unwrap(), b"MZ the shim, version two");
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let (_tmp, paths, exe) = setup();
        install_shim_files(&paths, &exe).unwrap();
        let count = fs::read_dir(paths.shims_dir()).unwrap().count();
        assert_eq!(count, 5);
    }

    #[test]
    fn removing_takes_the_shims_and_the_empty_folder_but_not_strangers() {
        let (_tmp, paths, exe) = setup();
        install_shim_files(&paths, &exe).unwrap();
        let removed = remove_shim_files(&paths).unwrap();
        assert_eq!(removed.len(), 5);
        assert!(!paths.shims_dir().exists(), "an empty folder goes too");

        install_shim_files(&paths, &exe).unwrap();
        let mine = paths.shims_dir().join("my-tool.cmd");
        fs::write(&mine, "@echo hi").unwrap();
        remove_shim_files(&paths).unwrap();
        assert!(
            mine.is_file(),
            "files that are not shims stay, and so does the folder"
        );
    }

    #[test]
    fn removing_with_nothing_there_is_fine() {
        let (_tmp, paths, _exe) = setup();
        assert_eq!(remove_shim_files(&paths).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_missing_program_is_an_error_before_anything_is_written() {
        let (tmp, paths, _exe) = setup();
        assert!(install_shim_files(&paths, &tmp.path().join("nope.exe")).is_err());
        assert!(!paths.shims_dir().exists());
    }
}
