//! Finding and starting the terminals of `crisol shell` (ADR-0019). What the terminal gets
//! (arguments, `PATH`, variables, profile) is decided in `crisol_core::shell`.

use std::io;
use std::path::PathBuf;
use std::process::Command;

use crisol_core::shell::{ShellLaunch, Terminal};

use crate::system_paths::{powershell_exe_in, program_files_directory, system_directory};

/// The program of `terminal` on this computer. `parent` is the program that started `crisol`:
/// a Git Bash it names is the one opened again.
#[must_use]
pub fn find_terminal(terminal: Terminal, parent: Option<&str>) -> Option<PathBuf> {
    let found = match terminal {
        Terminal::Cmd => system_directory().ok().map(|s| s.join("cmd.exe")),
        Terminal::WindowsPowerShell => system_directory().ok().map(|s| powershell_exe_in(&s)),
        // Installed apart: from the Microsoft Store (an alias in the PATH) or the MSI.
        Terminal::Pwsh => in_path("pwsh.exe").or_else(|| {
            program_files_directory()
                .ok()
                .map(|p| p.join("PowerShell").join("7").join("pwsh.exe"))
        }),
        Terminal::Bash => parent
            .filter(|p| Terminal::from_program(p) == Some(Terminal::Bash))
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(git_bash),
    };
    found.filter(|p| p.is_file())
}

/// `name` in a folder of the `PATH` of this process.
fn in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// The `bin\bash.exe` of the Git for Windows that gives the first `git.exe` of the `PATH`
/// (`C:\Program Files\Git\cmd\git.exe` → `C:\Program Files\Git\bin\bash.exe`). Never
/// `git-bash.exe`, which opens another window.
fn git_bash() -> Option<PathBuf> {
    let git = in_path("git.exe")?;
    git.ancestors().skip(1).find_map(|dir| {
        let bash = dir.join("bin").join("bash.exe");
        (bash.is_file() && dir.join("usr").join("bin").is_dir()).then_some(bash)
    })
}

/// Starts the terminal in this window and waits for it. Gives its exit code.
pub fn run_terminal(launch: &ShellLaunch) -> io::Result<i32> {
    let mut command = Command::new(&launch.program);
    command.args(&launch.args);
    for name in &launch.remove {
        command.env_remove(name);
    }
    command.envs(launch.set.iter().map(|(n, v)| (n, v)));
    // Ctrl+C in the terminal reaches `crisol` too, which must stay to wait for it.
    crate::survive_ctrl_c()?;
    let status = command.status()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::*;

    /// Whether `path` names a terminal program that exists.
    fn is_terminal(path: &Path) -> bool {
        path.is_file() && path.to_str().and_then(Terminal::from_program).is_some()
    }

    #[test]
    fn cmd_and_windows_powershell_are_always_there() {
        for terminal in [Terminal::Cmd, Terminal::WindowsPowerShell] {
            let found = find_terminal(terminal, None).unwrap();
            assert!(is_terminal(&found), "{found:?}");
        }
    }

    #[test]
    fn a_bash_that_started_crisol_is_the_one_opened() {
        let cmd = find_terminal(Terminal::Cmd, None).unwrap();
        // A parent that is not a bash says nothing about where Git Bash is.
        let from_cmd = find_terminal(Terminal::Bash, cmd.to_str());
        assert_ne!(from_cmd.as_deref(), Some(cmd.as_path()));
        if let Some(bash) = from_cmd {
            assert_eq!(find_terminal(Terminal::Bash, bash.to_str()), Some(bash));
        }
    }

    #[test]
    fn the_terminal_runs_with_the_variables_set_and_its_exit_code_comes_back() {
        let launch = ShellLaunch {
            program: find_terminal(Terminal::Cmd, None).unwrap(),
            args: [
                "/d",
                "/c",
                "if \"%CRISOL_SHELL_TEST%\"==\"yes\" (exit 7) else (exit 3)",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
            set: vec![("CRISOL_SHELL_TEST".into(), "yes".into())],
            remove: vec!["CRISOL_SHELL_NEVER_SET".into()],
        };
        assert_eq!(run_terminal(&launch).unwrap(), 7);
    }
}
