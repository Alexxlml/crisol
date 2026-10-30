//! The real `hosts` file (F7.3, ADR-0014): reading it, rewriting it safely, and the elevated way
//! to change its Crisol block.
//!
//! The file is rewritten **in place**, keeping the file itself (its owner, its permissions and
//! its attributes stay exactly as they were). A replacement through a temporary file and
//! `ReplaceFileW` was tried first and the hand test of 2026-10-16 showed that, run elevated, it
//! changes the owner of `hosts` (`SYSTEM` becomes `Administrators`) and adds an explicit entry
//! to its permissions. What protects the content instead:
//!
//! 1. before every change the current file is copied to `hosts.crisol-bak.new` (what the file is
//!    right now), and the good copy of the change before, `hosts.crisol-bak`, is **not**
//!    touched yet;
//! 2. the new content is written with a single `write_all` and `sync_all`;
//! 3. the file is read back and compared with what was written. If it is right, the new copy
//!    takes the place of `hosts.crisol-bak`. If it differs (a scanner, a full disk), the new
//!    copy is written back and removed, and the change is reported as failed;
//! 4. if even that restoration fails, or the process dies in the middle, `hosts.crisol-bak.new`
//!    stays: it is how the file was before the change. Nothing else is done to `hosts` until a
//!    person has looked at it and deleted (or put back) that copy, so a damaged file never
//!    replaces a good copy and nothing is rewritten on top of an unfinished change.
//!
//! The copy is deleted when the block goes away.

use std::fs;
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crisol_core::dns::ElevatedOp;
use crisol_core::hosts::{
    apply_block, block_names, clean_names, Conflict, HostsFile, HostsManager, HostsManagerError,
    BEGIN,
};

use crate::helper_location::HelperLocation;
use crate::system_paths::{hosts_file_in, ipconfig_exe_in, system_directory};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BACKUP_EXTENSION: &str = "crisol-bak";
const PENDING_EXTENSION: &str = "crisol-bak.new";
/// `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION`: somebody (often a scanner) has the
/// file open for a moment.
const BUSY: [i32; 2] = [32, 33];
const ATTEMPTS: u32 = 5;

/// A `hosts` file at some path.
#[derive(Debug, Clone)]
pub struct FileHosts {
    path: PathBuf,
    /// Test hook: damages the file right after it is replaced, as a scanner could.
    #[cfg(test)]
    tamper: Option<fn(&std::path::Path)>,
}

impl FileHosts {
    /// The file at this path (it may not exist yet; that reads as empty).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            #[cfg(test)]
            tamper: None,
        }
    }

    /// The machine's `hosts`, found through the system directory the operating system
    /// reports, never through `%SystemRoot%`.
    pub fn system() -> io::Result<Self> {
        Ok(Self::new(hosts_file_in(&system_directory()?)))
    }

    /// The path of the file.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Where the previous version goes.
    #[must_use]
    pub fn backup_path(&self) -> PathBuf {
        self.sibling(BACKUP_EXTENSION)
    }

    /// The copy of how the file was before a change that did not finish, if there is one. While
    /// it exists the file is not touched: see the module documentation.
    #[must_use]
    pub fn pending_path(&self) -> PathBuf {
        self.sibling(PENDING_EXTENSION)
    }

    /// An error telling that an earlier change did not finish, if one did not.
    pub fn check_no_unfinished_change(&self) -> io::Result<()> {
        let pending = self.pending_path();
        if pending.exists() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "an earlier change to the hosts file did not finish; how it was before is in `{}`. \
                     Look at the hosts file, then put that copy back or delete it, and try again",
                    pending.display()
                ),
            ));
        }
        Ok(())
    }

    fn sibling(&self, extension: &str) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".");
        name.push(extension);
        self.path.with_file_name(name)
    }

    /// Overwrites the existing file with `bytes`, in place, trying again a few times if
    /// another program has it open for a moment.
    fn overwrite(&self, bytes: &[u8]) -> io::Result<()> {
        let mut attempt = 1;
        loop {
            let opened = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&self.path);
            match opened {
                Ok(mut file) => {
                    return file.write_all(bytes).and_then(|()| file.sync_all());
                }
                Err(e) => {
                    let busy = e.raw_os_error().is_some_and(|code| BUSY.contains(&code));
                    if !busy || attempt >= ATTEMPTS {
                        return Err(e);
                    }
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

impl HostsFile for FileHosts {
    fn read(&self) -> io::Result<String> {
        match fs::read(&self.path) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the hosts file is not UTF-8; Crisol will not rewrite it",
                )
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    fn write(&self, content: &str) -> io::Result<()> {
        let backup = self.backup_path();
        if self.path.exists() {
            self.check_no_unfinished_change()?;
            let pending = self.pending_path();
            fs::copy(&self.path, &pending)?; // what the file is right now, before this change
            let written = self.overwrite(content.as_bytes());
            #[cfg(test)]
            if let Some(tamper) = self.tamper {
                tamper(&self.path);
            }
            // Read back what is there now: a scanner or a full disk can make it different.
            if written.is_ok() && self.read().ok().as_deref() == Some(content) {
                fs::rename(&pending, &backup)?; // the new copy replaces the old one only now
            } else {
                return Err(self.undo(&pending, written.err()));
            }
        } else {
            fs::write(&self.path, content)?; // a new file has no permissions to keep
        }
        if !content.contains(BEGIN) {
            match fs::remove_file(&backup) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

impl FileHosts {
    /// A change failed: put the file back as `pending` has it (unless it is still that) and
    /// say what happened. `pending` is removed only when the file is known to be right; if it
    /// cannot be, it stays, and the message says so.
    fn undo(&self, pending: &Path, cause: Option<io::Error>) -> io::Error {
        let before = fs::read(pending);
        let untouched = before
            .as_ref()
            .is_ok_and(|bytes| fs::read(&self.path).ok().as_ref() == Some(bytes));
        let restored = untouched
            || before
                .as_ref()
                .map_err(|e| io::Error::new(e.kind(), e.to_string()))
                .and_then(|bytes| self.overwrite(bytes))
                .is_ok();
        if restored {
            let _ = fs::remove_file(pending);
            return cause.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the hosts file did not hold what was written; the previous version was restored",
                )
            });
        }
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the hosts file did not hold what was written and could not be put back; how it was is in `{}`",
                pending.display()
            ),
        )
    }
}

/// Empties the DNS client cache so that a change to `hosts` is seen at once.
pub fn flush_dns() -> io::Result<()> {
    let status = Command::new(ipconfig_exe_in(&system_directory()?))
        .arg("/flushdns")
        .creation_flags(CREATE_NO_WINDOW)
        .output()?
        .status;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "ipconfig /flushdns failed ({status})"
        )))
    }
}

/// Reads the real file directly and changes its block through the elevated helper, one UAC
/// prompt per change.
#[derive(Debug, Clone)]
pub struct ElevatedHosts {
    helper: HelperLocation,
    file: FileHosts,
}

impl ElevatedHosts {
    /// Over the machine's `hosts`, with the helper at its fixed place
    /// (`%ProgramFiles%\Crisol\crisol-elevate.exe`).
    pub fn new() -> io::Result<Self> {
        Ok(Self::over(
            HelperLocation::installed()?,
            FileHosts::system()?,
        ))
    }

    /// Reading another file, with the helper at another place (still checked before every
    /// prompt; the helper edits the machine's own `hosts`). For tests.
    #[must_use]
    pub(crate) fn over(helper: HelperLocation, file: FileHosts) -> Self {
        Self { helper, file }
    }

    fn run(&self, op: &ElevatedOp) -> Result<(), HostsManagerError> {
        Ok(self.helper.run(op)?)
    }
}

impl HostsManager for ElevatedHosts {
    fn names(&self) -> Result<Vec<String>, HostsManagerError> {
        Ok(block_names(&self.file.read()?)?)
    }

    fn set(&self, names: &[String]) -> Result<Vec<Conflict>, HostsManagerError> {
        let names = clean_names(names)?;
        let current = self.file.read()?;
        // Checked here first: a bad name or a damaged file never reaches the prompt.
        let update = apply_block(&current, &names)?;
        if update.content != current {
            self.file.check_no_unfinished_change()?;
            if names.is_empty() {
                self.run(&ElevatedOp::HostsClear)?;
            } else {
                self.run(&ElevatedOp::HostsSet { names })?;
            }
        }
        Ok(update.conflicts)
    }

    fn clear(&self) -> Result<(), HostsManagerError> {
        let current = self.file.read()?;
        if block_names(&current)?.is_empty() && !current.contains(BEGIN) {
            return Ok(());
        }
        self.file.check_no_unfinished_change()?;
        self.run(&ElevatedOp::HostsClear)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::hosts::DirectHosts;
    use std::os::windows::fs::MetadataExt;
    use std::path::Path;
    use tempfile::TempDir;

    const HIDDEN: u32 = 0x2;

    const ORIGINAL: &[u8] =
        b"# Copyright\r\n127.0.0.1 localhost\r\n# <HERD>\r\n127.0.0.1 old.test\r\n# </HERD>\r\n";

    fn copy() -> (TempDir, FileHosts) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        fs::write(&path, ORIGINAL).unwrap();
        (dir, FileHosts::new(path))
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_copy_goes_through_set_and_clear_and_comes_back_byte_for_byte() {
        let (dir, file) = copy();
        let hosts = DirectHosts::new(file.clone());
        let conflicts = hosts.set(&names(&["shop.test", "old.test"])).unwrap();
        assert_eq!(conflicts.len(), 1, "old.test is Herd's");
        assert!(fs::read(&file.path).unwrap().starts_with(ORIGINAL));
        assert_eq!(hosts.names().unwrap(), names(&["old.test", "shop.test"]));
        assert_eq!(fs::read(file.backup_path()).unwrap(), ORIGINAL);

        // The copy is renewed at every change: it is what the file was just before this one,
        // never a stale version from an earlier session.
        let after_first = fs::read(&file.path).unwrap();
        hosts.set(&names(&["shop.test"])).unwrap();
        assert_eq!(fs::read(file.backup_path()).unwrap(), after_first);

        hosts.clear().unwrap();
        assert_eq!(
            fs::read(&file.path).unwrap(),
            ORIGINAL,
            "uninstalled: as before"
        );
        assert!(!file.backup_path().exists(), "no leftovers");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_missing_file_reads_as_empty_and_is_created_by_the_first_block() {
        let dir = tempfile::tempdir().unwrap();
        let file = FileHosts::new(dir.path().join("hosts"));
        assert_eq!(file.read().unwrap(), "");
        let hosts = DirectHosts::new(file.clone());
        hosts.set(&names(&["a.test"])).unwrap();
        assert!(file.read().unwrap().contains("127.0.0.1 a.test"));
        hosts.clear().unwrap();
        assert_eq!(file.read().unwrap(), "");
    }

    #[test]
    fn a_file_that_is_not_utf8_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let bytes = b"127.0.0.1 localhost # caf\xe9\r\n";
        fs::write(&path, bytes).unwrap();
        let hosts = DirectHosts::new(FileHosts::new(&path));
        assert!(hosts.set(&names(&["a.test"])).is_err());
        assert!(hosts.clear().is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!FileHosts::new(&path).backup_path().exists());
    }

    #[test]
    fn a_damaged_copy_is_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        fs::write(&path, "x\r\n# BEGIN CRISOL\r\n").unwrap();
        let hosts = DirectHosts::new(FileHosts::new(&path));
        assert!(hosts.set(&names(&["a.test"])).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "x\r\n# BEGIN CRISOL\r\n"
        );
    }

    #[test]
    fn the_system_path_is_the_drivers_etc_file() {
        let path = FileHosts::system().unwrap().path;
        let text = path.to_string_lossy().to_ascii_lowercase();
        assert!(text.ends_with(r"system32\drivers\etc\hosts"), "{path:?}");
        assert!(path.is_absolute());
    }

    fn helper_at(path: PathBuf) -> HelperLocation {
        HelperLocation::at(
            path,
            std::sync::Arc::new(crisol_core::paths::fake::FakePrivateDirs::new()),
        )
    }

    #[test]
    fn the_elevated_manager_checks_before_it_asks_for_permission() {
        // The helper does not exist, so reaching it is an I/O error: what must not reach it
        // is a bad name, a damaged file, or a change that is not one.
        let (dir, file) = copy();
        let helper = dir.path().join("no-helper.exe");
        let hosts = ElevatedHosts::over(helper_at(helper.clone()), file.clone());
        hosts.clear().unwrap(); // no block: nothing to ask for
        assert!(matches!(
            hosts.set(&names(&["example.com"])),
            Err(HostsManagerError::Hosts(_))
        ));
        // The helper is not there: that is said (kind NotFound) before any prompt.
        match hosts.set(&names(&["a.test"])) {
            Err(HostsManagerError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read(&file.path).unwrap(), ORIGINAL);

        let damaged = dir.path().join("damaged");
        fs::write(&damaged, "# BEGIN CRISOL\r\n").unwrap();
        let hosts = ElevatedHosts::over(helper_at(helper), FileHosts::new(&damaged));
        assert!(matches!(
            hosts.set(&names(&["a.test"])),
            Err(HostsManagerError::Hosts(_))
        ));
        assert!(matches!(hosts.clear(), Err(HostsManagerError::Hosts(_))));
    }

    // ---- review of phase 7, finding 3: replaced, not truncated -------------------------------

    /// What `icacls` says about a file, minus the line with its own name.
    fn acl_of(path: &Path) -> String {
        let output = Command::new("icacls").arg(path).output().unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.contains("Successfully processed"))
            .map(|l| l.replace(&*path.to_string_lossy(), "<file>"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn icacls(path: &Path, args: &[&str]) {
        let output = Command::new("icacls")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "icacls {args:?}");
    }

    #[test]
    fn the_permissions_and_attributes_of_the_original_survive_the_replacement() {
        let (_dir, file) = copy();
        // A hidden file, and a DACL that is not what a new file would get: no inheritance and
        // explicit grants. (All on the test's own copy of the file.)
        let hide = Command::new("attrib")
            .arg("+h")
            .arg(&file.path)
            .output()
            .unwrap();
        assert!(hide.status.success());
        icacls(&file.path, &["/inheritance:r"]);
        // The test's own account keeps full control (a rewrite in place needs to write).
        let me = format!("*{}:(F)", crate::current_user_sid().unwrap());
        icacls(&file.path, &["/grant:r", &me]);
        icacls(&file.path, &["/grant:r", "*S-1-5-32-545:(R)"]); // Users: read
        icacls(&file.path, &["/grant", "*S-1-5-11:(R)"]); // Authenticated Users: read
        let before = acl_of(&file.path);
        assert!(before.contains("(R)"), "{before}");
        {
            use std::os::windows::fs::MetadataExt;
            let a = fs::metadata(&file.path).unwrap().file_attributes();
            assert_ne!(a & 0x2, 0, "attrib did not hide the copy: {a:#x}");
        }

        let hosts = DirectHosts::new(file.clone());
        hosts.set(&names(&["shop.test"])).unwrap();
        assert!(fs::read_to_string(&file.path)
            .unwrap()
            .contains("shop.test"));
        assert_eq!(
            acl_of(&file.path),
            before,
            "same permissions after a change"
        );
        let attributes = fs::metadata(&file.path).unwrap();
        assert_ne!(attributes.file_attributes() & HIDDEN, 0, "still hidden");

        hosts.clear().unwrap();
        assert_eq!(acl_of(&file.path), before, "and after the block is gone");
    }

    // `set_readonly(false)` is only dangerous on Unix; here it clears the attribute.
    #[allow(clippy::permissions_set_readonly_false)]
    #[test]
    fn no_temporary_file_is_left_behind_after_a_success_or_a_failure() {
        let (dir, file) = copy();
        let hosts = DirectHosts::new(file.clone());
        let listing = || {
            let mut names: Vec<String> = fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        hosts.set(&names(&["a.test"])).unwrap();
        assert_eq!(listing(), ["hosts", "hosts.crisol-bak"]);

        hosts.set(&names(&["b.test"])).unwrap();
        assert_eq!(listing(), ["hosts", "hosts.crisol-bak"]);

        // A read-only hosts cannot be replaced: the error is clean, nothing changes, and no
        // temporary file stays.
        let before = fs::read(&file.path).unwrap();
        let mut permissions = fs::metadata(&file.path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file.path, permissions.clone()).unwrap();
        assert!(hosts.set(&names(&["c.test"])).is_err());
        assert_eq!(fs::read(&file.path).unwrap(), before);
        assert_eq!(listing(), ["hosts", "hosts.crisol-bak"]);
        permissions.set_readonly(false);
        fs::set_permissions(&file.path, permissions).unwrap();
    }

    #[test]
    fn a_file_that_is_not_what_was_written_is_put_back_and_reported() {
        let (_dir, mut file) = copy();
        // Something damages the file right after it is replaced.
        file.tamper = Some(|path| fs::write(path, "").unwrap());
        let hosts = DirectHosts::new(file.clone());
        let error = hosts.set(&names(&["shop.test"])).unwrap_err();
        assert!(
            error.to_string().contains("previous version was restored"),
            "{error}"
        );
        assert_eq!(fs::read(&file.path).unwrap(), ORIGINAL, "back as it was");
    }

    #[test]
    fn a_file_that_is_busy_for_a_moment_is_tried_again() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_dir, file) = copy();
        // Open for reading only, as a scanner could (others may read, not replace), and let go
        // after a short while.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&file.path)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            drop(held);
        });
        DirectHosts::new(file.clone())
            .set(&names(&["shop.test"]))
            .unwrap();
        release.join().unwrap();
        assert!(fs::read_to_string(&file.path)
            .unwrap()
            .contains("shop.test"));
    }

    // ---- review of phase 7, W2: a damaged file never replaces a good copy -------------------

    /// Breaks the file and makes it read-only, as a scanner that holds on to it could.
    fn damage_and_lock(path: &Path) {
        fs::write(path, "").unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[allow(clippy::permissions_set_readonly_false)]
    fn unlock(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn the_good_copy_is_replaced_only_after_the_change_is_confirmed() {
        let (dir, file) = copy();
        let hosts = DirectHosts::new(file.clone());
        hosts.set(&names(&["a.test"])).unwrap();
        let first_change = fs::read(&file.path).unwrap();
        assert_eq!(fs::read(file.backup_path()).unwrap(), ORIGINAL);

        // A change that fails and is undone: the copy is still the one from before, and no
        // new copy is left over.
        let mut failing = file.clone();
        failing.tamper = Some(|path| fs::write(path, "").unwrap());
        let error = DirectHosts::new(failing)
            .set(&names(&["b.test"]))
            .unwrap_err();
        assert!(
            error.to_string().contains("previous version was restored"),
            "{error}"
        );
        assert_eq!(fs::read(&file.path).unwrap(), first_change, "put back");
        assert_eq!(
            fs::read(file.backup_path()).unwrap(),
            ORIGINAL,
            "the copy was not renewed"
        );
        assert!(!file.pending_path().exists());

        // A change that works renews it.
        hosts.set(&names(&["b.test"])).unwrap();
        assert_eq!(fs::read(file.backup_path()).unwrap(), first_change);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn if_the_restoration_fails_the_good_copies_stay_and_nothing_else_is_written() {
        let (_dir, file) = copy();
        let hosts = DirectHosts::new(file.clone());
        hosts.set(&names(&["a.test"])).unwrap();
        let good = fs::read(&file.path).unwrap();
        let older_copy = fs::read(file.backup_path()).unwrap();

        let mut failing = file.clone();
        failing.tamper = Some(damage_and_lock);
        let error = DirectHosts::new(failing)
            .set(&names(&["b.test"]))
            .unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("could not be put back") && text.contains("hosts.crisol-bak.new"),
            "{text}"
        );
        // The older copy is untouched, and the copy of how the file was is kept.
        assert_eq!(fs::read(file.backup_path()).unwrap(), older_copy);
        assert_eq!(fs::read(file.pending_path()).unwrap(), good);

        // The file is damaged and locked; the next attempt does not copy it over anything and
        // does not touch it, and says why.
        unlock(&file.path);
        let damaged = fs::read(&file.path).unwrap();
        let again = hosts.set(&names(&["c.test"])).unwrap_err();
        assert!(again.to_string().contains("did not finish"), "{again}");
        assert_eq!(fs::read(&file.path).unwrap(), damaged, "not touched");
        assert_eq!(fs::read(file.backup_path()).unwrap(), older_copy);
        assert_eq!(fs::read(file.pending_path()).unwrap(), good);
        hosts.clear().unwrap(); // no block in it, so nothing to change
        assert_eq!(fs::read(&file.path).unwrap(), damaged);

        // A person puts the copy back and deletes it: everything works again.
        fs::write(&file.path, &good).unwrap();
        fs::remove_file(file.pending_path()).unwrap();
        hosts.set(&names(&["c.test"])).unwrap();
        assert!(fs::read_to_string(&file.path).unwrap().contains("c.test"));
    }

    #[test]
    fn the_elevated_manager_refuses_before_the_prompt_while_a_change_is_unfinished() {
        let (dir, file) = copy();
        // A file with a block (so `clear` has something to do) and an unfinished change.
        DirectHosts::new(file.clone())
            .set(&names(&["a.test"]))
            .unwrap();
        fs::write(file.pending_path(), ORIGINAL).unwrap();
        let hosts = ElevatedHosts::over(helper_at(dir.path().join("no-helper.exe")), file.clone());
        // Not NotFound (the helper is missing): it stopped before ever looking for it.
        for result in [
            hosts.set(&names(&["z.test"])),
            hosts.clear().map(|()| Vec::new()),
        ] {
            match result {
                Err(HostsManagerError::Io(e)) => {
                    assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
                    assert!(e.to_string().contains("did not finish"), "{e}");
                }
                other => panic!("{other:?}"),
            }
        }
    }
}
