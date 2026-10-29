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
use crate::install::{
    install, install_from, is_unverified, InstallError, InstallOptions, Installed, Origin,
    Progress, STAGING_PREFIX,
};
use crate::paths::{require_private_home, CrisolPaths, HomeError, PathsError, PrivateDirs};
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
    /// The home is open to other accounts: nothing is installed or removed.
    #[error(transparent)]
    Home(#[from] HomeError),
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
    /// `--from` needs the exact version (`8.4.28`), not a line or `latest`.
    #[error("`--from` needs an exact version such as 8.4.28, not `{0}`")]
    FromNeedsExactVersion(String),
    /// `--sha256` is not 64 hexadecimal digits.
    #[error("`--sha256` must be 64 hexadecimal digits")]
    FromBadHash,
    /// The catalog already has that version with another hash: `--from` cannot override it.
    #[error(
        "the catalog already has {service} {version} with sha256 {expected}; \
         `--from` cannot replace it (the file you gave has {given})"
    )]
    FromContradictsCatalog {
        /// Tool.
        service: String,
        /// Version.
        version: String,
        /// The catalog's hash.
        expected: String,
        /// The hash given.
        given: String,
    },
    /// The catalog does not know the tool, so there is no layout to unpack it with.
    #[error("the catalog has no {0} to take the archive layout from; `--from` needs one")]
    FromUnknownService(String),
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
    dirs: &dyn PrivateDirs,
) -> Result<Installed, ManageError> {
    require_private_home(env.paths, dirs)?;
    let package = if request == "latest" {
        env.catalog.latest(service, env.target)?
    } else {
        env.catalog.find(service, request, env.target)?
    };
    Ok(install(package, env.paths, env.fetcher, options, progress)?)
}

/// An archive the user downloaded and the SHA-256 they vouch for (`install --from`, ADR-0020).
#[derive(Debug, Clone, Copy)]
pub struct LocalArchive<'a> {
    /// The file on disk.
    pub path: &'a Path,
    /// Its SHA-256 as the user typed it, from the provider's page.
    pub sha256: &'a str,
}

/// Installs `service` `version` from an archive the user already downloaded (ADR-0020).
///
/// The way out when the catalog built into this `crisol.exe` lacks a version (a security fix
/// published after it was built). `sha256` is the hash the user vouches for and it is checked
/// before anything is unpacked. The archive layout (`format`, `root`, executable) is that of
/// the highest version of the tool in the catalog. The result is `unverified` unless the
/// catalog has this exact version with this same hash, in which case this is just an offline
/// install; a catalog version with another hash is refused.
pub fn install_version_from(
    env: &Environment<'_>,
    service: &str,
    version: &str,
    archive: LocalArchive<'_>,
    options: &InstallOptions,
    progress: &mut dyn FnMut(Progress),
    dirs: &dyn PrivateDirs,
) -> Result<Installed, ManageError> {
    require_private_home(env.paths, dirs)?;
    let parsed = Version::parse(version)?;
    if parsed.is_prerelease() || parsed.parts().len() < 3 {
        return Err(ManageError::FromNeedsExactVersion(version.to_owned()));
    }
    let given = normalize_hash(archive.sha256).ok_or(ManageError::FromBadHash)?;

    let in_catalog = env
        .catalog
        .available(service, env.target)
        .into_iter()
        .find(|p| p.version == parsed);
    let (package, unverified) = if let Some(known) = in_catalog {
        if known.sha256 != given {
            return Err(ManageError::FromContradictsCatalog {
                service: service.to_owned(),
                version: known.version.to_string(),
                expected: known.sha256.clone(),
                given,
            });
        }
        (known.clone(), false)
    } else {
        let template = env
            .catalog
            .template(service, env.target)
            .ok_or_else(|| ManageError::FromUnknownService(service.to_owned()))?;
        let package = Package {
            version: parsed,
            sha256: given,
            size: None,
            end_of_life: false,
            ..template.clone()
        };
        (package, true)
    };
    let origin = Origin::Local {
        path: archive.path,
        unverified,
    };
    Ok(install_from(
        &package, env.paths, origin, options, progress,
    )?)
}

fn normalize_hash(text: &str) -> Option<String> {
    let text = text.trim();
    (text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// Whether the installation in `version.path` was made with `--from` (ADR-0020).
#[must_use]
pub fn is_unverified_install(version: &InstalledVersion) -> bool {
    is_unverified(&version.path)
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
    dirs: &dyn PrivateDirs,
) -> Result<Removed, ManageError> {
    require_private_home(paths, dirs)?;
    paths.bin_for(service, "x")?; // the service name must be a safe folder name
    let wanted = VersionReq::parse(request)?;
    let of_service: Vec<InstalledVersion> = list_installed(paths)?
        .into_iter()
        .filter(|i| i.service == service)
        .collect();
    let names = |versions: &[&InstalledVersion]| -> Vec<String> {
        versions.iter().map(|i| i.version.to_string()).collect()
    };

    // "Exact" is the folder name as written: `8.4` and `8.4.0` compare equal as versions, but
    // `8.4` must stay a prefix when `8.4.0` and `8.4.26` are both installed.
    let exact: Vec<&InstalledVersion> = of_service
        .iter()
        .filter(|i| i.version.to_string() == request)
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
    fs::rename(dir, &doomed).map_err(|source| {
        // A program whose working directory is inside the folder gives a sharing violation
        // (32), which is not `PermissionDenied` but means the same thing here.
        if source.kind() == io::ErrorKind::PermissionDenied || is_busy(&source) {
            ManageError::InUse(dir.to_path_buf())
        } else {
            ManageError::Io {
                action: "cannot move out of the way",
                path: dir.to_path_buf(),
                source,
            }
        }
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
