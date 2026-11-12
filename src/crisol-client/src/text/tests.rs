use std::collections::BTreeMap;

use crisol_core::msg;
use crisol_protocol::{CheckStatus, ErrorCode};

use super::*;

fn done(message: &str, warnings: &[&str]) -> DoneInfo {
    DoneInfo {
        message: message.to_owned(),
        changed: true,
        warnings: warnings.iter().map(|w| (*w).to_owned()).collect(),
        message_text: None,
        warnings_text: Vec::new(),
    }
}

fn check(detail: &str) -> Check {
    Check {
        name: "services".to_owned(),
        status: CheckStatus::Ok,
        detail: detail.to_owned(),
        fix: Some("crisol start".to_owned()),
        action: None,
        name_text: Some(msg!("doctor.name.services").to_text()),
        detail_text: None,
        fix_text: None,
    }
}

#[test]
fn a_text_is_said_in_the_language_of_the_user() {
    let text = msg!("done.service_starting", service = "mysql").to_text();
    assert_eq!(
        say(Language::SPANISH, Some(&text), "Starting mysql"),
        "Arrancando mysql"
    );
    assert_eq!(
        say(Language::ENGLISH, Some(&text), "Starting mysql"),
        "Starting mysql"
    );
}

#[test]
fn without_a_text_the_sentence_of_the_daemon_stays() {
    assert_eq!(say(Language::SPANISH, None, "plain"), "plain");
}

#[test]
fn a_key_no_dictionary_has_leaves_the_sentence_of_the_daemon() {
    // A daemon newer than this client: it knows a key this one does not. Never a bare key.
    let text = Text::new("done.from_the_future").with("thing", "x");
    assert_eq!(
        say(Language::SPANISH, Some(&text), "The English sentence"),
        "The English sentence"
    );
}

#[test]
fn a_reply_loses_its_keys_and_keeps_the_sentences_in_the_language() {
    let mut info = done("Starting mysql", &[]);
    info.message_text = Some(msg!("done.service_starting", service = "mysql").to_text());
    let mut reply = Reply::Done(info);
    localize_reply(&mut reply, Language::SPANISH);
    let Reply::Done(info) = reply else {
        panic!("a Done stays a Done")
    };
    assert_eq!(info.message, "Arrancando mysql");
    assert!(info.message_text.is_none() && info.warnings_text.is_empty());
}

#[test]
fn warnings_are_said_one_by_one_when_the_texts_line_up() {
    let mut info = done(
        "Starting the databases",
        &["the web stack was not started: x", "other"],
    );
    info.warnings_text = vec![
        Some(msg!("done.databases_web_not_started", reason = "x").to_text()),
        None,
    ];
    let mut reply = Reply::Done(info);
    localize_reply(&mut reply, Language::SPANISH);
    let Reply::Done(info) = reply else {
        panic!("a Done stays a Done")
    };
    assert_eq!(info.warnings, ["la pila web no se arrancó: x", "other"]);
}

#[test]
fn warnings_whose_texts_do_not_line_up_are_left_alone() {
    let mut info = done("x", &["one", "two"]);
    info.warnings_text = vec![Some(Text::new("done.databases_starting"))];
    let mut reply = Reply::Done(info);
    localize_reply(&mut reply, Language::SPANISH);
    let Reply::Done(info) = reply else {
        panic!("a Done stays a Done")
    };
    assert_eq!(info.warnings, ["one", "two"]);
}

#[test]
fn a_check_says_its_detail_but_keeps_its_name() {
    let mut row = check("running: nginx");
    row.detail_text = Some(msg!("doctor.services.running", list = "nginx").to_text());
    let mut reply = Reply::Doctor { checks: vec![row] };
    localize_reply(&mut reply, Language::SPANISH);
    let Reply::Doctor { checks } = reply else {
        panic!("a Doctor stays a Doctor")
    };
    assert_eq!(checks[0].detail, "corriendo: nginx");
    // The row is still known by its English name, and its translated name is still there.
    assert_eq!(checks[0].name, "services");
    assert!(checks[0].name_text.is_some());
    // A fix with no text is a command and stays as it is.
    assert_eq!(checks[0].fix.as_deref(), Some("crisol start"));
}

#[test]
fn an_error_is_said_in_the_language_and_its_fix_is_left_alone() {
    let mut error = RpcError::new(ErrorCode::Io, "Starting mysql")
        .with_text(msg!("done.service_starting", service = "mysql").to_text())
        .with_fix("icacls the-home");
    localize_error(&mut error, Language::SPANISH);
    assert_eq!(error.message, "Arrancando mysql");
    assert_eq!(error.fix.as_deref(), Some("icacls the-home"));
    assert!(error.text.is_none() && error.fix_text.is_none());
}

#[test]
fn a_text_that_is_not_one_still_reads_the_reply() {
    // `args` of the wrong shape: the text reads as absent and the sentence is the one sent.
    let json = serde_json::json!({
        "message": "Done", "changed": true,
        "message_text": {"key": 5, "args": "no"}
    });
    let info: DoneInfo = serde_json::from_value(json).expect("tolerant");
    assert!(info.message_text.is_none());
    let _: BTreeMap<String, String> = BTreeMap::new();
}
