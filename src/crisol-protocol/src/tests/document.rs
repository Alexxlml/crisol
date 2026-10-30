//! `docs/protocolo.md` (F12.1): the contract as people read it, checked against the code.

use std::collections::BTreeSet;

use super::*;

const DOC: &str = include_str!("../../../../docs/protocolo.md");

/// The text of `## <title>` up to the next `## `.
fn section(title: &str) -> &'static str {
    let start = DOC
        .find(&format!("\n## {title}\n"))
        .unwrap_or_else(|| panic!("no section `{title}`"));
    let rest = &DOC[start + 1..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    &rest[..end]
}

/// The names of the level-3 headings of a section that are one name in backticks.
fn headings(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("### `")?.strip_suffix('`'))
        .map(str::to_owned)
        .collect()
}

/// The `type` of a reply or an event, as it travels.
fn type_of<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).unwrap()["type"]
        .as_str()
        .unwrap()
        .to_owned()
}

// The matches have no wildcard on purpose: a new kind does not compile until it gets a number
// here, and the test does not pass until a sample of it is listed and it is documented.
fn call_kind(call: &Call) -> usize {
    match call {
        Call::Hello { .. } => 0,
        Call::Status { .. } => 1,
        Call::Start { .. } => 2,
        Call::Stop { .. } => 3,
        Call::Restart { .. } => 4,
        Call::Shutdown => 5,
        Call::Install { .. } => 6,
        Call::Uninstall { .. } => 7,
        Call::List { .. } => 8,
        Call::Use { .. } => 9,
        Call::Park { .. } => 10,
        Call::Unpark { .. } => 11,
        Call::Link { .. } => 12,
        Call::Unlink { .. } => 13,
        Call::Sites => 14,
        Call::Secure { .. } => 15,
        Call::Unsecure { .. } => 16,
        Call::Logs { .. } => 17,
        Call::Doctor => 18,
        Call::Credentials { .. } => 19,
        Call::Enable { .. } => 20,
        Call::Disable { .. } => 21,
        Call::ResetPassword { .. } => 22,
        Call::Subscribe => 23,
    }
}
const CALL_KINDS: usize = 24;

fn reply_kind(reply: &Reply) -> usize {
    match reply {
        Reply::Hello(_) => 0,
        Reply::Status(_) => 1,
        Reply::Done(_) => 2,
        Reply::Installed(_) => 3,
        Reply::Removed(_) => 4,
        Reply::Versions(_) => 5,
        Reply::Used(_) => 6,
        Reply::Sites { .. } => 7,
        Reply::Logs(_) => 8,
        Reply::Doctor { .. } => 9,
        Reply::Credentials(_) => 10,
    }
}
const REPLY_KINDS: usize = 11;

fn event_kind(event: &Event) -> usize {
    match event {
        Event::ServiceState { .. } => 0,
        Event::ReloadStarted => 1,
        Event::Reloaded => 2,
        Event::ReloadFailed { .. } => 3,
        Event::Blocked { .. } => 4,
        Event::Log { .. } => 5,
        Event::Progress { .. } => 6,
        Event::ShuttingDown => 7,
    }
}
const EVENT_KINDS: usize = 8;

fn every_reply() -> Vec<Reply> {
    let text = String::new;
    vec![
        Reply::Hello(HelloInfo {
            daemon: text(),
            protocol: PROTOCOL_VERSION,
            home: text(),
        }),
        Reply::Status(StatusInfo {
            services: vec![],
            warnings: vec![],
            seq: None,
        }),
        Reply::Done(DoneInfo {
            message: text(),
            changed: false,
            warnings: vec![],
        }),
        Reply::Installed(InstalledInfo {
            service: text(),
            version: text(),
            path: text(),
            already: false,
            end_of_life: false,
            downloaded: 0,
            unverified: false,
            advisories: vec![],
        }),
        Reply::Removed(RemovedInfo {
            service: text(),
            version: text(),
        }),
        Reply::Versions(VersionList {
            installed: vec![],
            available: vec![],
        }),
        Reply::Used(UsedInfo {
            file: text(),
            request: text(),
            selects: text(),
            previous: None,
            changed: false,
            warnings: vec![],
        }),
        Reply::Sites {
            sites: vec![],
            warnings: vec![],
        },
        Reply::Logs(LogChunk {
            lines: vec![],
            next: 0,
            path: text(),
        }),
        Reply::Doctor { checks: vec![] },
        Reply::Credentials(CredentialsInfo {
            service: text(),
            lines: vec![],
            notes: vec![],
        }),
    ]
}

fn every_event() -> Vec<Event> {
    vec![
        Event::ServiceState {
            service: String::new(),
            state: String::new(),
            detail: None,
        },
        Event::ReloadStarted,
        Event::Reloaded,
        Event::ReloadFailed {
            reason: String::new(),
        },
        Event::Blocked {
            reason: String::new(),
        },
        Event::Log {
            service: String::new(),
            line: String::new(),
        },
        Event::Progress {
            what: String::new(),
            done: 0,
            total: None,
            from_file: false,
        },
        Event::ShuttingDown,
    ]
}

/// The samples cover every kind, and the headings of the section are exactly their names.
fn check<T>(
    what: &str,
    samples: &[T],
    kind: fn(&T) -> usize,
    kinds: usize,
    name: fn(&T) -> String,
    title: &str,
) {
    let covered: BTreeSet<usize> = samples.iter().map(kind).collect();
    assert_eq!(
        covered,
        (0..kinds).collect(),
        "a sample of every {what} is listed"
    );
    let names: BTreeSet<String> = samples.iter().map(name).collect();
    assert_eq!(names.len(), kinds, "every {what} has its own name");
    assert_eq!(
        headings(section(title)),
        names,
        "docs/protocolo.md, section `{title}`: one heading per {what}, and no other"
    );
}

#[test]
fn the_protocol_document_names_every_method_reply_event_and_error() {
    check(
        "method",
        &every_call(),
        call_kind,
        CALL_KINDS,
        |c| c.method().to_owned(),
        "Métodos",
    );
    check(
        "reply",
        &every_reply(),
        reply_kind,
        REPLY_KINDS,
        type_of,
        "Respuestas",
    );
    check(
        "event",
        &every_event(),
        event_kind,
        EVENT_KINDS,
        type_of,
        "Eventos",
    );

    // Every error code, with its number, in the table.
    let errors = section("Errores");
    let codes: Vec<ErrorCode> = (-33_000..1_000)
        .filter_map(|n| ErrorCode::try_from(n).ok())
        .collect();
    assert_eq!(codes.len(), 11);
    for code in codes {
        let row = format!("| {} | `", code.number());
        assert!(errors.contains(&row), "no row for error {code:?}: {row}");
    }

    assert!(
        DOC.starts_with(&format!(
            "# Protocolo de `crisold` (versión {PROTOCOL_VERSION})"
        )),
        "the title names the version"
    );
    assert!(DOC.contains(&format!("`PROTOCOL_VERSION = {PROTOCOL_VERSION}`")));
    assert!(DOC.contains(&format!("**{} MiB**", MAX_LINE >> 20)));
}
