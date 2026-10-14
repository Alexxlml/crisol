//! An in-memory [`PortProbe`] for tests.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{PortError, PortOwner, PortProbe, PortStatus};

#[derive(Debug, Default)]
struct State {
    owners: BTreeMap<u16, Vec<PortOwner>>,
    reserved: Vec<std::ops::RangeInclusive<u16>>,
    unknown_owner: Vec<u16>,
    probes: Vec<u16>,
    listen_checks: Vec<u16>,
    failure: Option<String>,
}

/// Scriptable fake of [`PortProbe`]. Every port is free until a test says otherwise.
#[derive(Debug, Default)]
pub struct FakePortProbe {
    state: Mutex<State>,
}

impl FakePortProbe {
    /// A probe where every port is free.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes `port` busy, held by `owner`. Call again to add more listeners.
    pub fn occupy(&self, port: u16, owner: PortOwner) {
        self.state().owners.entry(port).or_default().push(owner);
    }

    /// Makes `port` busy without knowing who holds it.
    pub fn occupy_unknown(&self, port: u16) {
        self.state().unknown_owner.push(port);
    }

    /// Reserves a range for the operating system.
    pub fn reserve(&self, range: std::ops::RangeInclusive<u16>) {
        self.state().reserved.push(range);
    }

    /// Frees `port` again (not the reserved ranges).
    pub fn release(&self, port: u16) {
        let mut state = self.state();
        state.owners.remove(&port);
        state.unknown_owner.retain(|p| *p != port);
    }

    /// Makes every following probe fail with `message`.
    pub fn fail_with(&self, message: &str) {
        self.state().failure = Some(message.to_owned());
    }

    /// Ports probed so far, in order.
    #[must_use]
    pub fn probed(&self) -> Vec<u16> {
        self.state().probes.clone()
    }

    /// Ports asked about with [`PortProbe::is_listening`] so far, in order. Kept apart from
    /// [`probed`](Self::probed) so tests can tell "can I start here?" from "is it up yet?".
    #[must_use]
    pub fn listen_checked(&self) -> Vec<u16> {
        self.state().listen_checks.clone()
    }
}

impl PortProbe for FakePortProbe {
    fn probe(&self, port: u16) -> Result<PortStatus, PortError> {
        let mut state = self.state();
        state.probes.push(port);
        if let Some(message) = &state.failure {
            return Err(PortError::Os(message.clone()));
        }
        if state.reserved.iter().any(|r| r.contains(&port)) {
            return Ok(PortStatus::Reserved);
        }
        if let Some(owners) = state.owners.get(&port) {
            return Ok(PortStatus::InUse(owners.clone()));
        }
        if state.unknown_owner.contains(&port) {
            return Ok(PortStatus::InUse(Vec::new()));
        }
        Ok(PortStatus::Free)
    }

    fn is_listening(&self, port: u16) -> Result<bool, PortError> {
        let mut state = self.state();
        state.listen_checks.push(port);
        if let Some(message) = &state.failure {
            return Err(PortError::Os(message.clone()));
        }
        Ok(state.owners.contains_key(&port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    fn owner(pid: u32, name: &str, port: u16) -> PortOwner {
        PortOwner {
            pid,
            name: Some(name.to_owned()),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        }
    }

    #[test]
    fn every_port_is_free_by_default() {
        let probe = FakePortProbe::new();
        assert_eq!(probe.probe(19_000).unwrap(), PortStatus::Free);
        assert!(probe.probe(80).unwrap().is_free());
    }

    #[test]
    fn an_occupied_port_reports_every_owner() {
        let probe = FakePortProbe::new();
        probe.occupy(8080, owner(10, "nginx.exe", 8080));
        probe.occupy(8080, owner(11, "nginx.exe", 8080));
        match probe.probe(8080).unwrap() {
            PortStatus::InUse(owners) => {
                assert_eq!(owners.iter().map(|o| o.pid).collect::<Vec<_>>(), [10, 11]);
                assert_eq!(owners[0].name.as_deref(), Some("nginx.exe"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(!probe.probe(8080).unwrap().is_free());
    }

    #[test]
    fn a_port_in_use_by_someone_unknown_has_no_owners() {
        let probe = FakePortProbe::new();
        probe.occupy_unknown(3306);
        assert_eq!(probe.probe(3306).unwrap(), PortStatus::InUse(Vec::new()));
    }

    #[test]
    fn reserved_ranges_win_and_release_frees_ports() {
        let probe = FakePortProbe::new();
        probe.reserve(50_000..=50_059);
        probe.occupy(50_010, owner(1, "x.exe", 50_010));
        assert_eq!(probe.probe(50_010).unwrap(), PortStatus::Reserved);
        assert_eq!(probe.probe(50_060).unwrap(), PortStatus::Free);

        probe.occupy(9000, owner(2, "y.exe", 9000));
        probe.release(9000);
        assert!(probe.probe(9000).unwrap().is_free());
    }

    #[test]
    fn suggest_free_skips_busy_and_reserved_ports() {
        let probe = FakePortProbe::new();
        probe.occupy(8000, owner(1, "a.exe", 8000));
        probe.occupy(8001, owner(1, "a.exe", 8001));
        probe.reserve(8002..=8003);
        assert_eq!(probe.suggest_free(8000, 10).unwrap(), Some(8004));
        assert_eq!(probe.probed(), [8001, 8002, 8003, 8004]);
    }

    #[test]
    fn suggest_free_gives_up_after_the_window() {
        let probe = FakePortProbe::new();
        probe.reserve(8001..=8100);
        assert_eq!(probe.suggest_free(8000, 5).unwrap(), None);
        assert_eq!(
            probe.probed().len(),
            5,
            "it must stop after `attempts` ports"
        );
    }

    #[test]
    fn suggest_free_never_wraps_past_the_last_port() {
        let probe = FakePortProbe::new();
        probe.occupy(u16::MAX, owner(1, "a.exe", u16::MAX));
        assert_eq!(probe.suggest_free(u16::MAX - 1, 10).unwrap(), None);
        assert_eq!(probe.suggest_free(u16::MAX, 10).unwrap(), None);
    }

    #[test]
    fn failures_propagate() {
        let probe = FakePortProbe::new();
        probe.fail_with("no tables");
        assert!(matches!(probe.probe(80), Err(PortError::Os(m)) if m == "no tables"));
        assert!(probe.suggest_free(80, 3).is_err());
    }
}
