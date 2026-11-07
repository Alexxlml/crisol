//! A minimal client of the daemon (F12.2): it shows the state of the services in the console as
//! it changes.
//!
//! ```text
//! cargo run -p crisol-client --example watch
//! ```
//!
//! It is the proof that a shell (Slint, Tauri, a web panel) can be put on top of the daemon with
//! `crisol-client` alone: [`LiveView`] does the conversation of `docs/protocolo.md` (`hello`,
//! `subscribe` before `status`, the events numbered by `seq`, asking again when one is missing)
//! and gives a state that this example only compares with the last one it printed. It never
//! starts the daemon and never changes anything. Leave it running and use `crisol start`,
//! `crisol stop` or `crisol link` in another terminal. When the daemon goes away it says so and
//! keeps looking; it ends with Ctrl+C.

use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc;

use crisol_client::{Connection, Context, LiveConfig, LiveState, LiveView};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("watch: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    let context =
        Context::windows("watch example", env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    let (sender, receiver) = mpsc::channel();
    let _view = LiveView::start(LiveConfig::for_context(&context), move |state| {
        let _ = sender.send(state.clone());
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut shown = LiveState::default();
    // Until the thread says something, the daemon has not been seen: say so once.
    writeln!(out, "{}", waiting(&context)).map_err(|e| e.to_string())?;
    while let Ok(state) = receiver.recv() {
        for line in changes(&shown, &state) {
            writeln!(out, "{line}").map_err(|e| e.to_string())?;
        }
        out.flush().map_err(|e| e.to_string())?;
        shown = state;
    }
    Ok(())
}

#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err("Crisol runs on Windows".to_owned())
}

#[cfg(windows)]
fn waiting(context: &Context) -> String {
    format!(
        "looking for the daemon of {} (`crisol start` starts it)",
        context.paths.home().display()
    )
}

/// What to say, one line each, for the screen to go from `before` to `now`.
fn changes(before: &LiveState, now: &LiveState) -> Vec<String> {
    let mut lines = Vec::new();
    if before.connection != now.connection {
        match &now.connection {
            Connection::Connected => {
                if let Some(daemon) = &now.daemon {
                    lines.push(format!("crisold {} at {}", daemon.daemon, daemon.home));
                }
            }
            Connection::Disconnected => lines.push("the daemon is gone; looking for it".to_owned()),
            Connection::Refused(why) => lines.push(format!("cannot use the daemon: {why}")),
        }
    }
    if before.version_warning != now.version_warning {
        if let Some(check) = &now.version_warning {
            lines.push(format!(
                "warning: {} ({})",
                check.detail,
                check.fix.as_deref().unwrap_or("no fix known")
            ));
        }
    }
    // A new connection is a new picture: the whole table, not just what differs.
    let fresh = before.connection != Connection::Connected;
    for service in &now.services {
        let known = before.services.iter().find(|s| s.name == service.name);
        if fresh || known != Some(service) {
            lines.push(match &service.detail {
                Some(detail) => format!("{}: {} ({detail})", service.name, service.state),
                None => format!("{}: {}", service.name, service.state),
            });
        }
    }
    if now.connection == Connection::Connected && now.services.is_empty() && fresh {
        lines.push("no services".to_owned());
    }
    for warning in &now.warnings {
        if fresh || !before.warnings.contains(warning) {
            lines.push(format!("warning: {warning}"));
        }
    }
    if !before.reloading && now.reloading {
        lines.push("web: testing a new configuration".to_owned());
    }
    if before.reloading && !now.reloading && now.reload_error.is_none() {
        lines.push("web: new configuration in place".to_owned());
    }
    if before.reload_error != now.reload_error {
        if let Some(reason) = &now.reload_error {
            lines.push(format!(
                "web: the new configuration was not applied, the previous one stays: {reason}"
            ));
        }
    }
    if before.blocked != now.blocked {
        if let Some(reason) = &now.blocked {
            lines.push(format!("blocked: {reason}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use crisol_protocol::{HelloInfo, ServiceInfo};

    use super::*;

    fn service(name: &str, state: &str) -> ServiceInfo {
        ServiceInfo {
            name: name.to_owned(),
            state: state.to_owned(),
            detail: None,
        }
    }

    fn connected(services: Vec<ServiceInfo>) -> LiveState {
        LiveState {
            connection: Connection::Connected,
            daemon: Some(HelloInfo {
                daemon: "0.1.0".to_owned(),
                protocol: 4,
                home: "C:\\Crisol".to_owned(),
            }),
            services,
            ..LiveState::default()
        }
    }

    #[test]
    fn a_new_connection_says_who_answered_and_shows_the_whole_table() {
        let now = connected(vec![
            service("nginx", "running"),
            service("php-8.4.26", "stopped"),
        ]);
        assert_eq!(
            changes(&LiveState::default(), &now),
            [
                "crisold 0.1.0 at C:\\Crisol",
                "nginx: running",
                "php-8.4.26: stopped"
            ]
        );
        assert_eq!(
            changes(&LiveState::default(), &connected(vec![])),
            ["crisold 0.1.0 at C:\\Crisol", "no services"]
        );
    }

    #[test]
    fn afterwards_only_what_changed_is_said() {
        let before = connected(vec![
            service("nginx", "running"),
            service("mysql", "stopped"),
        ]);
        let mut now = before.clone();
        now.services[1] = ServiceInfo {
            detail: Some("port 3306 is in use".to_owned()),
            state: "failed".to_owned(),
            ..now.services[1].clone()
        };
        assert_eq!(
            changes(&before, &now),
            ["mysql: failed (port 3306 is in use)"]
        );
        assert_eq!(changes(&now, &now), Vec::<String>::new());
    }

    #[test]
    fn reloads_blocks_and_the_loss_of_the_daemon_are_one_line_each() {
        let idle = connected(vec![]);
        let mut reloading = idle.clone();
        reloading.reloading = true;
        assert_eq!(
            changes(&idle, &reloading),
            ["web: testing a new configuration"]
        );
        assert_eq!(
            changes(&reloading, &idle),
            ["web: new configuration in place"]
        );
        let mut failed = idle.clone();
        failed.reload_error = Some("nginx -t failed".to_owned());
        assert_eq!(
            changes(&reloading, &failed),
            ["web: the new configuration was not applied, the previous one stays: nginx -t failed"]
        );
        let mut blocked = idle.clone();
        blocked.blocked = Some("close the home".to_owned());
        assert_eq!(changes(&idle, &blocked), ["blocked: close the home"]);
        assert_eq!(
            changes(&idle, &LiveState::default()),
            ["the daemon is gone; looking for it"]
        );
        let refused = LiveState {
            connection: Connection::Refused("protocol 4".to_owned()),
            ..LiveState::default()
        };
        assert_eq!(
            changes(&LiveState::default(), &refused),
            ["cannot use the daemon: protocol 4"]
        );
    }
}
