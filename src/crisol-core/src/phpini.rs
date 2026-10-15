//! `php.ini` for each installed PHP version (F4.1).
//!
//! The file is **derived**: it lives in `etc/php/<version>/php.ini`, is rewritten whenever it
//! is generated, and is never edited by hand (`docs/arquitectura.md`). It starts from the
//! `php.ini-development` that ships with that exact PHP and changes only what Crisol needs:
//!
//! - `extension_dir` points at the `ext` folder of that version (other suites often leave
//!   this to be patched by hand);
//! - a set of common extensions is switched on, **only if their DLL is really there**: an
//!   extension a build does not ship is skipped and reported, instead of making PHP print a
//!   start-up warning on every run;
//! - a few development-friendly limits, the log file and, if configured, the time zone;
//! - finally the user's own `etc/php/overrides/*.ini` are appended, in name order, so they
//!   have the last word and are never touched.
//!
//! Every other byte of the shipped file, comments and line endings included, is kept. The
//! transformation is line by line and does not parse INI: a line is changed only when it is
//! exactly the commented-out directive PHP ships (`;extension=curl`), so the long
//! explanatory comments, which mention directives in prose, are left alone.
//!
//! [`render_php_ini`] is pure; [`write_php_ini`] does the file work and does not touch the
//! file when nothing changed.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::{CrisolPaths, PathsError};

/// The file PHP ships as the starting point.
pub const BASE_FILE: &str = "php.ini-development";

/// Extensions Crisol switches on when the build has them. `exif` has to come after `mbstring`;
/// that order is the one of the shipped file, which is kept.
pub const DEFAULT_EXTENSIONS: [&str; 22] = [
    "bz2",
    "curl",
    "exif",
    "fileinfo",
    "ftp",
    "gd",
    "gettext",
    "gmp",
    "intl",
    "mbstring",
    "mysqli",
    "openssl",
    "pdo_mysql",
    "pdo_pgsql",
    "pdo_sqlite",
    "pgsql",
    "soap",
    "sockets",
    "sodium",
    "sqlite3",
    "xsl",
    "zip",
];

/// What Crisol sets on top of `php.ini-development`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpIniSettings {
    /// Extensions to enable, when the build ships them.
    pub extensions: Vec<String>,
    /// Load Zend `OPcache` (`zend_extension=opcache`) when the build ships it.
    pub opcache: bool,
    /// `memory_limit`.
    pub memory_limit: String,
    /// `max_execution_time`, in seconds.
    pub max_execution_time: u32,
    /// `post_max_size`.
    pub post_max_size: String,
    /// `upload_max_filesize`.
    pub upload_max_filesize: String,
    /// `date.timezone`. `None` leaves it unset, so PHP uses UTC; resolving "the system time
    /// zone" needs the platform layer and is not done here.
    pub timezone: Option<String>,
}

impl Default for PhpIniSettings {
    fn default() -> Self {
        Self {
            extensions: DEFAULT_EXTENSIONS.iter().map(|e| (*e).to_owned()).collect(),
            opcache: true,
            memory_limit: "512M".to_owned(),
            max_execution_time: 120,
            post_max_size: "128M".to_owned(),
            upload_max_filesize: "128M".to_owned(),
            timezone: None,
        }
    }
}

/// Why a `php.ini` could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum PhpIniError {
    /// The service or version is not a valid folder name.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// This PHP version is not installed (its `bin/php/<version>/` folder is missing).
    #[error("PHP {0} is not installed")]
    NotInstalled(String),
    /// The installation lacks the `php.ini-development` to start from.
    #[error("`{}` has no {BASE_FILE}", .0.display())]
    NoBaseFile(PathBuf),
    /// A setting cannot be written into an INI file safely.
    #[error("invalid php.ini setting `{name}`: {reason}")]
    InvalidSetting {
        /// Setting name.
        name: &'static str,
        /// What is wrong.
        reason: String,
    },
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
) -> impl FnOnce(io::Error) -> PhpIniError + 'a {
    move |source| PhpIniError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Everything [`render_php_ini`] needs.
#[derive(Debug, Clone)]
pub struct PhpIniInput<'a> {
    /// PHP version, for the header.
    pub version: &'a str,
    /// Text of `php.ini-development`.
    pub base: &'a str,
    /// Absolute path of the `ext` folder of this PHP.
    pub extension_dir: &'a Path,
    /// Names of the extensions this build ships (`curl`, `opcache`…), from [`installed_extensions`].
    pub available: &'a BTreeSet<String>,
    /// Where PHP writes its errors; `None` keeps PHP's default (stderr).
    pub error_log: Option<&'a Path>,
    /// The CA bundle for cURL (`etc/php/cacert.pem`, ADR-0011); `None` leaves `curl.cainfo`
    /// as PHP ships it. `openssl.cafile` is never set: PHP's streams keep using the live
    /// Windows store. A value the user sets in an
    /// override still wins: overrides come last and PHP keeps the last assignment.
    pub ca_file: Option<&'a Path>,
    /// The user's `*.ini` fragments as `(file name, text)`, already in the order to append.
    pub overrides: &'a [(String, String)],
    /// Folder the overrides come from, for the header.
    pub overrides_dir: &'a Path,
    /// The settings.
    pub settings: &'a PhpIniSettings,
}

/// The generated text and what was decided on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPhpIni {
    /// The complete `php.ini`.
    pub text: String,
    /// Extensions that are switched on (`opcache` included).
    pub enabled: Vec<String>,
    /// Requested extensions this build does not ship.
    pub skipped: Vec<String>,
}

/// Builds the `php.ini`. Pure: same input, same text.
pub fn render_php_ini(input: &PhpIniInput<'_>) -> Result<GeneratedPhpIni, PhpIniError> {
    validate(input.settings)?;
    let eol = if input.base.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let settings = input.settings;

    let (wanted, skipped): (Vec<&str>, Vec<&str>) = settings
        .extensions
        .iter()
        .map(String::as_str)
        .partition(|name| input.available.contains(*name));
    let opcache = settings.opcache && input.available.contains("opcache");

    let directives = directives(input)?;
    let mut applied_extensions: BTreeSet<&str> = BTreeSet::new();
    let mut applied_directives: BTreeSet<&str> = BTreeSet::new();
    let mut applied_opcache = false;

    let mut body = String::with_capacity(input.base.len() + 2048);
    for line in input.base.split_inclusive('\n') {
        let (content, terminator) = split_terminator(line);
        let replaced = match commented_extension(content) {
            Some((Kind::Zend, "opcache", tail)) if opcache && !applied_opcache => {
                applied_opcache = true;
                Some(format!("zend_extension=opcache{tail}"))
            }
            Some((Kind::Normal, name, tail))
                if wanted.contains(&name) && !applied_extensions.contains(name) =>
            {
                applied_extensions.insert(name);
                Some(format!("extension={name}{tail}"))
            }
            Some(_) => None,
            None => directives
                .iter()
                .find(|d| !applied_directives.contains(d.key) && d.matches(content))
                .map(|d| {
                    applied_directives.insert(d.key);
                    d.line.clone()
                }),
        };
        body.push_str(replaced.as_deref().unwrap_or(content));
        body.push_str(terminator);
    }

    // Whatever the shipped file did not have a line for goes in a block of its own.
    let mut added: Vec<String> = wanted
        .iter()
        .filter(|name| !applied_extensions.contains(**name))
        .map(|name| format!("extension={name}"))
        .collect();
    if opcache && !applied_opcache {
        added.push("zend_extension=opcache".to_owned());
    }
    added.extend(
        directives
            .iter()
            .filter(|d| !applied_directives.contains(d.key))
            .map(|d| d.line.clone()),
    );

    let text = assemble(input, eol, &body, &added);

    let mut enabled: Vec<String> = wanted.iter().map(|w| (*w).to_owned()).collect();
    if opcache {
        enabled.push("opcache".to_owned());
    }
    Ok(GeneratedPhpIni {
        text,
        enabled,
        skipped: skipped.into_iter().map(str::to_owned).collect(),
    })
}

/// Puts the pieces together: header, the shipped file as changed, the block of additions
/// and the user's overrides, all with the line ending of the shipped file.
fn assemble(input: &PhpIniInput<'_>, eol: &str, body: &str, added: &[String]) -> String {
    let mut text = String::new();
    let line = |text: &mut String, content: &str| {
        text.push_str(content);
        text.push_str(eol);
    };
    line(
        &mut text,
        &format!(
            "; php.ini for PHP {} generated by Crisol from {BASE_FILE}.",
            input.version
        ),
    );
    line(
        &mut text,
        "; Do not edit: this file is rewritten. Put your own settings in",
    );
    line(
        &mut text,
        &format!(
            "; {}{}*.ini",
            input.overrides_dir.display(),
            std::path::MAIN_SEPARATOR
        ),
    );
    line(&mut text, ";");
    text.push_str(body);
    if !text.ends_with('\n') {
        text.push_str(eol);
    }
    if !added.is_empty() {
        line(&mut text, "");
        line(
            &mut text,
            "; ---- Added by Crisol (the shipped file has no line for these) ----",
        );
        for added_line in added {
            line(&mut text, added_line);
        }
    }
    for (name, content) in input.overrides {
        line(&mut text, "");
        line(&mut text, &format!("; ---- Override: {name} ----"));
        text.push_str(&content.replace("\r\n", "\n").replace('\n', eol));
        if !content.ends_with('\n') {
            text.push_str(eol);
        }
    }
    text
}

/// A setting Crisol puts in the file: the line to write and how to recognise the line it
/// replaces in the shipped file.
struct Directive {
    key: &'static str,
    /// The complete new line, without terminator.
    line: String,
    /// The shipped line is commented out (`;key =`) rather than active (`key =`).
    commented: bool,
    /// Only the shipped line whose value is exactly this is replaced (when the file has
    /// several commented lines for the same key).
    shipped_value: Option<&'static str>,
}

impl Directive {
    fn active(key: &'static str, value: impl std::fmt::Display) -> Self {
        Self {
            key,
            line: format!("{key} = {value}"),
            commented: false,
            shipped_value: None,
        }
    }

    fn commented(
        key: &'static str,
        value: impl std::fmt::Display,
        shipped: Option<&'static str>,
    ) -> Self {
        Self {
            key,
            line: format!("{key} = {value}"),
            commented: true,
            shipped_value: shipped,
        }
    }

    fn matches(&self, content: &str) -> bool {
        let rest = if self.commented {
            match content.strip_prefix(';') {
                Some(rest) => rest,
                None => return false,
            }
        } else {
            content
        };
        // `key`, optional blanks, then `=`: so `memory_limit` does not match
        // `memory_limit_other`, and prose that merely starts with the word does not match.
        let Some(after) = rest.strip_prefix(self.key) else {
            return false;
        };
        let Some(value) = after.trim_start().strip_prefix('=') else {
            return false;
        };
        self.shipped_value
            .is_none_or(|expected| value.trim().trim_matches('"') == expected)
    }
}

fn directives(input: &PhpIniInput<'_>) -> Result<Vec<Directive>, PhpIniError> {
    let s = input.settings;
    let mut list = vec![
        // The Windows build ships `;extension_dir = "ext"`, right under the Unix
        // `;extension_dir = "./"`: the Windows one is the one replaced.
        Directive::commented(
            "extension_dir",
            format!("\"{}\"", quote_path(input.extension_dir)?),
            Some("ext"),
        ),
        Directive::active("memory_limit", &s.memory_limit),
        Directive::active("max_execution_time", s.max_execution_time),
        Directive::active("post_max_size", &s.post_max_size),
        Directive::active("upload_max_filesize", &s.upload_max_filesize),
    ];
    if let Some(zone) = &s.timezone {
        list.push(Directive::commented(
            "date.timezone",
            format!("\"{zone}\""),
            None,
        ));
    }
    if let Some(log) = input.error_log {
        // The shipped file has `;error_log = php_errors.log` and `;error_log = syslog`.
        list.push(Directive::commented(
            "error_log",
            format!("\"{}\"", quote_path(log)?),
            Some("php_errors.log"),
        ));
    }
    if let Some(bundle) = input.ca_file {
        let quoted = format!("\"{}\"", quote_path(bundle)?);
        // `;curl.cainfo =` is under [curl].
        list.push(Directive::commented("curl.cainfo", quoted, None));
    }
    Ok(list)
}

/// A path as it goes between the quotes of an INI value: forward slashes (PHP on Windows
/// accepts them, and a backslash before the closing quote would be an escape).
fn quote_path(path: &Path) -> Result<String, PhpIniError> {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.contains(['"', '\n', '\r']) {
        return Err(PhpIniError::InvalidSetting {
            name: "path",
            reason: format!("`{text}` cannot be written between quotes"),
        });
    }
    Ok(text)
}

fn validate(settings: &PhpIniSettings) -> Result<(), PhpIniError> {
    let bad = |name: &'static str, reason: &str| PhpIniError::InvalidSetting {
        name,
        reason: reason.to_owned(),
    };
    let size = |value: &str| {
        let digits = value
            .strip_suffix(['K', 'M', 'G', 'k', 'm', 'g'])
            .unwrap_or(value);
        value == "-1" || (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
    };
    for (name, value) in [
        ("memory_limit", &settings.memory_limit),
        ("post_max_size", &settings.post_max_size),
        ("upload_max_filesize", &settings.upload_max_filesize),
    ] {
        if !size(value) {
            return Err(bad(
                name,
                "must be a number with an optional K, M or G (or -1)",
            ));
        }
    }
    if let Some(zone) = &settings.timezone {
        let ok = !zone.is_empty()
            && zone
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '+' | '-'));
        if !ok {
            return Err(bad(
                "date.timezone",
                "must be a zone name such as `Europe/Madrid`",
            ));
        }
    }
    for extension in &settings.extensions {
        let ok = !extension.is_empty()
            && extension
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(bad(
                "extensions",
                "names are lowercase letters, digits and `_`",
            ));
        }
    }
    Ok(())
}

/// Splits `line` into its text and its terminator (`\n`, `\r\n` or nothing).
fn split_terminator(line: &str) -> (&str, &str) {
    let content = line.strip_suffix('\n').unwrap_or(line);
    let content = content.strip_suffix('\r').unwrap_or(content);
    (content, &line[content.len()..])
}

/// Which directive an extension line is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `extension=`.
    Normal,
    /// `zend_extension=`.
    Zend,
}

/// For `;extension=curl` or `;zend_extension=opcache ; note`, the kind, the name and whatever
/// follows the name (kept as it was, blanks included). `None` for any other line, including
/// the prose comments of the shipped file (`;   extension=modulename`).
fn commented_extension(content: &str) -> Option<(Kind, &str, &str)> {
    let rest = content.strip_prefix(';')?;
    let (kind, rest) = if let Some(rest) = rest.strip_prefix("zend_extension=") {
        (Kind::Zend, rest)
    } else {
        (Kind::Normal, rest.strip_prefix("extension=")?)
    };
    let end = rest
        .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        .unwrap_or(rest.len());
    let (name, tail) = rest.split_at(end);
    let tail_ok = tail.trim().is_empty() || tail.trim_start().starts_with(';');
    (!name.is_empty() && tail_ok).then_some((kind, name, tail))
}

/// The extensions an `ext` folder ships: `php_curl.dll` is `curl` (and `libfoo.so` is `foo`
/// elsewhere). A missing folder ships none.
pub fn installed_extensions(extension_dir: &Path) -> io::Result<BTreeSet<String>> {
    let read = match fs::read_dir(extension_dir) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(e),
    };
    let mut found = BTreeSet::new();
    for entry in read {
        let name = entry?.file_name().to_string_lossy().to_ascii_lowercase();
        let stem = name
            .strip_suffix(".dll")
            .or_else(|| name.strip_suffix(".so"))
            .map(|stem| stem.strip_prefix("php_").unwrap_or(stem));
        if let Some(stem) = stem {
            found.insert(stem.to_owned());
        }
    }
    Ok(found)
}

/// The `*.ini` files of the overrides folder, in name order, as `(name, text)`. A missing
/// folder has none.
pub fn read_overrides(dir: &Path) -> io::Result<Vec<(String, String)>> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in read {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.to_ascii_lowercase().ends_with(".ini") && entry.file_type()?.is_file() {
            files.push((name, fs::read_to_string(entry.path())?));
        }
    }
    files.sort_by_key(|(name, _)| name.to_lowercase());
    Ok(files)
}

/// What [`write_php_ini`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenPhpIni {
    /// `etc/php/<version>/php.ini`.
    pub path: PathBuf,
    /// The file was created or its content changed. `false`: it was already up to date and
    /// was not touched.
    pub changed: bool,
    /// Extensions switched on (`opcache` included).
    pub enabled: Vec<String>,
    /// Requested extensions this build does not ship.
    pub skipped: Vec<String>,
    /// Things worth telling the user (for instance, that there is no CA bundle yet).
    pub warnings: Vec<String>,
}

/// Generates `etc/php/<version>/php.ini` for an installed PHP and writes it atomically. If
/// the file already has exactly that content it is left alone.
pub fn write_php_ini(
    paths: &CrisolPaths,
    version: &str,
    settings: &PhpIniSettings,
) -> Result<WrittenPhpIni, PhpIniError> {
    let install = paths.bin_for("php", version)?;
    if !install.is_dir() {
        return Err(PhpIniError::NotInstalled(version.to_owned()));
    }
    let base_path = install.join(BASE_FILE);
    let base = match fs::read_to_string(&base_path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(PhpIniError::NoBaseFile(install));
        }
        Err(e) => return Err(io_error("cannot read", &base_path)(e)),
    };
    let extension_dir = install.join("ext");
    let available =
        installed_extensions(&extension_dir).map_err(io_error("cannot read", &extension_dir))?;
    let overrides_dir = paths.overrides_for("php")?;
    let overrides =
        read_overrides(&overrides_dir).map_err(io_error("cannot read", &overrides_dir))?;
    let error_log = paths.logs_for("php")?.join("php_errors.log");
    let ca_file = paths.php_cacert();
    let ca_file = ca_file.is_file().then_some(ca_file);
    let mut warnings = Vec::new();
    if ca_file.is_none() {
        warnings.push(
            "there is no CA bundle (etc/php/cacert.pem): PHP's cURL will not trust any HTTPS site until one is built"
                .to_owned(),
        );
    }

    let generated = render_php_ini(&PhpIniInput {
        version,
        base: &base,
        extension_dir: &extension_dir,
        available: &available,
        error_log: Some(&error_log),
        ca_file: ca_file.as_deref(),
        overrides: &overrides,
        overrides_dir: &overrides_dir,
        settings,
    })?;

    let path = paths.php_ini(version)?;
    let changed = write_if_changed(&path, generated.text.as_bytes())?;
    Ok(WrittenPhpIni {
        path,
        changed,
        enabled: generated.enabled,
        skipped: generated.skipped,
        warnings,
    })
}

/// Writes `bytes` to `path` atomically (a temporary file in the same folder, renamed over
/// it), unless the file already has exactly that content. Returns whether it wrote.
pub(crate) fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool, PhpIniError> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(io_error("cannot create", dir))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".crisol-gen-")
        .tempfile_in(dir)
        .map_err(io_error("cannot create a temporary file in", dir))?;
    io::Write::write_all(&mut temp, bytes).map_err(io_error("cannot write", path))?;
    temp.persist(path)
        .map_err(|e| io_error("cannot write", path)(e.error))?;
    Ok(true)
}

#[cfg(test)]
mod tests;
