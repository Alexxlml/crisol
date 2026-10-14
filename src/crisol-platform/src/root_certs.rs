//! Windows implementation of [`RootCertificates`]: the roots the system trusts (ADR-0011).
//!
//! It reads the `ROOT` certificate store of every place Windows keeps one: the current user,
//! the machine, and what a company distributes by group policy or as an enterprise store. The
//! `Disallowed` store of the same places is read too, and anything in it is dropped: a
//! certificate somebody has explicitly distrusted must never end up in a trust file.
//!
//! The stores are opened **read-only** and existing only (`CERT_STORE_READONLY_FLAG`,
//! `CERT_STORE_OPEN_EXISTING_FLAG`): nothing is created and nothing is changed. A store that
//! is not there (a machine without group-policy roots) is simply empty. Windows downloads
//! some roots only when it first needs them, so the list can be missing some of those; the
//! Mozilla bundle covers the public ones.

use std::collections::BTreeSet;
use std::io;
use std::os::windows::ffi::OsStrExt;

use crisol_core::cacert::RootCertificates;
use windows::Win32::Security::Cryptography::{
    CertCloseStore, CertEnumCertificatesInStore, CertOpenStore, CERT_OPEN_STORE_FLAGS,
    CERT_QUERY_ENCODING_TYPE, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_W,
    CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER,
    CERT_SYSTEM_STORE_CURRENT_USER_GROUP_POLICY_ID, CERT_SYSTEM_STORE_LOCAL_MACHINE,
    CERT_SYSTEM_STORE_LOCAL_MACHINE_ENTERPRISE_ID, CERT_SYSTEM_STORE_LOCAL_MACHINE_GROUP_POLICY_ID,
    CERT_SYSTEM_STORE_LOCATION_SHIFT,
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
    fn trusted_roots(&self) -> io::Result<Vec<Vec<u8>>> {
        let stores = read_roots()?;
        Ok(stores
            .roots
            .into_iter()
            .filter(|der| !stores.disallowed.contains(der))
            .collect())
    }
}

/// What the stores of all locations hold, each without duplicates.
struct Stores {
    /// The `ROOT` stores.
    roots: Vec<Vec<u8>>,
    /// The `Disallowed` stores.
    disallowed: BTreeSet<Vec<u8>>,
}

fn read_roots() -> io::Result<Stores> {
    let mut roots = BTreeSet::new();
    let mut disallowed = BTreeSet::new();
    let mut opened_any = false;
    let mut last_error = None;
    for location in LOCATIONS {
        for (name, into) in [("ROOT", &mut roots), ("Disallowed", &mut disallowed)] {
            match read_store(location, name) {
                Ok(Some(certificates)) => {
                    opened_any = true;
                    into.extend(certificates);
                }
                // The store does not exist in this location: nothing there.
                Ok(None) => opened_any = true,
                Err(error) => last_error = Some(error),
            }
        }
    }
    match (opened_any, last_error) {
        (false, Some(error)) => Err(error),
        _ => Ok(Stores {
            roots: roots.into_iter().collect(),
            disallowed,
        }),
    }
}

/// Every certificate of the store `name` at `location`, DER encoded. `Ok(None)` if there is
/// no such store.
fn read_store(location: u32, name: &str) -> io::Result<Option<Vec<Vec<u8>>>> {
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
        found.push(der);
        previous = Some(context.cast_const());
    }
    // SAFETY: `store` was opened above and nothing refers to it any more.
    let _ = unsafe { CertCloseStore(Some(store), 0) };
    Ok(Some(found))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::cacert::merge_bundle;

    #[test]
    fn the_system_has_trusted_roots_and_they_look_like_certificates() {
        let roots = WindowsRootCertificates.trusted_roots().unwrap();
        assert!(roots.len() > 5, "only {} roots", roots.len());
        for der in &roots {
            assert_eq!(der[0], 0x30, "a DER certificate starts with a SEQUENCE");
            assert!(der.len() > 200);
        }
    }

    #[test]
    fn nothing_distrusted_is_ever_returned() {
        let Stores { roots, disallowed } = read_roots().unwrap();
        let trusted = WindowsRootCertificates.trusted_roots().unwrap();
        assert!(trusted.iter().all(|der| !disallowed.contains(der)));
        // And the filter is the only reason a root can be missing from the result.
        let kept: BTreeSet<_> = trusted.into_iter().collect();
        for der in roots {
            assert_eq!(kept.contains(&der), !disallowed.contains(&der));
        }
    }

    #[test]
    fn the_list_has_no_duplicates() {
        let roots = WindowsRootCertificates.trusted_roots().unwrap();
        let unique: BTreeSet<_> = roots.iter().collect();
        assert_eq!(unique.len(), roots.len());
    }

    #[test]
    fn the_real_roots_go_through_the_merge_without_losing_any() {
        let roots = WindowsRootCertificates.trusted_roots().unwrap();
        let merged = merge_bundle("## nothing from Mozilla\n", &roots);
        assert_eq!(merged.added, roots.len());
        assert_eq!(merged.skipped, 0);
        assert_eq!(
            merged.text.matches("-----BEGIN CERTIFICATE-----").count(),
            roots.len()
        );
    }
}
