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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crisol_core::dns::ElevatedOp;
use crisol_core::PrivateDirs;

use crate::nrpt::run_elevated_op;
use crate::private_dir::WindowsPrivateDirs;
use crate::system_paths::{helper_exe_in, program_files_directory};

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
                    "the privileged helper is not installed at {}; Crisol's installer puts it \
                     there (it needs an administrator)",
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
        Ok(())
    }

    /// Checks the helper, and only then shows the UAC prompt and runs `op`.
    pub fn run(&self, op: &ElevatedOp) -> io::Result<()> {
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
        fs::write(&exe, b"not really a program").unwrap();
        (dir, exe, Arc::new(FakePrivateDirs::new()))
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
        assert!(error.to_string().contains("installer"), "{error}");
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
}
