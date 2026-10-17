//! Installing and removing the privileged helper (F8.5, F8.6, ADR-0014).
//!
//! The helper is installed **once**, by itself, with administrator rights: `crisol system install`
//! runs `crisol-elevate.exe install-self <suffix>` through the UAC prompt, and that process
//!
//! 1. copies its own program into `%ProgramFiles%\Crisol\`, a folder that inherits the
//!    permissions of `Program Files` (only administrators, `SYSTEM` and the system's installer can
//!    write there) and which it checks again, strictly, before trusting it;
//! 2. writes the manifest next to it: its version and the SHA-256 of what it copied, so that
//!    later nobody has to run it to know which program it is ([`crisol_core::setup`]);
//! 3. makes the NRPT rule for the suffix, so that it takes one prompt, not two.
//!
//! Removal is the same in reverse (the NRPT rule if it is Crisol's, the block of `hosts`, then the
//! folder) and only runs from the installed copy.
//!
//! The pieces that do the work take their places as parameters ([`install_into`],
//! [`remove_system_changes`], [`remove_installation`]) so that tests run them on temporary
//! folders and doubles. What a test never does is raise the UAC prompt, touch the real
//! `Program Files`, `hosts` or the NRPT; those are tried by hand.
//!
//! **What this trusts.** The installing process copies the program it was started from. The
//! person who runs `crisol system install` is choosing to give administrator rights to the
//! `crisol-elevate.exe` that came with their `crisol.exe`, exactly as with any installer they
//! download. Two checks narrow that (review of phase 8, finding 3): before the UAC prompt, and
//! again inside the elevated process, neither the program nor its folder may be writable by
//! other accounts, and the program is read **once**: the bytes that are written to Program Files
//! are the bytes the manifest hashes. Everything after that (the helper is only ever run from
//! `Program Files`, after its hash and the permissions of its folder have been checked) does not
//! depend on the source any more. Signing the published binaries with Authenticode, and checking
//! the signature of the source before the prompt, is packaging work (`docs/plan.md`).

use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use crisol_core::dns::{disable, enable, DnsError, NrptPolicy, DEFAULT_DNS_ADDR};
use crisol_core::hosts::HostsManager;
use crisol_core::paths::PrivateDirs;
use crisol_core::programs::{require_trusted_program, ProgramError};
use crisol_core::setup::{
    Manifest, SetupFailure, SetupOp, SetupReport, SystemSetup, MANIFEST_FILE,
};
use std::os::windows::process::CommandExt;

use crate::helper_location::{HelperLocation, HELPER_VERSION};
use crate::nrpt::run_elevated;
use crate::system_paths::{
    helper_exe_in, program_files_directory, system_directory, HELPER_FOLDER,
};
use crate::{flush_dns, FileHosts, PowerShellNrpt, WindowsPrivateDirs};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Puts `source` in `program_files\Crisol\` as the helper and writes its manifest. Returns the
/// path of the installed program, verified.
///
/// The folder is created if it is missing (and then inherits the permissions of
/// `program_files`); if it exists, or after it is made, `dirs` must say that nobody but the
/// administrators can write it, or nothing is copied. A program that is already there is
/// replaced; the manifest is written last, so a half-finished install has none and verifies as
/// "install it again".
///
/// `source` and its folder must not be writable by other accounts (`PermissionDenied`, with a
/// [`ProgramError`] inside), and `source` is read once: those bytes are written and hashed.
pub fn install_into(
    program_files: &Path,
    source: &Path,
    dirs: Arc<dyn PrivateDirs>,
) -> io::Result<PathBuf> {
    require_trusted_program(source, dirs.as_ref()).map_err(program_io_error)?;
    let program = fs::read(source)?;
    let folder = program_files.join(HELPER_FOLDER);
    fs::create_dir_all(&folder)?;
    if dirs.is_writable_by_non_administrators(&folder)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} can be written by accounts that are not administrators, so the helper will \
                 not be installed in it",
                folder.display()
            ),
        ));
    }
    let destination = helper_exe_in(program_files);
    if source != destination {
        let staged = folder.join("crisol-elevate.exe.new");
        fs::write(&staged, &program)?;
        fs::rename(&staged, &destination)?;
    }
    // The hash is of the bytes read above, which are the ones written; `verify` below hashes the
    // file as it is on disk and so catches any difference.
    let manifest = Manifest::of(HELPER_VERSION, &program);
    let staged = folder.join(format!("{MANIFEST_FILE}.new"));
    fs::write(&staged, manifest.to_text())?;
    fs::rename(&staged, folder.join(MANIFEST_FILE))?;
    HelperLocation::at(destination.clone(), dirs).verify()?;
    Ok(destination)
}

/// A [`ProgramError`] as the I/O error `install_into` returns, keeping it inside.
fn program_io_error(error: ProgramError) -> io::Error {
    match error {
        ProgramError::Missing { .. } => io::Error::new(io::ErrorKind::NotFound, error),
        ProgramError::Exposed { .. } => io::Error::new(io::ErrorKind::PermissionDenied, error),
        ProgramError::Check { source, .. } => source,
    }
}

/// Undoes what the helper changed in the system: Crisol's NRPT rule for `suffix` and the
/// managed block of `hosts`. A rule that another tool made is left alone and said so. Each
/// step is attempted even if an earlier one failed, and the failures are returned with the
/// steps that worked.
pub fn remove_system_changes(
    nrpt: &dyn NrptPolicy,
    hosts: &dyn HostsManager,
    suffix: &str,
) -> (SetupReport, Vec<String>) {
    let mut report = SetupReport::default();
    let mut failures = Vec::new();
    match disable(nrpt, suffix) {
        Ok(true) => report
            .steps
            .push(format!("Removed the DNS rule for .{suffix}")),
        Ok(false) => report
            .steps
            .push(format!("There was no DNS rule of Crisol for .{suffix}")),
        Err(DnsError::Foreign { namespace, .. }) => report.steps.push(format!(
            "Left alone the DNS rule for {namespace}: it is not Crisol's"
        )),
        Err(error) => failures.push(error.to_string()),
    }
    match hosts.clear() {
        Ok(()) => report
            .steps
            .push("Took Crisol's block out of the hosts file".to_owned()),
        Err(error) => failures.push(error.to_string()),
    }
    (report, failures)
}

/// Deletes the files of the installation in `program_files\Crisol\` except `running` (the
/// program that is doing this, which Windows will not delete while it runs). Returns the
/// folder, if anything of it is left, so the caller can arrange for it to go afterwards.
pub fn remove_installation(
    program_files: &Path,
    running: Option<&Path>,
) -> io::Result<Option<PathBuf>> {
    let folder = program_files.join(HELPER_FOLDER);
    if !folder.is_dir() {
        return Ok(None);
    }
    for entry in fs::read_dir(&folder)? {
        let path = entry?.path();
        if running.is_some_and(|r| r == path) {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    if running.is_some_and(|r| r.starts_with(&folder)) {
        Ok(Some(folder))
    } else {
        fs::remove_dir(&folder)?;
        Ok(None)
    }
}

/// What `crisol-elevate install-self <suffix>` does, run elevated from the program to install.
pub fn install_self(suffix: &str) -> Result<SetupReport, SetupFailure> {
    let program_files = program_files_directory().map_err(|_| SetupFailure::Failed)?;
    let source = std::env::current_exe().map_err(|_| SetupFailure::Failed)?;
    let mut report = SetupReport::default();
    let destination = install_into(&program_files, &source, Arc::new(WindowsPrivateDirs::new()))
        .map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied
                if e.get_ref()
                    .is_some_and(<dyn std::error::Error + Send + Sync>::is::<ProgramError>) =>
            {
                SetupFailure::SourceOpen
            }
            io::ErrorKind::PermissionDenied => SetupFailure::Folder,
            io::ErrorKind::InvalidData => SetupFailure::Verify,
            _ => SetupFailure::Copy,
        })?;
    report
        .steps
        .push(format!("Installed the helper in {}", destination.display()));
    let server = IpAddr::V4(DEFAULT_DNS_ADDR);
    match enable(&PowerShellNrpt, suffix, server) {
        Ok(_) => report
            .steps
            .push(format!("Made the DNS rule for .{suffix}")),
        Err(DnsError::Foreign { .. }) => return Err(SetupFailure::ForeignRule),
        Err(_) => return Err(SetupFailure::Rule),
    }
    Ok(report)
}

/// What `crisol-elevate uninstall-self <suffix>` does: only from the installed copy.
pub fn uninstall_self(suffix: &str) -> Result<SetupReport, SetupFailure> {
    let program_files = program_files_directory().map_err(|_| SetupFailure::Failed)?;
    let running = std::env::current_exe().map_err(|_| SetupFailure::Failed)?;
    if !same_file(&running, &helper_exe_in(&program_files)) {
        return Err(SetupFailure::NotInstalledCopy);
    }
    let hosts = crisol_core::hosts::DirectHosts::new(
        FileHosts::system().map_err(|_| SetupFailure::Failed)?,
    );
    let (mut report, failures) = remove_system_changes(&PowerShellNrpt, &hosts, suffix);
    let _ = flush_dns();
    if !failures.is_empty() {
        // The rule or the block could not be taken out: keep the helper, which is what would
        // be needed to try again.
        return Err(SetupFailure::Rule);
    }
    match remove_installation(&program_files, Some(&running)) {
        Ok(Some(folder)) => {
            schedule_removal(&folder).map_err(|_| SetupFailure::Failed)?;
            report.steps.push(format!(
                "The helper and {} go away as soon as this program ends",
                folder.display()
            ));
        }
        Ok(None) => report.steps.push("Removed the helper".to_owned()),
        Err(_) => return Err(SetupFailure::Failed),
    }
    Ok(report)
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Deletes `folder` after this program has ended: a detached `cmd` waits a moment and removes
/// it. Every program it names is an absolute path in the system directory.
fn schedule_removal(folder: &Path) -> io::Result<()> {
    let system = system_directory()?;
    let ping = system.join("PING.EXE");
    let command = format!(
        "\"\"{}\" -n 3 127.0.0.1 >nul & rmdir /S /Q \"{}\"\"",
        ping.display(),
        folder.display()
    );
    Command::new(system.join("cmd.exe"))
        .arg("/C")
        .raw_arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map(drop)
}

/// Installs and removes the helper through the UAC prompt, from the program that is next to
/// this one.
#[derive(Clone)]
pub struct WindowsSetup {
    helper_source: PathBuf,
    dirs: Arc<dyn PrivateDirs>,
}

impl std::fmt::Debug for WindowsSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsSetup")
            .field("helper_source", &self.helper_source)
            .finish_non_exhaustive()
    }
}

impl WindowsSetup {
    /// With the `crisol-elevate.exe` that came with Crisol (the one `install` will copy).
    #[must_use]
    pub fn new(helper_source: impl Into<PathBuf>) -> Self {
        Self::with_dirs(helper_source, Arc::new(WindowsPrivateDirs::new()))
    }

    /// Like [`new`](Self::new), judging permissions with `dirs` (for tests).
    #[must_use]
    pub fn with_dirs(helper_source: impl Into<PathBuf>, dirs: Arc<dyn PrivateDirs>) -> Self {
        Self {
            helper_source: helper_source.into(),
            dirs,
        }
    }
}

fn failure(code: u32) -> io::Error {
    let (kind, message) = SetupFailure::from_code(code).map_or_else(
        || {
            (
                io::ErrorKind::Other,
                format!("the elevated step failed (exit code {code})"),
            )
        },
        |failure| (io::ErrorKind::Other, failure.message().to_owned()),
    );
    io::Error::new(kind, message)
}

impl SystemSetup for WindowsSetup {
    fn install(&self, suffix: &str) -> io::Result<SetupReport> {
        let op = SetupOp::InstallSelf {
            suffix: suffix.to_owned(),
        };
        // Validated here too, before a prompt is shown for something that will be refused.
        if let Some(Err(error)) = SetupOp::parse(&op.to_args()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                error.to_string(),
            ));
        }
        if !self.helper_source.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} was not found next to crisol.exe",
                    self.helper_source.display()
                ),
            ));
        }
        // The elevated process checks again; this is so the prompt is not shown for a program
        // that will be refused, and so the message has the command that closes the folder.
        require_trusted_program(&self.helper_source, self.dirs.as_ref())
            .map_err(program_io_error)?;
        match run_elevated(&self.helper_source, &op.to_args())? {
            0 => {}
            code => return Err(failure(code)),
        }
        HelperLocation::installed()?.verify()?;
        Ok(SetupReport {
            steps: vec![
                "Installed the privileged helper in Program Files".to_owned(),
                format!("Made the DNS rule for .{suffix}"),
            ],
        })
    }

    fn uninstall(&self, suffix: &str) -> io::Result<SetupReport> {
        let location = HelperLocation::installed()?;
        match location.run_setup(&SetupOp::UninstallSelf {
            suffix: suffix.to_owned(),
        })? {
            0 => Ok(SetupReport {
                steps: vec![
                    format!(
                        "Removed the DNS rule for .{suffix} and Crisol's block of the hosts file"
                    ),
                    "Removed the privileged helper".to_owned(),
                ],
            }),
            code => Err(failure(code)),
        }
    }
}

#[cfg(test)]
mod tests;
