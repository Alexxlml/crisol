//! The translatable texts (ADR-0030, tests 6 and 7): optional fields next to the sentences of
//! always, read with tolerance, left out of `--json`, and documented.

use serde_json::{json, Value};

use super::*;

const DOC: &str = include_str!("../../../../docs/protocolo.md");

fn text(key: &str) -> Text {
    Text::new(key).with("name", "nginx")
}

fn full_check() -> Check {
    Check {
        name: "services".to_owned(),
        status: CheckStatus::Ok,
        detail: "running: nginx".to_owned(),
        fix: Some("crisol start".to_owned()),
        action: None,
        name_text: Some(text("doctor.name.services")),
        detail_text: Some(text("doctor.services.running")),
        fix_text: Some(text("doctor.fix")),
    }
}

fn full_done() -> DoneInfo {
    DoneInfo {
        message: "Starting nginx".to_owned(),
        changed: true,
        warnings: vec!["a".to_owned(), "b".to_owned()],
        message_text: Some(text("done.service_starting")),
        warnings_text: vec![Some(text("done.warning")), None],
    }
}

fn full_status() -> StatusInfo {
    StatusInfo {
        services: vec![],
        warnings: vec!["a".to_owned()],
        seq: None,
        blocked: None,
        reload_error: None,
        warnings_text: vec![Some(text("status.warning"))],
    }
}

fn full_error() -> RpcError {
    RpcError::new(ErrorCode::HomeExposed, "open home")
        .with_fix("icacls home")
        .with_text(text("home.exposed"))
        .with_fix_text(text("home.exposed_fix"))
}

/// The JSON of a reply or an error, as `--json` prints it.
fn as_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn a_text_is_a_key_and_its_data() {
    assert_eq!(
        as_json(&text("a.b")),
        json!({"key": "a.b", "args": {"name": "nginx"}})
    );
    // With no data there is no `args`.
    assert_eq!(as_json(&Text::new("a.b")), json!({"key": "a.b"}));
    let back: Text = serde_json::from_value(json!({"key": "a.b"})).unwrap();
    assert_eq!(back, Text::new("a.b"));
}

#[test]
fn what_an_older_daemon_sends_reads_without_texts() {
    let check: Check =
        serde_json::from_value(json!({"name": "x", "status": "ok", "detail": "d"})).unwrap();
    assert!(check.name_text.is_none() && check.detail_text.is_none() && check.fix_text.is_none());

    let done: DoneInfo = serde_json::from_value(json!({"message": "m", "changed": false})).unwrap();
    assert!(done.message_text.is_none() && done.warnings_text.is_empty());

    let status: StatusInfo =
        serde_json::from_value(json!({"services": [], "warnings": ["w"]})).unwrap();
    assert_eq!(status.warnings_text.len(), 0);

    let error: RpcError = serde_json::from_value(json!({"code": 5, "message": "m"})).unwrap();
    assert!(error.text.is_none() && error.fix_text.is_none());
}

#[test]
fn a_client_that_does_not_know_the_fields_reads_what_it_always_read() {
    // The old reader is a struct without the new fields: serde ignores what it does not know.
    #[derive(Debug, PartialEq, Deserialize)]
    struct OldCheck {
        name: String,
        status: CheckStatus,
        detail: String,
        fix: Option<String>,
    }
    #[derive(Debug, PartialEq, Deserialize)]
    struct OldError {
        code: ErrorCode,
        message: String,
        fix: Option<String>,
    }
    let check: OldCheck = serde_json::from_value(as_json(&full_check())).unwrap();
    assert_eq!(check.detail, "running: nginx");
    assert_eq!(check.fix.as_deref(), Some("crisol start"));
    let error: OldError = serde_json::from_value(as_json(&full_error())).unwrap();
    assert_eq!(error.message, "open home");
    assert_eq!(error.fix.as_deref(), Some("icacls home"));
}

#[test]
fn a_text_that_is_not_understood_reads_as_absent_and_the_rest_is_kept() {
    let check: Check = serde_json::from_value(json!({
        "name": "x", "status": "ok", "detail": "d",
        "name_text": 7, "detail_text": {"key": ["no"]}, "fix_text": null
    }))
    .unwrap();
    assert_eq!(check.detail, "d");
    assert!(check.name_text.is_none() && check.detail_text.is_none());

    let error: RpcError = serde_json::from_value(json!({
        "code": 5, "message": "m", "text": "a string, not a text"
    }))
    .unwrap();
    assert_eq!(error.message, "m");
    assert!(error.text.is_none());

    // One bad element does not take the others with it.
    let done: DoneInfo = serde_json::from_value(json!({
        "message": "m", "changed": true, "warnings": ["a", "b"],
        "warnings_text": [{"key": "k"}, 3]
    }))
    .unwrap();
    assert_eq!(done.warnings_text, vec![Some(Text::new("k")), None]);
}

#[test]
fn texts_travel_and_come_back() {
    assert_eq!(
        round_trip(&Reply::Done(full_done())),
        Reply::Done(full_done())
    );
    assert_eq!(
        round_trip(&Reply::Doctor {
            checks: vec![full_check()]
        }),
        Reply::Doctor {
            checks: vec![full_check()]
        }
    );
    assert_eq!(round_trip(&full_error()), full_error());
}

#[test]
fn a_list_of_texts_of_another_length_is_ignored() {
    let texts = vec![Some(text("a")), None];
    assert_eq!(text_at(&texts, 2, 0), Some(&text("a")));
    assert_eq!(text_at(&texts, 2, 1), None);
    assert_eq!(text_at(&texts, 3, 0), None, "the lengths do not match");
    assert_eq!(text_at(&[], 2, 0), None, "empty means no texts");
}

#[test]
fn json_without_texts_is_the_json_of_before() {
    let mut reply = Reply::Doctor {
        checks: vec![full_check()],
    };
    reply.strip_texts();
    assert_eq!(
        as_json(&reply),
        json!({"type": "doctor", "checks": [{
            "name": "services", "status": "ok", "detail": "running: nginx", "fix": "crisol start"
        }]})
    );

    let mut done = Reply::Done(full_done());
    done.strip_texts();
    assert_eq!(
        as_json(&done),
        json!({"type": "done", "message": "Starting nginx", "changed": true, "warnings": ["a", "b"]})
    );

    let mut status = Reply::Status(full_status());
    status.strip_texts();
    assert_eq!(
        as_json(&status),
        json!({"type": "status", "services": [], "warnings": ["a"]})
    );

    let mut error = full_error();
    error.strip_texts();
    assert_eq!(
        as_json(&error),
        json!({"code": 4, "message": "open home", "fix": "icacls home"})
    );
}

/// A reply and an error with every text there is, so a new field cannot be forgotten.
fn text_fields() -> Vec<String> {
    let mut found = Vec::new();
    let mut collect = |value: &Value| {
        if let Value::Object(map) = value {
            for key in map.keys() {
                if key == "text" || key.ends_with("_text") {
                    found.push(key.clone());
                }
            }
        }
    };
    collect(&as_json(&full_error()));
    collect(&as_json(&full_check()));
    collect(&as_json(&full_done()));
    collect(&as_json(&full_status()));
    found.sort();
    found.dedup();
    found
}

#[test]
fn the_document_names_every_text_field_and_what_a_text_is() {
    let start = DOC.find("\n## Textos traducibles\n").expect("a section");
    let rest = &DOC[start + 1..];
    let section = &rest[..rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3)];
    let fields = text_fields();
    assert!(fields.len() >= 6, "{fields:?}");
    for field in fields {
        assert!(
            section.contains(&format!("`{field}`")),
            "`{field}` is not in «Textos traducibles»"
        );
    }
    for word in ["key", "args", "strip_texts", "_one", "_other", "count"] {
        assert!(section.contains(word), "{word}");
    }
}
