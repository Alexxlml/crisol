//! The certificate for the sites (F6.2, ADR-0013).
//!
//! One certificate serves every secured site, signed by the local CA ([`crate::ca`]):
//!
//! ```text
//! certs/sites.pem   # the certificate Nginx sends (the end certificate only)
//! certs/sites.key   # its private key (its own: the CA's key never signs anything else)
//! ```
//!
//! Names: the host of every secured site (`shop.test`), plus `localhost`, `127.0.0.1` and
//! `::1`. **No wildcard.** ADR-0013 planned `*.<tld>`, but `rustls-webpki` (Rust and Deno
//! clients) rejects a wildcard directly under a single-label TLD as a malformed name and
//! refuses the whole certificate, wherever in the list it is (checked with the verifier, see
//! the tests). Listing the secured hosts is also safer: the certificate vouches only for the
//! sites the user asked to secure. Securing or unsecuring a site changes the list and so
//! issues a new certificate, which is a reload that happens anyway.
//!
//! The certificate is *derived*, like everything Crisol generates: [`ensure_site_cert`] keeps
//! the one in `certs/` while it is current and makes a new one, without asking, when it is not:
//! missing, unreadable, not signed by this CA (the CA was replaced), for other names (the list
//! of secured sites changed), or with less than 30 days left. It lasts a year. A caller that sees
//! [`SiteCert::changed`] must reload Nginx, which keeps the old certificate in memory.

use std::collections::BTreeSet;
use std::fs;
use std::net::IpAddr;
use std::time::SystemTime;

use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SerialNumber,
};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::ca::{write_files, CaError, LocalCa};
use crate::paths::CrisolPaths;

/// How long a certificate lasts.
const VALIDITY_DAYS: i64 = 365;

/// A certificate with fewer days than this left is renewed.
const RENEW_BEFORE_DAYS: i64 = 30;

/// Why a certificate was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    /// There was none.
    Missing,
    /// The files cannot be read as a certificate and its key.
    Unreadable,
    /// It was not issued by the CA in `certs/` (the CA was replaced).
    OtherCa,
    /// It is for other names (a site was secured or unsecured).
    Names,
    /// It expires within 30 days, or has expired.
    Expiring,
}

/// The certificate for the sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteCert {
    /// `certs/sites.pem`.
    pub cert: std::path::PathBuf,
    /// `certs/sites.key`.
    pub key: std::path::PathBuf,
    /// Why a new one was made; `None` if the existing one was kept.
    pub changed: Option<Renewal>,
}

/// The names the certificate must cover, given the host names of the secured sites
/// (`shop.test`): those, sorted and without repeats, then the local names.
#[must_use]
pub fn expected_names(secured_hosts: &[String]) -> Vec<String> {
    let hosts: BTreeSet<String> = secured_hosts
        .iter()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        .collect();
    let mut names: Vec<String> = hosts.into_iter().collect();
    for local in ["localhost", "127.0.0.1", "::1"] {
        if !names.iter().any(|n| n == local) {
            names.push(local.to_owned());
        }
    }
    names
}

/// A certificate and its key that have been made but not put in `certs/` yet.
#[derive(Clone, PartialEq, Eq)]
pub struct IssuedPair {
    cert_pem: String,
    key_pem: String,
}

impl std::fmt::Debug for IssuedPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key is never printed.
        f.debug_struct("IssuedPair").finish_non_exhaustive()
    }
}

/// What [`plan_site_cert`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteCertPlan {
    /// `certs/sites.pem`: where the certificate goes (or already is).
    pub cert: std::path::PathBuf,
    /// `certs/sites.key`.
    pub key: std::path::PathBuf,
    /// Why a new one is needed; `None` if the one in `certs/` can be kept.
    pub change: Option<Renewal>,
    /// The new pair, made but **not written anywhere**. Test it first
    /// ([`stage_site_cert`]) and put it in place only afterwards ([`install_site_cert`]).
    pub pending: Option<IssuedPair>,
}

/// Decides whether the certificate in `certs/` is current and, if it is not, makes the new one
/// **in memory**. Nothing is written: a configuration that Nginx then rejects must not leave a
/// new certificate next to the old configuration (review of phase 6, finding 3).
///
/// `ca` must be the CA of `certs/`; `secured_hosts` are the host names of the secured sites,
/// all of which it must be able to issue for ([`LocalCa::covers`]).
pub fn plan_site_cert(
    paths: &CrisolPaths,
    ca: &LocalCa,
    secured_hosts: &[String],
    now: SystemTime,
) -> Result<SiteCertPlan, CaError> {
    if let Some(host) = secured_hosts.iter().find(|h| !ca.covers(h)) {
        return Err(CaError::NotCovered(host.clone()));
    }
    let names = expected_names(secured_hosts);
    let (change, pending) = match why_not_current(paths, ca, &names, now) {
        None => (None, None),
        Some(reason) => (Some(reason), Some(issue(ca, &names, now)?)),
    };
    Ok(SiteCertPlan {
        cert: paths.site_cert(),
        key: paths.site_key(),
        change,
        pending,
    })
}

/// Where a pair waits while Nginx tests it: `certs/.next/sites.pem` and `sites.key`, inside the
/// private `certs/`.
#[must_use]
pub fn staged_paths(paths: &CrisolPaths) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = paths.certs_dir().join(".next");
    (dir.join("sites.pem"), dir.join("sites.key"))
}

/// Writes the pair into the staging folder, replacing whatever was staged before, and returns
/// the paths for the configuration under test.
pub fn stage_site_cert(
    paths: &CrisolPaths,
    pair: &IssuedPair,
) -> Result<(std::path::PathBuf, std::path::PathBuf), CaError> {
    let (cert, key) = staged_paths(paths);
    if let Some(dir) = cert.parent() {
        let _ = fs::remove_dir_all(dir);
        write_files(
            dir,
            &[(key.clone(), &pair.key_pem), (cert.clone(), &pair.cert_pem)],
        )?;
    }
    Ok((cert, key))
}

/// Puts the pair in `certs/` (both files, or neither) and removes the staging folder.
pub fn install_site_cert(paths: &CrisolPaths, pair: &IssuedPair) -> Result<(), CaError> {
    write_files(
        &paths.certs_dir(),
        &[
            (paths.site_key(), &pair.key_pem),
            (paths.site_cert(), &pair.cert_pem),
        ],
    )?;
    if let Some(dir) = staged_paths(paths).0.parent() {
        let _ = fs::remove_dir_all(dir);
    }
    Ok(())
}

/// The certificate in `certs/`, made (or made again) and written at once when it is not
/// current: [`plan_site_cert`] and [`install_site_cert`] together, for callers that have no
/// configuration to test first.
pub fn ensure_site_cert(
    paths: &CrisolPaths,
    ca: &LocalCa,
    secured_hosts: &[String],
    now: SystemTime,
) -> Result<SiteCert, CaError> {
    let plan = plan_site_cert(paths, ca, secured_hosts, now)?;
    if let Some(pair) = &plan.pending {
        install_site_cert(paths, pair)?;
    }
    Ok(SiteCert {
        cert: plan.cert,
        key: plan.key,
        changed: plan.change,
    })
}

/// `None` if the certificate in `certs/` can be kept.
fn why_not_current(
    paths: &CrisolPaths,
    ca: &LocalCa,
    names: &[String],
    now: SystemTime,
) -> Option<Renewal> {
    let (cert_path, key_path) = (paths.site_cert(), paths.site_key());
    if !cert_path.exists() && !key_path.exists() {
        return Some(Renewal::Missing);
    }
    let (Ok(cert_pem), Ok(key_pem)) = (
        fs::read_to_string(&cert_path),
        fs::read_to_string(&key_path),
    ) else {
        return Some(Renewal::Unreadable);
    };
    let (Ok(key), Ok((_, pem))) = (
        KeyPair::from_pem(&key_pem),
        x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()),
    ) else {
        return Some(Renewal::Unreadable);
    };
    let Ok((_, cert)) = X509Certificate::from_der(&pem.contents) else {
        return Some(Renewal::Unreadable);
    };
    if cert.public_key().subject_public_key.data.as_ref() != key.public_key_raw() {
        return Some(Renewal::Unreadable);
    }
    let Ok((_, ca_cert)) = X509Certificate::from_der(ca.cert_der()) else {
        return Some(Renewal::OtherCa);
    };
    if cert.issuer() != ca_cert.subject()
        || cert.verify_signature(Some(ca_cert.public_key())).is_err()
    {
        return Some(Renewal::OtherCa);
    }
    if san_set(&cert) != names.iter().cloned().collect() {
        return Some(Renewal::Names);
    }
    let left = cert.validity().not_after.timestamp() - unix(now);
    let started = cert.validity().not_before.timestamp() <= unix(now);
    if left < RENEW_BEFORE_DAYS * 86_400 || !started {
        return Some(Renewal::Expiring);
    }
    None
}

/// The subject alternative names of a certificate, DNS names and addresses as text.
fn san_set(cert: &X509Certificate<'_>) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            match name {
                GeneralName::DNSName(dns) => {
                    set.insert((*dns).to_ascii_lowercase());
                }
                GeneralName::IPAddress(bytes) => {
                    if let Some(ip) = ip_from(bytes) {
                        set.insert(ip.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    set
}

fn ip_from(bytes: &[u8]) -> Option<IpAddr> {
    if let Ok(v4) = <[u8; 4]>::try_from(bytes) {
        Some(IpAddr::from(v4))
    } else if let Ok(v6) = <[u8; 16]>::try_from(bytes) {
        Some(IpAddr::from(v6))
    } else {
        None
    }
}

fn unix(time: SystemTime) -> i64 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Makes a certificate with its own key, signed by the CA, and writes both files.
fn issue(ca: &LocalCa, names: &[String], now: SystemTime) -> Result<IssuedPair, CaError> {
    let invalid = |e: rcgen::Error| CaError::Invalid(e.to_string());
    let key = KeyPair::generate().map_err(invalid)?;
    let mut params = CertificateParams::new(names.to_vec()).map_err(invalid)?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Crisol local sites");
    name.push(DnType::OrganizationName, "Crisol");
    params.distinguished_name = name;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let start = OffsetDateTime::from(now) - Duration::days(1);
    params.not_before = start;
    params.not_after = start + Duration::days(VALIDITY_DAYS + 1);
    params.serial_number = Some(serial(&key, now));

    let issuer = Issuer::from_ca_cert_pem(ca.cert_pem(), ca.key()).map_err(invalid)?;
    let cert = params.signed_by(&key, &issuer).map_err(invalid)?;
    Ok(IssuedPair {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

fn serial(key: &KeyPair, now: SystemTime) -> SerialNumber {
    let mut hasher = Sha256::new();
    hasher.update(b"sites");
    hasher.update(key.public_key_raw());
    hasher.update(unix(now).to_be_bytes());
    let mut bytes = hasher.finalize()[..16].to_vec();
    bytes[0] &= 0x7f;
    if bytes[0] == 0 {
        bytes[0] = 1;
    }
    SerialNumber::from_slice(&bytes)
}

#[cfg(test)]
mod tests;
