//! Windows implementation of [`RootCertificates`]: the roots the system trusts (ADR-0011).
//!
//! It reads the `ROOT` certificate store of every place Windows keeps one: the current user,
//! the machine, and what a company distributes by group policy or as an enterprise store. The
//! `Disallowed` store of the same places is read too, and anything in it is dropped: a
//! certificate somebody has explicitly distrusted must never end up in a trust file.
//!
//! Besides the certificate, each root carries what Windows says about its **use**: the
//! extended key usage property (a root limited to code signing or mail) and the partial
//! distrust properties (how Microsoft retires a CA without deleting it). The core decides
//! with them which roots may authenticate TLS servers; this module only reads the properties.
//!
//! The stores are opened **read-only** and existing only (`CERT_STORE_READONLY_FLAG`,
//! `CERT_STORE_OPEN_EXISTING_FLAG`): nothing is created and nothing is changed. A store that
//! is not there (a machine without group-policy roots) is simply empty. Windows downloads
//! some roots only when it first needs them, so the list can be missing some of those; the
//! Mozilla bundle covers the public ones.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::windows::ffi::OsStrExt;

use crisol_core::cacert::{RootCertificates, SystemRoot};
use windows::Win32::Security::Cryptography::{
    CertCloseStore, CertEnumCertificatesInStore, CertGetCertificateContextProperty,
    CertGetEnhancedKeyUsage, CertOpenStore, CERT_CONTEXT, CERT_DISALLOWED_ENHKEY_USAGE_PROP_ID,
    CERT_DISALLOWED_FILETIME_PROP_ID, CERT_ENHKEY_USAGE_PROP_ID,
    CERT_FIND_PROP_ONLY_ENHKEY_USAGE_FLAG, CERT_OPEN_STORE_FLAGS, CERT_QUERY_ENCODING_TYPE,
    CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG,
    CERT_SYSTEM_STORE_CURRENT_USER, CERT_SYSTEM_STORE_CURRENT_USER_GROUP_POLICY_ID,
    CERT_SYSTEM_STORE_LOCAL_MACHINE, CERT_SYSTEM_STORE_LOCAL_MACHINE_ENTERPRISE_ID,
    CERT_SYSTEM_STORE_LOCAL_MACHINE_GROUP_POLICY_ID, CERT_SYSTEM_STORE_LOCATION_SHIFT, CTL_USAGE,
};

/// Where Windows keeps certificate stores. A location is its id in the high half of the
/// flags; the crate only names the first two as ready-made flags.
const LOCATIONS: [u32; 5] = [
    CERT_SYSTEM_STORE_CURRENT_USER,
    CERT_SYSTEM_STORE_LOCAL_MACHINE,
    CERT_SYSTEM_STORE_CURRENT_USER_GROUP_POLICY_ID << CERT_SYSTEM_STORE_LOCATION_SHIFT,
    CERT_SYSTEM_STORE_LOCAL_MACHINE_GROUP_POLICY_ID << CERT_SYSTEM_STORE_LOCATION_SHIFT,
    CERT_SYSTEM_STORE_LOCAL_MACHINE_ENTERPRISE_ID << CERT_SYSTEM_STORE_LOCATION_SHIFT,
];

/// Reads the trusted root certificates of Windows.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsRootCertificates;

impl WindowsRootCertificates {
    /// A new reader. It holds no state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl RootCertificates for WindowsRootCertificates {
    fn trusted_roots(&self) -> io::Result<Vec<SystemRoot>> {
        let stores = read_roots()?;
        Ok(stores
            .roots
            .into_values()
            .filter(|root| !stores.disallowed.contains(&root.der))
            .collect())
    }
}

/// What the stores of all locations hold, each without duplicates.
struct Stores {
    /// The `ROOT` stores, by certificate. A certificate in several places is merged keeping
    /// the most restrictive reading of its properties.
    roots: BTreeMap<Vec<u8>, SystemRoot>,
    /// The `Disallowed` stores.
    disallowed: BTreeSet<Vec<u8>>,
}

fn read_roots() -> io::Result<Stores> {
    let mut roots: BTreeMap<Vec<u8>, SystemRoot> = BTreeMap::new();
    let mut disallowed = BTreeSet::new();
    let mut opened_any = false;
    let mut last_error = None;
    for location in LOCATIONS {
        for (name, is_root) in [("ROOT", true), ("Disallowed", false)] {
            match read_store(location, name, is_root) {
                Ok(Some(found)) => {
                    opened_any = true;
                    for root in found {
                        if !is_root {
                            disallowed.insert(root.der);
                        } else if let Some(known) = roots.get_mut(&root.der) {
                            restrict(known, root);
                        } else {
                            roots.insert(root.der.clone(), root);
                        }
                    }
                }
                // The store does not exist in this location: nothing there.
                Ok(None) => opened_any = true,
                Err(error) => last_error = Some(error),
            }
        }
    }
    match (opened_any, last_error) {
        (false, Some(error)) => Err(error),
        _ => Ok(Stores { roots, disallowed }),
    }
}

/// Folds a second reading of the same certificate into `known`, keeping the stricter one.
fn restrict(known: &mut SystemRoot, other: SystemRoot) {
    known.partially_distrusted |= other.partially_distrusted;
    known.usages = match (known.usages.take(), other.usages) {
        (Some(a), Some(b)) => Some(a.into_iter().filter(|oid| b.contains(oid)).collect()),
        (a, b) => a.or(b),
    };
}

/// Every certificate of the store `name` at `location`, DER encoded, with its usage
/// properties when `with_properties`. `Ok(None)` if there is no such store.
fn read_store(
    location: u32,
    name: &str,
    with_properties: bool,
) -> io::Result<Option<Vec<SystemRoot>>> {
    let wide: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let flags = CERT_OPEN_STORE_FLAGS(
        location | CERT_STORE_READONLY_FLAG.0 | CERT_STORE_OPEN_EXISTING_FLAG.0,
    );
    // SAFETY: the provider is the documented `CERT_STORE_PROV_SYSTEM_W`, whose parameter is a
    // NUL-terminated UTF-16 store name; `wide` outlives the call.
    let store = match unsafe {
        CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            CERT_QUERY_ENCODING_TYPE(0),
            None,
            flags,
            Some(wide.as_ptr().cast()),
        )
    } {
        Ok(store) => store,
        Err(error) => {
            // ERROR_FILE_NOT_FOUND (2) / ERROR_PATH_NOT_FOUND (3): the store is not there.
            let code = error.code().0 & 0xFFFF;
            return if code == 2 || code == 3 {
                Ok(None)
            } else {
                Err(io::Error::from(error))
            };
        }
    };

    let mut found = Vec::new();
    let mut previous = None;
    loop {
        // SAFETY: `store` is open. Passing back the previous context lets the call free it,
        // so no context is left to release when it returns null at the end.
        let context = unsafe { CertEnumCertificatesInStore(store, previous) };
        if context.is_null() {
            break;
        }
        // SAFETY: a non-null context points to a valid CERT_CONTEXT whose encoded bytes stay
        // valid until the next call; they are copied right away.
        let der = unsafe {
            std::slice::from_raw_parts((*context).pbCertEncoded, (*context).cbCertEncoded as usize)
                .to_vec()
        };
        let (usages, partially_distrusted) = if with_properties {
            // SAFETY: `context` is a valid context of the open store.
            unsafe { (enhanced_usages(context), is_partially_distrusted(context)) }
        } else {
            (None, false)
        };
        found.push(SystemRoot {
            der,
            usages,
            partially_distrusted,
        });
        previous = Some(context.cast_const());
    }
    // SAFETY: `store` was opened above and nothing refers to it any more.
    let _ = unsafe { CertCloseStore(Some(store), 0) };
    Ok(Some(found))
}

/// Whether the certificate has the property `id` at all.
///
/// # Safety
/// `context` must point to a valid certificate context.
unsafe fn has_property(context: *const CERT_CONTEXT, id: u32) -> bool {
    let mut size = 0u32;
    // SAFETY: asking for the size only (no buffer) is the documented way to test for a property.
    unsafe { CertGetCertificateContextProperty(context, id, None, &raw mut size) }.is_ok()
}

/// The purposes the certificate is limited to by its extended key usage **property** (the one
/// Windows keeps next to the certificate, not the extension inside it), as dotted OIDs.
/// `None`: the property is absent, so there is no restriction.
///
/// # Safety
/// `context` must point to a valid certificate context.
unsafe fn enhanced_usages(context: *const CERT_CONTEXT) -> Option<Vec<String>> {
    // SAFETY: forwarded from this function's contract.
    if !unsafe { has_property(context, CERT_ENHKEY_USAGE_PROP_ID) } {
        return None;
    }
    let flags = CERT_FIND_PROP_ONLY_ENHKEY_USAGE_FLAG.0;
    let mut size = 0u32;
    // SAFETY: the first call only asks how big the buffer must be.
    if unsafe { CertGetEnhancedKeyUsage(context, flags, None, &raw mut size) }.is_err() {
        // A restriction that cannot be read is treated as the strictest one.
        return Some(Vec::new());
    }
    // u64 elements keep the buffer aligned for the structure Windows writes in it.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
    let usage = buffer.as_mut_ptr().cast::<CTL_USAGE>();
    // SAFETY: the buffer is at least `size` bytes and aligned for CTL_USAGE.
    if unsafe { CertGetEnhancedKeyUsage(context, flags, Some(usage), &raw mut size) }.is_err() {
        return Some(Vec::new());
    }
    // SAFETY: on success `usage` holds `cUsageIdentifier` pointers to NUL-terminated ANSI
    // strings stored inside the same buffer, which is alive here.
    let oids = unsafe {
        let count = (*usage).cUsageIdentifier as usize;
        (0..count)
            .filter_map(|i| (*(*usage).rgpszUsageIdentifier.add(i)).to_string().ok())
            .collect()
    };
    Some(oids)
}

/// Whether Windows partially distrusts the certificate: a "disallowed since" date or a list of
/// purposes it must no longer be used for. The second property's exact content is not parsed;
/// having it at all counts, which can only make the result stricter.
///
/// # Safety
/// `context` must point to a valid certificate context.
unsafe fn is_partially_distrusted(context: *const CERT_CONTEXT) -> bool {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        has_property(context, CERT_DISALLOWED_FILETIME_PROP_ID)
            || has_property(context, CERT_DISALLOWED_ENHKEY_USAGE_PROP_ID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::cacert::merge_bundle;

    fn ders(roots: Vec<SystemRoot>) -> Vec<Vec<u8>> {
        roots.into_iter().map(|r| r.der).collect()
    }

    #[test]
    fn the_system_has_trusted_roots_and_they_look_like_certificates() {
        let roots = WindowsRootCertificates.trusted_roots().unwrap();
        assert!(roots.len() > 5, "only {} roots", roots.len());
        for root in &roots {
            assert_eq!(
                root.der[0], 0x30,
                "a DER certificate starts with a SEQUENCE"
            );
            assert!(root.der.len() > 200);
        }
    }

    #[test]
    fn nothing_distrusted_is_ever_returned() {
        let Stores { roots, disallowed } = read_roots().unwrap();
        let trusted = ders(WindowsRootCertificates.trusted_roots().unwrap());
        assert!(trusted.iter().all(|der| !disallowed.contains(der)));
        // And the filter is the only reason a root can be missing from the result.
        let kept: BTreeSet<_> = trusted.into_iter().collect();
        for der in roots.into_keys() {
            assert_eq!(kept.contains(&der), !disallowed.contains(&der));
        }
    }

    #[test]
    fn the_list_has_no_duplicates() {
        let roots = ders(WindowsRootCertificates.trusted_roots().unwrap());
        let unique: BTreeSet<_> = roots.iter().collect();
        assert_eq!(unique.len(), roots.len());
    }

    #[test]
    fn the_real_store_has_roots_fit_for_tls_and_the_count_is_reported() {
        let roots = WindowsRootCertificates.trusted_roots().unwrap();
        let fit = roots.iter().filter(|r| r.is_fit_for_tls()).count();
        // Printed so the figure can be noted in the ADR (`cargo test -- --nocapture`).
        println!(
            "system roots: {} trusted, {fit} fit for TLS, {} restricted or partially distrusted",
            roots.len(),
            roots.len() - fit
        );
        assert!(fit > 5, "only {fit} roots fit for TLS");
    }

    #[test]
    fn the_real_roots_go_through_the_merge_without_losing_any() {
        let roots = ders(WindowsRootCertificates.trusted_roots().unwrap());
        let merged = merge_bundle("## nothing from Mozilla\n", &roots);
        assert_eq!(merged.added, roots.len());
        assert_eq!(merged.skipped, 0);
        assert_eq!(
            merged.text.matches("-----BEGIN CERTIFICATE-----").count(),
            roots.len()
        );
    }

    #[test]
    fn two_readings_of_a_certificate_keep_the_stricter_one() {
        let mut known = SystemRoot::unrestricted(vec![0x30; 20]);
        let mut other = known.clone();
        other.usages = Some(vec!["1.3.6.1.5.5.7.3.1".into(), "1.3.6.1.5.5.7.3.3".into()]);
        restrict(&mut known, other);
        assert_eq!(known.usages.as_ref().map(Vec::len), Some(2));

        let mut third = SystemRoot::unrestricted(vec![0x30; 20]);
        third.usages = Some(vec!["1.3.6.1.5.5.7.3.3".into()]);
        third.partially_distrusted = true;
        restrict(&mut known, third);
        assert_eq!(known.usages, Some(vec!["1.3.6.1.5.5.7.3.3".to_owned()]));
        assert!(known.partially_distrusted);
        assert!(!known.is_fit_for_tls());
    }
}
