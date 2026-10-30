//! An in-memory [`TrustStore`] for tests.

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{AddOutcome, RemoveOutcome, TrustStore};

#[derive(Debug, Default)]
struct State {
    certs: Vec<Vec<u8>>,
    decline_next: bool,
    decline_removals: bool,
    fail: Option<String>,
    adds: usize,
    removes: usize,
}

/// Scriptable fake of [`TrustStore`].
#[derive(Debug, Default)]
pub struct FakeTrustStore {
    state: Mutex<State>,
}

impl FakeTrustStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that already holds these certificates (DER).
    #[must_use]
    pub fn with(certs: &[&[u8]]) -> Self {
        let store = Self::new();
        store.state().certs = certs.iter().map(|c| c.to_vec()).collect();
        store
    }

    /// The next [`add`](TrustStore::add) of a certificate that is not there is refused, like a
    /// user who clicks "No" in the system's dialog.
    pub fn decline_next(&self) {
        self.state().decline_next = true;
    }

    /// Every [`remove`](TrustStore::remove) of a certificate that is there is refused from now
    /// on, like a user who clicks "No" each time.
    pub fn decline_removals(&self) {
        self.state().decline_removals = true;
    }

    /// Every call fails with this message until [`recover`](Self::recover).
    pub fn fail_with(&self, message: &str) {
        self.state().fail = Some(message.to_owned());
    }

    /// Calls work again.
    pub fn recover(&self) {
        self.state().fail = None;
    }

    /// How many certificates were added and removed.
    #[must_use]
    pub fn changes(&self) -> (usize, usize) {
        let state = self.state();
        (state.adds, state.removes)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check(&self) -> io::Result<()> {
        match &self.state().fail {
            Some(message) => Err(io::Error::other(message.clone())),
            None => Ok(()),
        }
    }
}

impl TrustStore for FakeTrustStore {
    fn contains(&self, cert_der: &[u8]) -> io::Result<bool> {
        self.check()?;
        Ok(self.state().certs.iter().any(|c| c == cert_der))
    }

    fn add(&self, cert_der: &[u8]) -> io::Result<AddOutcome> {
        self.check()?;
        let mut state = self.state();
        if state.certs.iter().any(|c| c == cert_der) {
            return Ok(AddOutcome::AlreadyThere);
        }
        if std::mem::take(&mut state.decline_next) {
            return Ok(AddOutcome::Declined);
        }
        state.certs.push(cert_der.to_vec());
        state.adds += 1;
        Ok(AddOutcome::Added)
    }

    fn remove(&self, cert_der: &[u8]) -> io::Result<RemoveOutcome> {
        self.check()?;
        let mut state = self.state();
        if !state.certs.iter().any(|c| c == cert_der) {
            return Ok(RemoveOutcome::Absent);
        }
        if state.decline_removals {
            return Ok(RemoveOutcome::Declined);
        }
        state.certs.retain(|c| c != cert_der);
        state.removes += 1;
        Ok(RemoveOutcome::Removed)
    }

    fn certificates(&self) -> io::Result<Vec<Vec<u8>>> {
        self.check()?;
        Ok(self.state().certs.clone())
    }
}
