//! The catalog of installable tool versions: `catalog.toml`.
//!
//! Each `[[package]]` is one build of one tool for one platform, with the official URL, the
//! SHA-256 of the download, how to unpack it and where the main executable ends up:
//!
//! ```toml
//! schema = 1
//! serial = 7
//! generated = "2026-10-14"
//!
//! [[package]]
//! service = "nginx"
//! version = "1.28.2"
//! os = "windows"
//! arch = "x86_64"
//! url = "https://nginx.org/download/nginx-1.28.2.zip"
//! sha256 = "…64 hex digits…"
//! format = "zip"          # zip | tar.gz | tar.xz | file
//! root = "nginx-1.28.2"   # optional: folder inside the archive to hoist to the top
//! executable = "nginx.exe"
//! size = 1802240          # optional, bytes: for progress and disk checks
//! end_of_life = true      # optional: no longer receives security fixes (ADR-0012)
//!
//! [recommended]           # optional: the line Crisol uses by default, per tool (ADR-0012)
//! php = "8.4"
//! ```
//!
//! Trust (ADR-0005):
//! - The catalog built into the binary ([`Catalog::embedded`]) needs no signature.
//! - A catalog from anywhere else goes through [`Catalog::load_update`], the **only** public
//!   way to read one: it checks the minisign signature against the project's public key, and
//!   that the `serial` is not lower than the highest one seen, so an old, validly signed
//!   catalog cannot be replayed to bring back a vulnerable version. Parsing without those
//!   checks is private to the crate, so forgetting one is not possible from outside.
//! - Every download is later checked against the `sha256` here (F3.3).
//!
//! Parsing is strict about things that decide what gets downloaded and where it is unpacked:
//! URLs must be `https`, hashes must be 64 hex digits, and paths must stay inside the install
//! folder. Unknown fields are only warnings, like in `crisol.toml`.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use serde::Deserialize;

use crate::config::{deserialize_reporting_unknown, ConfigWarning};
use crate::version::{Version, VersionError, VersionReq};

mod advisory;
pub use advisory::Advisory;

/// The catalog compiled into the binary.
pub const EMBEDDED_CATALOG: &str = include_str!("../data/catalog.toml");

/// Public key (minisign) that signed catalogs must verify with. The matching private key is
/// held by the project owner and never enters the repository.
///
/// `None` would mean "no key configured": only the built-in catalog is then accepted.
pub const CATALOG_PUBLIC_KEY: Option<&str> =
    Some("RWT5f7dA5mhS5I60To3gZ6B22D7XYEhMn34zm+ujr/xr8LcdIXdmP0vH");

/// The only catalog format this version understands.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// Operating systems Crisol knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    /// Microsoft Windows.
    Windows,
    /// Linux.
    Linux,
    /// macOS.
    Macos,
}

/// CPU architectures Crisol knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// 64-bit x86.
    X86_64,
    /// 64-bit ARM.
    Aarch64,
}

/// A platform a package is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Target {
    /// Operating system.
    pub os: Os,
    /// CPU architecture.
    pub arch: Arch,
}

impl Target {
    /// The platform this program runs on, if Crisol supports it.
    #[must_use]
    pub fn current() -> Option<Self> {
        let os = match std::env::consts::OS {
            "windows" => Os::Windows,
            "linux" => Os::Linux,
            "macos" => Os::Macos,
            _ => return None,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            _ => return None,
        };
        Some(Self { os, arch })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let os = match self.os {
            Os::Windows => "windows",
            Os::Linux => "linux",
            Os::Macos => "macos",
        };
        let arch = match self.arch {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        };
        write!(f, "{os}-{arch}")
    }
}

/// How a download is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ArchiveFormat {
    /// A `.zip` archive.
    #[serde(rename = "zip")]
    Zip,
    /// A gzip-compressed tar archive.
    #[serde(rename = "tar.gz")]
    TarGz,
    /// An xz-compressed tar archive.
    #[serde(rename = "tar.xz")]
    TarXz,
    /// A single file that is installed as it is (e.g. `composer.phar`).
    #[serde(rename = "file")]
    File,
}

/// What the file on disk looks like before validation.
#[derive(Debug, Deserialize)]
struct RawCatalog {
    schema: u32,
    serial: u64,
    #[serde(default)]
    generated: Option<String>,
    #[serde(default)]
    recommended: BTreeMap<String, String>,
    #[serde(default)]
    package: Vec<RawPackage>,
    #[serde(default)]
    advisory: Vec<advisory::RawAdvisory>,
}

#[derive(Debug, Deserialize)]
struct RawPackage {
    service: String,
    version: String,
    os: Os,
    arch: Arch,
    url: String,
    sha256: String,
    format: ArchiveFormat,
    #[serde(default)]
    root: Option<String>,
    executable: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    end_of_life: bool,
}

/// One downloadable build, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Tool name (`php`, `nginx`, `node`).
    pub service: String,
    /// Version of the tool.
    pub version: Version,
    /// Platform it is built for.
    pub target: Target,
    /// Official download URL (always `https`).
    pub url: String,
    /// SHA-256 of the download, as 64 lowercase hex digits.
    pub sha256: String,
    /// How the download is packaged.
    pub format: ArchiveFormat,
    /// Folder inside the archive to hoist to the top, slash-separated and relative.
    pub root: Option<String>,
    /// Main executable, relative to the install folder, slash-separated.
    pub executable: String,
    /// Size of the download in bytes, if known.
    pub size: Option<u64>,
    /// The version no longer receives security fixes: it can be installed (an old project may
    /// need it) but is never a default.
    pub end_of_life: bool,
}

impl Package {
    /// Host part of the URL.
    #[must_use]
    pub fn host(&self) -> &str {
        host_of(&self.url).unwrap_or_default()
    }

    /// `php 8.4.21 (windows-x86_64)`, for messages.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} {} ({})", self.service, self.version, self.target)
    }
}

/// A catalog that could not be used.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// Not valid TOML, or a value has the wrong type.
    #[error("invalid catalog: {0}")]
    Parse(#[from] toml::de::Error),
    /// The file uses a format this version does not understand.
    #[error("catalog schema {0} is not supported (expected {SUPPORTED_SCHEMA})")]
    UnsupportedSchema(u32),
    /// An entry has a value that is not acceptable.
    #[error("catalog entry {entry}: `{field}` {reason}")]
    Invalid {
        /// Which entry (`php 8.4.21` or `#3`).
        entry: String,
        /// Field name.
        field: &'static str,
        /// What is wrong.
        reason: String,
    },
    /// Two entries describe the same build.
    #[error("catalog lists {0} twice")]
    Duplicate(String),
    /// The signature is missing, malformed or does not match.
    #[error("catalog signature check failed: {0}")]
    Signature(String),
    /// No public key is configured, so signed catalogs cannot be trusted yet.
    #[error("no catalog public key is configured; only the built-in catalog can be used")]
    NoTrustedKey,
    /// The catalog is older than the one in use.
    #[error("catalog serial {offered} is older than the one in use ({current})")]
    Rollback {
        /// Serial of the rejected catalog.
        offered: u64,
        /// Serial that is in use.
        current: u64,
    },
    /// The requested version is not a version.
    #[error(transparent)]
    BadRequest(#[from] VersionError),
    /// Nothing in the catalog matches.
    #[error("no {service} {request} for {target} in the catalog")]
    NotFound {
        /// Tool asked for.
        service: String,
        /// Version asked for (`8.3`, `latest`).
        request: String,
        /// Platform asked for.
        target: Target,
    },
}

/// A validated catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    serial: u64,
    generated: Option<String>,
    recommended: BTreeMap<String, String>,
    packages: Vec<Package>,
    advisories: Vec<Advisory>,
    warnings: Vec<ConfigWarning>,
}

impl Catalog {
    /// The catalog built into the binary.
    pub fn embedded() -> Result<Self, CatalogError> {
        Self::parse(EMBEDDED_CATALOG)
    }

    /// Reads a catalog that came from outside the binary (a download, a file): verifies its
    /// signature with the project's key, parses it, and refuses it if its `serial` is lower
    /// than `highest_serial_seen`. Pass the higher of the built-in catalog's serial and the
    /// highest serial ever accepted, so that neither an old download nor an old binary can
    /// bring back a version that was withdrawn.
    ///
    /// Fails with [`CatalogError::NoTrustedKey`] while no public key is configured.
    pub fn load_update(
        text: &str,
        signature: &str,
        highest_serial_seen: u64,
    ) -> Result<Self, CatalogError> {
        Self::load_update_with(text, signature, CATALOG_PUBLIC_KEY, highest_serial_seen)
    }

    /// [`Catalog::load_update`] with the trusted key passed in, so the "no key" case and a
    /// test key can be exercised.
    fn load_update_with(
        text: &str,
        signature: &str,
        trusted_key: Option<&str>,
        highest_serial_seen: u64,
    ) -> Result<Self, CatalogError> {
        let catalog = Self::parse_signed_with(text, signature, trusted_key)?;
        if catalog.serial < highest_serial_seen {
            return Err(CatalogError::Rollback {
                offered: catalog.serial,
                current: highest_serial_seen,
            });
        }
        Ok(catalog)
    }

    /// Parses catalog text with no signature check, for the tests of other crates that need a
    /// small catalog of their own. Nothing in the product calls it: real catalogs are the
    /// built-in one and [`Catalog::load_update`].
    #[doc(hidden)]
    pub fn parse_for_tests(text: &str) -> Result<Self, CatalogError> {
        Self::parse(text)
    }

    /// Parses and validates catalog text. Does **not** check any signature: use it for text
    /// that is already trusted (the built-in catalog); see [`Catalog::load_update`].
    pub(crate) fn parse(text: &str) -> Result<Self, CatalogError> {
        let (raw, warnings): (RawCatalog, _) = deserialize_reporting_unknown(text)?;
        if raw.schema != SUPPORTED_SCHEMA {
            return Err(CatalogError::UnsupportedSchema(raw.schema));
        }
        let mut packages = Vec::with_capacity(raw.package.len());
        for (index, entry) in raw.package.into_iter().enumerate() {
            packages.push(validate(index, entry)?);
        }
        reject_duplicates(&packages)?;
        let mut advisories = Vec::with_capacity(raw.advisory.len());
        for (index, entry) in raw.advisory.into_iter().enumerate() {
            let advisory = advisory::validate(index, entry)?;
            if advisories.iter().any(|a: &Advisory| a.id == advisory.id) {
                return Err(CatalogError::Duplicate(format!("advisory {}", advisory.id)));
            }
            advisories.push(advisory);
        }
        for (service, line) in &raw.recommended {
            let invalid = |field: &'static str, reason: &str| CatalogError::Invalid {
                entry: format!("[recommended] {service}"),
                field,
                reason: reason.to_owned(),
            };
            if !is_service_name(service) {
                return Err(invalid(
                    "service",
                    "must be lowercase letters, digits, `-` or `_`",
                ));
            }
            if VersionReq::parse(line).is_err() {
                return Err(invalid(
                    "recommended",
                    "is not a version line such as `8.4`",
                ));
            }
        }
        packages.sort_by(|a, b| {
            (a.service.as_str(), a.target, &a.version).cmp(&(
                b.service.as_str(),
                b.target,
                &b.version,
            ))
        });
        Ok(Self {
            serial: raw.serial,
            generated: raw.generated,
            recommended: raw.recommended,
            packages,
            advisories,
            warnings,
        })
    }

    /// Verifies the minisign `signature` of `text` with `public_key`, and only then parses it.
    ///
    /// `public_key` is the contents of a minisign `.pub` file or its bare base64 line;
    /// `signature` is the contents of the `.minisig` file.
    pub(crate) fn parse_signed(
        text: &str,
        signature: &str,
        public_key: &str,
    ) -> Result<Self, CatalogError> {
        verify_signature(text, signature, public_key)?;
        Self::parse(text)
    }

    /// Verifies with `trusted_key`; with none configured fails with
    /// [`CatalogError::NoTrustedKey`].
    fn parse_signed_with(
        text: &str,
        signature: &str,
        trusted_key: Option<&str>,
    ) -> Result<Self, CatalogError> {
        let key = trusted_key.ok_or(CatalogError::NoTrustedKey)?;
        Self::parse_signed(text, signature, key)
    }

    /// Increases with every published catalog.
    #[must_use]
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Date the catalog was generated, as written in the file.
    #[must_use]
    pub fn generated(&self) -> Option<&str> {
        self.generated.as_deref()
    }

    /// Unknown fields found while parsing.
    #[must_use]
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// The line of `service` that Crisol uses by default (`8.4`), if the catalog names one.
    /// It lives here, not in the binary, so a signed catalog can move it (ADR-0012).
    #[must_use]
    pub fn recommended(&self, service: &str) -> Option<&str> {
        self.recommended.get(service).map(String::as_str)
    }

    /// Whether the catalog marks this version of `service` as no longer supported.
    #[must_use]
    pub fn is_end_of_life(&self, service: &str, version: &Version) -> bool {
        self.packages
            .iter()
            .any(|p| p.end_of_life && p.service == service && &p.version == version)
    }

    /// The advisories that affect this version of `service` (ADR-0020).
    #[must_use]
    pub fn advisories_for(&self, service: &str, version: &Version) -> Vec<&Advisory> {
        self.advisories
            .iter()
            .filter(|a| a.affects(service, version))
            .collect()
    }

    /// Every advisory in the catalog.
    #[must_use]
    pub fn advisories(&self) -> &[Advisory] {
        &self.advisories
    }

    /// The highest stable build of `service` for `target`: the one whose layout (format,
    /// `root`, executable) `install --from` assumes for a version the catalog lacks (ADR-0020).
    #[must_use]
    pub fn template(&self, service: &str, target: Target) -> Option<&Package> {
        self.available(service, target)
            .into_iter()
            .rfind(|p| !p.version.is_prerelease())
    }

    /// Every package, ordered by service, platform and version.
    #[must_use]
    pub fn packages(&self) -> &[Package] {
        &self.packages
    }

    /// Names of the tools in the catalog, sorted.
    #[must_use]
    pub fn services(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.packages.iter().map(|p| p.service.as_str()).collect();
        names.dedup();
        names
    }

    /// Versions of `service` available for `target`, oldest first.
    #[must_use]
    pub fn available(&self, service: &str, target: Target) -> Vec<&Package> {
        self.packages
            .iter()
            .filter(|p| p.service == service && p.target == target)
            .collect()
    }

    /// The highest version matching `request` (`8`, `8.3`, `8.3.12`, or an exact
    /// pre-release). Pre-releases are never picked by a prefix.
    pub fn find(
        &self,
        service: &str,
        request: &str,
        target: Target,
    ) -> Result<&Package, CatalogError> {
        let wanted = VersionReq::parse(request)?;
        self.available(service, target)
            .into_iter()
            .rfind(|p| wanted.matches(&p.version))
            .ok_or_else(|| not_found(service, request, target))
    }

    /// The highest stable version of `service` for `target`, preferring one that still
    /// receives security fixes.
    pub fn latest(&self, service: &str, target: Target) -> Result<&Package, CatalogError> {
        let stable = || {
            self.available(service, target)
                .into_iter()
                .filter(|p| !p.version.is_prerelease())
        };
        stable()
            .rfind(|p| !p.end_of_life)
            .or_else(|| stable().next_back())
            .ok_or_else(|| not_found(service, "latest", target))
    }
}

fn not_found(service: &str, request: &str, target: Target) -> CatalogError {
    CatalogError::NotFound {
        service: service.to_owned(),
        request: request.to_owned(),
        target,
    }
}

fn verify_signature(text: &str, signature: &str, public_key: &str) -> Result<(), CatalogError> {
    let fail =
        |what: &str, e: minisign_verify::Error| CatalogError::Signature(format!("{what}: {e}"));
    let key = if public_key.contains('\n') || public_key.contains("untrusted comment") {
        minisign_verify::PublicKey::decode(public_key)
    } else {
        minisign_verify::PublicKey::from_base64(public_key.trim())
    }
    .map_err(|e| fail("bad public key", e))?;
    let signature =
        minisign_verify::Signature::decode(signature).map_err(|e| fail("bad signature file", e))?;
    key.verify(text.as_bytes(), &signature, false)
        .map_err(|e| fail("the signature does not match", e))
}

// ---- validation -------------------------------------------------------------------

fn validate(index: usize, raw: RawPackage) -> Result<Package, CatalogError> {
    let entry = format!("#{} ({} {})", index + 1, raw.service, raw.version);
    let invalid = |field: &'static str, reason: &str| CatalogError::Invalid {
        entry: entry.clone(),
        field,
        reason: reason.to_owned(),
    };

    if !is_service_name(&raw.service) {
        return Err(invalid(
            "service",
            "must be lowercase letters, digits, `-` or `_`",
        ));
    }
    let version =
        Version::parse(&raw.version).map_err(|_| invalid("version", "is not a valid version"))?;
    check_url(&raw.url).map_err(|reason| invalid("url", reason))?;
    let sha256 = normalize_sha256(&raw.sha256)
        .ok_or_else(|| invalid("sha256", "must be 64 hexadecimal digits"))?;
    let executable = normalize_relative(&raw.executable).ok_or_else(|| {
        invalid(
            "executable",
            "must be a relative path that stays inside the install folder",
        )
    })?;
    let root = match &raw.root {
        Some(root) => Some(normalize_relative(root).ok_or_else(|| {
            invalid(
                "root",
                "must be a relative path that stays inside the archive",
            )
        })?),
        None => None,
    };
    if raw.size == Some(0) {
        return Err(invalid("size", "must be above zero when given"));
    }
    Ok(Package {
        service: raw.service,
        version,
        target: Target {
            os: raw.os,
            arch: raw.arch,
        },
        url: raw.url,
        sha256,
        format: raw.format,
        root,
        executable,
        size: raw.size,
        end_of_life: raw.end_of_life,
    })
}

fn is_service_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn reject_duplicates(packages: &[Package]) -> Result<(), CatalogError> {
    let mut seen = HashSet::new();
    for package in packages {
        let key = (
            package.service.clone(),
            package.target,
            package.version.clone(),
        );
        if !seen.insert(key) {
            return Err(CatalogError::Duplicate(package.label()));
        }
    }
    Ok(())
}

/// The host of an `https://` URL.
fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                authority
            }
        });
    (!host.is_empty()).then_some(host)
}

fn check_url(url: &str) -> Result<(), &'static str> {
    if !url.starts_with("https://") {
        return Err("must start with https://");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("must not contain spaces or control characters");
    }
    let authority = url["https://".len()..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') {
        // `https://trusted.example@evil.example/` really goes to evil.example.
        return Err("must not contain a user name or password");
    }
    let host = host_of(url).ok_or("has no host")?;
    // Each label: letters, digits and inner hyphens, between 1 and 63 characters.
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let host_ok = host.split('.').all(label_ok);
    if host_ok {
        Ok(())
    } else {
        Err("has an invalid host")
    }
}

fn normalize_sha256(text: &str) -> Option<String> {
    let valid = text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit());
    valid.then(|| text.to_ascii_lowercase())
}

/// A relative path made of plain components, joined with `/`.
fn normalize_relative(path: &str) -> Option<String> {
    let unified = path.replace('\\', "/");
    if unified.is_empty() || unified.starts_with('/') {
        return None;
    }
    let ok = unified.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains(':')
            && !part.chars().any(char::is_control)
    });
    ok.then_some(unified)
}

#[cfg(test)]
mod tests;
