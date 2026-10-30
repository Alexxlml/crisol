//! A minimal client of the daemon (F12.2): it subscribes to the events and shows the state of
//! the services in the console as it changes.
//!
//! ```text
//! cargo run -p crisol-platform --example watch
//! ```
//!
//! It is the proof that a shell (Slint, Tauri, a web panel) can be put on top of the daemon
//! with nothing but the contract in `docs/protocolo.md`: it uses `crisol-protocol` for the
//! messages and, from the rest of Crisol, only what finds the home and opens the pipe
//! (`resolve_paths`, `pipe_name_for_user` and `connect_to_daemon`). It never starts the daemon
//! and never changes anything; it only reads. Leave it running and use `crisol start`,
//! `crisol stop` or `crisol link` in another terminal. It ends with Ctrl+C or when the daemon
//! goes away.
//!
//! The order matters, and a GUI would follow it too:
//!
//! 1. `hello`, to know the daemon speaks this protocol;
//! 2. `subscribe` **before** asking for the state, so that no change falls between the two;
//! 3. `status`, whose answer replaces whatever the events said before it;
//! 4. from then on, every event updates the board.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use crisol_platform::{connect_to_daemon, pipe_name_for_user, resolve_paths};
use crisol_protocol::{Call, Client, Event, Reply, ServiceInfo, StatusInfo, PROTOCOL_VERSION};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("watch: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let paths = resolve_paths().map_err(|e| e.to_string())?;
    let pipe = pipe_name_for_user(paths.home()).map_err(|e| e.to_string())?;
    let mut client = connect_to_daemon(&pipe).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!(
            "no daemon is running for {}; start it with `crisol start`",
            paths.home().display()
        ),
        _ => format!("cannot reach the daemon: {e}"),
    })?;
    let stdout = std::io::stdout();
    watch(&mut client, &mut stdout.lock())
}

/// Talks to the daemon on `client` and writes what happens to `out` until the daemon leaves.
fn watch<R: BufRead, W: Write>(
    client: &mut Client<R, W>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let hello = client
        .call(&Call::Hello {
            client: format!("watch example {}", crisol_platform::VERSION),
            protocol: PROTOCOL_VERSION,
        })
        .map_err(|e| e.to_string())?;
    let Reply::Hello(hello) = hello else {
        return Err(format!("`hello` answered {hello:?}"));
    };
    client.call(&Call::Subscribe).map_err(|e| e.to_string())?;

    let mut board = Board::default();
    let status = client
        .call_with_events(&Call::Status { service: None }, &mut |event| {
            board.apply(&event);
        })
        .map_err(|e| e.to_string())?;
    let Reply::Status(status) = status else {
        return Err(format!("`status` answered {status:?}"));
    };
    board.replace(status);

    let print = |out: &mut dyn Write, line: &str| -> Result<(), String> {
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string())
    };
    print(out, &format!("crisold {} at {}", hello.daemon, hello.home))?;
    for line in board.table() {
        print(out, &line)?;
    }
    loop {
        let Some(event) = client.next_event().map_err(|e| e.to_string())? else {
            return print(out, "the daemon closed the connection");
        };
        print(out, &board.apply(&event))?;
        match event {
            Event::ServiceState { .. } => print(out, &format!("  now: {}", board.summary()))?,
            Event::ShuttingDown => return Ok(()),
            _ => {}
        }
    }
}

/// What the client knows about the services: the last `status`, updated by the events.
#[derive(Debug, Default)]
struct Board {
    services: Vec<ServiceInfo>,
    warnings: Vec<String>,
}

impl Board {
    /// The daemon's own picture, which replaces what the board had.
    fn replace(&mut self, status: StatusInfo) {
        self.services = status.services;
        self.warnings = status.warnings;
    }

    /// Takes `event` into account and says it in one line.
    fn apply(&mut self, event: &Event) -> String {
        match event {
            Event::ServiceState {
                service,
                state,
                detail,
            } => {
                match self.services.iter_mut().find(|s| s.name == *service) {
                    Some(known) => {
                        known.state.clone_from(state);
                        known.detail.clone_from(detail);
                    }
                    None => self.services.push(ServiceInfo {
                        name: service.clone(),
                        state: state.clone(),
                        detail: detail.clone(),
                    }),
                }
                match detail {
                    Some(detail) => format!("{service}: {state} ({detail})"),
                    None => format!("{service}: {state}"),
                }
            }
            Event::ReloadStarted => "web: testing a new configuration".to_owned(),
            Event::Reloaded => "web: new configuration in place".to_owned(),
            Event::ReloadFailed { reason } => {
                format!(
                    "web: the new configuration was not applied, the previous one stays: {reason}"
                )
            }
            Event::Blocked { reason } => format!("blocked: {reason}"),
            Event::Log { service, line } => format!("[{service}] {line}"),
            Event::Progress {
                what, done, total, ..
            } => match total {
                Some(total) => format!("{what}: {done} of {total} bytes"),
                None => format!("{what}: {done} bytes"),
            },
            Event::ShuttingDown => "the daemon is going away".to_owned(),
        }
    }

    /// One line per service, then the warnings.
    fn table(&self) -> Vec<String> {
        if self.services.is_empty() {
            return vec!["no services".to_owned()];
        }
        let width = self
            .services
            .iter()
            .map(|s| s.name.len())
            .max()
            .unwrap_or(0);
        let mut lines: Vec<String> = self
            .services
            .iter()
            .map(|s| {
                let line = format!("  {:width$}  {}", s.name, s.state);
                match &s.detail {
                    Some(detail) => format!("{line} ({detail})"),
                    None => line,
                }
            })
            .collect();
        lines.extend(self.warnings.iter().map(|w| format!("warning: {w}")));
        lines
    }

    /// Every service and its state, in one line.
    fn summary(&self) -> String {
        self.services
            .iter()
            .map(|s| format!("{} {}", s.name, s.state))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    // Test code: a failed `unwrap` is the failure report.
    #![allow(clippy::unwrap_used)]

    use std::io::Cursor;

    use super::*;

    fn service(name: &str, state: &str) -> ServiceInfo {
        ServiceInfo {
            name: name.to_owned(),
            state: state.to_owned(),
            detail: None,
        }
    }

    fn state(service: &str, state: &str) -> Event {
        Event::ServiceState {
            service: service.to_owned(),
            state: state.to_owned(),
            detail: None,
        }
    }

    #[test]
    fn events_update_the_services_and_new_ones_are_added_at_the_end() {
        let mut board = Board::default();
        board.replace(StatusInfo {
            services: vec![
                service("nginx", "stopped"),
                service("php-8.4.26", "stopped"),
            ],
            warnings: vec![],
        });
        assert_eq!(board.apply(&state("nginx", "running")), "nginx: running");
        let failed = Event::ServiceState {
            service: "mysql".to_owned(),
            state: "failed".to_owned(),
            detail: Some("port 3306 is in use".to_owned()),
        };
        assert_eq!(board.apply(&failed), "mysql: failed (port 3306 is in use)");
        assert_eq!(
            board.summary(),
            "nginx running, php-8.4.26 stopped, mysql failed"
        );
        assert_eq!(
            board.table(),
            [
                "  nginx       running",
                "  php-8.4.26  stopped",
                "  mysql       failed (port 3306 is in use)",
            ]
        );
    }

    #[test]
    fn the_status_replaces_what_earlier_events_said() {
        let mut board = Board::default();
        board.apply(&state("nginx", "starting"));
        board.replace(StatusInfo {
            services: vec![service("nginx", "running")],
            warnings: vec!["no PHP installed".to_owned()],
        });
        assert_eq!(
            board.table(),
            ["  nginx  running", "warning: no PHP installed"]
        );
    }

    #[test]
    fn every_event_is_said_in_one_line() {
        let mut board = Board::default();
        let lines = [
            Event::ReloadStarted,
            Event::Reloaded,
            Event::ReloadFailed {
                reason: "nginx -t failed".to_owned(),
            },
            Event::Blocked {
                reason: "close the home".to_owned(),
            },
            Event::Log {
                service: "nginx".to_owned(),
                line: "GET /".to_owned(),
            },
            Event::Progress {
                what: "php 8.4.26".to_owned(),
                done: 10,
                total: Some(20),
                from_file: false,
            },
            Event::ShuttingDown,
        ]
        .iter()
        .map(|event| board.apply(event))
        .collect::<Vec<_>>();
        assert!(lines
            .iter()
            .all(|line| !line.is_empty() && !line.contains('\n')));
        assert_eq!(lines[4], "[nginx] GET /");
        assert_eq!(lines[5], "php 8.4.26: 10 of 20 bytes");
        assert_eq!(board.services, []);
    }

    /// The whole conversation, against what a daemon would write: the requests go out in the
    /// documented order, an event that arrives before the `status` answer does not survive it,
    /// and the client ends when the daemon says it is leaving.
    #[test]
    fn the_client_says_hello_subscribes_asks_and_follows_until_the_daemon_leaves() {
        let daemon = [
            r#"{"jsonrpc":"2.0","id":1,"result":{"type":"hello","daemon":"0.1.0","protocol":3,"home":"C:\\Crisol"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"type":"done","message":"subscribed","changed":true,"warnings":[]}}"#,
            r#"{"jsonrpc":"2.0","method":"event","params":{"type":"service_state","service":"nginx","state":"starting"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"result":{"type":"status","services":[{"name":"nginx","state":"running"}],"warnings":[]}}"#,
            r#"{"jsonrpc":"2.0","method":"event","params":{"type":"service_state","service":"nginx","state":"stopping"}}"#,
            r#"{"jsonrpc":"2.0","method":"event","params":{"type":"shutting_down"}}"#,
        ]
        .map(|line| format!("{line}\n"))
        .concat();
        let mut client = Client::new(Cursor::new(daemon.into_bytes()), Vec::new());
        let mut out = Vec::new();

        watch(&mut client, &mut out).unwrap();

        let sent = String::from_utf8(client.writer().clone()).unwrap();
        let methods: Vec<String> = sent
            .lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                value["method"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(methods, ["hello", "subscribe", "status"]);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "crisold 0.1.0 at C:\\Crisol\n\
             \x20 nginx  running\n\
             nginx: stopping\n\
             \x20 now: nginx stopping\n\
             the daemon is going away\n"
        );
    }

    #[test]
    fn a_closed_connection_ends_the_watch_without_an_error() {
        let daemon = [
            r#"{"jsonrpc":"2.0","id":1,"result":{"type":"hello","daemon":"0.1.0","protocol":3,"home":"C:\\Crisol"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"type":"done","message":"subscribed","changed":true,"warnings":[]}}"#,
            r#"{"jsonrpc":"2.0","id":3,"result":{"type":"status","services":[],"warnings":[]}}"#,
        ]
        .map(|line| format!("{line}\n"))
        .concat();
        let mut client = Client::new(Cursor::new(daemon.into_bytes()), Vec::new());
        let mut out = Vec::new();

        watch(&mut client, &mut out).unwrap();

        assert!(String::from_utf8(out)
            .unwrap()
            .ends_with("no services\nthe daemon closed the connection\n"));
    }
}
