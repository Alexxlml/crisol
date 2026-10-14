//! Port probing: is a TCP port free, and if not, who has it?
//!
//! Crisol never frees a port by killing whoever holds it (that is the wrong call:
//! the owner may be the user's other dev tool). It reports the owner and suggests
//! another port; the user decides.
//!
//! [`fake::FakePortProbe`] is the test double used by everything that depends on the trait.

pub mod fake;

use std::net::SocketAddr;

/// Highest port number.
pub const MAX_PORT: u16 = u16::MAX;

/// A process that is listening on a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortOwner {
    /// Operating-system process id.
    pub pid: u32,
    /// Executable name (e.g. `nginx.exe`), if the system lets us read it. System processes
    /// and processes of other users may hide it.
    pub name: Option<String>,
    /// Address and port it is bound to.
    pub address: SocketAddr,
}

/// What a probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortStatus {
    /// Nothing is listening and a service could bind it.
    Free,
    /// Something is listening. The list can be empty when the system refuses to bind the
    /// port but does not say who holds it.
    InUse(Vec<PortOwner>),
    /// The operating system keeps this port for itself (on Windows: the excluded port
    /// ranges used by Hyper-V, WSL and Docker). No program can bind it.
    Reserved,
}

impl PortStatus {
    /// Whether a service could bind the port.
    #[must_use]
    pub fn is_free(&self) -> bool {
        matches!(self, Self::Free)
    }
}

/// Why a probe could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    /// The operating system failed to answer.
    #[error("{0}")]
    Os(String),
}

/// Looks at TCP ports.
pub trait PortProbe: Send + Sync {
    /// Describes the state of `port` (TCP, IPv4 and IPv6, any local address).
    ///
    /// This may briefly try to claim the port to learn more, so it is for the question "can
    /// I start here?". It must **not** be used to ask whether a service that is starting is
    /// up yet: the trial could make the service's own bind fail. Use
    /// [`is_listening`](PortProbe::is_listening) for that.
    fn probe(&self, port: u16) -> Result<PortStatus, PortError>;

    /// Whether something accepts connections on `port` right now. Implementations should
    /// only read the system's listener tables and never claim the port. The default goes
    /// through [`probe`](PortProbe::probe), which suits doubles that claim nothing.
    fn is_listening(&self, port: u16) -> Result<bool, PortError> {
        Ok(matches!(self.probe(port)?, PortStatus::InUse(owners) if !owners.is_empty()))
    }

    /// The first free port after `port`, looking at most `attempts` ports ahead. Ports that
    /// are in use or reserved are skipped. `None` if none is free in that window.
    fn suggest_free(&self, port: u16, attempts: u16) -> Result<Option<u16>, PortError> {
        let last = port.saturating_add(attempts);
        for candidate in port.saturating_add(1)..=last {
            if self.probe(candidate)?.is_free() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }
}
