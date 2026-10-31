//! The general state, the menu entry and the warning on leaving, from states a daemon can be in.

use crisol_protocol::{Call, Check, CheckStatus, HelloInfo, ServiceInfo};
use serde_json::json;

use super::*;

fn service(name: &str, state: &str) -> ServiceInfo {
    ServiceInfo {
        name: name.to_owned(),
        state: state.to_owned(),
        detail: None,
    }
}

fn connected(services: &[(&str, &str)]) -> LiveState {
    LiveState {
        connection: Connection::Connected,
        daemon: Some(HelloInfo {
            daemon: "0.1.0".to_owned(),
            protocol: 3,
            home: r"C:\Crisol".to_owned(),
        }),
        services: services.iter().map(|(n, s)| service(n, s)).collect(),
        ..LiveState::default()
    }
}

#[test]
fn without_a_daemon_nothing_else_matters() {
    let mut state = connected(&[("nginx", "failed")]);
    state.connection = Connection::Disconnected;
    assert_eq!(state.overall(), Overall::NoDaemon);
    state.connection = Connection::Refused("another protocol".to_owned());
    assert_eq!(state.overall(), Overall::NoDaemon);
}

#[test]
fn the_general_state_goes_from_the_worst_to_the_best() {
    assert_eq!(connected(&[]).overall(), Overall::Stopped);
    assert_eq!(
        connected(&[("nginx", "stopped"), ("php-8.4", "stopped")]).overall(),
        Overall::Stopped
    );
    assert_eq!(
        connected(&[("nginx", "running"), ("php-8.4", "running")]).overall(),
        Overall::Running
    );
    assert_eq!(
        connected(&[("nginx", "running"), ("php-8.4", "starting")]).overall(),
        Overall::Starting
    );
    assert_eq!(
        connected(&[("nginx", "stopping")]).overall(),
        Overall::Starting
    );
    assert_eq!(
        connected(&[
            ("nginx", "running"),
            ("mysql", "failed"),
            ("php", "starting")
        ])
        .overall(),
        Overall::Failed
    );
}

#[test]
fn a_web_configuration_that_was_not_applied_is_something_that_failed() {
    let mut state = connected(&[("nginx", "running")]);
    state.reload_error = Some("nginx -t said no".to_owned());
    assert_eq!(state.overall(), Overall::Failed);
}

#[test]
fn a_blocked_stack_is_a_notice_and_not_a_failure() {
    let mut state = connected(&[("nginx", "running")]);
    state.blocked = Some("close the folder".to_owned());
    assert_eq!(state.overall(), Overall::Running);
}

#[test]
fn the_menu_offers_stop_while_anything_runs_or_starts() {
    let toggle = |services: &[(&str, &str)]| connected(services).summary().toggle;
    assert_eq!(toggle(&[]), Toggle::StartAll);
    assert_eq!(toggle(&[("nginx", "stopped")]), Toggle::StartAll);
    assert_eq!(toggle(&[("nginx", "failed")]), Toggle::StartAll);
    assert_eq!(toggle(&[("nginx", "running")]), Toggle::StopAll);
    assert_eq!(toggle(&[("nginx", "starting")]), Toggle::StopAll);
    // Something that is only stopping will be gone in a moment: starting is what is left.
    assert_eq!(toggle(&[("nginx", "stopping")]), Toggle::StartAll);
}

#[test]
fn the_summary_carries_the_version_and_the_refusal_as_they_are() {
    let summary = connected(&[]).summary();
    assert_eq!(summary.daemon_version.as_deref(), Some("0.1.0"));
    assert_eq!(summary.refusal, None);

    let refused = LiveState {
        connection: Connection::Refused("the pipe is not yours".to_owned()),
        ..LiveState::default()
    }
    .summary();
    assert_eq!(refused.refusal.as_deref(), Some("the pipe is not yours"));
    assert_eq!(refused.daemon_version, None);
    assert_eq!(refused.overall, Overall::NoDaemon);
}

#[test]
fn leaving_warns_once_and_only_when_services_go_on() {
    let running = connected(&[("nginx", "running")]);
    assert_eq!(running.leave_action(false), LeaveAction::WarnFirst);
    assert_eq!(running.leave_action(true), LeaveAction::Leave);

    let stopped = connected(&[("nginx", "stopped")]);
    assert_eq!(stopped.leave_action(false), LeaveAction::Leave);

    let mut gone = running;
    gone.connection = Connection::Disconnected;
    assert_eq!(gone.leave_action(false), LeaveAction::Leave);
}

/// The page reads these shapes; changing one breaks it, so they are pinned.
#[test]
fn the_json_the_page_reads() {
    let value = |state: &LiveState| serde_json::to_value(state).expect("serializes");

    let mut state = connected(&[("nginx", "running")]);
    assert_eq!(value(&state)["connection"], json!({"kind": "connected"}));
    assert_eq!(
        value(&state)["services"],
        // `detail` is left out when there is none; the page treats it as absent.
        json!([{"name": "nginx", "state": "running"}])
    );
    assert_eq!(value(&state)["daemon"]["daemon"], json!("0.1.0"));
    assert_eq!(value(&state)["reloading"], json!(false));
    assert_eq!(value(&state)["blocked"], json!(null));

    state.connection = Connection::Disconnected;
    assert_eq!(value(&state)["connection"], json!({"kind": "disconnected"}));

    state.connection = Connection::Refused("no".to_owned());
    assert_eq!(
        value(&state)["connection"],
        json!({"kind": "refused", "message": "no"})
    );

    state.version_warning = Some(Check {
        name: "daemon version".to_owned(),
        status: CheckStatus::Warning,
        detail: "old".to_owned(),
        fix: Some("crisol shutdown".to_owned()),
    });
    let warning = &value(&state)["version_warning"];
    assert_eq!(warning["fix"], json!("crisol shutdown"));
    assert_eq!(warning["status"], json!("warning"));
}

#[test]
fn the_summary_json() {
    let summary = serde_json::to_value(connected(&[("nginx", "running")]).summary()).expect("json");
    assert_eq!(
        summary,
        json!({
            "overall": "running",
            "toggle": "stop_all",
            "daemon_version": "0.1.0",
            "refusal": null,
            "rows": [{"name": "nginx", "state": "running", "detail": null, "metal": "nginx"}],
        })
    );
    let none = serde_json::to_value(LiveState::default().summary()).expect("json");
    assert_eq!(none["overall"], json!("no_daemon"));
    assert_eq!(none["toggle"], json!("start_all"));
}

#[test]
fn every_state_has_its_icon_in_both_themes() {
    let tray = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/tray");
    for overall in [
        Overall::Running,
        Overall::Starting,
        Overall::Stopped,
        Overall::Failed,
        Overall::NoDaemon,
    ] {
        for theme in ["dark", "light"] {
            let file = tray.join(format!("{}-{theme}.svg", overall.icon_name()));
            assert!(file.exists(), "{} is missing", file.display());
        }
    }
}

#[test]
fn a_service_gets_the_dot_of_its_technology() {
    for (name, metal) in [
        ("nginx", "nginx"),
        ("php-8.4.26", "php"),
        ("mysql", "mysql"),
        ("mariadb", "mariadb"),
        ("postgresql", "postgresql"),
        ("mongodb", "mongodb"),
        ("redis", "redis"),
        ("meilisearch", "meilisearch"),
        ("mailpit", "other"),
        ("memcached", "other"),
        ("garnet", "other"),
        ("something-new", "other"),
        ("", "other"),
    ] {
        assert_eq!(metal_of(name), metal, "{name}");
    }
}

#[test]
fn every_dot_has_its_token() {
    let tokens = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/tokens.css"),
    )
    .expect("tokens.css");
    for name in [
        "php",
        "nginx",
        "mysql",
        "mariadb",
        "postgresql",
        "mongodb",
        "redis",
        "meilisearch",
        "node",
        "other",
    ] {
        assert!(
            tokens.contains(&format!("--metal-{name}:")),
            "--metal-{name}"
        );
    }
}

#[test]
fn the_menu_entry_makes_the_call_of_the_cli() {
    assert_eq!(Toggle::StartAll.call(), Call::Start { service: None });
    assert_eq!(Toggle::StopAll.call(), Call::Stop { service: None });
}

#[test]
fn the_rows_keep_the_order_and_the_words_of_the_daemon() {
    let mut state = connected(&[("php-8.4.26", "failed"), ("nginx", "running")]);
    state.services[0].detail = Some("exited with 3".to_owned());
    let rows = state.summary().rows;
    assert_eq!(rows[0].name, "php-8.4.26");
    assert_eq!(rows[0].detail.as_deref(), Some("exited with 3"));
    assert_eq!(rows[0].metal, "php");
    assert_eq!(rows[1].name, "nginx");
    assert_eq!(rows[1].detail, None);
}
