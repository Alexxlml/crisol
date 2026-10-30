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

use crate::globals::{shimmed_globals, sync_global_shims, GlobalSync};
use crate::paths::{CrisolPaths, PrivateDirs};
use crate::programs::{require_trusted_program, ProgramError};
use crate::shell::powershell_quote;
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
    /// The commands of global programs (`tsc`) that got a shim now.
    pub globals_added: Vec<String>,
    /// The commands of global programs whose shim was deleted because the program is gone.
    pub globals_removed: Vec<String>,
    /// Commands of global programs left alone, with why.
    pub globals_skipped: Vec<(String, String)>,
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
    reserved: &dyn Fn(&str) -> bool,
) -> Result<ShimsInstalled, ShimsError> {
    require_trusted_program(shim_exe, dirs)?;
    let done = install_shim_files(paths, shim_exe, dirs)?;
    // The shims of the global programs are copies of the same program: they follow it.
    let globals = sync_global_shims(paths, shim_exe, reserved)?;
    Ok(ShimsInstalled {
        globals_added: globals.added,
        globals_removed: globals.removed,
        globals_skipped: globals.skipped,
        written: done.written.iter().map(|p| file_name(p)).collect(),
        unchanged: done.unchanged.iter().map(|p| file_name(p)).collect(),
        failed: done
            .failed
            .iter()
            .map(|(p, e)| (file_name(p), e.to_string()))
            .collect(),
    })
}

/// Makes the commands of the global programs match what the Node versions have, with the same
/// distrust of the program as [`install_shims`].
pub fn sync_globals(
    paths: &CrisolPaths,
    shim_exe: &Path,
    dirs: &dyn PrivateDirs,
    reserved: &dyn Fn(&str) -> bool,
) -> Result<GlobalSync, ShimsError> {
    require_trusted_program(shim_exe, dirs)?;
    Ok(sync_global_shims(paths, shim_exe, reserved)?)
}

/// Removes the shims (and the folder, if that leaves it empty). Returns the file names.
pub fn remove_shims_files(paths: &CrisolPaths) -> io::Result<Vec<String>> {
    Ok(remove_shim_files(paths)?
        .iter()
        .map(|p| file_name(p))
        .collect())
}

/// The commands of global programs that have a shim (`tsc`).
#[must_use]
pub fn global_shims(paths: &CrisolPaths) -> Vec<String> {
    shimmed_globals(paths)
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

/// The line that puts the shims first in a PowerShell. It names the shell: `crisol env` alone
/// guesses it, and a wrong guess prints a line PowerShell runs without an error and without
/// effect.
pub const ENV_POWERSHELL: &str = "crisol env --shell powershell | Invoke-Expression";
/// The same for Git Bash.
pub const ENV_BASH: &str = "eval \"$(crisol env --shell bash)\"";

/// One sentence per folder that wins over the shims: which commands run from it, why Crisol
/// cannot change that, and the way out.
#[must_use]
pub fn describe_shadows(shadows: &[Shadow]) -> Vec<String> {
    let mut groups: Vec<(String, bool, Option<&'static str>, Vec<&str>)> = Vec::new();
    for shadow in shadows {
        let folder = shadow
            .found
            .parent()
            .map_or_else(String::new, |p| p.display().to_string());
        match groups
            .iter_mut()
            .find(|(f, machine, _, _)| *f == folder && *machine == shadow.in_system_path)
        {
            Some((_, _, _, programs)) => programs.push(shadow.program),
            None => groups.push((
                folder,
                shadow.in_system_path,
                shadow.suite,
                vec![shadow.program],
            )),
        }
    }
    groups
        .into_iter()
        .map(|(folder, machine, suite, programs)| {
            let quoted: Vec<String> = programs.iter().map(|p| format!("`{p}`")).collect();
            let (names, verb, which) = match quoted.as_slice() {
                [one] => (one.clone(), "runs", "the one"),
                [init @ .., last] => (format!("{} and {last}", init.join(", ")), "run", "the ones"),
                [] => (String::new(), "run", "the ones"),
            };
            let who = suite.map_or_else(String::new, |suite| format!(" ({suite})"));
            if machine {
                format!(
                    "{names} in a terminal {verb} {which} in {folder}{who}, not Crisol's: that folder is in the machine PATH, which \
                     Windows searches before your user PATH, and Crisol never changes the machine PATH. A terminal where Crisol's \
                     shims come first: `{ENV_POWERSHELL}` in PowerShell, `{ENV_BASH}` in Git Bash"
                )
            } else {
                format!(
                    "{names} in a terminal {verb} {which} in {folder}{who}, not Crisol's: it comes earlier in your user PATH. \
                     `crisol path add` puts Crisol's shims first"
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

impl Shell {
    /// The shell a program is, from its file name (`pwsh.exe`, `C:\…\cmd.exe`, `bash`).
    #[must_use]
    pub fn from_program(program: &str) -> Option<Self> {
        let name = Path::new(program)
            .file_name()?
            .to_str()?
            .to_ascii_lowercase();
        let stem = name.strip_suffix(".exe").unwrap_or(&name);
        match stem {
            "pwsh" | "powershell" | "powershell_ise" => Some(Self::PowerShell),
            "cmd" => Some(Self::Cmd),
            "bash" | "sh" | "dash" | "zsh" | "ksh" => Some(Self::Bash),
            _ => None,
        }
    }
}

/// The shell the user is probably in. The program that started this one (`parent`) says it
/// best. Without it, the variables shells leave: `SHELL` is set by POSIX shells, `PROMPT` by
/// `cmd.exe`, and the rest is PowerShell. Those are inherited, though: a PowerShell opened
/// from `cmd` has `PROMPT`, and every terminal of a VS Code started from Git Bash has `SHELL`.
#[must_use]
pub fn detect_shell(parent: Option<&str>, lookup: &dyn Fn(&str) -> Option<String>) -> Shell {
    if let Some(shell) = parent.and_then(Shell::from_program) {
        return shell;
    }
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
/// written anywhere). Run twice, the PowerShell and Bash ones change nothing the second time;
/// the `cmd` one puts the folder in front once more, which is harmless.
#[must_use]
pub fn env_script(shims: &Path, shell: Shell) -> String {
    let dir = shims.to_string_lossy();
    match shell {
        Shell::PowerShell => {
            let quoted = powershell_quote(&dir);
            let in_front = powershell_quote(&format!("{dir};"));
            format!(
                "if (($env:Path -split ';')[0] -ne {quoted}) {{ $env:Path = {in_front} + $env:Path }}\n"
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
