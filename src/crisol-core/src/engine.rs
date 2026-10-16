//! The operations that only change files (F8.2).
//!
//! Everything the command line and, later, a GUI can ask for goes through
//! [`crisol_protocol::Call`]. The operations that act on files (`install`, `use`, `link`,
//! `secure`, `list`, `sites`, `logs`, `doctor`, …) live here, in the [`Engine`]: the daemon
//! answers them with it, and a client that finds no daemon runs them with the very same code, so
//! the answer is the same either way. The operations on running services are the
//! [`crate::daemon::Daemon`]'s.
//!
//! Nothing here blocks on anything but the file system and, for `install`, the download. Errors
//! become [`RpcError`]s with a code a client can act on, a sentence that can be shown as it is,
//! and, when there is one, the exact command that fixes the problem.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crisol_protocol::{
    Call, Check, CheckStatus, DoneInfo, ErrorCode, Event, InstalledInfo, LogChunk, RemovedInfo,
    Reply, RpcError, SiteInfo, Tool as WireTool, UsedInfo, VersionInfo, VersionList,
};

use crate::catalog::{Catalog, Target};
use crate::config::Config;
use crate::fetch::Fetch;
use crate::install::{InstallError, InstallOptions, Progress};
use crate::manage::{
    install_version, list_available, list_installed, uninstall, Environment, ManageError,
};
use crate::paths::{require_private_home, CrisolPaths, HomeError, PrivateDirs};
use crate::pin::{use_global, use_local, UseError, Used};
use crate::ports::PortProbe;
use crate::process::ProcessSupervisor;
use crate::project::FileOwners;
use crate::resolve::Tool;
use crate::shims::{shim_file_name, SHIM_COMMANDS};
use crate::sites::{
    discover, link, park, set_secure, unlink, unpark, Site, SiteKind, SitesError, SitesFile,
};

/// Checks that depend on the operating system, for `doctor`. The Windows implementation (the
/// PATH, the privileged helper, the NRPT rule, `hosts`, the trust store) is in
/// `crisol-platform`.
pub trait SystemChecks: Send + Sync {
    /// The checks, in the order they should be shown. Read only: nothing is changed.
    fn checks(&self, paths: &CrisolPaths) -> Vec<Check>;
}

/// No system checks.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSystemChecks;

impl SystemChecks for NoSystemChecks {
    fn checks(&self, _paths: &CrisolPaths) -> Vec<Check> {
        Vec::new()
    }
}

/// How terminals see the environment: a variable by name.
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Everything the operations need besides the Crisol home.
#[derive(Clone)]
pub struct Deps {
    /// Starts and stops processes.
    pub supervisor: Arc<dyn ProcessSupervisor>,
    /// Looks at ports.
    pub probe: Arc<dyn PortProbe>,
    /// Makes private folders and judges existing ones.
    pub dirs: Arc<dyn PrivateDirs>,
    /// Downloads.
    pub fetcher: Arc<dyn Fetch>,
    /// Who owns a file (for the `.crisol.toml` search).
    pub owners: Arc<dyn FileOwners>,
    /// The environment variables of the process.
    pub env: EnvLookup,
    /// The checks that need the operating system.
    pub system: Arc<dyn SystemChecks>,
    /// What can be installed.
    pub catalog: Catalog,
    /// The platform to install for.
    pub target: Target,
}

impl std::fmt::Debug for Deps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deps")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// Runs the operations that only change files.
#[derive(Debug, Clone)]
pub struct Engine {
    paths: CrisolPaths,
    deps: Deps,
}

/// Longest stretch of a log read in one go, in bytes.
const LOG_WINDOW: u64 = 256 * 1024;

impl Engine {
    /// An engine over this home.
    #[must_use]
    pub fn new(paths: CrisolPaths, deps: Deps) -> Self {
        Self { paths, deps }
    }

    /// The Crisol home.
    #[must_use]
    pub fn paths(&self) -> &CrisolPaths {
        &self.paths
    }

    /// The dependencies.
    #[must_use]
    pub fn deps(&self) -> &Deps {
        &self.deps
    }

    /// Does `call` if it is one of the file operations; `None` if it needs the daemon.
    /// `events` receives the progress of long operations (a download).
    pub fn handle(
        &self,
        call: &Call,
        events: &mut dyn FnMut(Event),
    ) -> Option<Result<Reply, RpcError>> {
        Some(match call {
            Call::List { available } => self.list(*available),
            Call::Install { service, version } => self.install(service, version, events),
            Call::Uninstall { service, version } => self.uninstall(service, version),
            Call::Use { tool, version, dir } => self.use_version(*tool, version, dir.as_deref()),
            Call::Park { path } => self.park(path),
            Call::Unpark { path } => self.unpark(path),
            Call::Link {
                name,
                path,
                docroot,
            } => self.link(name, path, docroot.as_deref()),
            Call::Unlink { name } => self.unlink(name),
            Call::Sites => self.sites(),
            Call::Secure { name } => self.secure(name, true),
            Call::Unsecure { name } => self.secure(name, false),
            Call::Logs {
                service,
                lines,
                from,
            } => self.logs(service, *lines, *from),
            Call::Doctor => Ok(self.doctor()),
            Call::Hello { .. }
            | Call::Status { .. }
            | Call::Start { .. }
            | Call::Stop { .. }
            | Call::Restart { .. }
            | Call::Shutdown
            | Call::Subscribe => return None,
        })
    }

    fn load_config(&self) -> Result<Config, RpcError> {
        Config::load_from(&self.paths)
            .map(|loaded| loaded.config)
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string()))
    }

    // ---- versions ---------------------------------------------------------------------

    fn list(&self, available: bool) -> Result<Reply, RpcError> {
        let installed = list_installed(&self.paths).map_err(manage_error)?;
        let info =
            |service: &str, version: String, installed: bool, eol: bool| VersionInfo {
                service: service.to_owned(),
                recommended: self.deps.catalog.recommended(service).is_some_and(|line| {
                    version == line || version.starts_with(&format!("{line}."))
                }),
                version,
                installed,
                end_of_life: eol,
            };
        let installed_info = installed
            .iter()
            .map(|i| {
                let eol = self.deps.catalog.is_end_of_life(&i.service, &i.version);
                info(&i.service, i.version.to_string(), true, eol)
            })
            .collect();
        let available_info = if available {
            list_available(&self.paths, &self.deps.catalog, self.deps.target)
                .map_err(manage_error)?
                .into_iter()
                .map(|a| {
                    info(
                        &a.package.service,
                        a.package.version.to_string(),
                        a.installed,
                        a.package.end_of_life,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Reply::Versions(VersionList {
            installed: installed_info,
            available: available_info,
        }))
    }

    fn install(
        &self,
        service: &str,
        version: &str,
        events: &mut dyn FnMut(Event),
    ) -> Result<Reply, RpcError> {
        let env = Environment {
            paths: &self.paths,
            catalog: &self.deps.catalog,
            target: self.deps.target,
            fetcher: self.deps.fetcher.as_ref(),
        };
        let what = format!("{service} {version}");
        let mut total: Option<u64> = None;
        let result = install_version(
            &env,
            service,
            version,
            &InstallOptions::default(),
            &mut |progress| {
                if let Progress::Downloading { done, total: t } = progress {
                    total = t.or(total);
                    events(Event::Progress {
                        what: what.clone(),
                        done,
                        total,
                    });
                }
            },
            self.deps.dirs.as_ref(),
        );
        match result {
            Ok(installed) => {
                let (name, number) = installed_name(&installed.path);
                Ok(Reply::Installed(InstalledInfo {
                    service: name.unwrap_or_else(|| service.to_owned()),
                    version: number,
                    path: installed.path.display().to_string(),
                    already: false,
                    end_of_life: installed.end_of_life,
                    downloaded: installed.downloaded,
                }))
            }
            Err(ManageError::Install(InstallError::AlreadyInstalled(path))) => {
                let (name, number) = installed_name(&path);
                Ok(Reply::Installed(InstalledInfo {
                    service: name.unwrap_or_else(|| service.to_owned()),
                    version: number,
                    path: path.display().to_string(),
                    already: true,
                    end_of_life: false,
                    downloaded: 0,
                }))
            }
            Err(error) => Err(manage_error(error)),
        }
    }

    fn uninstall(&self, service: &str, version: &str) -> Result<Reply, RpcError> {
        let removed = uninstall(&self.paths, service, version, self.deps.dirs.as_ref())
            .map_err(manage_error)?;
        Ok(Reply::Removed(RemovedInfo {
            service: removed.version.service,
            version: removed.version.version.to_string(),
        }))
    }

    fn use_version(
        &self,
        tool: WireTool,
        version: &str,
        dir: Option<&str>,
    ) -> Result<Reply, RpcError> {
        let tool = match tool {
            WireTool::Php => Tool::Php,
            WireTool::Node => Tool::Node,
        };
        let env = |name: &str| (self.deps.env)(name);
        let used = match dir {
            None => use_global(&self.paths, tool, version, &env),
            Some(dir) => use_local(
                &self.paths,
                Path::new(dir),
                tool,
                version,
                &env,
                self.deps.owners.as_ref(),
                None,
            ),
        }
        .map_err(use_error)?;
        Ok(Reply::Used(used_info(&used)))
    }

    // ---- sites ------------------------------------------------------------------------

    fn park(&self, path: &str) -> Result<Reply, RpcError> {
        let changed = park(&self.paths, Path::new(path)).map_err(sites_error)?;
        Ok(done(
            if changed {
                format!("Parked {path}: every folder in it is a site")
            } else {
                format!("{path} was already parked")
            },
            changed,
        ))
    }

    fn unpark(&self, path: &str) -> Result<Reply, RpcError> {
        let changed = unpark(&self.paths, Path::new(path)).map_err(sites_error)?;
        if !changed {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                format!("{path} is not a parked folder"),
            ));
        }
        Ok(done(format!("Unparked {path}"), true))
    }

    fn link(&self, name: &str, path: &str, docroot: Option<&str>) -> Result<Reply, RpcError> {
        let changed = link(&self.paths, name, Path::new(path), docroot).map_err(sites_error)?;
        Ok(done(
            if changed {
                format!("Linked {name} to {path}")
            } else {
                format!("{name} already points to {path}")
            },
            changed,
        ))
    }

    fn unlink(&self, name: &str) -> Result<Reply, RpcError> {
        if !unlink(&self.paths, name).map_err(sites_error)? {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                format!("there is no linked site called `{name}`"),
            ));
        }
        Ok(done(format!("Unlinked {name}"), true))
    }

    fn sites(&self) -> Result<Reply, RpcError> {
        let (list, _) = self.discover()?;
        Ok(Reply::Sites {
            sites: list.sites.iter().map(site_info).collect(),
            warnings: list.warnings,
        })
    }

    fn secure(&self, name: &str, on: bool) -> Result<Reply, RpcError> {
        let (list, _) = self.discover()?;
        if !list.sites.iter().any(|s| s.name == name) {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                format!("there is no site called `{name}`; `crisol sites` lists them"),
            ));
        }
        let changed = set_secure(&self.paths, name, on).map_err(sites_error)?;
        let tld = self.load_config()?.sites.tld;
        let message = match (on, changed) {
            (true, true) => format!("{name}.{tld} will be served over HTTPS"),
            (true, false) => format!("{name}.{tld} was already served over HTTPS"),
            (false, true) => format!("{name}.{tld} is served over HTTP only"),
            (false, false) => format!("{name}.{tld} was not served over HTTPS"),
        };
        Ok(done(message, changed))
    }

    /// The sites that exist now, and the configuration they were found with.
    fn discover(&self) -> Result<(crate::sites::SiteList, Config), RpcError> {
        let config = self.load_config()?;
        let linked = SitesFile::load(&self.paths)
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string()))?;
        Ok((discover(&self.paths, &config, &linked), config))
    }

    // ---- logs and doctor --------------------------------------------------------------

    fn logs(&self, service: &str, lines: usize, from: Option<u64>) -> Result<Reply, RpcError> {
        let path = self
            .paths
            .runtime_log(service)
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string()))?;
        if !path.is_file() {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                format!("there is no log for `{service}` yet ({})", path.display()),
            ));
        }
        read_log(&path, lines, from).map(Reply::Logs).map_err(|e| {
            RpcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {e}", path.display()),
            )
        })
    }

    fn doctor(&self) -> Reply {
        let mut checks = vec![
            self.check_home(),
            self.check_config(),
            self.check_installed(),
            self.check_shims(),
            self.check_sites(),
        ];
        checks.extend(self.deps.system.checks(&self.paths));
        Reply::Doctor { checks }
    }

    fn check_home(&self) -> Check {
        let name = "home permissions".to_owned();
        match require_private_home(&self.paths, self.deps.dirs.as_ref()) {
            Ok(()) => Check {
                name,
                status: CheckStatus::Ok,
                detail: format!(
                    "only you, the system and administrators can write in {}",
                    self.paths.home().display()
                ),
                fix: None,
            },
            Err(error) => {
                let rpc = home_error(&error);
                Check {
                    name,
                    status: CheckStatus::Problem,
                    detail: rpc.message,
                    fix: rpc.fix,
                }
            }
        }
    }

    fn check_config(&self) -> Check {
        let name = "configuration".to_owned();
        match Config::load_from(&self.paths) {
            Ok(loaded) if loaded.warnings.is_empty() => Check {
                name,
                status: CheckStatus::Ok,
                detail: format!("{} is valid", self.paths.config_file().display()),
                fix: None,
            },
            Ok(loaded) => Check {
                name,
                status: CheckStatus::Warning,
                detail: loaded
                    .warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
                fix: Some(format!("edit {}", self.paths.config_file().display())),
            },
            Err(error) => Check {
                name,
                status: CheckStatus::Problem,
                detail: error.to_string(),
                fix: Some(format!("fix {}", self.paths.config_file().display())),
            },
        }
    }

    fn check_installed(&self) -> Check {
        let name = "installed tools".to_owned();
        let installed = match list_installed(&self.paths) {
            Ok(list) => list,
            Err(error) => {
                return Check {
                    name,
                    status: CheckStatus::Problem,
                    detail: error.to_string(),
                    fix: None,
                }
            }
        };
        let has = |service: &str| installed.iter().any(|i| i.service == service);
        let missing: Vec<&str> = ["nginx", "php"].into_iter().filter(|s| !has(s)).collect();
        if missing.is_empty() {
            Check {
                name,
                status: CheckStatus::Ok,
                detail: installed
                    .iter()
                    .map(|i| format!("{} {}", i.service, i.version))
                    .collect::<Vec<_>>()
                    .join(", "),
                fix: None,
            }
        } else {
            Check {
                name,
                status: CheckStatus::Warning,
                detail: format!("not installed: {}", missing.join(", ")),
                fix: Some(format!("crisol install {}", missing[0])),
            }
        }
    }

    fn check_shims(&self) -> Check {
        let name = "shims".to_owned();
        let dir = self.paths.shims_dir();
        let missing: Vec<String> = SHIM_COMMANDS
            .iter()
            .map(|c| shim_file_name(c))
            .filter(|f| !dir.join(f).is_file())
            .collect();
        if missing.is_empty() {
            Check {
                name,
                status: CheckStatus::Ok,
                detail: format!("{} has php, node, npm, npx and composer", dir.display()),
                fix: None,
            }
        } else {
            Check {
                name,
                status: CheckStatus::Warning,
                detail: format!("missing in {}: {}", dir.display(), missing.join(", ")),
                fix: Some("crisol shims install".to_owned()),
            }
        }
    }

    fn check_sites(&self) -> Check {
        let name = "sites".to_owned();
        match self.discover() {
            Ok((list, config)) if list.warnings.is_empty() => Check {
                name,
                status: CheckStatus::Ok,
                detail: format!("{} site(s) under .{}", list.sites.len(), config.sites.tld),
                fix: None,
            },
            Ok((list, _)) => Check {
                name,
                status: CheckStatus::Warning,
                detail: list.warnings.join("; "),
                fix: None,
            },
            Err(error) => Check {
                name,
                status: CheckStatus::Problem,
                detail: error.message,
                fix: None,
            },
        }
    }
}

// ---- conversions ----------------------------------------------------------------------

fn done(message: String, changed: bool) -> Reply {
    Reply::Done(DoneInfo {
        message,
        changed,
        warnings: Vec::new(),
    })
}

fn site_info(site: &Site) -> SiteInfo {
    SiteInfo {
        name: site.name.clone(),
        domain: site.domain.clone(),
        root: site.root.display().to_string(),
        docroot: site.docroot.display().to_string(),
        kind: match site.kind {
            SiteKind::Parked => "parked",
            SiteKind::Linked => "linked",
        }
        .to_owned(),
        secure: site.secure,
    }
}

fn used_info(used: &Used) -> UsedInfo {
    UsedInfo {
        file: used.file.display().to_string(),
        request: used.request.clone(),
        selects: format!("{} {}", used.selects.service, used.selects.version),
        previous: used.previous.clone(),
        changed: used.changed,
        warnings: used.warnings.clone(),
    }
}

/// `(service, version)` from an installation folder `bin/<service>/<version>`.
fn installed_name(path: &Path) -> (Option<String>, String) {
    let version = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let service = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    (service, version)
}

/// A [`HomeError`] as an error a client can show: the sentence, and the command apart.
#[must_use]
pub fn home_error(error: &HomeError) -> RpcError {
    match error {
        HomeError::Exposed { path, fix } => RpcError::new(
            ErrorCode::HomeExposed,
            format!(
                "`{}` can be modified by other accounts of this computer, and Crisol runs and trusts what is inside it, so it will not go on",
                path.display()
            ),
        )
        .with_fix(fix.clone()),
        HomeError::Check { .. } => RpcError::new(ErrorCode::Io, error.to_string()),
    }
}

fn manage_error(error: ManageError) -> RpcError {
    match error {
        ManageError::Home(home) => home_error(&home),
        ManageError::Catalog(e) => RpcError::new(ErrorCode::NotFound, e.to_string()),
        ManageError::NotInstalled { .. } => RpcError::new(ErrorCode::NotFound, error.to_string()),
        ManageError::Ambiguous { .. } | ManageError::Version(_) | ManageError::Paths(_) => {
            RpcError::new(ErrorCode::InvalidParams, error.to_string())
        }
        ManageError::InUse(_) => RpcError::new(ErrorCode::Conflict, error.to_string()),
        ManageError::UnsupportedPlatform => {
            RpcError::new(ErrorCode::NotAvailable, error.to_string())
        }
        ManageError::Install(_) | ManageError::Io { .. } => {
            RpcError::new(ErrorCode::Io, error.to_string())
        }
    }
}

fn use_error(error: UseError) -> RpcError {
    match error {
        UseError::BadVersion(_) => RpcError::new(ErrorCode::InvalidParams, error.to_string()),
        UseError::NotInstalled { .. } | UseError::NotAFolder(_) => {
            RpcError::new(ErrorCode::NotFound, error.to_string())
        }
        UseError::Manage(e) => manage_error(e),
        UseError::Edit(_) => RpcError::new(ErrorCode::Io, error.to_string()),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn sites_error(error: SitesError) -> RpcError {
    match error {
        SitesError::Invalid { .. } => RpcError::new(ErrorCode::InvalidParams, error.to_string()),
        SitesError::NotAFolder(_) => RpcError::new(ErrorCode::NotFound, error.to_string()),
        SitesError::Edit(_) => RpcError::new(ErrorCode::Io, error.to_string()),
    }
}

// ---- logs ------------------------------------------------------------------------------

/// The last `lines` lines of `path`, or what was added after byte `from`. A `from` beyond the
/// end of the file means it was rotated: it is read from the start.
fn read_log(path: &Path, lines: usize, from: Option<u64>) -> std::io::Result<LogChunk> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = match from {
        Some(offset) if offset <= len => offset,
        Some(_) => 0,
        None => len.saturating_sub(LOG_WINDOW),
    };
    // A window of a big file starts in the middle of a line: that line is dropped.
    let skip_first = from.is_none() && start > 0;
    let window = if from.is_some() {
        len - start
    } else {
        len.min(LOG_WINDOW)
    };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(window.min(LOG_WINDOW * 4))
        .read_to_end(&mut bytes)?;
    let end = start + bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.lines().collect();
    if skip_first && !all.is_empty() {
        all.remove(0);
    }
    // For a continuation, a last line without its newline is not finished: wait for it.
    let mut next = end;
    if from.is_some() && !text.ends_with('\n') {
        if let Some(partial) = all.pop() {
            next -= partial.len() as u64;
        }
    }
    let keep = if from.is_some() { all.len() } else { lines };
    let tail = all.len().saturating_sub(keep);
    Ok(LogChunk {
        lines: all[tail..].iter().map(|l| (*l).to_owned()).collect(),
        next,
        path: path.display().to_string(),
    })
}

/// The path of a log, for tests and for clients that follow it themselves.
#[must_use]
pub fn log_path(paths: &CrisolPaths, service: &str) -> Option<PathBuf> {
    paths.runtime_log(service).ok()
}

#[cfg(test)]
mod tests;
