//! What a tray icon and a header say about the daemon, decided from a [`LiveState`] (F13.3).
//!
//! The interface only draws this: which icon, which entry the menu offers, and whether leaving
//! needs a word of warning. The words themselves (translated) belong to the interface; what
//! comes from the core (a refusal, a warning) is passed on as it is.

use crisol_protocol::Call;
use serde::{Deserialize, Serialize};

use crate::live::{Connection, LiveState};

/// The general state, which picks the tray icon (ADR-0027).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    /// Something is running and nothing failed or is changing.
    Running,
    /// A service is starting or stopping.
    Starting,
    /// Nothing runs.
    Stopped,
    /// A service failed, or the last web configuration was not applied.
    Failed,
    /// There is no daemon to ask (not started, gone, or one that cannot be used).
    NoDaemon,
}

impl Toggle {
    /// The call the "all" entry makes.
    #[must_use]
    pub fn call(self) -> Call {
        match self {
            Self::StartAll => Call::Start { service: None },
            Self::StopAll => Call::Stop { service: None },
        }
    }
}

impl Overall {
    /// The name of the tray icon for this state, without the theme: the files of
    /// `assets/brand/tray/` are `<name>-dark` and `<name>-light` (ADR-0027).
    #[must_use]
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Starting => "starting",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::NoDaemon => "no-daemon",
        }
    }
}

/// What the "all" entry of the menu does now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Toggle {
    /// `start` without a service.
    StartAll,
    /// `stop` without a service.
    StopAll,
}

/// One service as a list draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceRow {
    /// `nginx`, `php-8.4.26`, …
    pub name: String,
    /// `stopped`, `starting`, `running`, `stopping` or `failed`.
    pub state: String,
    /// Why, for a failure; as the core said it.
    pub detail: Option<String>,
    /// The technology the name belongs to, which picks its dot (`--metal-<name>` in
    /// `tokens.css`, ADR-0027): only an identification, never a state.
    pub metal: &'static str,
}

/// The technology a service belongs to, by name. `other` for the ones that have no color of
/// their own.
#[must_use]
pub fn metal_of(service: &str) -> &'static str {
    let family = service.split('-').next().unwrap_or(service);
    match family {
        "php" => "php",
        "nginx" => "nginx",
        "mysql" => "mysql",
        "mariadb" => "mariadb",
        "postgresql" => "postgresql",
        "mongodb" => "mongodb",
        "redis" => "redis",
        "meilisearch" => "meilisearch",
        // `node` is not a service today, but its color is in the tokens.
        "node" => "node",
        _ => "other",
    }
}

/// What the interface needs to draw the tray and the header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// The general state.
    pub overall: Overall,
    /// What the menu entry for "all" does.
    pub toggle: Toggle,
    /// The version of the daemon that answers, when one does.
    pub daemon_version: Option<String>,
    /// Words of the core to show as they are: why the daemon cannot be used.
    pub refusal: Option<String>,
    /// The services, in the order the daemon gives them.
    pub rows: Vec<ServiceRow>,
}

/// The states in which a service holds a place that "stop all" would free.
fn is_active(state: &str) -> bool {
    matches!(state, "running" | "starting")
}

/// What to do before leaving the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveAction {
    /// Just leave.
    Leave,
    /// Say once that the services go on, then leave.
    WarnFirst,
}

impl LiveState {
    /// The general state of what the daemon runs.
    #[must_use]
    pub fn overall(&self) -> Overall {
        if self.connection != Connection::Connected {
            return Overall::NoDaemon;
        }
        let in_state = |wanted: &str| self.services.iter().any(|s| s.state == wanted);
        if in_state("failed") || self.reload_error.is_some() {
            Overall::Failed
        } else if in_state("starting") || in_state("stopping") {
            Overall::Starting
        } else if in_state("running") {
            Overall::Running
        } else {
            Overall::Stopped
        }
    }

    /// Whether any service is running or on its way to.
    #[must_use]
    pub fn any_active(&self) -> bool {
        self.services.iter().any(|s| is_active(&s.state))
    }

    /// What the tray and the header need.
    #[must_use]
    pub fn summary(&self) -> Summary {
        Summary {
            overall: self.overall(),
            toggle: if self.any_active() {
                Toggle::StopAll
            } else {
                Toggle::StartAll
            },
            daemon_version: self.daemon.as_ref().map(|d| d.daemon.clone()),
            refusal: match &self.connection {
                Connection::Refused(why) => Some(why.clone()),
                _ => None,
            },
            rows: self
                .services
                .iter()
                .map(|service| ServiceRow {
                    name: service.name.clone(),
                    state: service.state.clone(),
                    detail: service.detail.clone(),
                    metal: metal_of(&service.name),
                })
                .collect(),
        }
    }

    /// Whether leaving the interface needs the one-time warning that the services go on:
    /// they do, and the user has not been told yet (`already_told`).
    #[must_use]
    pub fn leave_action(&self, already_told: bool) -> LeaveAction {
        if !already_told && self.connection == Connection::Connected && self.any_active() {
            LeaveAction::WarnFirst
        } else {
            LeaveAction::Leave
        }
    }
}

#[cfg(test)]
mod tests;
