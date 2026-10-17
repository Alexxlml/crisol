//! Where `crisol-elevate.exe` may run from, and the check before the UAC prompt (ADR-0014,
//! review of phase 7, finding 1).
//!
//! The helper is the one program of Crisol that runs as an administrator, so it must be
//! something the user (and any other account) cannot replace without elevating. It lives in
//! `%ProgramFiles%\Crisol\`, put there by an installer that runs once as an administrator, and
//! it is **never** run from the Crisol home or from any other folder an ordinary account can
//! write to. The callers do not choose the path: [`HelperLocation::installed`] builds it from
//! the known-folder API. Before the prompt, [`HelperLocation::verify`] reads the permissions of
//! the program and of its folder with the stricter rule of
//! [`PrivateDirs::is_writable_by_non_administrators`](crisol_core::PrivateDirs); if anybody
//! but the administrators, `SYSTEM` or the system's installer can write there, or owns them, no
//! prompt is shown and the reason is explained.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crisol_core::dns::ElevatedOp;
use crisol_core::setup::{check as check_manifest, SetupOp, MANIFEST_FILE};
use crisol_core::PrivateDirs;

use crate::nrpt::{run_elevated, run_elevated_op};
use crate::private_dir::WindowsPrivateDirs;
use crate::system_paths::{helper_exe_in, program_files_directory};

/// The version of Crisol this program was built as. The helper is built from the same
/// workspace, so an installed helper of another version is one to install again.
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The most characters of arguments handed to the helper (Windows allows 32 767 for the whole
/// command line, and `ShellExecute` has to fit the program path in it too).
pub(crate) const MAX_COMMAND_LINE: usize = 30_000;

/// The helper's place and the means to judge it.
#[derive(Clone)]
pub struct HelperLocation {
    path: PathBuf,
    dirs: Arc<dyn PrivateDirs>,
}

impl fmt::Debug for HelperLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelperLocation")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl HelperLocation {
    /// `%ProgramFiles%\Crisol\crisol-elevate.exe`, judged with the real permissions.
    pub fn installed() -> io::Result<Self> {
        Ok(Self::at(
            helper_exe_in(&program_files_directory()?),
            Arc::new(WindowsPrivateDirs::new()),
        ))
    }

    /// Any path and any judge. For tests: production code only ever uses
    /// [`installed`](Self::installed).
    #[must_use]
    pub(crate) fn at(path: PathBuf, dirs: Arc<dyn PrivateDirs>) -> Self {
        Self { path, dirs }
    }

    /// The path of the helper.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the helper is there and nobody but the administrators can have changed it.
    pub fn verify(&self) -> io::Result<()> {
        if !self.path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "the privileged helper is not installed at {}; `crisol system install` puts it \
                     there (it needs an administrator, once)",
                    self.path.display()
                ),
            ));
        }
        let folder = self.path.parent().unwrap_or(&self.path);
        for item in [folder, self.path.as_path()] {
            if self.dirs.is_writable_by_non_administrators(item)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{} can be changed or owned by accounts that are not administrators, so \
                         Crisol will not run the helper with administrator rights. Reinstall it \
                         with the installer, or make sure only Administrators and SYSTEM can \
                         write to {}",
                        item.display(),
                        folder.display()
                    ),
                ));
            }
        }
        // Which program it is, without running it (running it would show the UAC prompt): the
        // manifest the installer wrote, in a folder only administrators can have written.
        let manifest = folder.join(MANIFEST_FILE);
        let text = match fs::read_to_string(&manifest) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        check_manifest(text.as_deref(), &fs::read(&self.path)?, HELPER_VERSION)
            .map_err(|problem| io::Error::new(io::ErrorKind::InvalidData, problem.to_string()))
    }

    /// Checks the helper, and only then shows the UAC prompt and runs a setup operation (its
    /// removal), which it must be the installed copy to do.
    pub fn run_setup(&self, op: &SetupOp) -> io::Result<u32> {
        self.verify()?;
        run_elevated(&self.path, &op.to_args())
    }

    /// Checks the helper, and only then shows the UAC prompt and runs `op`. An operation whose
    /// arguments would not fit on one command line is refused first, before anything is shown.
    pub fn run(&self, op: &ElevatedOp) -> io::Result<()> {
        let length: usize = op.to_args().iter().map(|a| a.len() + 1).sum();
        if length > MAX_COMMAND_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "this change names too many sites to hand to the helper in one go ({length} \
                     characters; the limit is {MAX_COMMAND_LINE}). Park fewer sites, or add the \
                     names to the hosts file by hand"
                ),
            ));
        }
        self.verify()?;
        run_elevated_op(&self.path, op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::paths::fake::FakePrivateDirs;
    use std::fs;
    use std::process::Command;

    fn installed_in_a_temp_folder() -> (tempfile::TempDir, PathBuf, Arc<FakePrivateDirs>) {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Crisol");
        fs::create_dir(&folder).unwrap();
        let exe = folder.join("crisol-elevate.exe");
        fs::write(&exe, PROGRAM).unwrap();
        write_manifest(&folder, HELPER_VERSION, PROGRAM);
        (dir, exe, Arc::new(FakePrivateDirs::new()))
    }

    const PROGRAM: &[u8] = b"not really a program";

    fn write_manifest(folder: &Path, version: &str, program: &[u8]) {
        let manifest = crisol_core::setup::Manifest::of(version, program);
        fs::write(folder.join(MANIFEST_FILE), manifest.to_text()).unwrap();
    }

    #[test]
    fn a_helper_in_a_folder_only_administrators_write_passes() {
        let (_dir, exe, dirs) = installed_in_a_temp_folder();
        HelperLocation::at(exe, dirs).verify().unwrap();
    }

    #[test]
    fn a_helper_the_current_user_can_write_is_refused_before_any_prompt() {
        let (_dir, exe, dirs) = installed_in_a_temp_folder();
        let folder = exe.parent().unwrap().to_path_buf();
        let location = HelperLocation::at(exe.clone(), dirs.clone());

        // The folder is the user's own (like the Crisol home): the stricter rule refuses it.
        dirs.make_user_writable(&folder);
        let error = location.verify().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let text = error.to_string();
        assert!(
            text.contains("not administrators") && text.contains("Reinstall"),
            "{text}"
        );
        assert!(!text.contains("  "), "{text}");
        dirs.close(&folder);

        // Only the program itself.
        dirs.make_user_writable(&exe);
        assert!(location.verify().is_err());
        dirs.close(&exe);

        // Open to everybody counts too, and `run` stops at the same place: no prompt, so the
        // error is the permission one, not whatever launching would have said.
        dirs.expose(&folder);
        let error = location.run(&ElevatedOp::HostsClear).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_missing_helper_is_said_to_be_missing() {
        let dir = tempfile::tempdir().unwrap();
        let location = HelperLocation::at(
            dir.path().join("Crisol").join("crisol-elevate.exe"),
            Arc::new(FakePrivateDirs::new()),
        );
        let error = location.verify().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            error.to_string().contains("crisol system install"),
            "{error}"
        );
    }

    #[test]
    fn the_installed_location_is_in_program_files() {
        let location = HelperLocation::installed().unwrap();
        assert!(
            location.path().ends_with(r"Crisol\crisol-elevate.exe"),
            "{:?}",
            location.path()
        );
        assert!(location
            .path()
            .starts_with(program_files_directory().unwrap()));
    }

    /// The real permissions: a folder made by a test is writable by the current user, so the
    /// real judge refuses a helper placed there (this is the Crisol home case). `icacls` is
    /// only run on the test's own temporary folder, to read.
    #[test]
    fn the_real_judge_refuses_a_helper_in_a_folder_the_user_made() {
        let (dir, exe, _) = installed_in_a_temp_folder();
        let location = HelperLocation::at(exe, Arc::new(WindowsPrivateDirs::new()));
        let error = location.verify().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        let _ = Command::new("icacls").arg(dir.path()).output();
    }

    /// The real `Program Files` folder is something a standard install protects; judged by the
    /// real rule, a stand-in file inside a folder of it is not created (that would need an
    /// administrator), but `Program Files` itself must pass as a place only administrators
    /// write. Read only.
    #[test]
    fn the_real_program_files_is_a_place_only_administrators_write() {
        let files = program_files_directory().unwrap();
        assert!(!WindowsPrivateDirs::new()
            .is_writable_by_non_administrators(&files)
            .unwrap());
    }

    // ---- review of phase 7, finding 5: too many names for one prompt -----------------------

    #[test]
    fn an_operation_that_does_not_fit_on_a_command_line_is_refused_before_anything_else() {
        // The helper is not even there: the length is judged first, so no prompt and no
        // permission check for something that could not be handed over anyway.
        let dir = tempfile::tempdir().unwrap();
        let location = HelperLocation::at(
            dir.path().join("missing.exe"),
            Arc::new(FakePrivateDirs::new()),
        );
        let names: Vec<String> = (0..2000)
            .map(|i| format!("site-number-{i:05}.test"))
            .collect();
        let error = location.run(&ElevatedOp::HostsSet { names }).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let text = error.to_string();
        assert!(
            text.contains("too many sites") && text.contains("30000"),
            "{text}"
        );
        assert!(!text.contains("  "), "{text}");

        // A hundred sites go through to the next check (here: the missing helper).
        let names: Vec<String> = (0..100).map(|i| format!("site-{i}.test")).collect();
        let error = location.run(&ElevatedOp::HostsSet { names }).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    // ---- review of phase 7, W1: which program it is, without running it ----------------------

    #[test]
    fn a_helper_without_a_manifest_or_with_a_different_program_or_version_is_refused() {
        let (_dir, exe, dirs) = installed_in_a_temp_folder();
        let folder = exe.parent().unwrap().to_path_buf();
        let location = HelperLocation::at(exe.clone(), dirs);
        location.verify().unwrap();

        // Swapped for another program: the hash no longer matches what the installer wrote.
        fs::write(&exe, b"something else entirely").unwrap();
        let error = location.verify().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("not the program that was installed"),
            "{error}"
        );
        fs::write(&exe, PROGRAM).unwrap();
        location.verify().unwrap();

        // Installed by another version of Crisol.
        write_manifest(&folder, "0.0.1", PROGRAM);
        assert!(location.verify().unwrap_err().to_string().contains("0.0.1"));
        write_manifest(&folder, HELPER_VERSION, PROGRAM);

        // No manifest, or one nobody can read.
        fs::remove_file(folder.join(MANIFEST_FILE)).unwrap();
        assert!(location
            .verify()
            .unwrap_err()
            .to_string()
            .contains("no manifest"));
        fs::write(folder.join(MANIFEST_FILE), "junk").unwrap();
        assert!(location
            .verify()
            .unwrap_err()
            .to_string()
            .contains("damaged"));
    }

    #[test]
    fn the_helper_is_never_run_while_it_does_not_verify() {
        // `run` and `run_setup` check first: with a swapped program neither gets as far as
        // showing a prompt, and the error says why.
        let (_dir, exe, dirs) = installed_in_a_temp_folder();
        let location = HelperLocation::at(exe.clone(), dirs);
        fs::write(&exe, b"swapped").unwrap();
        let error = location.run(&ElevatedOp::HostsClear).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
        let error = location
            .run_setup(&SetupOp::UninstallSelf {
                suffix: "test".to_owned(),
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
    }
}
