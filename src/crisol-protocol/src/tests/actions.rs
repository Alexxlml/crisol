//! `Check.action` (ADR-0021 section 5): what travels, what an older reader does with it, and
//! the command that stands for a button.

use super::*;

fn check_with(action: Option<CheckAction>) -> Check {
    Check {
        name: "installed tools".to_owned(),
        status: CheckStatus::Warning,
        detail: "not installed: php".to_owned(),
        fix: Some("crisol install php".to_owned()),
        action,
    }
}

#[test]
fn every_kind_of_action_survives_a_round_trip() {
    let actions = [
        CheckAction::install("php", Some("8.4.27")),
        CheckAction::reinstall("php", "8.4.27"),
        CheckAction::start(),
        CheckAction::Local { op: LocalOp::Trust },
        CheckAction::Link {
            url: "https://example.org/x".to_owned(),
        },
        CheckAction::ShowLog {
            service: "nginx".to_owned(),
        },
        CheckAction::Copy {
            text: "ipconfig /flushdns".to_owned(),
        },
        CheckAction::Reveal {
            path: r"C:\Crisol\crisol.toml".to_owned(),
        },
    ];
    for action in actions {
        let check = check_with(Some(action));
        let line = serde_json::to_string(&check).unwrap();
        assert_eq!(
            serde_json::from_str::<Check>(&line).unwrap(),
            check,
            "{line}"
        );
    }
}

#[test]
fn a_check_without_an_action_does_not_carry_the_field() {
    let value = serde_json::to_value(check_with(None)).unwrap();
    assert!(value.get("action").is_none(), "{value}");
    // And one from a daemon that predates the field reads as before.
    let old = r#"{"name":"x","status":"ok","detail":"d"}"#;
    assert_eq!(serde_json::from_str::<Check>(old).unwrap().action, None);
}

#[test]
fn an_action_this_version_does_not_know_is_left_out_and_the_check_is_kept() {
    let newer = [
        r#"{"kind":"open_settings","page":"privacy"}"#,
        r#"{"kind":"local","op":"flush_dns"}"#,
        r#"{"kind":"calls","calls":[{"method":"teleport","params":{}}]}"#,
        r#""not an object""#,
        "null",
    ];
    for action in newer {
        let text = format!(
            r#"{{"name":"x","status":"warning","detail":"d","fix":"f","action":{action}}}"#
        );
        let check: Check = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(check.action, None, "{text}");
        assert_eq!(check.fix.as_deref(), Some("f"), "{text}");
    }
}

#[test]
fn the_command_of_an_action_is_what_the_cli_takes() {
    let command = |action: CheckAction| action.command();
    assert_eq!(
        command(CheckAction::install("php", None)).as_deref(),
        Some("crisol install php")
    );
    assert_eq!(
        command(CheckAction::install("php", Some("8.4.27"))).as_deref(),
        Some("crisol install php 8.4.27")
    );
    assert_eq!(
        command(CheckAction::reinstall("php", "7.4.33")).as_deref(),
        Some("crisol uninstall php 7.4.33 && crisol install php 7.4.33")
    );
    assert_eq!(
        command(CheckAction::start()).as_deref(),
        Some("crisol start")
    );
    assert_eq!(
        command(CheckAction::Local {
            op: LocalOp::UntrustStale
        })
        .as_deref(),
        Some("crisol untrust --stale")
    );
    assert_eq!(
        command(CheckAction::ShowLog {
            service: "nginx".to_owned()
        })
        .as_deref(),
        Some("crisol logs nginx")
    );
    // A call the CLI has no short form for has no command, and neither has the whole list.
    let odd = CheckAction::Calls {
        calls: vec![Call::Start { service: None }, Call::Sites],
    };
    assert_eq!(odd.command(), None);
}
