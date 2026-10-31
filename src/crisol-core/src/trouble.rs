//! What still holds against the web stack: the last `blocked` and the last `reload_failed`
//! (ADR-0025, extension of 2026-10-31).
//!
//! The daemon folds its own events with [`WebTrouble::note`] to put them in the `status`
//! picture, and a client that follows the events folds them the same way, so the picture and
//! the events never disagree on what is still wrong.

use crisol_protocol::Event;

/// The service whose start makes earlier trouble moot.
const NGINX: &str = "nginx";

/// What still holds against the web stack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebTrouble {
    /// Crisol will not change the running web stack until a folder is closed.
    pub blocked: Option<String>,
    /// The new web configuration was not applied, and the previous one goes on.
    pub reload_error: Option<String>,
}

impl WebTrouble {
    /// Takes `event` into account:
    ///
    /// - `blocked` and `reload_failed` are remembered;
    /// - `reload_started` forgets the last failure (a new attempt is under way);
    /// - `reloaded` forgets both: the configuration of the files is the one Nginx serves, so
    ///   whatever was holding it back no longer does;
    /// - `nginx` going to `starting` forgets both: a new start tests the configuration again
    ///   and says what goes wrong after this event.
    pub fn note(&mut self, event: &Event) {
        match event {
            Event::Blocked { reason } => self.blocked = Some(reason.clone()),
            Event::ReloadFailed { reason } => self.reload_error = Some(reason.clone()),
            Event::ReloadStarted => self.reload_error = None,
            Event::Reloaded => *self = Self::default(),
            Event::ServiceState { service, state, .. }
                if service == NGINX && state == "starting" =>
            {
                *self = Self::default();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nginx(state: &str) -> Event {
        Event::ServiceState {
            service: NGINX.to_owned(),
            state: state.to_owned(),
            detail: None,
        }
    }

    fn failed(reason: &str) -> Event {
        Event::ReloadFailed {
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn a_block_and_a_failure_stay_until_a_configuration_is_applied() {
        let mut trouble = WebTrouble::default();
        trouble.note(&Event::Blocked {
            reason: "close the home".to_owned(),
        });
        trouble.note(&failed("nginx -t said no"));
        trouble.note(&nginx("running"));
        assert_eq!(trouble.blocked.as_deref(), Some("close the home"));
        assert_eq!(trouble.reload_error.as_deref(), Some("nginx -t said no"));
        trouble.note(&Event::Reloaded);
        assert_eq!(trouble, WebTrouble::default());
    }

    #[test]
    fn a_new_attempt_forgets_the_last_failure_but_not_the_block() {
        let mut trouble = WebTrouble {
            blocked: Some("close the home".to_owned()),
            reload_error: Some("old".to_owned()),
        };
        trouble.note(&Event::ReloadStarted);
        assert_eq!(trouble.reload_error, None);
        assert_eq!(trouble.blocked.as_deref(), Some("close the home"));
    }

    #[test]
    fn a_new_start_of_nginx_forgets_both_and_what_fails_after_it_counts() {
        let mut trouble = WebTrouble {
            blocked: Some("close the home".to_owned()),
            reload_error: Some("could not start: the home is open".to_owned()),
        };
        trouble.note(&nginx("starting"));
        assert_eq!(trouble, WebTrouble::default());
        trouble.note(&failed(
            "the new configuration failed; the earlier one is in use",
        ));
        assert!(trouble.reload_error.is_some());
        // Another service starting says nothing about Nginx.
        trouble.note(&Event::ServiceState {
            service: "php-8.4.26".to_owned(),
            state: "starting".to_owned(),
            detail: None,
        });
        assert!(trouble.reload_error.is_some());
    }
}
