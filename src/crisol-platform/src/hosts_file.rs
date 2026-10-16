//! The real `hosts` file (F7.3, ADR-0014): reading it, writing it in place with a backup of the
//! original, and the elevated way to change its Crisol block.
//!
//! [`FileHosts`] works on any path, which is how the tests use it (copies in a temporary
//! folder); [`FileHosts::system`] is `%SystemRoot%\System32\drivers\etc\hosts`, writable only by
//! an administrator, so only the helper (`crisol-elevate`) writes it.
//!
//! The file is written **in place** and not replaced by a temporary file: a replacement would
//! lose the permissions and attributes of the original. It is a few kilobytes written in one
//! call, and the original is copied next to it (`hosts.crisol-bak`) before the first change, so
//! a failure can always be undone; the copy is deleted when the block goes away.

use std::fs;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crisol_core::dns::ElevatedOp;
use crisol_core::hosts::{
    apply_block, block_names, clean_names, Conflict, HostsFile, HostsManager, HostsManagerError,
    BEGIN,
};

use crate::nrpt::run_elevated_op;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BACKUP_EXTENSION: &str = "crisol-bak";

/// A `hosts` file at some path.
#[derive(Debug, Clone)]
pub struct FileHosts {
    path: PathBuf,
}

impl FileHosts {
    /// The file at this path (it may not exist yet; that reads as empty).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The machine's `hosts`.
    #[must_use]
    pub fn system() -> Self {
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        Self::new(
            root.join("System32")
                .join("drivers")
                .join("etc")
                .join("hosts"),
        )
    }

    /// Where the copy of the original goes.
    #[must_use]
    pub fn backup_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".");
        name.push(BACKUP_EXTENSION);
        self.path.with_file_name(name)
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
        if self.path.exists() && !backup.exists() {
            fs::copy(&self.path, &backup)?;
        }
        // Truncate and write in place: the file keeps its permissions.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.path)?;
        io::Write::write_all(&mut file, content.as_bytes())?;
        if !content.contains(BEGIN) {
            match fs::remove_file(&backup) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Empties the DNS client cache so that a change to `hosts` is seen at once.
pub fn flush_dns() -> io::Result<()> {
    let status = Command::new("ipconfig")
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
    helper: PathBuf,
    file: FileHosts,
}

impl ElevatedHosts {
    /// Over the machine's `hosts`, with the path of `crisol-elevate.exe`.
    #[must_use]
    pub fn new(helper: impl Into<PathBuf>) -> Self {
        Self::over(helper, FileHosts::system())
    }

    /// Reading another file (the helper still edits the machine's own). For tests.
    #[must_use]
    pub fn over(helper: impl Into<PathBuf>, file: FileHosts) -> Self {
        Self {
            helper: helper.into(),
            file,
        }
    }

    fn run(&self, op: &ElevatedOp) -> Result<(), HostsManagerError> {
        Ok(run_elevated_op(&self.helper, op)?)
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
        self.run(&ElevatedOp::HostsClear)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::hosts::DirectHosts;
    use tempfile::TempDir;

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

        hosts.set(&names(&["shop.test"])).unwrap();
        assert_eq!(
            fs::read(file.backup_path()).unwrap(),
            ORIGINAL,
            "the first original"
        );

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
        let path = FileHosts::system().path;
        assert!(path.ends_with(r"System32\drivers\etc\hosts"), "{path:?}");
    }

    #[test]
    fn the_elevated_manager_checks_before_it_asks_for_permission() {
        // The helper does not exist, so reaching it is an I/O error: what must not reach it
        // is a bad name, a damaged file, or a change that is not one.
        let (dir, file) = copy();
        let helper = dir.path().join("no-helper.exe");
        let hosts = ElevatedHosts::over(&helper, file.clone());
        hosts.clear().unwrap(); // no block: nothing to ask for
        assert!(matches!(
            hosts.set(&names(&["example.com"])),
            Err(HostsManagerError::Hosts(_))
        ));
        assert!(matches!(
            hosts.set(&names(&["a.test"])),
            Err(HostsManagerError::Io(_))
        ));
        assert_eq!(fs::read(&file.path).unwrap(), ORIGINAL);

        let damaged = dir.path().join("damaged");
        fs::write(&damaged, "# BEGIN CRISOL\r\n").unwrap();
        let hosts = ElevatedHosts::over(&helper, FileHosts::new(&damaged));
        assert!(matches!(
            hosts.set(&names(&["a.test"])),
            Err(HostsManagerError::Hosts(_))
        ));
        assert!(matches!(hosts.clear(), Err(HostsManagerError::Hosts(_))));
    }
}
