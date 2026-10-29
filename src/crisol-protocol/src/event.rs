//! What the daemon tells without being asked.

use serde::{Deserialize, Serialize};

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
    /// The daemon is going away.
    ShuttingDown,
}
