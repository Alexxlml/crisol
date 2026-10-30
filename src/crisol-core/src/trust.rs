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
use std::time::SystemTime;

use x509_parser::prelude::{FromDer, X509Certificate};

use crate::ca::{ensure_ca, load_ca, CaError, LocalCa};
use crate::paths::{require_private_home, CrisolPaths, HomeError, PrivateDirs};

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

/// What happened when a certificate was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// It is no longer in the store.
    Removed,
    /// It was not there; nothing changed.
    Absent,
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

    /// Removes this exact certificate. The operating system may ask the user to confirm; a
    /// refusal is [`RemoveOutcome::Declined`], not an error.
    fn remove(&self, cert_der: &[u8]) -> io::Result<RemoveOutcome>;

    /// Every certificate in the store (DER). Read only.
    fn certificates(&self) -> io::Result<Vec<Vec<u8>>>;

    /// How the system's own confirmation names this certificate (Windows shows its SHA-1
    /// thumbprint), so the user can tell it is the one Crisol described. `None` if the system
    /// shows nothing of the kind.
    fn thumbprint(&self, _cert_der: &[u8]) -> Option<String> {
        None
    }
}

/// Why the CA was not trusted.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// The home is open to other accounts: nothing is trusted from it.
    #[error(transparent)]
    Home(#[from] HomeError),
    /// The local CA could not be made or read.
    #[error(transparent)]
    Ca(#[from] CaError),
    /// The trust store failed.
    #[error("trust store: {0}")]
    Store(#[from] io::Error),
}

/// What `crisol trust` shows before the system asks: the CA, made now if there was none.
#[derive(Debug)]
pub struct TrustPlan {
    /// The CA to trust.
    pub ca: LocalCa,
    /// It was made now.
    pub created: bool,
    /// It is trusted already: nothing will be asked.
    pub already: bool,
    /// How the system's confirmation names it ([`TrustStore::thumbprint`]).
    pub thumbprint: Option<String>,
    /// Older Crisol CAs still trusted (`crisol untrust --stale` removes them).
    pub stale: Vec<String>,
}

/// Gets the CA ready to be trusted (F11.1): the one in `certs/`, or a new one for `tld` if there
/// is none. Nothing is added to the store yet: the caller shows the plan and then calls
/// [`trust_ca`], which is when the system asks the user.
pub fn prepare_trust(
    store: &dyn TrustStore,
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    tld: &str,
    now: SystemTime,
) -> Result<TrustPlan, TrustError> {
    require_private_home(paths, dirs)?;
    let ensured = ensure_ca(paths, dirs, tld, now)?;
    let state = status(store, &ensured.ca)?;
    Ok(TrustPlan {
        thumbprint: store.thumbprint(ensured.ca.cert_der()),
        already: state.trusted,
        stale: state.stale.into_iter().map(|c| c.common_name).collect(),
        created: ensured.created,
        ca: ensured.ca,
    })
}

/// What `crisol untrust` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untrusted {
    /// The CAs taken out of the store, by name.
    pub removed: Vec<String>,
    /// The CAs the user would not let go of, by name.
    pub declined: Vec<String>,
    /// The name of the CA in `certs/`, if there is one.
    pub current: Option<String>,
    /// Older Crisol CAs still in the store after this.
    pub stale_left: usize,
}

/// Takes the CA of `certs/` out of the store, or, with `stale`, the older Crisol CAs and never
/// the current one. The files in `certs/` stay: the sites keep their certificates, which
/// browsers then warn about. A CA that can no longer be read (expired, say) is not the current
/// one, so `stale` takes it out too.
pub fn untrust_local(
    store: &dyn TrustStore,
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    stale: bool,
    now: SystemTime,
) -> Result<Untrusted, TrustError> {
    let current = match load_ca(paths, dirs, now) {
        Ok(ca) => ca,
        Err(_) if stale => None,
        Err(error) => return Err(error.into()),
    };
    let current_der = current.as_ref().map(LocalCa::cert_der);
    let (mut removed, mut declined) = (Vec::new(), Vec::new());
    let mut note = |name: String, outcome| match outcome {
        RemoveOutcome::Removed => removed.push(name),
        RemoveOutcome::Declined => declined.push(name),
        RemoveOutcome::Absent => {}
    };
    if stale {
        for old in crisol_cas_except(store, current_der)? {
            note(old.common_name, store.remove(&old.der)?);
        }
    } else if let Some(ca) = &current {
        note(ca.info().common_name.clone(), untrust_ca(store, ca)?);
    }
    let stale_left = crisol_cas_except(store, current_der)?.len();
    Ok(Untrusted {
        removed,
        declined,
        current: current.map(|ca| ca.info().common_name.clone()),
        stale_left,
    })
}

/// Makes the system trust the CA. Refuses if the home, or the folders Crisol runs or trusts,
/// can be written by other accounts: whoever could replace a binary could also have replaced
/// what this certificate vouches for.
pub fn trust_ca(
    store: &dyn TrustStore,
    ca: &LocalCa,
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
) -> Result<AddOutcome, TrustError> {
    require_private_home(paths, dirs)?;
    Ok(store.add(ca.cert_der())?)
}

/// Stops trusting the CA.
pub fn untrust_ca(store: &dyn TrustStore, ca: &LocalCa) -> io::Result<RemoveOutcome> {
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
    crisol_cas_except(store, Some(current.cert_der()))
}

/// The Crisol CAs in the store other than `current` (every one when there is none).
fn crisol_cas_except(store: &dyn TrustStore, current: Option<&[u8]>) -> io::Result<Vec<TrustedCa>> {
    let mut found = Vec::new();
    for der in store.certificates()? {
        if current == Some(der.as_slice()) {
            continue;
        }
        if let Some(common_name) = crisol_ca_name(&der) {
            found.push(TrustedCa { common_name, der });
        }
    }
    found.sort_by(|a, b| a.common_name.cmp(&b.common_name));
    Ok(found)
}

/// Removes the older Crisol CAs ([`stale_cas`]) from the store and returns the ones that went.
pub fn remove_stale(store: &dyn TrustStore, current: &LocalCa) -> io::Result<Vec<TrustedCa>> {
    let stale = stale_cas(store, current)?;
    let mut removed = Vec::new();
    for ca in stale {
        if store.remove(&ca.der)? == RemoveOutcome::Removed {
            removed.push(ca);
        }
    }
    Ok(removed)
}

/// Every Crisol CA in the store, the current one included: what `uninstall --all` takes out.
pub fn crisol_cas(store: &dyn TrustStore) -> io::Result<Vec<TrustedCa>> {
    crisol_cas_except(store, None)
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
