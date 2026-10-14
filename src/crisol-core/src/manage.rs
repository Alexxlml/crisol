//! The version-management commands of the core: list, install and uninstall.
//!
//! The command-line tool and, later, the GUI call these functions and add nothing of their
//! own. What is installed is simply what is on disk: `bin/<service>/<version>/`, with no
//! database to drift out of sync. Folders that are not a recognisable version, hidden
//! folders and the `.staging-*` work areas of the installer are not installations.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::catalog::{Catalog, CatalogError, Package, Target};
use crate::fetch::Fetch;
use crate::install::{install, InstallError, InstallOptions, Installed, Progress, STAGING_PREFIX};
use crate::paths::{CrisolPaths, PathsError};
use crate::version::{Version, VersionError, VersionReq};

/// A version that is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledVersion {
    /// Tool name (`php`).
    pub service: String,
    /// Its version.
    pub version: Version,
    /// `bin/<service>/<version>/`.
    pub path: PathBuf,
}

/// A catalog entry for this platform, and whether it is already installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableVersion {
    /// The catalog entry.
    pub package: Package,
    /// Whether that exact version is installed.
    pub installed: bool,
}

/// What an uninstall removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    /// The version that was removed.
    pub version: InstalledVersion,
    /// Whether the now-empty `bin/<service>/` folder was removed too.
    pub service_folder_removed: bool,
}

/// Why a command failed.
#[derive(Debug, thiserror::Error)]
pub enum ManageError {
    /// The installation itself failed.
    #[error(transparent)]
    Install(#[from] InstallError),
    /// The catalog has no such version.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// A name is not a valid folder name.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// The version text is not a version.
    #[error(transparent)]
    Version(#[from] VersionError),
    /// This operating system or CPU has no catalog entries.
    #[error("this platform is not supported by Crisol")]
    UnsupportedPlatform,
    /// That version is not installed.
    #[error(
        "{service} {request} is not installed (installed: {})",
        list_or_none(installed)
    )]
    NotInstalled {
        /// Tool asked for.
        service: String,
        /// Version asked for.
        request: String,
        /// Versions of that tool that are installed.
        installed: Vec<String>,
    },
    /// The request matches more than one installed version.
    #[error("{service} {request} matches several installed versions ({}); give the full version", matches.join(", "))]
    Ambiguous {
        /// Tool asked for.
        service: String,
        /// Version asked for.
        request: String,
        /// The versions it matches.
        matches: Vec<String>,
    },
    /// A file of that installation is open or running, so it cannot be removed. Nothing was
    /// changed. Carries the file that is busy (or the folder, if the folder itself is).
    #[error("`{}` is in use (is one of its programs running?); nothing was removed", .0.display())]
    InUse(PathBuf),
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

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// Every installed version, ordered by service and then version (oldest first).
pub fn list_installed(paths: &CrisolPaths) -> Result<Vec<InstalledVersion>, ManageError> {
    let bin = paths.bin_dir();
    let mut found = Vec::new();
    for service_dir in visible_subdirs(&bin)? {
        let Some(service) = file_name(&service_dir) else {
            continue;
        };
        for version_dir in visible_subdirs(&service_dir)? {
            let Some(text) = file_name(&version_dir) else {
                continue;
            };
            if let Ok(version) = Version::parse(&text) {
                found.push(InstalledVersion {
                    service: service.clone(),
                    version,
                    path: version_dir,
                });
            }
        }
    }
    found.sort_by(|a, b| (&a.service, &a.version).cmp(&(&b.service, &b.version)));
    Ok(found)
}

/// The versions the catalog offers for `target`, marking the installed ones.
pub fn list_available(
    paths: &CrisolPaths,
    catalog: &Catalog,
    target: Target,
) -> Result<Vec<AvailableVersion>, ManageError> {
    let installed = list_installed(paths)?;
    Ok(catalog
        .packages()
        .iter()
        .filter(|package| package.target == target)
        .map(|package| AvailableVersion {
            installed: installed
                .iter()
                .any(|i| i.service == package.service && i.version == package.version),
            package: package.clone(),
        })
        .collect())
}

/// What installing needs besides the request itself.
#[derive(Clone, Copy)]
pub struct Environment<'a> {
    /// Where everything lives.
    pub paths: &'a CrisolPaths,
    /// What can be installed.
    pub catalog: &'a Catalog,
    /// The platform to install for, normally [`Target::current`].
    pub target: Target,
    /// How to download.
    pub fetcher: &'a dyn Fetch,
}

impl std::fmt::Debug for Environment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Environment")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// Installs the version of `service` that matches `request` (`8.4`, `22`, `latest`).
///
/// Nothing is downloaded when the catalog has no such version or it is already installed.
pub fn install_version(
    env: &Environment<'_>,
    service: &str,
    request: &str,
    options: &InstallOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<Installed, ManageError> {
    let package = if request == "latest" {
        env.catalog.latest(service, env.target)?
    } else {
        env.catalog.find(service, request, env.target)?
    };
    Ok(install(package, env.paths, env.fetcher, options, progress)?)
}

/// The highest installed version of `service` that satisfies `request`, if any.
pub fn resolve_installed(
    paths: &CrisolPaths,
    service: &str,
    request: &str,
) -> Result<Option<InstalledVersion>, ManageError> {
    let wanted = VersionReq::parse(request)?;
    Ok(list_installed(paths)?
        .into_iter()
        .rfind(|i| i.service == service && wanted.matches(&i.version)))
}

/// Uninstalls one version of `service`.
///
/// `request` is an exact version, or a prefix (`8.4`) that matches exactly one installed
/// version; several matches are an error, because this never removes more than one. The
/// folder is first renamed out of `bin/`, which fails cleanly if a program from it is
/// running, and only then deleted. If that leaves `bin/<service>/` empty it is removed too,
/// so installing and uninstalling a tool leaves `bin/` as it was.
pub fn uninstall(
    paths: &CrisolPaths,
    service: &str,
    request: &str,
) -> Result<Removed, ManageError> {
    paths.bin_for(service, "x")?; // the service name must be a safe folder name
    let wanted = VersionReq::parse(request)?;
    let of_service: Vec<InstalledVersion> = list_installed(paths)?
        .into_iter()
        .filter(|i| i.service == service)
        .collect();
    let names = |versions: &[&InstalledVersion]| -> Vec<String> {
        versions.iter().map(|i| i.version.to_string()).collect()
    };

    let exact: Vec<&InstalledVersion> = of_service
        .iter()
        .filter(|i| Version::parse(request).is_ok_and(|v| v == i.version))
        .collect();
    let matching: Vec<&InstalledVersion> = if exact.is_empty() {
        of_service
            .iter()
            .filter(|i| wanted.matches(&i.version))
            .collect()
    } else {
        exact
    };
    let target = match matching.as_slice() {
        [] => {
            return Err(ManageError::NotInstalled {
                service: service.to_owned(),
                request: request.to_owned(),
                installed: names(&of_service.iter().collect::<Vec<_>>()),
            });
        }
        [one] => (*one).clone(),
        several => {
            return Err(ManageError::Ambiguous {
                service: service.to_owned(),
                request: request.to_owned(),
                matches: names(several),
            });
        }
    };

    // Windows lets a folder be renamed while a program from it runs (it only forbids deleting
    // the program), so look for busy files first instead of trusting the rename to refuse.
    if let Some(busy) = first_file_in_use(&target.path) {
        return Err(ManageError::InUse(busy));
    }
    remove_dir_safely(&paths.bin_dir(), &target.path)?;

    let service_dir = target
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    // `remove_dir` only removes an empty folder: other versions keep theirs.
    let service_folder_removed = fs::remove_dir(&service_dir).is_ok();
    Ok(Removed {
        version: target,
        service_folder_removed,
    })
}

/// A file under `dir` that cannot be opened for writing because a program has it loaded or
/// open, if any. Opening does not change the file.
fn first_file_in_use(dir: &Path) -> Option<PathBuf> {
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        let Ok(read) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && is_open_elsewhere(&path) {
                return Some(path);
            }
        }
    }
    None
}

/// Whether another program has `file` open or loaded: it cannot be opened for writing.
fn is_open_elsewhere(file: &Path) -> bool {
    fs::OpenOptions::new()
        .write(true)
        .open(file)
        .is_err_and(|e| is_busy(&e))
}

/// Whether an open error means "another program is using this file", as opposed to a missing
/// permission.
fn is_busy(error: &io::Error) -> bool {
    use io::ErrorKind::{ExecutableFileBusy, ResourceBusy};
    // Windows reports a sharing violation (32) or a lock violation (33) as an uncategorized
    // error; Unix reports a running executable as `ExecutableFileBusy`.
    matches!(error.kind(), ExecutableFileBusy | ResourceBusy)
        || (cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33)))
}

/// Renames `dir` into a work area inside `bin_dir` (all or nothing, and it fails while a
/// program from it runs), then deletes it. A deletion that fails half-way leaves the rest
/// in a hidden `.staging-*` folder, which the installer sweeps later; it never looks
/// installed.
fn remove_dir_safely(bin_dir: &Path, dir: &Path) -> Result<(), ManageError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    let doomed = bin_dir.join(format!(
        "{STAGING_PREFIX}removing-{}-{nanos}",
        process::id()
    ));
    fs::rename(dir, &doomed).map_err(|source| match source.kind() {
        io::ErrorKind::PermissionDenied => ManageError::InUse(dir.to_path_buf()),
        _ => ManageError::Io {
            action: "cannot move out of the way",
            path: dir.to_path_buf(),
            source,
        },
    })?;
    // The version is gone from the user's point of view; failing to free the disk space now
    // is not worth an error, the leftovers are swept on the next install.
    let _ = fs::remove_dir_all(&doomed);
    Ok(())
}

/// Sub-folders of `dir` whose names do not start with a dot (so `.staging-*` is skipped).
/// A missing `dir` has none.
fn visible_subdirs(dir: &Path) -> Result<Vec<PathBuf>, ManageError> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ManageError::Io {
                action: "cannot read",
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut dirs = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| ManageError::Io {
            action: "cannot read",
            path: dir.to_path_buf(),
            source,
        })?;
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && !entry.file_name().to_string_lossy().starts_with('.') {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests;
