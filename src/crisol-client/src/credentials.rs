//! The credentials screen (F13.6): what a project needs to connect to each data service, and
//! whether its password can be changed right now.
//!
//! The daemon says the `.env` lines (`credentials`); this marks which values are secret, makes
//! the text a click copies and decides whether *reset password* is offered, with the reason
//! when it is not, so a click never reaches the daemon only to be refused. The page draws it.

use crisol_core::dbservice::{DbKind, PasswordChange};
use crisol_protocol::{Call, CredentialsInfo, ErrorCode, Reply, RpcError};
use serde::Serialize;

use crate::connect;
use crate::context::Context;
use crate::live::{Connection, LiveState};

/// The services that have credentials to show, in the order Crisol lists them.
#[must_use]
pub fn credential_services() -> Vec<&'static str> {
    DbKind::ALL.iter().map(|kind| kind.service()).collect()
}

/// Whether the password of a service can be changed now, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetOffer {
    /// Yes: the button is there.
    Available,
    /// The service has no password (Mailpit, Memcached): no button.
    NoPassword,
    /// Changing a password goes through the daemon, and there is none.
    NeedsDaemon,
    /// It reads its password only when it starts (Garnet, Meilisearch): stop it first.
    StopFirst,
    /// It is changed by asking the running server (MySQL, MariaDB, PostgreSQL, MongoDB): start
    /// it first.
    StartFirst,
    /// It is starting or stopping: wait a moment.
    Busy,
}

/// Whether the password of `kind` can be changed given the state of its service (`running`,
/// `stopped`…; `None` when the daemon does not list it) and whether there is a daemon. The rule
/// is the daemon's own ([`DbKind::password_change`]), so that the answer to a click is not a
/// refusal.
#[must_use]
pub fn reset_offer(kind: DbKind, service_state: Option<&str>, daemon: bool) -> ResetOffer {
    if kind.password_change(false) == PasswordChange::NoPassword {
        return ResetOffer::NoPassword;
    }
    if !daemon {
        return ResetOffer::NeedsDaemon;
    }
    let state = service_state.unwrap_or("stopped");
    if matches!(state, "starting" | "stopping") {
        return ResetOffer::Busy;
    }
    match kind.password_change(state == "running") {
        PasswordChange::Ready => ResetOffer::Available,
        PasswordChange::NoPassword => ResetOffer::NoPassword,
        PasswordChange::StopFirst => ResetOffer::StopFirst,
        PasswordChange::StartFirst => ResetOffer::StartFirst,
    }
}

/// One line of the `.env`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialLine {
    /// `DB_PASSWORD`.
    pub key: String,
    /// Its value.
    pub value: String,
    /// The value is a password or a key: drawn hidden until it is asked for.
    pub secret: bool,
}

/// Whether the value of `key` is something to keep out of sight.
#[must_use]
pub fn is_secret(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    ["PASSWORD", "SECRET", "KEY", "TOKEN"]
        .iter()
        .any(|word| key.contains(word))
}

/// What the credentials screen draws for one service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialsView {
    /// The service.
    pub service: String,
    /// The lines, in order.
    pub lines: Vec<CredentialLine>,
    /// Things to know, as the core words them.
    pub notes: Vec<String>,
    /// All the lines as the text of an `.env`, for the button that copies them.
    pub env: String,
    /// Whether the password can be changed now.
    pub reset: ResetOffer,
}

/// Puts the daemon's answer in the shape the screen draws. `service_state` is the state of the
/// service in the live view.
#[must_use]
pub fn credentials_view(
    info: CredentialsInfo,
    service_state: Option<&str>,
    daemon: bool,
) -> CredentialsView {
    let reset = DbKind::from_service(&info.service).map_or(ResetOffer::NoPassword, |kind| {
        reset_offer(kind, service_state, daemon)
    });
    let env = info.lines.iter().fold(String::new(), |mut text, line| {
        text.push_str(&line.key);
        text.push('=');
        text.push_str(&line.value);
        text.push('\n');
        text
    });
    CredentialsView {
        reset,
        env,
        lines: info
            .lines
            .into_iter()
            .map(|line| CredentialLine {
                secret: is_secret(&line.key),
                key: line.key,
                value: line.value,
            })
            .collect(),
        notes: info.notes,
        service: info.service,
    }
}

/// Asks for the credentials of `service`. Reading them starts nothing; a service that is not
/// installed is an error with the daemon's words.
pub fn fetch_credentials(
    context: &Context,
    service: &str,
    live: &LiveState,
) -> Result<CredentialsView, RpcError> {
    let call = Call::Credentials {
        service: service.to_owned(),
    };
    match connect::call(context, &call, &mut |_| {})? {
        Reply::Credentials(info) => {
            let state = live
                .services
                .iter()
                .find(|s| s.name == service)
                .map(|s| s.state.as_str());
            Ok(credentials_view(
                info,
                state,
                live.connection == Connection::Connected,
            ))
        }
        other => Err(RpcError::new(
            ErrorCode::Internal,
            format!("unexpected answer to `credentials`: {other:?}"),
        )),
    }
}

#[cfg(test)]
mod tests;
