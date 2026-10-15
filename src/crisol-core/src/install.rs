//! Installing a catalog package: download, verify, unpack, move into place.
//!
//! The order is the security argument (ADR-0005), and each step is strict about what it
//! lets through:
//!
//! 1. **Download** into a private staging folder inside `bin/` (same disk, so the final move
//!    is a rename). The SHA-256 is computed while the bytes arrive; the size is bounded by the
//!    catalog's `size` (or a hard cap), so an endless or wrong stream is cut short.
//! 2. **Verify** the hash against the catalog. A mismatch stops everything: **nothing is
//!    unpacked** from a download that is not the one the catalog vouches for.
//! 3. **Unpack** into the staging folder, refusing anything that could write outside it:
//!    `..`, absolute paths, drive letters and alternate streams (`:`), symbolic links,
//!    Windows device names (`NUL`, `COM1`...), names Windows would silently rewrite
//!    (trailing dots or spaces), entries outside the declared `root`, duplicate names, and
//!    archives that unpack to absurd sizes or numbers of files.
//! 4. **Check** that the declared executable is there.
//! 5. **Move** the finished tree to `bin/<service>/<version>/` with a single rename.
//!
//! Whatever happens (an error, a cancellation, even a panic) the staging folder is deleted,
//! so `bin/` only ever holds complete installations. If the process is killed mid-way a
//! `.staging-*` folder can be left behind; [`remove_stale_staging`] sweeps those up and runs
//! at the start of every install.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::catalog::{ArchiveFormat, Package};
use crate::fetch::{Fetch, FetchError};
use crate::paths::{is_reserved_device_name, CrisolPaths, PathsError};
use crate::version::Version;

/// Prefix of the temporary folders created inside `bin/`.
pub const STAGING_PREFIX: &str = ".staging-";

/// Leftover staging folders older than this are removed at the start of an install.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const GIB: u64 = 1024 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
/// Minimum number of bytes between two download progress reports.
const PROGRESS_STEP: u64 = 256 * 1024;

/// Limits and behaviour of an installation.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Largest download accepted when the catalog does not state a size.
    pub max_download_bytes: u64,
    /// Largest total size an archive may unpack to (a guard against zip bombs).
    pub max_extracted_bytes: u64,
    /// Most entries an archive may have.
    pub max_entries: usize,
    /// How many times to try the final move; antivirus scanners briefly lock new files.
    pub move_attempts: u32,
    /// Pause between those tries.
    pub move_delay: Duration,
    /// Set this flag to stop the installation at the next chunk or entry.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            max_download_bytes: 2 * GIB,
            max_extracted_bytes: 4 * GIB,
            max_entries: 200_000,
            move_attempts: 5,
            move_delay: Duration::from_millis(200),
            cancel: None,
        }
    }
}

/// What the installer is doing, for a progress bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Opening the connection.
    Connecting,
    /// Receiving bytes. `total` is known when the catalog or the server says so.
    Downloading {
        /// Bytes received.
        done: u64,
        /// Expected total.
        total: Option<u64>,
    },
    /// Checking the SHA-256.
    Verifying,
    /// Unpacking the archive, entry by entry.
    Extracting {
        /// Entries processed.
        done: u64,
        /// Entries in the archive.
        total: u64,
    },
    /// Moving the finished tree into place.
    Installing,
}

/// A finished installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// `bin/<service>/<version>/`.
    pub path: PathBuf,
    /// Bytes downloaded.
    pub downloaded: u64,
    /// Files written.
    pub files: u64,
    /// The version no longer receives security fixes (the catalog says so): the caller should
    /// tell the user (ADR-0012).
    pub end_of_life: bool,
}

/// Why an installation failed. In every case nothing is left installed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// That version is already installed; nothing was touched.
    #[error("already installed at {}", .0.display())]
    AlreadyInstalled(PathBuf),
    /// The package name or version does not make a valid folder name.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// The download could not be started.
    #[error(transparent)]
    Fetch(#[from] FetchError),
    /// The connection failed while receiving the body.
    #[error("the download was interrupted: {0}")]
    Interrupted(String),
    /// The server's announced size, or the size received, is not what the catalog says.
    #[error("the download is {actual} bytes but the catalog says {expected}")]
    SizeMismatch {
        /// Size in the catalog.
        expected: u64,
        /// Size announced or received.
        actual: u64,
    },
    /// The stream went past the size limit.
    #[error("the download is larger than the allowed {limit} bytes")]
    TooLarge {
        /// The limit that was exceeded.
        limit: u64,
    },
    /// The SHA-256 of the download is not the one in the catalog. Nothing was unpacked.
    #[error("the download does not match the catalog (expected sha256 {expected}, got {actual})")]
    HashMismatch {
        /// Hash in the catalog.
        expected: String,
        /// Hash of the download.
        actual: String,
    },
    /// An entry of the archive would be written somewhere it must not.
    #[error("unsafe entry `{entry}` in the archive: {reason}")]
    UnsafeEntry {
        /// The entry name as stored.
        entry: String,
        /// Why it was refused.
        reason: String,
    },
    /// The archive is damaged.
    #[error("the archive is damaged: {0}")]
    BadArchive(String),
    /// The archive has more entries than allowed.
    #[error("the archive has more than {limit} entries")]
    TooManyEntries {
        /// The limit.
        limit: usize,
    },
    /// The archive unpacks to more than allowed.
    #[error("the archive unpacks to more than {limit} bytes")]
    ExtractedTooLarge {
        /// The limit.
        limit: u64,
    },
    /// The archive format is not supported on this version of Crisol.
    #[error("the {0:?} archive format is not supported yet")]
    UnsupportedFormat(ArchiveFormat),
    /// The installed tree lacks the executable the catalog promised.
    #[error("the archive does not contain `{0}`")]
    MissingExecutable(String),
    /// The installation was cancelled.
    #[error("the installation was cancelled")]
    Cancelled,
    /// A file operation failed.
    #[error("{action} `{}`: {source}", path.display())]
    Io {
        /// What was being done.
        action: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

fn io_error<'a>(
    action: &'static str,
    path: &'a Path,
) -> impl FnOnce(io::Error) -> InstallError + 'a {
    move |source| InstallError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Downloads, verifies, unpacks and installs `package` under `bin/<service>/<version>/`.
///
/// `progress` is called as the work advances. See the module docs for the guarantees.
pub fn install(
    package: &Package,
    paths: &CrisolPaths,
    fetcher: &dyn Fetch,
    options: &InstallOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<Installed, InstallError> {
    let dest = paths.bin_for(&package.service, &package.version.to_string())?;
    if let Some(existing) = installed_equivalent(&dest, package) {
        return Err(InstallError::AlreadyInstalled(existing));
    }

    let bin = paths.bin_dir();
    fs::create_dir_all(&bin).map_err(io_error("cannot create", &bin))?;
    let _ = remove_stale_staging(&bin, STALE_AFTER);
    // Dropping this deletes the folder, on success and on every error path.
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(&bin)
        .map_err(io_error("cannot create a staging folder in", &bin))?;

    let archive = staging.path().join("download");
    let downloaded = download(package, fetcher, options, &archive, progress)?;

    let content = staging.path().join("content");
    fs::create_dir(&content).map_err(io_error("cannot create", &content))?;
    let files = extract(package, &archive, &content, options, progress)?;
    // A single-file package has already been moved into place; an archive is still here.
    match fs::remove_file(&archive) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return Err(io_error("cannot remove", &archive)(e));
        }
        _ => {}
    }

    let executable = content.join(&package.executable);
    if !executable.is_file() {
        return Err(InstallError::MissingExecutable(package.executable.clone()));
    }

    check_cancel(options)?;
    progress(Progress::Installing);
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&parent).map_err(io_error("cannot create", &parent))?;
    if let Some(existing) = installed_equivalent(&dest, package) {
        return Err(InstallError::AlreadyInstalled(existing));
    }
    let moved = retry_if(
        options.move_attempts,
        options.move_delay,
        is_transient,
        || fs::rename(&content, &dest),
    );
    if let Err(error) = moved {
        // Another install of the same version may have won the race between the check above
        // and the rename: that is "already installed", not an I/O failure.
        if dest.exists() {
            return Err(InstallError::AlreadyInstalled(dest));
        }
        return Err(io_error("cannot move into place", &dest)(error));
    }
    Ok(Installed {
        path: dest,
        downloaded,
        files,
        end_of_life: package.end_of_life,
    })
}

/// Removes `.staging-*` folders in `bin_dir` that are older than `older_than`. Returns how
/// many were removed.
pub fn remove_stale_staging(bin_dir: &Path, older_than: Duration) -> io::Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(bin_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(STAGING_PREFIX) || !entry.file_type()?.is_dir() {
            continue;
        }
        let age = entry
            .metadata()?
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .unwrap_or_default();
        if age >= older_than && fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn check_cancel(options: &InstallOptions) -> Result<(), InstallError> {
    match &options.cancel {
        Some(flag) if flag.load(Ordering::Relaxed) => Err(InstallError::Cancelled),
        _ => Ok(()),
    }
}

/// Runs `action` up to `attempts` times, pausing between tries, and returns the last error.
/// An error for which `transient` says no is returned at once.
fn retry_if<T>(
    attempts: u32,
    delay: Duration,
    transient: impl Fn(&io::Error) -> bool,
    mut action: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut tries = 1;
    loop {
        match action() {
            Ok(value) => return Ok(value),
            Err(error) if tries >= attempts.max(1) || !transient(&error) => return Err(error),
            Err(_) => {
                tries += 1;
                thread::sleep(delay);
            }
        }
    }
}

/// Errors that go away by themselves: an antivirus scanner holding a new file shows up as
/// access denied or a sharing/lock violation.
fn is_transient(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33)))
}

/// An installation of the same service whose version equals `package`'s (`8.4` and `8.4.0`
/// are the same version but different folder names), or `dest` itself if it exists.
fn installed_equivalent(dest: &Path, package: &Package) -> Option<PathBuf> {
    if dest.exists() {
        return Some(dest.to_path_buf());
    }
    let service_dir = dest.parent()?;
    fs::read_dir(service_dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .find(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| Version::parse(name).ok())
                .is_some_and(|version| version == package.version)
        })
        .map(|entry| entry.path())
}

// ---- download -------------------------------------------------------------------

fn download(
    package: &Package,
    fetcher: &dyn Fetch,
    options: &InstallOptions,
    target: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<u64, InstallError> {
    check_cancel(options)?;
    progress(Progress::Connecting);
    let response = fetcher.fetch(&package.url)?;

    if let (Some(announced), Some(expected)) = (response.content_length, package.size) {
        if announced != expected {
            return Err(InstallError::SizeMismatch {
                expected,
                actual: announced,
            });
        }
    }
    let limit = package.size.unwrap_or(options.max_download_bytes);
    let total = package.size.or(response.content_length);

    let mut out = File::create(target).map_err(io_error("cannot create", target))?;
    let mut body = response.body;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK];
    let (mut done, mut reported) = (0_u64, 0_u64);
    progress(Progress::Downloading { done, total });
    loop {
        check_cancel(options)?;
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(InstallError::Interrupted(e.to_string())),
        };
        done += u64::try_from(read).unwrap_or(u64::MAX);
        if done > limit {
            return Err(InstallError::TooLarge { limit });
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])
            .map_err(io_error("cannot write", target))?;
        if done - reported >= PROGRESS_STEP {
            reported = done;
            progress(Progress::Downloading { done, total });
        }
    }
    out.flush().map_err(io_error("cannot write", target))?;
    drop(out);
    progress(Progress::Downloading { done, total });

    if let Some(expected) = package.size {
        if done != expected {
            return Err(InstallError::SizeMismatch {
                expected,
                actual: done,
            });
        }
    }
    progress(Progress::Verifying);
    let actual = hex(&hasher.finalize());
    if actual != package.sha256 {
        return Err(InstallError::HashMismatch {
            expected: package.sha256.clone(),
            actual,
        });
    }
    Ok(done)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

// ---- unpacking ------------------------------------------------------------------

fn extract(
    package: &Package,
    archive: &Path,
    content: &Path,
    options: &InstallOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<u64, InstallError> {
    match package.format {
        ArchiveFormat::Zip => extract_zip(package, archive, content, options, progress),
        ArchiveFormat::File => {
            let target = content.join(&package.executable);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(io_error("cannot create", parent))?;
            }
            fs::rename(archive, &target).map_err(io_error("cannot place", &target))?;
            Ok(1)
        }
        other @ (ArchiveFormat::TarGz | ArchiveFormat::TarXz) => {
            Err(InstallError::UnsupportedFormat(other))
        }
    }
}

fn extract_zip(
    package: &Package,
    archive: &Path,
    content: &Path,
    options: &InstallOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<u64, InstallError> {
    let file = File::open(archive).map_err(io_error("cannot open", archive))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| InstallError::BadArchive(e.to_string()))?;
    let entries = zip.len();
    if entries > options.max_entries {
        return Err(InstallError::TooManyEntries {
            limit: options.max_entries,
        });
    }
    let total = u64::try_from(entries).unwrap_or(u64::MAX);

    let (mut files, mut written) = (0_u64, 0_u64);
    for index in 0..entries {
        check_cancel(options)?;
        let mut entry = zip
            .by_index(index)
            .map_err(|e| InstallError::BadArchive(e.to_string()))?;
        let name = entry
            .name()
            .map_err(|e| InstallError::BadArchive(format!("entry {index}: {e}")))?
            .into_owned();
        let unsafe_entry = |reason: &str| InstallError::UnsafeEntry {
            entry: name.clone(),
            reason: reason.to_owned(),
        };

        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err(unsafe_entry("symbolic links are not allowed"));
        }
        let Some(relative) =
            safe_relative_path(&name, package.root.as_deref()).map_err(|r| unsafe_entry(&r))?
        else {
            progress(Progress::Extracting {
                done: u64::try_from(index + 1).unwrap_or(total),
                total,
            });
            continue; // the root folder itself
        };
        let target = content.join(&relative);

        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            fs::create_dir_all(&target).map_err(io_error("cannot create", &target))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(io_error("cannot create", parent))?;
            }
            // `create_new` refuses to overwrite: duplicate names (also differing only in case
            // on Windows) and a file where a folder was made are errors, not silent overwrites.
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        unsafe_entry("the same path appears twice")
                    } else {
                        io_error("cannot create", &target)(e)
                    }
                })?;
            let remaining = options.max_extracted_bytes.saturating_sub(written);
            let copied = io::copy(
                &mut (&mut entry).take(remaining.saturating_add(1)),
                &mut out,
            )
            .map_err(|e| InstallError::BadArchive(format!("{name}: {e}")))?;
            written += copied;
            if written > options.max_extracted_bytes {
                return Err(InstallError::ExtractedTooLarge {
                    limit: options.max_extracted_bytes,
                });
            }
            files += 1;
        }
        progress(Progress::Extracting {
            done: u64::try_from(index + 1).unwrap_or(total),
            total,
        });
    }
    Ok(files)
}

/// Turns an archive entry name into a path **relative to the install folder**, or says why
/// it cannot be trusted. `Ok(None)` is the `root` folder itself.
///
/// Both `/` and `\` separate components, because Windows treats them alike and a hostile
/// archive can use either.
pub fn safe_relative_path(name: &str, root: Option<&str>) -> Result<Option<PathBuf>, String> {
    let unified = name.replace('\\', "/");
    if unified.starts_with('/') {
        return Err("absolute path".to_owned());
    }
    let trimmed = unified.strip_suffix('/').unwrap_or(&unified);
    let components: Vec<&str> = trimmed.split('/').collect();
    for component in &components {
        check_component(component)?;
    }

    let mut rest: &[&str] = &components;
    if let Some(root) = root {
        let root_parts: Vec<&str> = root.split('/').collect();
        if !rest.starts_with(&root_parts) {
            return Err(format!("outside the expected folder `{root}`"));
        }
        rest = &rest[root_parts.len()..];
    }
    Ok(if rest.is_empty() {
        None
    } else {
        Some(rest.iter().collect())
    })
}

fn check_component(component: &str) -> Result<(), String> {
    if component.is_empty() {
        return Err("empty path component".to_owned());
    }
    if component == "." || component == ".." {
        return Err("`.` and `..` are not allowed".to_owned());
    }
    if component
        .chars()
        .any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(
            "characters Windows does not allow in file names (or `:` for drives and streams)"
                .to_owned(),
        );
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Err("Windows would silently rename names that end in a dot or a space".to_owned());
    }
    if is_reserved_device_name(component) {
        return Err("reserved Windows device name".to_owned());
    }
    if component.len() > 255 {
        return Err("name too long".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
