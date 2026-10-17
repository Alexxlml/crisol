//! The commands that put the shims in place: `crisol shims install|remove|status`, `crisol path
//! add|remove|status` and `crisol env` (F10.2).
//!
//! Everything here happens only because the user asked for it with a command. The pieces are the
//! ones of F4.4 ([`crate::shims`] for the files and [`crate::userpath`] for the user `PATH`);
//! this module adds the decisions around them: the program that is copied must not be one other
//! accounts could have replaced, and what to tell the user when something else in the `PATH`
//! wins over the shims (the Node of the machine, say).

use std::io;
use std::path::Path;

use crate::paths::{CrisolPaths, PrivateDirs};
use crate::programs::{require_trusted_program, ProgramError};
use crate::shims::{install_shim_files, remove_shim_files, shim_file_name, SHIM_COMMANDS};
use crate::userpath::{
    add_shims, remove_shims, shadowing, shims_in_path, PathChange, Shadow, UserPath,
};

/// Why the shims could not be installed.
#[derive(Debug, thiserror::Error)]
pub enum ShimsError {
    /// The shim program is not one that can be trusted (missing, or open to other accounts).
    #[error(transparent)]
    Program(#[from] ProgramError),
    /// A file could not be written.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What `crisol shims install` did, in file names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShimsInstalled {
    /// Shims created or brought up to date.
    pub written: Vec<String>,
    /// Shims that already were this program.
    pub unchanged: Vec<String>,
    /// Shims that could not be written, with why.
    pub failed: Vec<(String, String)>,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Copies `shim_exe` into `shims/` once for every command. Refuses a program or a folder that
/// other accounts can write to: whoever replaced it would be running in every terminal.
pub fn install_shims(
    paths: &CrisolPaths,
    shim_exe: &Path,
    dirs: &dyn PrivateDirs,
) -> Result<ShimsInstalled, ShimsError> {
    require_trusted_program(shim_exe, dirs)?;
    let done = install_shim_files(paths, shim_exe, dirs)?;
    Ok(ShimsInstalled {
        written: done.written.iter().map(|p| file_name(p)).collect(),
        unchanged: done.unchanged.iter().map(|p| file_name(p)).collect(),
        failed: done
            .failed
            .iter()
            .map(|(p, e)| (file_name(p), e.to_string()))
            .collect(),
    })
}

/// Removes the shims (and the folder, if that leaves it empty). Returns the file names.
pub fn remove_shims_files(paths: &CrisolPaths) -> io::Result<Vec<String>> {
    Ok(remove_shim_files(paths)?
        .iter()
        .map(|p| file_name(p))
        .collect())
}

/// Which shims are in `shims/`: `(command, present)`.
#[must_use]
pub fn shims_present(paths: &CrisolPaths) -> Vec<(&'static str, bool)> {
    let dir = paths.shims_dir();
    SHIM_COMMANDS
        .iter()
        .map(|c| (*c, dir.join(shim_file_name(c)).is_file()))
        .collect()
}

/// Puts `shims/` first in the user `PATH`.
pub fn path_add(store: &dyn UserPath, paths: &CrisolPaths) -> io::Result<PathChange> {
    add_shims(store, &paths.shims_dir())
}

/// Takes `shims/` out of the user `PATH` and nothing else.
pub fn path_remove(store: &dyn UserPath, paths: &CrisolPaths) -> io::Result<PathChange> {
    remove_shims(store, &paths.shims_dir())
}

/// Where the shims stand in the user `PATH`, and what wins over them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStatus {
    /// `shims/` is in the user `PATH`.
    pub in_path: bool,
    /// The programs that something else in the `PATH` would run instead of the shims.
    pub shadows: Vec<Shadow>,
}

/// Looks at the user `PATH` and the machine one. Read only.
pub fn path_status(
    store: &dyn UserPath,
    paths: &CrisolPaths,
    exists: &dyn Fn(&Path) -> bool,
) -> io::Result<PathStatus> {
    let shims = paths.shims_dir();
    Ok(PathStatus {
        in_path: shims_in_path(store, &shims)?,
        shadows: shadowing(store, &shims, exists)?,
    })
}

/// One sentence per program that wins over the shims: what runs, why Crisol cannot change it,
/// and the way out.
#[must_use]
pub fn describe_shadows(shadows: &[Shadow]) -> Vec<String> {
    shadows
        .iter()
        .map(|s| {
            let who = s
                .suite
                .map_or_else(String::new, |suite| format!(" ({suite})"));
            if s.in_system_path {
                format!(
                    "`{}` in a terminal runs {}{who}, not Crisol's: that folder is in the machine PATH, which Windows searches before your \
                     user PATH, and Crisol never changes the machine PATH. A terminal where Crisol's shims come first: `crisol env` \
                     (PowerShell: `crisol env | Invoke-Expression`)",
                    s.program,
                    s.found.display()
                )
            } else {
                format!(
                    "`{}` in a terminal runs {}{who}, not Crisol's: it comes earlier in your user PATH. `crisol path add` puts Crisol's \
                     shims first",
                    s.program,
                    s.found.display()
                )
            }
        })
        .collect()
}

/// The shells `crisol env` can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// Windows PowerShell and PowerShell 7.
    PowerShell,
    /// `cmd.exe`.
    Cmd,
    /// Git Bash and other POSIX shells.
    Bash,
}

/// The shell the user is probably in, from its environment: `SHELL` is set by POSIX shells,
/// `PROMPT` by `cmd.exe` (PowerShell does not define it), and the rest is PowerShell.
#[must_use]
pub fn detect_shell(lookup: &dyn Fn(&str) -> Option<String>) -> Shell {
    if lookup("SHELL").is_some_and(|s| !s.is_empty()) {
        Shell::Bash
    } else if lookup("PROMPT").is_some_and(|s| !s.is_empty()) {
        Shell::Cmd
    } else {
        Shell::PowerShell
    }
}

/// `D:\a\b` as `/d/a/b`, the way Git Bash writes it.
fn posix_path(dir: &Path) -> String {
    let text = dir.to_string_lossy().replace('\\', "/");
    match text.as_bytes() {
        [drive, b':', b'/', ..] if drive.is_ascii_alphabetic() => {
            format!("/{}{}", drive.to_ascii_lowercase() as char, &text[2..])
        }
        _ => text,
    }
}

/// The lines that put `shims/` first in the `PATH` **of that terminal only** (nothing is
/// written anywhere), safe to run twice.
#[must_use]
pub fn env_script(shims: &Path, shell: Shell) -> String {
    let dir = shims.to_string_lossy();
    match shell {
        Shell::PowerShell => {
            let quoted = dir.replace('\'', "''");
            format!(
                "if (($env:Path -split ';')[0] -ne '{quoted}') {{ $env:Path = '{quoted};' + $env:Path }}\n"
            )
        }
        Shell::Cmd => format!("set \"PATH={dir};%PATH%\"\n"),
        Shell::Bash => {
            let posix = posix_path(shims).replace('\'', "'\\''");
            format!(
                "case \":$PATH:\" in ':{posix}:'*) ;; *) export PATH='{posix}':\"$PATH\" ;; esac\n"
            )
        }
    }
}

#[cfg(test)]
mod tests;
