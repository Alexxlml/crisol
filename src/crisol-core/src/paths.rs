//! Where everything lives under `CRISOL_HOME`.
//!
//! Every path handed out by [`CrisolPaths`] is built with [`Path::join`] from a validated
//! component; nothing is ever assembled by concatenating strings. The default home is
//! platform specific, so it is supplied by the caller (see `crisol-platform`).

pub mod fake;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that overrides the Crisol home directory.
pub const HOME_ENV_VAR: &str = "CRISOL_HOME";

/// File names and directory names inside `CRISOL_HOME`.
mod names {
    pub const CONFIG_FILE: &str = "crisol.toml";
    pub const SITES_FILE: &str = "sites.toml";
    pub const STATE: &str = "state";
    pub const BIN: &str = "bin";
    pub const ETC: &str = "etc";
    pub const OVERRIDES: &str = "overrides";
    pub const DATA: &str = "data";
    pub const LOGS: &str = "logs";
    pub const RUNTIME_LOG: &str = "runtime.log";
    pub const CERTS: &str = "certs";
    pub const SHIMS: &str = "shims";
    pub const WWW: &str = "www";
}

/// Errors produced while resolving or preparing the directory layout.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// `CRISOL_HOME` (or the default) is not an absolute path.
    #[error("the Crisol home must be an absolute path, got `{}`", .0.display())]
    NotAbsolute(PathBuf),
    /// No `CRISOL_HOME` was set and the platform could not provide a default.
    #[error("CRISOL_HOME is not set and no default home directory is available")]
    NoHome,
    /// The home holds a character that breaks the `PATH` entry for its `shims` folder.
    #[error(
        "the Crisol home `{}` contains `{ch}`, which cannot be part of a PATH entry: choose another folder",
        .path.display()
    )]
    UnusableCharacter {
        /// The home that was given.
        path: PathBuf,
        /// The offending character (`;` separates PATH entries; `%` starts a variable).
        ch: char,
    },
    /// A service name, version or other path component is not allowed.
    #[error("invalid path component `{0}`: use letters, digits, `.`, `_`, `-` or `+`")]
    InvalidComponent(String),
    /// A directory could not be created.
    #[error("cannot create `{}`: {source}", path.display())]
    Create {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The home exists and belongs to another account of this computer.
    #[error("{}", other_account_message(path))]
    OtherAccount {
        /// The home.
        path: PathBuf,
    },
}

/// The sentence for a home that is another person's. The default home (`C:\Crisol`) is the
/// same for every account, so on a computer used by two people the second finds the first's.
fn other_account_message(path: &Path) -> String {
    format!(
        "`{}` belongs to another account of this computer. Each person who uses Crisol needs \
         their own home: set the CRISOL_HOME environment variable to a folder of yours (in cmd: \
         setx CRISOL_HOME \"%USERPROFILE%\\Crisol\") and open a new terminal",
        path.display()
    )
}

/// Creates folders that only the current user (and the system) can write to, and tells
/// whether an existing one is open to others. The implementation is per operating system
/// (`crisol-platform`); the core only decides when to ask.
///
/// Why: a home like `C:\Crisol` inherits write access for every account on the machine from
/// the drive root, which would let any of them replace a binary Crisol later runs.
pub trait PrivateDirs: Send + Sync {
    /// Creates `dir` (its parent must exist) restricted to the current user, the system and
    /// the administrators, and inherited by everything created inside. If `dir` already
    /// exists it is left exactly as it is. Returns whether it was created.
    fn create_private_dir(&self, dir: &Path) -> io::Result<bool>;

    /// Whether accounts other than the current user, the system and the administrators can
    /// write in `dir` or in what is created inside it.
    fn is_writable_by_others(&self, dir: &Path) -> io::Result<bool>;

    /// Whether `path` is owned by an ordinary account other than the current user (not the
    /// system or the administrators, who may own what an installer made). A home like that is
    /// somebody else's, whatever its permissions say. Platforms without owners say `false`.
    fn owned_by_another_account(&self, _path: &Path) -> io::Result<bool> {
        Ok(false)
    }

    /// The stricter rule for what runs with administrator rights (`crisol-elevate`, ADR-0014):
    /// whether **anybody but the administrators, the system and the system's installer** can
    /// write to `path` (a file or a folder), or owns it. The current user counts as somebody
    /// else here: the program must not be something the user can replace without elevating.
    fn is_writable_by_non_administrators(&self, path: &Path) -> io::Result<bool>;

    /// The exact command that closes `path` to other accounts, for messages. `None` when this
    /// platform has no such command to give.
    fn fix_command(&self, _path: &Path) -> Option<String> {
        None
    }
}

/// Why Crisol will not run or generate anything from this home.
#[derive(Debug, thiserror::Error)]
pub enum HomeError {
    /// The home, or a folder of it that Crisol runs or trusts, can be written by other accounts.
    #[error(
        "`{}` can be modified by other accounts of this computer, and Crisol runs and trusts \
         what is inside it, so it will not go on. Remove their write access: {fix}",
        path.display()
    )]
    Exposed {
        /// The folder that is open.
        path: PathBuf,
        /// What to do about it (the exact command, when the platform has one).
        fix: String,
    },
    /// The home, or a folder in it, belongs to another account of this computer.
    #[error("{}", other_account_message(path))]
    OtherAccount {
        /// The folder that is not this user's.
        path: PathBuf,
    },
    /// The permissions of a folder could not be read.
    #[error("cannot check who can write to `{}`: {source}", path.display())]
    Check {
        /// The folder.
        path: PathBuf,
        /// What failed.
        source: io::Error,
    },
}

/// The common check before anything that runs or establishes trust (ADR-0008, corrected
/// 2026-10-16): the home and the folders Crisol executes from or trusts (`bin/`, `etc/`,
/// `certs/`, `shims/`) must not be writable by other accounts. Folders that do not exist yet
/// are skipped. Nothing is ever changed: a folder that exists keeps its permissions, and the
/// error says how to close it. If the permissions cannot be read, it fails as well.
///
/// Read-only operations (`list_installed`, `list_available`, `status`) do not call this.
pub fn require_private_home(paths: &CrisolPaths, dirs: &dyn PrivateDirs) -> Result<(), HomeError> {
    for path in [
        paths.home().to_path_buf(),
        paths.bin_dir(),
        paths.etc_dir(),
        paths.certs_dir(),
        paths.shims_dir(),
        // Where PHP of the projects runs from, if the user parks nothing else.
        paths.www_dir(),
    ] {
        if !path.exists() {
            continue;
        }
        if foreign_owner(dirs, &path)? {
            return Err(HomeError::OtherAccount { path });
        }
        let open = dirs
            .is_writable_by_others(&path)
            .map_err(|source| HomeError::Check {
                path: path.clone(),
                source,
            })?;
        if open {
            let fix = dirs.fix_command(&path).unwrap_or_else(|| {
                format!(
                    "remove the write access of other accounts on `{}`",
                    path.display()
                )
            });
            return Err(HomeError::Exposed { path, fix });
        }
    }
    Ok(())
}

/// Whether the home that already exists is another account's (reading its owner may itself be
/// denied: then it is not this user's).
fn home_is_another_accounts(private: &dyn PrivateDirs, home: &Path) -> bool {
    match private.owned_by_another_account(home) {
        Ok(other) => other,
        Err(e) => e.kind() == io::ErrorKind::PermissionDenied,
    }
}

/// Whether the folder is another account's. If its owner cannot even be read because access is
/// denied, it is not this user's either.
fn foreign_owner(dirs: &dyn PrivateDirs, path: &Path) -> Result<bool, HomeError> {
    match dirs.owned_by_another_account(path) {
        Ok(other) => Ok(other),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Ok(true),
        Err(source) => Err(HomeError::Check {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The `icacls` command that leaves `path` writable only by the user with SID `user_sid`, the
/// system and the administrators, with no inheritance from the parent. Folders get the
/// inheritable flags (`(OI)(CI)`) so that what is created inside is private too; a file does
/// not (the flags mean nothing for it). It is written to work as typed in `cmd` and in
/// PowerShell: the user is named by SID (`%USERNAME%` is `cmd` only), every grant is one quoted
/// word, and nothing in it is expanded by either shell (a path with a `$` or a backtick would
/// be, in PowerShell).
///
/// Besides cutting the inheritance and granting the three, it removes whatever was granted
/// *explicitly* to Everyone (`S-1-1-0`), Authenticated Users (`S-1-5-11`) and Users
/// (`S-1-5-32-545`): a folder opened by a direct grant, not only by inheritance, is closed too.
/// An explicit grant to some other named account cannot be listed generically; `doctor`
/// (F11.1) shows the folder's permissions for that case.
#[must_use]
pub fn private_acl_fix_command(path: &Path, is_dir: bool, user_sid: &str) -> String {
    let flags = if is_dir { "(OI)(CI)" } else { "" };
    let grant = |who: &str| format!("\"*{who}:{flags}F\"");
    format!(
        "icacls \"{}\" /inheritance:r /grant:r {} {} {} /remove:g \"*S-1-1-0\" \"*S-1-5-11\" \"*S-1-5-32-545\"",
        path.display(),
        grant(user_sid),
        grant("S-1-5-18"),
        grant("S-1-5-32-544"),
    )
}

/// How the home directory stood after [`CrisolPaths::ensure_private_layout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeSecurity {
    /// The home did not exist and was created private.
    Created,
    /// The home existed and nobody else can write in it.
    Private,
    /// The home existed and other accounts can write in it. Crisol does not change an
    /// existing folder's permissions on its own: the caller should warn (`doctor`).
    WritableByOthers,
}

/// Resolved locations inside a Crisol home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrisolPaths {
    home: PathBuf,
}

impl CrisolPaths {
    /// Uses `home` as the Crisol home. It must be absolute.
    pub fn new(home: impl Into<PathBuf>) -> Result<Self, PathsError> {
        let home = home.into();
        if !home.is_absolute() {
            return Err(PathsError::NotAbsolute(home));
        }
        // The shims folder goes into PATH, where `;` splits an entry in two and `%` is
        // expanded as a variable (review finding 7).
        if let Some(ch) = home
            .to_string_lossy()
            .chars()
            .find(|c| matches!(c, ';' | '%'))
        {
            return Err(PathsError::UnusableCharacter { path: home, ch });
        }
        Ok(Self { home })
    }

    /// Resolves the home from the value of `CRISOL_HOME`, falling back to `default_home`.
    ///
    /// An unset or empty `env_home` counts as "not set". The environment is read by the
    /// caller, which keeps this function pure and testable.
    pub fn resolve(
        env_home: Option<OsString>,
        default_home: impl FnOnce() -> Option<PathBuf>,
    ) -> Result<Self, PathsError> {
        match env_home.filter(|value| !value.is_empty()) {
            Some(value) => Self::new(PathBuf::from(value)),
            None => Self::new(default_home().ok_or(PathsError::NoHome)?),
        }
    }

    /// Reads `CRISOL_HOME` from the process environment and falls back to `default_home`.
    pub fn from_env(default_home: impl FnOnce() -> Option<PathBuf>) -> Result<Self, PathsError> {
        Self::resolve(std::env::var_os(HOME_ENV_VAR), default_home)
    }

    /// The Crisol home directory.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Global user configuration: `crisol.toml`. Owned by the user.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.home.join(names::CONFIG_FILE)
    }

    /// Linked and parked sites: `sites.toml`. Edited only through commands.
    #[must_use]
    pub fn sites_file(&self) -> PathBuf {
        self.home.join(names::SITES_FILE)
    }

    /// Internal state (PIDs, active versions, cache). Safe to delete.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.home.join(names::STATE)
    }

    /// Downloaded binaries, untouched: `bin/`.
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.home.join(names::BIN)
    }

    /// Binaries of one installed version: `bin/<service>/<version>/`.
    pub fn bin_for(&self, service: &str, version: &str) -> Result<PathBuf, PathsError> {
        Ok(self
            .bin_dir()
            .join(validate_component(service)?)
            .join(validate_component(version)?))
    }

    /// Generated configuration root: `etc/`. Derived; regenerated, never edited by hand.
    #[must_use]
    pub fn etc_dir(&self) -> PathBuf {
        self.home.join(names::ETC)
    }

    /// Generated configuration of one service: `etc/<service>/`.
    pub fn etc_for(&self, service: &str) -> Result<PathBuf, PathsError> {
        Ok(self.etc_dir().join(validate_component(service)?))
    }

    /// User fragments included by the generated configuration: `etc/<service>/overrides/`.
    pub fn overrides_for(&self, service: &str) -> Result<PathBuf, PathsError> {
        Ok(self.etc_for(service)?.join(names::OVERRIDES))
    }

    /// Generated `php.ini` of one PHP version: `etc/php/<version>/php.ini`. Derived; see
    /// [`crate::phpini`].
    pub fn php_ini(&self, version: &str) -> Result<PathBuf, PathsError> {
        Ok(self
            .etc_for("php")?
            .join(validate_component(version)?)
            .join("php.ini"))
    }

    /// The CA bundle PHP's cURL and OpenSSL use: `etc/php/cacert.pem`. Derived; see
    /// [`crate::cacert`].
    #[must_use]
    pub fn php_cacert(&self) -> PathBuf {
        self.etc_dir().join("php").join("cacert.pem")
    }

    /// Database and service data root: `data/`.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.home.join(names::DATA)
    }

    /// Data of one service, split by major version: `data/<service>/<major>/`.
    pub fn data_for(&self, service: &str, major: &str) -> Result<PathBuf, PathsError> {
        Ok(self
            .data_dir()
            .join(validate_component(service)?)
            .join(validate_component(major)?))
    }

    /// Logs root: `logs/`.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.home.join(names::LOGS)
    }

    /// Logs of one service: `logs/<service>/`.
    pub fn logs_for(&self, service: &str) -> Result<PathBuf, PathsError> {
        Ok(self.logs_dir().join(validate_component(service)?))
    }

    /// Combined stdout and stderr of one service: `logs/<service>/runtime.log`.
    pub fn runtime_log(&self, service: &str) -> Result<PathBuf, PathsError> {
        Ok(self.logs_for(service)?.join(names::RUNTIME_LOG))
    }

    /// Local CA and per-site certificates: `certs/`.
    #[must_use]
    pub fn certs_dir(&self) -> PathBuf {
        self.home.join(names::CERTS)
    }

    /// The local CA certificate (public): `certs/ca.pem`. See [`crate::ca`].
    #[must_use]
    pub fn ca_cert(&self) -> PathBuf {
        self.certs_dir().join("ca.pem")
    }

    /// The local CA certificate as Node gets it: `etc/node/ca.pem`. A copy the core writes
    /// only from a CA it has validated, so the shim can point `NODE_EXTRA_CA_CERTS` at it
    /// without reading `certs/` (ADR-0013).
    pub fn node_ca(&self) -> Result<PathBuf, PathsError> {
        Ok(self.etc_for("node")?.join("ca.pem"))
    }

    /// The certificate for the sites (wildcard, signed by the local CA): `certs/sites.pem`.
    /// Derived: renewed whenever it is not current. See [`crate::tls`].
    #[must_use]
    pub fn site_cert(&self) -> PathBuf {
        self.certs_dir().join("sites.pem")
    }

    /// The private key of the sites certificate: `certs/sites.key`.
    #[must_use]
    pub fn site_key(&self) -> PathBuf {
        self.certs_dir().join("sites.key")
    }

    /// The private key of the local CA: `certs/ca.key`. Never leaves the private home.
    #[must_use]
    pub fn ca_key(&self) -> PathBuf {
        self.certs_dir().join("ca.key")
    }

    /// The only directory that enters the user `PATH`: `shims/`.
    #[must_use]
    pub fn shims_dir(&self) -> PathBuf {
        self.home.join(names::SHIMS)
    }

    /// Default parked directory: `www/`.
    #[must_use]
    pub fn www_dir(&self) -> PathBuf {
        self.home.join(names::WWW)
    }

    /// Like [`ensure_layout`](Self::ensure_layout), but a home that does not exist yet is
    /// created through `private`, so it (and everything below, by inheritance) is writable
    /// only by the current user. A home that already exists is never re-permissioned; the
    /// result says whether it is open to others.
    pub fn ensure_private_layout(
        &self,
        private: &dyn PrivateDirs,
    ) -> Result<HomeSecurity, PathsError> {
        let create = |path: &Path, source| PathsError::Create {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = self.home.parent() {
            fs::create_dir_all(parent).map_err(|e| create(parent, e))?;
        }
        let created = private
            .create_private_dir(&self.home)
            .map_err(|e| create(&self.home, e))?;
        if !created && home_is_another_accounts(private, &self.home) {
            return Err(PathsError::OtherAccount {
                path: self.home.clone(),
            });
        }
        let security = if created {
            HomeSecurity::Created
        } else if private
            .is_writable_by_others(&self.home)
            .map_err(|e| create(&self.home, e))?
        {
            HomeSecurity::WritableByOthers
        } else {
            HomeSecurity::Private
        };
        self.ensure_layout()?;
        Ok(security)
    }

    /// Creates the home and every top-level directory that is missing.
    ///
    /// Existing directories and files are left alone, and the user files (`crisol.toml`,
    /// `sites.toml`) are never created or touched. Per-service directories are created on
    /// demand by the code that needs them.
    pub fn ensure_layout(&self) -> Result<(), PathsError> {
        let dirs = [
            self.home.clone(),
            self.state_dir(),
            self.bin_dir(),
            self.etc_dir(),
            self.data_dir(),
            self.logs_dir(),
            self.certs_dir(),
            self.shims_dir(),
            self.www_dir(),
        ];
        for path in dirs {
            fs::create_dir_all(&path).map_err(|source| PathsError::Create { path, source })?;
        }
        Ok(())
    }
}

/// Whether Windows treats `name` (with or without an extension) as a device: opening a file
/// called like that talks to the device instead of creating a file.
pub(crate) fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    // COM0-COM9 and LPT0-LPT9, also with the superscript digits Windows accepts.
    ["COM", "LPT"].iter().any(|prefix| {
        upper.strip_prefix(prefix).is_some_and(|rest| {
            let mut chars = rest.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
            )
        })
    })
}

/// Accepts a single path component made of safe characters.
///
/// This blocks separators, drive prefixes (`C:`), `.`/`..`, names Windows rewrites (a trailing
/// dot: `...` would resolve to the parent folder) and device names (`CON`, `NUL`...), so a
/// service name or version can never point outside the directory it is joined to.
pub(crate) fn validate_component(value: &str) -> Result<&str, PathsError> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+');
    let valid = !value.is_empty()
        && value.chars().all(allowed)
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !is_reserved_device_name(value);
    if valid {
        Ok(value)
    } else {
        Err(PathsError::InvalidComponent(value.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn paths_in(dir: &TempDir) -> CrisolPaths {
        CrisolPaths::new(dir.path().join("home")).expect("temp dirs are absolute")
    }

    #[test]
    fn env_value_wins_over_default() {
        let dir = TempDir::new().unwrap();
        let from_env = dir.path().join("env");
        let paths = CrisolPaths::resolve(Some(from_env.clone().into()), || {
            panic!("default must not be evaluated when the env var is set")
        })
        .unwrap();
        assert_eq!(paths.home(), from_env);
    }

    #[test]
    fn missing_or_empty_env_uses_default() {
        let dir = TempDir::new().unwrap();
        let default = dir.path().join("default");
        for env in [None, Some(OsString::new())] {
            let paths = CrisolPaths::resolve(env, || Some(default.clone())).unwrap();
            assert_eq!(paths.home(), default);
        }
    }

    #[test]
    fn no_env_and_no_default_is_an_error() {
        let err = CrisolPaths::resolve(None, || None).unwrap_err();
        assert!(matches!(err, PathsError::NoHome));
    }

    #[test]
    fn relative_home_is_rejected() {
        let err = CrisolPaths::new("relative/crisol").unwrap_err();
        assert!(matches!(err, PathsError::NotAbsolute(_)));
        let err = CrisolPaths::resolve(Some("relative".into()), || None).unwrap_err();
        assert!(matches!(err, PathsError::NotAbsolute(_)));
    }

    #[test]
    fn a_home_that_would_break_the_path_entry_is_rejected() {
        let tmp = TempDir::new().unwrap();
        for bad in ["a;b", "100%", "%USERPROFILE%x"] {
            let err = CrisolPaths::new(tmp.path().join(bad)).unwrap_err();
            assert!(matches!(err, PathsError::UnusableCharacter { .. }), "{bad}");
            assert!(err.to_string().contains("PATH"), "{err}");
        }
        assert!(CrisolPaths::new(tmp.path().join("my home (1)")).is_ok());
    }

    #[test]
    fn fixed_locations_match_the_architecture() {
        let dir = TempDir::new().unwrap();
        let paths = paths_in(&dir);
        let home = paths.home().to_path_buf();
        assert_eq!(paths.config_file(), home.join("crisol.toml"));
        assert_eq!(paths.sites_file(), home.join("sites.toml"));
        assert_eq!(paths.state_dir(), home.join("state"));
        assert_eq!(paths.bin_dir(), home.join("bin"));
        assert_eq!(paths.etc_dir(), home.join("etc"));
        assert_eq!(paths.data_dir(), home.join("data"));
        assert_eq!(paths.logs_dir(), home.join("logs"));
        assert_eq!(paths.certs_dir(), home.join("certs"));
        assert_eq!(paths.shims_dir(), home.join("shims"));
        assert_eq!(paths.www_dir(), home.join("www"));
    }

    #[test]
    fn per_service_locations_nest_correctly() {
        let dir = TempDir::new().unwrap();
        let paths = paths_in(&dir);
        let home = paths.home().to_path_buf();
        assert_eq!(
            paths.bin_for("php", "8.3.12").unwrap(),
            home.join("bin").join("php").join("8.3.12")
        );
        assert_eq!(
            paths.etc_for("nginx").unwrap(),
            home.join("etc").join("nginx")
        );
        assert_eq!(
            paths.overrides_for("nginx").unwrap(),
            home.join("etc").join("nginx").join("overrides")
        );
        assert_eq!(
            paths.data_for("mysql", "8.4").unwrap(),
            home.join("data").join("mysql").join("8.4")
        );
        assert_eq!(
            paths.logs_for("redis").unwrap(),
            home.join("logs").join("redis")
        );
        assert_eq!(
            paths.runtime_log("redis").unwrap(),
            home.join("logs").join("redis").join("runtime.log")
        );
        assert!(paths.runtime_log("../x").is_err());
    }

    /// A stand-in that records what it was asked and answers as scripted.
    struct FakePrivate {
        exists: bool,
        open_to_others: bool,
        created: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl FakePrivate {
        fn new(exists: bool, open_to_others: bool) -> Self {
            Self {
                exists,
                open_to_others,
                created: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl PrivateDirs for FakePrivate {
        fn create_private_dir(&self, dir: &Path) -> io::Result<bool> {
            if self.exists {
                return Ok(false);
            }
            self.created.lock().unwrap().push(dir.to_path_buf());
            fs::create_dir(dir)?;
            Ok(true)
        }

        fn is_writable_by_others(&self, _dir: &Path) -> io::Result<bool> {
            Ok(self.open_to_others)
        }

        fn is_writable_by_non_administrators(&self, _path: &Path) -> io::Result<bool> {
            Ok(true)
        }
    }

    #[test]
    fn the_icacls_command_differs_for_a_folder_and_a_file_and_names_the_user_by_sid() {
        let sid = "S-1-5-21-1-2-3-1001";
        let folder = private_acl_fix_command(Path::new(r"C:\Crisol\etc\node"), true, sid);
        assert_eq!(
            folder,
            "icacls \"C:\\Crisol\\etc\\node\" /inheritance:r /grant:r \"*S-1-5-21-1-2-3-1001:(OI)(CI)F\" \
             \"*S-1-5-18:(OI)(CI)F\" \"*S-1-5-32-544:(OI)(CI)F\" /remove:g \"*S-1-1-0\" \"*S-1-5-11\" \"*S-1-5-32-545\""
        );
        let file = private_acl_fix_command(Path::new(r"C:\Crisol\etc\node\ca.pem"), false, sid);
        assert_eq!(
            file,
            "icacls \"C:\\Crisol\\etc\\node\\ca.pem\" /inheritance:r /grant:r \
             \"*S-1-5-21-1-2-3-1001:F\" \"*S-1-5-18:F\" \"*S-1-5-32-544:F\" \
             /remove:g \"*S-1-1-0\" \"*S-1-5-11\" \"*S-1-5-32-545\""
        );
        // Nothing that only `cmd` expands, nothing that PowerShell would treat as a variable.
        for command in [&folder, &file] {
            assert!(
                !command.contains('%') && !command.contains('$'),
                "{command}"
            );
            assert!(!command.contains("  "), "{command}");
        }
    }

    #[test]
    fn a_new_home_is_created_through_the_private_creator_and_the_layout_follows() {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path().join("deep").join("home")).unwrap();
        let private = FakePrivate::new(false, false);

        assert_eq!(
            paths.ensure_private_layout(&private).unwrap(),
            HomeSecurity::Created
        );
        assert_eq!(
            *private.created.lock().unwrap(),
            [paths.home().to_path_buf()]
        );
        for sub in [paths.bin_dir(), paths.state_dir(), paths.shims_dir()] {
            assert!(sub.is_dir(), "{}", sub.display());
        }
    }

    #[test]
    fn an_existing_home_is_reported_not_re_permissioned() {
        let dir = TempDir::new().unwrap();
        let paths = paths_in(&dir);
        fs::create_dir_all(paths.home()).unwrap();

        let closed = FakePrivate::new(true, false);
        assert_eq!(
            paths.ensure_private_layout(&closed).unwrap(),
            HomeSecurity::Private
        );
        let open = FakePrivate::new(true, true);
        assert_eq!(
            paths.ensure_private_layout(&open).unwrap(),
            HomeSecurity::WritableByOthers
        );
        assert!(
            open.created.lock().unwrap().is_empty(),
            "an existing folder is never touched"
        );
        assert!(paths.bin_dir().is_dir(), "the layout is still completed");
    }

    #[test]
    fn components_cannot_escape_their_directory() {
        let dir = TempDir::new().unwrap();
        let paths = paths_in(&dir);
        for bad in [
            "",
            ".",
            "..",
            "../etc",
            "a/b",
            "a\\b",
            "C:",
            "C:\\x",
            "/abs",
            "a b",
            "é",
            // Windows strips trailing dots: `...` is the parent folder, `php.` is `php`.
            "...",
            "php.",
            ".hidden",
            "CON",
            "nul",
            "aux.txt",
            "COM1",
            "lpt9",
            "COM\u{b9}",
        ] {
            assert!(
                matches!(paths.etc_for(bad), Err(PathsError::InvalidComponent(_))),
                "`{bad}` must be rejected"
            );
            assert!(
                paths.bin_for("php", bad).is_err(),
                "`{bad}` must be rejected as version"
            );
        }
        for good in [
            "php",
            "node-22",
            "8.3.12",
            "8.4.0-rc1",
            "1.2+build.5",
            "mysql_8",
        ] {
            assert!(paths.etc_for(good).is_ok(), "`{good}` must be accepted");
        }
    }

    #[test]
    fn ensure_layout_creates_the_structure() {
        let dir = TempDir::new().unwrap();
        let paths = paths_in(&dir);
        assert!(!paths.home().exists());
        paths.ensure_layout().unwrap();
        for sub in [
            paths.state_dir(),
            paths.bin_dir(),
            paths.etc_dir(),
            paths.data_dir(),
            paths.logs_dir(),
            paths.certs_dir(),
            paths.shims_dir(),
            paths.www_dir(),
        ] {
            assert!(sub.is_dir(), "{} should exist", sub.display());
        }
    }

    #[test]
    fn ensure_layout_does_not_create_or_touch_user_files() {
        let dir = TempDir::new().unwrap();
        let paths = paths_in(&dir);
        paths.ensure_layout().unwrap();
        assert!(!paths.config_file().exists());
        assert!(!paths.sites_file().exists());

        fs::write(paths.config_file(), "# mine\n").unwrap();
        fs::write(paths.www_dir().join("keep.txt"), "x").unwrap();
        paths.ensure_layout().unwrap();
        assert_eq!(fs::read_to_string(paths.config_file()).unwrap(), "# mine\n");
        assert!(paths.www_dir().join("keep.txt").is_file());
    }

    #[test]
    fn ensure_layout_reports_the_failing_path() {
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("home");
        fs::write(&blocker, "a file where the home should be").unwrap();
        let paths = CrisolPaths::new(&blocker).unwrap();
        match paths.ensure_layout().unwrap_err() {
            PathsError::Create { path, .. } => assert_eq!(path, blocker),
            other => panic!("unexpected error: {other}"),
        }
    }
}
