//! Trusting the local CA (F6.3, ADR-0013).
//!
//! The browsers (Edge, Chrome) validate HTTPS against the operating system's trust store, so for
//! `https://shop.test` to open without a warning the local CA ([`crate::ca`]) has to be in it.
//! [`TrustStore`] is that store, behind a trait: the Windows implementation (`crisol-platform`)
//! is the *current user's* `Root` store, which needs no administrator but makes Windows show a
//! confirmation dialog when a certificate is added; [`fake::FakeTrustStore`] is the double that
//! tests use.
//!
//! Rules (ADR-0013):
//!
//! - **Only the CA, only on request.** Nothing here runs by itself: installing is a command the
//!   user types (`crisol trust`), and what is installed is `certs/ca.pem`, never a site
//!   certificate.
//! - **Tests never touch the real store.** They use the double, or, for the Windows
//!   implementation, a store private to the test. The one real installation is tried by hand by
//!   the owner (`crisol-platform/examples/trust_manual.rs`).
//! - **Removal is exact.** [`untrust_ca`] removes the certificate it is given and nothing else.
//!   A CA that was replaced ([`crate::ca::create_ca`] with `replace`) stays trusted until it is
//!   removed; [`stale_cas`] finds those (a certificate named `Crisol Local CA …`, issued by
//!   itself, which is not the current one) and [`remove_stale`] removes them on request.

use std::io;

use x509_parser::prelude::{FromDer, X509Certificate};

use crate::ca::LocalCa;

pub mod fake;

/// What happened when a certificate was added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// It is in the store now.
    Added,
    /// It already was; nothing changed.
    AlreadyThere,
    /// The user answered no to the operating system's confirmation. Nothing changed.
    Declined,
}

/// A store of certificates the system trusts as roots.
pub trait TrustStore: Send + Sync {
    /// Whether this exact certificate (DER) is in the store.
    fn contains(&self, cert_der: &[u8]) -> io::Result<bool>;

    /// Adds the certificate as a trusted root. The operating system may ask the user to
    /// confirm; a refusal is [`AddOutcome::Declined`], not an error.
    fn add(&self, cert_der: &[u8]) -> io::Result<AddOutcome>;

    /// Removes this exact certificate. Returns whether it was there.
    fn remove(&self, cert_der: &[u8]) -> io::Result<bool>;

    /// Every certificate in the store (DER). Read only.
    fn certificates(&self) -> io::Result<Vec<Vec<u8>>>;
}

/// Makes the system trust the CA.
pub fn trust_ca(store: &dyn TrustStore, ca: &LocalCa) -> io::Result<AddOutcome> {
    store.add(ca.cert_der())
}

/// Stops trusting the CA. Returns whether it was trusted.
pub fn untrust_ca(store: &dyn TrustStore, ca: &LocalCa) -> io::Result<bool> {
    store.remove(ca.cert_der())
}

/// A Crisol CA found in a trust store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedCa {
    /// Its common name: `Crisol Local CA 7F3A9C`.
    pub common_name: String,
    /// The certificate (DER).
    pub der: Vec<u8>,
}

/// Where the CA stands in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustStatus {
    /// The current CA is trusted.
    pub trusted: bool,
    /// Older Crisol CAs that are still trusted.
    pub stale: Vec<TrustedCa>,
}

/// Whether the CA is trusted, and which older Crisol CAs are still in the store.
pub fn status(store: &dyn TrustStore, ca: &LocalCa) -> io::Result<TrustStatus> {
    Ok(TrustStatus {
        trusted: store.contains(ca.cert_der())?,
        stale: stale_cas(store, ca)?,
    })
}

/// The Crisol CAs in the store other than `current`: a certificate whose common name starts
/// with `Crisol Local CA `, whose organization is `Crisol`, that is a CA and is issued by
/// itself. Nothing else is ever treated as ours.
pub fn stale_cas(store: &dyn TrustStore, current: &LocalCa) -> io::Result<Vec<TrustedCa>> {
    let mut found = Vec::new();
    for der in store.certificates()? {
        if der == current.cert_der() {
            continue;
        }
        if let Some(common_name) = crisol_ca_name(&der) {
            found.push(TrustedCa { common_name, der });
        }
    }
    found.sort_by(|a, b| a.common_name.cmp(&b.common_name));
    Ok(found)
}

/// Removes the older Crisol CAs ([`stale_cas`]) from the store and returns them.
pub fn remove_stale(store: &dyn TrustStore, current: &LocalCa) -> io::Result<Vec<TrustedCa>> {
    let stale = stale_cas(store, current)?;
    let mut removed = Vec::new();
    for ca in stale {
        if store.remove(&ca.der)? {
            removed.push(ca);
        }
    }
    Ok(removed)
}

/// The common name if `der` is one of our CAs.
fn crisol_ca_name(der: &[u8]) -> Option<String> {
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    let name = cert.subject().iter_common_name().next()?.as_str().ok()?;
    let ours = name.starts_with("Crisol Local CA ")
        && cert
            .subject()
            .iter_organization()
            .any(|o| o.as_str().is_ok_and(|o| o == "Crisol"))
        && cert.issuer() == cert.subject()
        && cert
            .basic_constraints()
            .ok()
            .flatten()
            .is_some_and(|b| b.value.ca);
    ours.then(|| name.to_owned())
}

#[cfg(test)]
mod tests;
