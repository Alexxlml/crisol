//! What the daemon tells without being asked.

use serde::{Deserialize, Serialize};

/// An event as it travels: its fields and, from the daemon's stream of events, its number.
///
/// The numbers go up by one in the order the daemon sends the events, so a client knows two
/// things (review of phase 12, finding 1): which events a `status` picture already includes
/// (those numbered up to [`StatusInfo::seq`](crate::StatusInfo::seq)), and that it lost some
/// when a number is skipped. The progress of a call, sent only to the one who asked, has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbered {
    /// What happened.
    #[serde(flatten)]
    pub event: Event,
    /// Its place in the stream of events; absent for the progress of a call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// Something that happened, pushed to subscribers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A service changed state (`stopped`, `starting`, `running`, `stopping`, `failed`).
    ServiceState {
        /// `nginx`, `php-8.4.26`, …
        service: String,
        /// The state it is in now.
        state: String,
        /// Why, for a failure (`port 80 is already in use by …`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A new web configuration is being tested.
    ReloadStarted,
    /// Nginx accepted the configuration and was told to reload.
    Reloaded,
    /// The new configuration was not put in place.
    ReloadFailed {
        /// What went wrong.
        reason: String,
    },
    /// Crisol will not start or change anything until a folder is closed; the message is meant
    /// to be shown as it is.
    Blocked {
        /// The reason and the command to fix it.
        reason: String,
    },
    /// A line was added to a service's log.
    Log {
        /// The service.
        service: String,
        /// The line, without its newline.
        line: String,
    },
    /// A download or an installation advanced.
    Progress {
        /// What is being installed (`php 8.4.26`).
        what: String,
        /// Bytes received so far.
        done: u64,
        /// Bytes in total, when the server said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        /// The bytes come from a file the user gave (`install --from`), not from the network.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        from_file: bool,
    },
    /// What the daemon has to say about the running web stack changed: this is the whole list
    /// now, the same as `status` would give (empty when nothing holds any more).
    Warnings {
        /// The texts, ready to show.
        warnings: Vec<String>,
    },
    /// The daemon is going away: its services are down (or were killed) and this is the last
    /// event a subscriber gets.
    ShuttingDown,
    /// An event of a newer daemon that this version does not know. Never sent: it is what one
    /// reads as, so that a new kind of event is compatible (protocol 4). Its `seq` still counts.
    #[serde(other)]
    Unknown,
}
