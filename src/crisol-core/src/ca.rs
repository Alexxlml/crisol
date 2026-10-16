//! The local certificate authority (F6.1, ADR-0013).
//!
//! Crisol serves local sites over HTTPS with certificates signed by a CA that exists only on
//! this computer. The CA is made once and lives in `certs/`:
//!
//! ```text
//! certs/ca.pem   # the certificate (public; this is what gets trusted)
//! certs/ca.key   # its private key (never leaves the private home, ADR-0008)
//! ```
//!
//! What makes it safe to trust:
//!
//! - **Name constraints (critical).** It can only issue for `test`, `localhost`, the TLD set in
//!   `crisol.toml` when the CA was made, `127.0.0.0/8` and `::1`. A stolen key cannot
//!   impersonate a public site.
//! - `pathlen = 0`: it signs end certificates, never other CAs.
//! - Its name carries a short id taken from its public key (`Crisol Local CA 7F3A9C`), so it can
//!   be told apart in a trust store and removed without doubt.
//!
//! Nothing trusts a CA that has not passed [`inspect`]'s checks, and nothing builds one without
//! them: a [`LocalCa`] can only come from [`ensure_ca`], [`load_ca`] or [`create_ca`], which
//! require, every time:
//!
//! - that `certs/` and the CA files are **not writable by other accounts** (the folder is made
//!   with [`PrivateDirs`] when it does not exist, even if the home was not made private yet,
//!   ADR-0008); another account that can write there could swap in a CA of its own;
//! - that the certificate is **self-signed**, a CA with `pathlen = 0` and `keyCertSign`, with
//!   **critical** name constraints that only permit DNS names reserved for local use
//!   ([`is_special_use`]), `127.0.0.0/8` and `::1`, and no other kind of name. A CA without
//!   them could impersonate any site for the browser, PHP and Node.
//!
//! A CA that is already there is never replaced silently: a missing half, a key that does not
//! belong to the certificate, or a certificate that is not a CA is an error, because deleting
//! the key would leave the old CA trusted by the system with nothing able to use it. A new CA is
//! made only on request ([`create_ca`] with `replace`), which is also what changing the TLD to
//! one the CA does not cover needs ([`LocalCa::covers`]).

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType, GeneralSubtree,
    IsCa, KeyPair, KeyUsagePurpose, NameConstraints, SerialNumber,
};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::paths::{CrisolPaths, PrivateDirs};

/// The names (and everything under them) reserved for local use, the only ones the CA may be
/// restricted to: RFC 6761 (`test`, `localhost`, `example`, `invalid`), RFC 6762 (`local`),
/// RFC 8375 (`home.arpa`) and `internal` (ICANN reserved it for private use). Any other TLD
/// could be a real public domain.
pub const SPECIAL_USE: [&str; 7] = [
    "test",
    "localhost",
    "example",
    "invalid",
    "local",
    "internal",
    "home.arpa",
];

/// Whether `name` is one of [`SPECIAL_USE`] or under one of them (`dev.test`).
#[must_use]
pub fn is_special_use(name: &str) -> bool {
    let name = name.trim_matches('.').to_ascii_lowercase();
    SPECIAL_USE
        .iter()
        .any(|d| name == *d || name.ends_with(&format!(".{d}")))
}

/// How long the CA is valid.
const CA_VALIDITY_DAYS: i64 = 3650;

/// Names every CA can issue for, whatever the configured TLD: the reserved names of RFC 6761.
const ALWAYS_PERMITTED: [&str; 2] = ["test", "localhost"];

/// Why the CA could not be made or read.
#[derive(Debug, thiserror::Error)]
pub enum CaError {
    /// A file could not be read or written.
    #[error("{action} `{}`: {source}", path.display())]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: std::path::PathBuf,
        /// The error.
        source: io::Error,
    },
    /// The certificate or the key is not valid.
    #[error("the local CA in `certs/` cannot be used: {0}")]
    Invalid(String),
    /// Only one of `ca.pem` and `ca.key` exists.
    #[error("`certs/` has `{present}` but not `{missing}`: restore the missing file or remove the other one and make a new CA (it must be trusted again)")]
    Incomplete {
        /// The file that is there.
        present: &'static str,
        /// The file that is not.
        missing: &'static str,
    },
    /// The key is not the one of the certificate.
    #[error(
        "`ca.key` is not the key of `ca.pem`. If a run of Crisol was interrupted while making the \
         CA, remove both files and make a new one (it must be trusted again); if not, somebody \
         changed one of them"
    )]
    KeyMismatch,
    /// The certificate is not a CA.
    #[error("`ca.pem` is not a certificate authority")]
    NotACa,
    /// The CA has expired.
    #[error("the local CA expired; make a new one (it must be trusted again)")]
    Expired,
    /// A CA exists and the request was to create one without replacing it.
    #[error("a local CA already exists in `certs/`")]
    AlreadyExists,
    /// The TLD is not one reserved for local use, so no CA is made for it.
    #[error(
        "`.{0}` is not a name reserved for local use (test, localhost, example, invalid, local, \
         internal, home.arpa): a local CA is not made for it, because it could vouch for real \
         public domains. Its sites are served over HTTP; use a TLD such as `.test` for HTTPS"
    )]
    TldNotSpecialUse(String),
    /// `certs/` or one of the CA files can be modified by other accounts of this computer.
    #[error(
        "`{}` can be modified by other accounts on this computer, so the local CA cannot be \
         trusted. Remove write access for the other accounts on that folder (on Windows: \
         `icacls \"{}\" /inheritance:r /grant:r \"%USERNAME%:(OI)(CI)F\" \"SYSTEM:(OI)(CI)F\" \
         \"*S-1-5-32-544:(OI)(CI)F\"`), or move CRISOL_HOME to a private folder and make the CA again",
        path.display(),
        path.display()
    )]
    Exposed {
        /// The folder or file that is open.
        path: std::path::PathBuf,
    },
    /// The CA cannot issue for this name (it was made for another TLD).
    #[error("the local CA cannot issue a certificate for `{0}`: make a new CA (it must be trusted again)")]
    NotCovered(String),
}

/// What can be read from the CA's certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaInfo {
    /// Common name: `Crisol Local CA 7F3A9C`.
    pub common_name: String,
    /// SHA-256 of the DER certificate, as 64 lowercase hex digits.
    pub fingerprint: String,
    /// Start of validity, seconds since the Unix epoch.
    pub not_before: i64,
    /// End of validity, seconds since the Unix epoch.
    pub not_after: i64,
    /// DNS names it may issue for (each one also covers its subdomains).
    pub permitted_dns: Vec<String>,
    /// Address ranges it may issue for, as `address/prefix`.
    pub permitted_ips: Vec<String>,
}

/// The CA: its certificate and the key that signs with it.
pub struct LocalCa {
    cert_pem: String,
    cert_der: Vec<u8>,
    key: KeyPair,
    info: CaInfo,
}

impl fmt::Debug for LocalCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The key is never printed.
        f.debug_struct("LocalCa")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl LocalCa {
    /// The certificate in PEM: what is trusted, and what PHP and Node are given.
    #[must_use]
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// The certificate in DER: what the trust store receives.
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// What the certificate says about itself.
    #[must_use]
    pub fn info(&self) -> &CaInfo {
        &self.info
    }

    /// The signing key, for issuing certificates ([`crate::ca`] only).
    pub(crate) fn key(&self) -> &KeyPair {
        &self.key
    }

    /// Whether this CA may issue for names under `tld` (`test`, `dev.test`, `localhost`…).
    #[must_use]
    pub fn covers(&self, tld: &str) -> bool {
        let tld = tld.trim_start_matches('.').to_ascii_lowercase();
        self.info
            .permitted_dns
            .iter()
            .any(|d| tld == *d || tld.ends_with(&format!(".{d}")))
    }
}

/// The result of [`ensure_ca`].
#[derive(Debug)]
pub struct EnsuredCa {
    /// The CA.
    pub ca: LocalCa,
    /// It was made now (and must be trusted); `false` if it already existed.
    pub created: bool,
}

/// The CA in `certs/`, made if there is none. `tld` and `now` matter only when it is made.
///
/// An existing CA is read and checked (privacy of `certs/`, pair, self-signed, constraints,
/// validity) and returned untouched; see the module documentation for what is an error.
pub fn ensure_ca(
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    tld: &str,
    now: SystemTime,
) -> Result<EnsuredCa, CaError> {
    match load_ca(paths, dirs, now)? {
        Some(ca) => Ok(EnsuredCa { ca, created: false }),
        None => Ok(EnsuredCa {
            ca: create_ca(paths, dirs, tld, now, false)?,
            created: true,
        }),
    }
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> CaError {
    let path = path.to_path_buf();
    move |source| CaError::Io {
        action,
        path,
        source,
    }
}

/// Creates `certs/` private if it is not there (and the home, if Crisol is the one making it),
/// whatever the state of the rest of the layout, and then checks that nothing is open.
fn ensure_private_certs(paths: &CrisolPaths, dirs: &dyn PrivateDirs) -> Result<(), CaError> {
    let certs = paths.certs_dir();
    if !certs.exists() {
        let home = paths.home();
        if !home.exists() {
            if let Some(parent) = home.parent() {
                fs::create_dir_all(parent).map_err(io_error("cannot create", parent))?;
            }
            dirs.create_private_dir(home)
                .map_err(io_error("cannot create", home))?;
        }
        dirs.create_private_dir(&certs)
            .map_err(io_error("cannot create", &certs))?;
    }
    check_not_exposed(paths, dirs)
}

/// Refuses if `certs/` or a CA file that exists can be written by other accounts. Also catches
/// a `certs/` that someone else replaced (the owner counts).
fn check_not_exposed(paths: &CrisolPaths, dirs: &dyn PrivateDirs) -> Result<(), CaError> {
    check_paths_not_exposed(dirs, &[paths.certs_dir(), paths.ca_cert(), paths.ca_key()])
}

/// Refuses if any of these paths that exists can be written by other accounts.
fn check_paths_not_exposed(
    dirs: &dyn PrivateDirs,
    list: &[std::path::PathBuf],
) -> Result<(), CaError> {
    for path in list {
        if path.exists()
            && dirs
                .is_writable_by_others(path)
                .map_err(io_error("cannot check the permissions of", path))?
        {
            return Err(CaError::Exposed { path: path.clone() });
        }
    }
    Ok(())
}

/// Reads the CA, if there is one. `Ok(None)` when neither file exists.
pub fn load_ca(
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    now: SystemTime,
) -> Result<Option<LocalCa>, CaError> {
    check_not_exposed(paths, dirs)?;
    let (cert_path, key_path) = (paths.ca_cert(), paths.ca_key());
    let read = |path: &Path| match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CaError::Io {
            action: "cannot read",
            path: path.to_path_buf(),
            source,
        }),
    };
    let (cert_pem, key_pem) = match (read(&cert_path)?, read(&key_path)?) {
        (None, None) => return Ok(None),
        (Some(cert), Some(key)) => (cert, key),
        (Some(_), None) => {
            return Err(CaError::Incomplete {
                present: "ca.pem",
                missing: "ca.key",
            })
        }
        (None, Some(_)) => {
            return Err(CaError::Incomplete {
                present: "ca.key",
                missing: "ca.pem",
            })
        }
    };
    let key = KeyPair::from_pem(&key_pem).map_err(|e| CaError::Invalid(format!("ca.key: {e}")))?;
    let cert_der = pem_to_der(&cert_pem)?;
    let info = inspect(&cert_der, &key)?;
    if unix(now) > info.not_after {
        return Err(CaError::Expired);
    }
    Ok(Some(LocalCa {
        cert_pem,
        cert_der,
        key,
        info,
    }))
}

/// Makes a new CA and writes it to `certs/`.
///
/// With `replace` false it refuses if there is any CA file; with `replace` true it overwrites
/// both. The old CA, if it was trusted, stays in the trust store until it is removed there.
pub fn create_ca(
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    tld: &str,
    now: SystemTime,
    replace: bool,
) -> Result<LocalCa, CaError> {
    if !is_special_use(tld) {
        return Err(CaError::TldNotSpecialUse(tld.trim_matches('.').to_owned()));
    }
    ensure_private_certs(paths, dirs)?;
    if !replace && (paths.ca_cert().exists() || paths.ca_key().exists()) {
        return Err(CaError::AlreadyExists);
    }
    let key = KeyPair::generate().map_err(|e| CaError::Invalid(e.to_string()))?;
    let id = key_id(&key);
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, format!("Crisol Local CA {id}"));
    name.push(DnType::OrganizationName, "Crisol");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.name_constraints = Some(name_constraints(tld));
    params.use_authority_key_identifier_extension = true;
    let start = OffsetDateTime::from(now) - Duration::days(1);
    params.not_before = start;
    params.not_after = start + Duration::days(CA_VALIDITY_DAYS + 1);
    params.serial_number = Some(serial(&key, now));

    let cert = params
        .self_signed(&key)
        .map_err(|e| CaError::Invalid(e.to_string()))?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let info = inspect(&cert_der, &key)?;

    write_files(
        &paths.certs_dir(),
        &[
            (paths.ca_key(), &key.serialize_pem()),
            (paths.ca_cert(), &cert_pem),
        ],
    )?;
    Ok(LocalCa {
        cert_pem,
        cert_der,
        key,
        info,
    })
}

fn name_constraints(tld: &str) -> NameConstraints {
    let tld = tld.trim_start_matches('.').to_ascii_lowercase();
    let mut dns: Vec<String> = ALWAYS_PERMITTED.iter().map(|s| (*s).to_owned()).collect();
    let covered = dns
        .iter()
        .any(|d| tld == *d || tld.ends_with(&format!(".{d}")));
    if !covered && !tld.is_empty() {
        dns.push(tld);
    }
    let mut permitted: Vec<GeneralSubtree> = dns.into_iter().map(GeneralSubtree::DnsName).collect();
    permitted.push(GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0)),
        8,
    )));
    permitted.push(GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        128,
    )));
    NameConstraints {
        permitted_subtrees: permitted,
        excluded_subtrees: Vec::new(),
    }
}

/// Six hex digits that identify the key: the start of the SHA-256 of its public half.
fn key_id(key: &KeyPair) -> String {
    let digest = Sha256::digest(key.public_key_raw());
    hex(&digest[..3]).to_ascii_uppercase()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// A serial number nobody else will pick: 16 bytes from the key and the time, positive.
fn serial(key: &KeyPair, now: SystemTime) -> SerialNumber {
    let mut hasher = Sha256::new();
    hasher.update(key.public_key_raw());
    hasher.update(unix(now).to_be_bytes());
    let mut bytes = hasher.finalize()[..16].to_vec();
    bytes[0] &= 0x7f;
    if bytes[0] == 0 {
        bytes[0] = 1;
    }
    SerialNumber::from_slice(&bytes)
}

fn unix(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

fn pem_to_der(pem: &str) -> Result<Vec<u8>, CaError> {
    let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|e| CaError::Invalid(format!("ca.pem: {e}")))?;
    if parsed.label != "CERTIFICATE" {
        return Err(CaError::Invalid(format!(
            "ca.pem holds a `{}`, not a certificate",
            parsed.label
        )));
    }
    Ok(parsed.contents)
}

/// Reads the certificate and checks everything the module documentation lists: it is the
/// certificate of `key`, self-signed, a CA with `pathlen = 0` and `keyCertSign`, with critical
/// name constraints that permit only local names.
fn inspect(der: &[u8], key: &KeyPair) -> Result<CaInfo, CaError> {
    let invalid = |what: &str, e: &dyn fmt::Display| CaError::Invalid(format!("{what}: {e}"));
    let refuse = |why: &str| CaError::Invalid(why.to_owned());
    let (_, cert) = X509Certificate::from_der(der).map_err(|e| invalid("the certificate", &e))?;
    if cert.public_key().subject_public_key.data.as_ref() != key.public_key_raw() {
        return Err(CaError::KeyMismatch);
    }
    if cert.issuer() != cert.subject() || cert.verify_signature(None).is_err() {
        return Err(refuse("it is not self-signed"));
    }
    let basic = cert
        .basic_constraints()
        .map_err(|e| invalid("basic constraints", &e))?
        .ok_or(CaError::NotACa)?;
    if !basic.value.ca {
        return Err(CaError::NotACa);
    }
    if basic.value.path_len_constraint != Some(0) {
        return Err(refuse(
            "its path length is not 0: it could sign other authorities",
        ));
    }
    let may_sign = cert
        .key_usage()
        .map_err(|e| invalid("key usage", &e))?
        .is_some_and(|u| u.value.key_cert_sign());
    if !may_sign {
        return Err(refuse("it may not sign certificates (no keyCertSign)"));
    }

    let constraints = cert
        .name_constraints()
        .map_err(|e| invalid("name constraints", &e))?
        .ok_or_else(|| refuse("it has no name constraints: it could vouch for any site"))?;
    if !constraints.critical {
        return Err(refuse(
            "its name constraints are not critical: a client may ignore them",
        ));
    }
    let permitted = constraints
        .value
        .permitted_subtrees
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| refuse("its name constraints permit no names"))?;
    let (mut permitted_dns, mut permitted_ips) = (Vec::new(), Vec::new());
    for subtree in permitted {
        match &subtree.base {
            GeneralName::DNSName(name) if is_special_use(name) => {
                permitted_dns.push((*name).to_ascii_lowercase());
            }
            GeneralName::DNSName(name) => {
                return Err(CaError::Invalid(format!(
                    "it may issue for `{name}`, which is not a name reserved for local use"
                )));
            }
            GeneralName::IPAddress(bytes) if ip_is_local(bytes) => permitted_ips.push(cidr(bytes)),
            GeneralName::IPAddress(bytes) => {
                return Err(CaError::Invalid(format!(
                    "it may issue for the addresses {}, which are not local",
                    cidr(bytes)
                )));
            }
            other => {
                return Err(CaError::Invalid(format!(
                    "it permits a kind of name Crisol never uses ({other:?})"
                )));
            }
        }
    }
    let common_name = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .unwrap_or_default()
        .to_owned();
    Ok(CaInfo {
        common_name,
        fingerprint: hex(&Sha256::digest(der)),
        not_before: cert.validity().not_before.timestamp(),
        not_after: cert.validity().not_after.timestamp(),
        permitted_dns,
        permitted_ips,
    })
}

/// Whether an IP name constraint (address followed by its mask) stays inside `127.0.0.0/8` or
/// is `::1/128`.
fn ip_is_local(bytes: &[u8]) -> bool {
    let ones = |mask: &[u8]| mask.iter().map(|b| b.count_ones()).sum::<u32>();
    match bytes.len() {
        8 => bytes[0] == 127 && ones(&bytes[4..]) >= 8 && bytes[4] == 0xFF,
        32 => {
            let mut address = [0_u8; 16];
            address.copy_from_slice(&bytes[..16]);
            Ipv6Addr::from(address) == Ipv6Addr::LOCALHOST && ones(&bytes[16..]) == 128
        }
        _ => false,
    }
}

/// `address/prefix` from the bytes of an IP name constraint (address followed by its mask).
fn cidr(bytes: &[u8]) -> String {
    let prefix = |mask: &[u8]| mask.iter().map(|b| b.count_ones()).sum::<u32>();
    match bytes.len() {
        8 => {
            let mut a = [0_u8; 4];
            a.copy_from_slice(&bytes[..4]);
            format!("{}/{}", Ipv4Addr::from(a), prefix(&bytes[4..]))
        }
        32 => {
            let mut a = [0_u8; 16];
            a.copy_from_slice(&bytes[..16]);
            format!("{}/{}", Ipv6Addr::from(a), prefix(&bytes[16..]))
        }
        _ => "?".to_owned(),
    }
}

/// Writes `files` (path, text) into `dir`: all of them to temporary files first, so a failure
/// while preparing the last one leaves none of them in place; then each is moved over its
/// target.
pub(crate) fn write_files(dir: &Path, files: &[(std::path::PathBuf, &str)]) -> Result<(), CaError> {
    let io = |action: &'static str, path: &Path| {
        let path = path.to_path_buf();
        move |source| CaError::Io {
            action,
            path,
            source,
        }
    };
    fs::create_dir_all(dir).map_err(io("cannot create", dir))?;
    let mut staged = Vec::with_capacity(files.len());
    for (path, text) in files {
        let mut file = tempfile::Builder::new()
            .prefix(".crisol-cert-")
            .tempfile_in(dir)
            .map_err(io("cannot create a temporary file in", dir))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.as_file().sync_all())
            .map_err(io("cannot write", path))?;
        staged.push((path, file));
    }
    for (path, file) in staged {
        file.persist(path)
            .map_err(|e| io("cannot write", path)(e.error))?;
    }
    Ok(())
}

/// Writes the copy of the CA certificate that Node is pointed at (`etc/node/ca.pem`), from a
/// CA that has just been validated, and returns its path. The folder is made private when it
/// does not exist, so the copy cannot be swapped while the rest of the home is still open; when
/// the folder or the file already exist they get the same check as `certs/`, and open to other
/// accounts means [`CaError::Exposed`] and nothing is written (review of phase 6, V1).
/// Nothing is written if the file already has exactly this content.
pub fn write_node_ca(
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    ca: &LocalCa,
) -> Result<std::path::PathBuf, CaError> {
    let path = paths
        .node_ca()
        .map_err(|e| CaError::Invalid(e.to_string()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    if dir.exists() {
        check_paths_not_exposed(dirs, &[dir.to_path_buf(), path.clone()])?;
    } else {
        if let Some(etc) = dir.parent() {
            fs::create_dir_all(etc).map_err(io_error("cannot create", etc))?;
        }
        dirs.create_private_dir(dir)
            .map_err(io_error("cannot create", dir))?;
    }
    if fs::read_to_string(&path).ok().as_deref() != Some(ca.cert_pem()) {
        write_files(dir, &[(path.clone(), ca.cert_pem())])?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
