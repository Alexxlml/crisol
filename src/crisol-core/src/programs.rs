//! The programs of Crisol itself that start on their own or are given more rights (review of
//! phase 8, finding 3).
//!
//! `crisold.exe` runs at every sign-in once autostart is on, and `crisol-elevate.exe` is run as
//! an administrator once, to be installed. If another account of the computer can replace either
//! (or the folder they are in), that account gets this user's rights, or an administrator's.
//! The home has had this rule since ADR-0008; this is the same rule for the programs: before
//! one is launched, registered or elevated, neither the file nor its folder may be writable by
//! other accounts. Nothing is ever changed: the error carries the command that closes them.

use std::io;
use std::path::{Path, PathBuf};

use crisol_protocol::{ErrorCode, RpcError};

use crate::paths::PrivateDirs;

/// Why a program of Crisol is not to be launched.
#[derive(Debug, thiserror::Error)]
pub enum ProgramError {
    /// The program is not there.
    #[error("`{}` was not found", path.display())]
    Missing {
        /// Where it was looked for.
        path: PathBuf,
    },
    /// The program, or the folder it is in, can be written by other accounts.
    #[error(
        "`{}` can be modified by other accounts of this computer, and Crisol would run \
         `{}` with your rights (or an administrator's), so it will not",
        path.display(), program.display()
    )]
    Exposed {
        /// The file or folder that is open.
        path: PathBuf,
        /// The program that was to be launched.
        program: PathBuf,
        /// What to do about it.
        fix: String,
    },
    /// The permissions could not be read.
    #[error("cannot check who can write to `{}`: {source}", path.display())]
    Check {
        /// What was being checked.
        path: PathBuf,
        /// What failed.
        source: io::Error,
    },
}

/// Refuses unless `program` exists and neither it nor its folder can be written by other
/// accounts. If the permissions cannot be read it refuses as well.
pub fn require_trusted_program(program: &Path, dirs: &dyn PrivateDirs) -> Result<(), ProgramError> {
    if !program.is_file() {
        return Err(ProgramError::Missing {
            path: program.to_path_buf(),
        });
    }
    // The folder first: it is what is named and fixed (with inheritance, so the program and
    // whatever else is beside it are closed at once). A DLL left next to the program would be
    // loaded by it, so closing only the file would leave the door open.
    let folder = program.parent().filter(|p| !p.as_os_str().is_empty());
    for path in folder.into_iter().chain(std::iter::once(program)) {
        let open = dirs
            .is_writable_by_others(path)
            .map_err(|source| ProgramError::Check {
                path: path.to_path_buf(),
                source,
            })?;
        if open {
            let fix = dirs.fix_command(path).unwrap_or_else(|| {
                format!(
                    "remove the write access of other accounts on `{}`",
                    path.display()
                )
            });
            return Err(ProgramError::Exposed {
                path: path.to_path_buf(),
                program: program.to_path_buf(),
                fix,
            });
        }
    }
    Ok(())
}

/// A [`ProgramError`] as an error a client can show: the sentence, and the command apart.
#[must_use]
pub fn program_error(error: &ProgramError) -> RpcError {
    match error {
        ProgramError::Exposed { path, program, fix } => RpcError::new(
            ErrorCode::HomeExposed,
            format!(
                "`{}` can be modified by other accounts of this computer, and Crisol would run `{}` with your rights (or an administrator's), so it will not",
                path.display(),
                program.display()
            ),
        )
        .with_fix(fix.clone()),
        ProgramError::Missing { .. } => RpcError::new(ErrorCode::NotAvailable, error.to_string()),
        ProgramError::Check { .. } => RpcError::new(ErrorCode::Io, error.to_string()),
    }
}

/// The winget package that installs Crisol (ADR-0024).
pub const WINGET_ID: &str = "Alexxlml.Crisol";

/// The folders winget unpacks packages in: the user's, and the one for all users.
/// `env` reads an environment variable.
#[must_use]
pub fn winget_package_folders(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let under = |variable: &str, rest: &[&str]| {
        env(variable).filter(|base| !base.is_empty()).map(|base| {
            rest.iter()
                .fold(PathBuf::from(base), |path, part| path.join(part))
        })
    };
    [
        under("LOCALAPPDATA", &["Microsoft", "WinGet", "Packages"]),
        under("ProgramFiles", &["WinGet", "Packages"]),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Whether `folder` is inside one of the folders winget keeps its packages in (`roots`): then
/// winget owns the programs, and deleting them by hand would leave it believing they are there.
/// Windows paths do not care about case or the kind of slash, and neither does this.
#[must_use]
pub fn is_winget_package(folder: &Path, roots: &[PathBuf]) -> bool {
    let normal = |path: &Path| {
        let mut text = path
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        while text.ends_with('\\') {
            text.pop();
        }
        text
    };
    let folder = normal(folder);
    roots.iter().any(|root| {
        let root = normal(root);
        !root.is_empty() && folder.starts_with(&format!("{root}\\"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::fake::FakePrivateDirs;

    fn program() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("bin");
        std::fs::create_dir(&folder).unwrap();
        let exe = folder.join("crisold.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        (dir, exe)
    }

    #[test]
    fn a_program_in_a_private_folder_is_trusted() {
        let (_dir, exe) = program();
        require_trusted_program(&exe, &FakePrivateDirs::new()).unwrap();
    }

    #[test]
    fn an_open_folder_or_an_open_file_is_refused_with_the_fix() {
        let (_dir, exe) = program();
        let dirs = FakePrivateDirs::new();
        dirs.expose(exe.parent().unwrap());
        let error = require_trusted_program(&exe, &dirs).unwrap_err();
        let ProgramError::Exposed { path, fix, .. } = &error else {
            panic!("{error:?}")
        };
        // The folder is named first, with the fix that inherits.
        assert_eq!(path, exe.parent().unwrap());
        assert!(
            fix.starts_with("icacls ") && fix.contains("(OI)(CI)"),
            "{fix}"
        );
        assert!(
            fix.contains(&exe.parent().unwrap().display().to_string()),
            "{fix}"
        );
        // One line for the message, the command apart.
        assert!(!error.to_string().contains("icacls"), "{error}");
        let rpc = program_error(&error);
        assert_eq!(rpc.fix.as_deref(), Some(fix.as_str()));
        assert!(!rpc.message.contains('\n'), "{rpc:?}");
        // Open only the file: still refused.
        let (_dir, exe) = program();
        let dirs = FakePrivateDirs::new();
        dirs.expose(&exe);
        assert!(matches!(
            require_trusted_program(&exe, &dirs),
            Err(ProgramError::Exposed { .. })
        ));
        let rpc = program_error(&ProgramError::Exposed {
            path: exe.clone(),
            program: exe,
            fix: "icacls x".to_owned(),
        });
        assert_eq!(rpc.code, ErrorCode::HomeExposed);
        assert_eq!(rpc.fix.as_deref(), Some("icacls x"));
    }

    #[test]
    fn a_folder_inside_wingets_packages_is_wingets_whatever_the_case_or_the_slashes() {
        let roots = winget_package_folders(&|name| match name {
            "LOCALAPPDATA" => Some(r"C:\Users\Ana\AppData\Local".to_owned()),
            "ProgramFiles" => Some(r"C:\Program Files".to_owned()),
            _ => None,
        });
        assert_eq!(roots.len(), 2);
        for inside in [
            r"C:\Users\Ana\AppData\Local\Microsoft\WinGet\Packages\Alexxlml.Crisol_Microsoft.Winget.Source_8wekyb3d8bbwe",
            r"c:/users/ana/appdata/local/microsoft/winget/packages/Alexxlml.Crisol",
            r"C:\Program Files\WinGet\Packages\Alexxlml.Crisol\",
        ] {
            assert!(is_winget_package(Path::new(inside), &roots), "{inside}");
        }
        for outside in [
            r"C:\Users\Ana\AppData\Local\Microsoft\WinGet\Links",
            r"C:\Users\Ana\AppData\Local\Microsoft\WinGet\Packages",
            r"C:\Users\Ana\AppData\Local\Microsoft\WinGet\PackagesX\Crisol",
            r"C:\Users\Ana\Crisol",
            r"D:\Microsoft\WinGet\Packages\Crisol",
        ] {
            assert!(!is_winget_package(Path::new(outside), &roots), "{outside}");
        }
        // Without the variables there is nothing to compare with: never "winget".
        assert_eq!(winget_package_folders(&|_| None), Vec::<PathBuf>::new());
        assert!(!is_winget_package(Path::new(r"C:\anything"), &[]));
    }

    #[test]
    fn a_missing_program_is_said_to_be_missing() {
        let (dir, _) = program();
        assert!(matches!(
            require_trusted_program(&dir.path().join("nope.exe"), &FakePrivateDirs::new()),
            Err(ProgramError::Missing { .. })
        ));
    }
}
