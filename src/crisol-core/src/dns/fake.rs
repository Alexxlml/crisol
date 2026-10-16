//! An in-memory [`NrptPolicy`] for tests.

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{NrptPolicy, NrptRule};

#[derive(Debug, Default)]
struct State {
    rules: Vec<NrptRule>,
    fail: Option<String>,
    writes: usize,
}

/// Scriptable fake of [`NrptPolicy`].
#[derive(Debug, Default)]
pub struct FakeNrpt {
    state: Mutex<State>,
}

impl FakeNrpt {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A table that already holds these rules.
    #[must_use]
    pub fn with(rules: Vec<NrptRule>) -> Self {
        let table = Self::new();
        table.state().rules = rules;
        table
    }

    /// Every call fails with this message.
    pub fn fail_with(&self, message: &str) {
        self.state().fail = Some(message.to_owned());
    }

    /// The rules now in the table.
    #[must_use]
    pub fn current(&self) -> Vec<NrptRule> {
        self.state().rules.clone()
    }

    /// How many times the table was changed.
    #[must_use]
    pub fn writes(&self) -> usize {
        self.state().writes
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check(state: &State) -> io::Result<()> {
        match &state.fail {
            Some(message) => Err(io::Error::other(message.clone())),
            None => Ok(()),
        }
    }
}

impl NrptPolicy for FakeNrpt {
    fn rules(&self) -> io::Result<Vec<NrptRule>> {
        let state = self.state();
        Self::check(&state)?;
        Ok(state.rules.clone())
    }

    fn add(&self, rule: &NrptRule) -> io::Result<()> {
        let mut state = self.state();
        Self::check(&state)?;
        state.rules.push(rule.clone());
        state.writes += 1;
        Ok(())
    }

    fn remove(&self, namespace: &str) -> io::Result<bool> {
        let mut state = self.state();
        Self::check(&state)?;
        let before = state.rules.len();
        state
            .rules
            .retain(|r| !r.namespace.eq_ignore_ascii_case(namespace));
        let removed = state.rules.len() != before;
        if removed {
            state.writes += 1;
        }
        Ok(removed)
    }
}
