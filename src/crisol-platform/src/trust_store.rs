//! Windows implementation of [`TrustStore`]: a certificate store (F6.3, ADR-0013).
//!
//! [`WindowsTrustStore::current_user_root`] is the **current user's** `Root` store
//! (`Certificates - Current User > Trusted Root Certification Authorities`). It needs no
//! administrator, and the machine's store is never touched. Windows asks the user to confirm
//! when a certificate is added to it (a dialog, and so an interactive session); answering no is
//! reported as [`AddOutcome::Declined`].
//!
//! The same code runs over a store that lives only in memory
//! ([`WindowsTrustStore::private_memory`]), which is what the tests use: they never add to or
//! remove from the real store. The real installation is tried by hand by the owner with
//! `cargo run -p crisol-platform --example trust_manual`.

use std::io;

use crisol_core::trust::{AddOutcome, TrustStore};
use windows::Win32::Security::Cryptography::{
    CertAddEncodedCertificateToStore, CertCloseStore, CertCreateCertificateContext,
    CertDeleteCertificateFromStore, CertEnumCertificatesInStore, CertFindCertificateInStore,
    CertFreeCertificateContext, CertOpenStore, CERT_CONTEXT, CERT_FIND_EXISTING,
    CERT_OPEN_STORE_FLAGS, CERT_QUERY_ENCODING_TYPE, CERT_STORE_ADD_NEW, CERT_STORE_PROV_MEMORY,
    CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_CURRENT_USER, HCERTSTORE, X509_ASN_ENCODING,
};

/// `ERROR_CANCELLED`: the user answered no.
const ERROR_CANCELLED: i32 = 1223;

/// A Windows certificate store.
#[derive(Debug)]
pub struct WindowsTrustStore {
    store: isize,
}

impl WindowsTrustStore {
    /// The current user's trusted root store. Adding to it makes Windows ask for confirmation.
    pub fn current_user_root() -> io::Result<Self> {
        let name: Vec<u16> = "ROOT".encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the provider is `CERT_STORE_PROV_SYSTEM_W`, whose parameter is a
        // NUL-terminated UTF-16 store name; `name` outlives the call.
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_CURRENT_USER),
                Some(name.as_ptr().cast()),
            )
        }
        .map_err(io::Error::from)?;
        Ok(Self::from_handle(store))
    }

    /// A store that exists only in memory and vanishes with this value. For tests.
    pub fn private_memory() -> io::Result<Self> {
        // SAFETY: the memory provider takes no parameter.
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_MEMORY,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(0),
                None,
            )
        }
        .map_err(io::Error::from)?;
        Ok(Self::from_handle(store))
    }

    fn from_handle(store: HCERTSTORE) -> Self {
        Self {
            store: store.0 as isize,
        }
    }

    fn handle(&self) -> HCERTSTORE {
        HCERTSTORE(self.store as *mut core::ffi::c_void)
    }

    /// The context in the store that is exactly `der`, if any. The caller must free it (or
    /// delete it, which frees it).
    fn find(&self, der: &[u8]) -> io::Result<*const CERT_CONTEXT> {
        // SAFETY: `der` is a valid slice; the call copies it.
        let wanted = unsafe { CertCreateCertificateContext(X509_ASN_ENCODING, der) };
        if wanted.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a DER certificate",
            ));
        }
        // SAFETY: `store` is open and `wanted` is a valid context; `CERT_FIND_EXISTING`
        // takes a context as its parameter.
        let found = unsafe {
            CertFindCertificateInStore(
                self.handle(),
                X509_ASN_ENCODING,
                0,
                CERT_FIND_EXISTING,
                Some(wanted.cast_const().cast()),
                None,
            )
        };
        // SAFETY: `wanted` was created above and is not used again.
        let _ = unsafe { CertFreeCertificateContext(Some(wanted.cast_const())) };
        Ok(found.cast_const())
    }
}

impl Drop for WindowsTrustStore {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed exactly once, here.
        let _ = unsafe { CertCloseStore(Some(self.handle()), 0) };
    }
}

impl TrustStore for WindowsTrustStore {
    fn contains(&self, cert_der: &[u8]) -> io::Result<bool> {
        let found = self.find(cert_der)?;
        if found.is_null() {
            return Ok(false);
        }
        // SAFETY: `found` is a context returned by the store.
        let _ = unsafe { CertFreeCertificateContext(Some(found)) };
        Ok(true)
    }

    fn add(&self, cert_der: &[u8]) -> io::Result<AddOutcome> {
        if self.contains(cert_der)? {
            return Ok(AddOutcome::AlreadyThere);
        }
        // SAFETY: `store` is open; `cert_der` is a valid slice; no context is asked back.
        let added = unsafe {
            CertAddEncodedCertificateToStore(
                Some(self.handle()),
                X509_ASN_ENCODING,
                cert_der,
                CERT_STORE_ADD_NEW,
                None,
            )
        };
        match added {
            Ok(()) => Ok(AddOutcome::Added),
            // The HRESULT of a Win32 error carries its code in the low 16 bits.
            Err(e) if (e.code().0 & 0xFFFF) == ERROR_CANCELLED => Ok(AddOutcome::Declined),
            Err(e) => Err(io::Error::from(e)),
        }
    }

    fn remove(&self, cert_der: &[u8]) -> io::Result<bool> {
        let found = self.find(cert_der)?;
        if found.is_null() {
            return Ok(false);
        }
        // SAFETY: `found` is a context of the store; deleting it also frees it.
        unsafe { CertDeleteCertificateFromStore(found) }.map_err(io::Error::from)?;
        Ok(true)
    }

    fn certificates(&self) -> io::Result<Vec<Vec<u8>>> {
        let mut found = Vec::new();
        let mut previous = None;
        loop {
            // SAFETY: `store` is open; passing back the previous context lets the call free it.
            let context = unsafe { CertEnumCertificatesInStore(self.handle(), previous) };
            if context.is_null() {
                break;
            }
            // SAFETY: a non-null context points to a valid CERT_CONTEXT whose bytes stay valid
            // until the next call; they are copied at once.
            found.push(unsafe {
                std::slice::from_raw_parts(
                    (*context).pbCertEncoded,
                    (*context).cbCertEncoded as usize,
                )
                .to_vec()
            });
            previous = Some(context.cast_const());
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use crisol_core::ca::ensure_ca;
    use crisol_core::paths::CrisolPaths;
    use crisol_core::trust::{remove_stale, stale_cas, status, trust_ca, untrust_ca};

    use super::*;

    fn ca() -> (tempfile::TempDir, crisol_core::ca::LocalCa) {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        let ca = ensure_ca(&paths, "test", SystemTime::now()).unwrap().ca;
        (tmp, ca)
    }

    #[test]
    fn a_certificate_goes_into_a_private_store_and_out_again() {
        let (_tmp, ca) = ca();
        let store = WindowsTrustStore::private_memory().unwrap();
        assert!(!store.contains(ca.cert_der()).unwrap());
        assert_eq!(trust_ca(&store, &ca).unwrap(), AddOutcome::Added);
        assert!(store.contains(ca.cert_der()).unwrap());
        assert_eq!(store.certificates().unwrap(), [ca.cert_der().to_vec()]);
        assert_eq!(trust_ca(&store, &ca).unwrap(), AddOutcome::AlreadyThere);

        assert!(untrust_ca(&store, &ca).unwrap());
        assert!(!store.contains(ca.cert_der()).unwrap());
        assert!(!untrust_ca(&store, &ca).unwrap(), "already gone");
        assert_eq!(store.certificates().unwrap(), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn removing_takes_exactly_one_certificate() {
        let (_tmp1, first) = ca();
        let (_tmp2, second) = ca();
        let store = WindowsTrustStore::private_memory().unwrap();
        trust_ca(&store, &first).unwrap();
        trust_ca(&store, &second).unwrap();
        assert_eq!(store.certificates().unwrap().len(), 2);
        assert!(untrust_ca(&store, &first).unwrap());
        assert_eq!(store.certificates().unwrap(), [second.cert_der().to_vec()]);
    }

    #[test]
    fn something_that_is_not_a_certificate_is_an_error() {
        let store = WindowsTrustStore::private_memory().unwrap();
        for call in [
            store.contains(b"nope").map(|_| ()),
            store.add(b"nope").map(|_| ()),
            store.remove(b"nope").map(|_| ()),
        ] {
            assert_eq!(call.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn an_old_crisol_ca_is_found_and_removed_by_the_core_logic_over_a_real_store() {
        let (_tmp1, old) = ca();
        let (_tmp2, current) = ca();
        let store = WindowsTrustStore::private_memory().unwrap();
        trust_ca(&store, &old).unwrap();
        trust_ca(&store, &current).unwrap();
        let state = status(&store, &current).unwrap();
        assert!(state.trusted);
        assert_eq!(state.stale.len(), 1);
        assert_eq!(stale_cas(&store, &current).unwrap()[0].der, old.cert_der());
        assert_eq!(remove_stale(&store, &current).unwrap().len(), 1);
        assert_eq!(store.certificates().unwrap(), [current.cert_der().to_vec()]);
    }

    #[test]
    fn two_private_stores_do_not_share_anything() {
        let (_tmp, ca) = ca();
        let (one, two) = (
            WindowsTrustStore::private_memory().unwrap(),
            WindowsTrustStore::private_memory().unwrap(),
        );
        trust_ca(&one, &ca).unwrap();
        assert!(!two.contains(ca.cert_der()).unwrap());
    }

    #[test]
    fn the_real_user_store_can_be_read_and_a_new_ca_is_not_in_it() {
        // Read only: nothing is added to or removed from the real store by a test.
        let real = WindowsTrustStore::current_user_root().unwrap();
        let (_tmp, ca) = ca();
        assert!(!real.contains(ca.cert_der()).unwrap());
        // Whatever the user has parses as certificates (there may be none in a fresh profile).
        for der in real.certificates().unwrap() {
            assert_eq!(der[0], 0x30);
        }
    }
}
