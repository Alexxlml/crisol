//! The files in `CRISOL_HOME\shims` (F4.4).
//!
//! A shim is a copy of the `crisol-shim` executable under the name of the command it stands for
//! (`php.exe`, `node.exe`, …): the program finds out which command it is from its own file name
//! (F4.2). Copies, not links, so that a shim keeps working if the original moves, and so that
//! Windows needs no special permission to make them.
//!
//! Only these five names (and the `.crisol-shim-old-*` leftovers described below) are ever
//! created or removed here. The folder is Crisol's, but nothing else in it is touched.
//!
//! Windows will not overwrite a program that is running, but it does let it be **renamed**. So a
//! shim in use (`php -S`, `npm run dev`…) is moved aside to `.crisol-shim-old-<n>`, the new one
//! takes its place, and the old copies are deleted the next time shims are installed or removed
//! (they cannot be while the program still runs).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::CrisolPaths;

/// Prefix of a shim that was moved aside because it was running.
const OLD_PREFIX: &str = ".crisol-shim-old-";

/// The commands that have a shim.
pub const SHIM_COMMANDS: [&str; 5] = ["php", "node", "npm", "npx", "composer"];

/// What [`install_shim_files`] did.
#[derive(Debug)]
pub struct ShimFiles {
    /// Shims created or brought up to date.
    pub written: Vec<PathBuf>,
    /// Shims that already were exactly this program.
    pub unchanged: Vec<PathBuf>,
    /// Shims that could not be written even after trying to move the old one aside, with why.
    /// The others are done regardless.
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
    remove_leftovers(&dir);
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
        let written = write_atomically(&dir, &target, &program)
            .or_else(|error| replace_running(&dir, &target, &program).map_err(|_| error));
        match written {
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

/// Moves the program at `target` (which cannot be overwritten, so it is probably running)
/// out of the way and puts `bytes` in its place. If the new file cannot be written, the old
/// one goes back.
fn replace_running(dir: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let aside = dir.join(format!("{OLD_PREFIX}{}-{nanos}", std::process::id()));
    fs::rename(target, &aside)?;
    if let Err(error) = write_atomically(dir, target, bytes) {
        let _ = fs::rename(&aside, target);
        return Err(error);
    }
    Ok(())
}

/// Deletes the shims moved aside by earlier installs. One that still runs cannot be deleted
/// and stays for the next time.
fn remove_leftovers(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(OLD_PREFIX) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Removes the shims, and the folder too if that leaves it empty. Returns the files removed.
/// Anything else in the folder stays.
pub fn remove_shim_files(paths: &CrisolPaths) -> io::Result<Vec<PathBuf>> {
    let dir = paths.shims_dir();
    let mut removed = Vec::new();
    remove_leftovers(&dir);
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

    /// A real program that stays alive for a while, named like a shim: `ping` waiting 30 s.
    #[cfg(windows)]
    fn running_shim(paths: &CrisolPaths) -> std::process::Child {
        let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap();
        let target = paths.shims_dir().join(shim_file_name("php"));
        fs::create_dir_all(paths.shims_dir()).unwrap();
        fs::copy(system.join("System32").join("PING.EXE"), &target).unwrap();
        std::process::Command::new(&target)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_shim_that_is_running_is_moved_aside_and_replaced() {
        // Review finding 5: a `php -S` left open must not block an update.
        let (_tmp, paths, exe) = setup();
        let mut running = running_shim(&paths);
        let php = paths.shims_dir().join(shim_file_name("php"));
        assert!(fs::write(&php, b"direct overwrite").is_err(), "the premise");

        let done = install_shim_files(&paths, &exe).unwrap();
        assert!(done.failed.is_empty(), "{:?}", done.failed);
        assert_eq!(done.written.len(), 5);
        assert_eq!(fs::read(&php).unwrap(), b"MZ the shim, version one");
        let leftovers = || {
            fs::read_dir(paths.shims_dir())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(OLD_PREFIX))
                .count()
        };
        assert_eq!(leftovers(), 1, "the running copy waits to be deleted");

        // Once it exits, the next install (or a removal) cleans it up.
        running.kill().unwrap();
        running.wait().unwrap();
        install_shim_files(&paths, &exe).unwrap();
        assert_eq!(leftovers(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn removing_cleans_up_what_an_earlier_update_left() {
        let (_tmp, paths, exe) = setup();
        let mut running = running_shim(&paths);
        install_shim_files(&paths, &exe).unwrap();
        running.kill().unwrap();
        running.wait().unwrap();
        remove_shim_files(&paths).unwrap();
        assert!(
            !paths.shims_dir().exists(),
            "nothing is left, folder included"
        );
    }

    #[test]
    fn a_missing_program_is_an_error_before_anything_is_written() {
        let (tmp, paths, _exe) = setup();
        assert!(install_shim_files(&paths, &tmp.path().join("nope.exe")).is_err());
        assert!(!paths.shims_dir().exists());
    }
}
