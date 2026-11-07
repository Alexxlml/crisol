//! The rules of the live view, against daemons that say what a real one would.

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::Mutex;

use crisol_protocol::{Client, PROTOCOL_VERSION};

use super::*;
use crate::link::Remote;

const HELLO: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"type":"hello","daemon":"0.1.0","protocol":4,"home":"C:\\Crisol"}}"#;
const SUBSCRIBED: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"type":"done","message":"subscribed","changed":true,"warnings":[]}}"#;
const REFUSAL: &str = r#"{"jsonrpc":"2.0","id":1,"error":{"code":1,"message":"this daemon speaks protocol 4 and the client 3; update one of them"}}"#;

/// The answer to `status`, the third request of a conversation.
fn status(services: &[(&str, &str)], seq: Option<u64>) -> String {
    status_numbered(3, services, seq)
}

fn status_numbered(id: u64, services: &[(&str, &str)], seq: Option<u64>) -> String {
    let services: Vec<String> = services
        .iter()
        .map(|(name, state)| format!(r#"{{"name":"{name}","state":"{state}"}}"#))
        .collect();
    let seq = seq.map_or_else(String::new, |seq| format!(r#","seq":{seq}"#));
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"type":"status","services":[{}],"warnings":[]{seq}}}}}"#,
        services.join(",")
    )
}

fn service_event(service: &str, state: &str, seq: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"event","params":{{"type":"service_state","service":"{service}","state":"{state}","seq":{seq}}}}}"#
    )
}

fn event(kind: &str, seq: u64) -> String {
    format!(r#"{{"jsonrpc":"2.0","method":"event","params":{{"type":"{kind}","seq":{seq}}}}}"#)
}

fn event_with(kind: &str, field: &str, value: &str, seq: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"event","params":{{"type":"{kind}","{field}":"{value}","seq":{seq}}}}}"#
    )
}

fn script(lines: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

fn hello_call() -> Call {
    Call::Hello {
        client: "test 0.1.0".to_owned(),
        protocol: PROTOCOL_VERSION,
    }
}

/// Follows `lines` to the end and gives how it ended, the state, every state published, and
/// the methods the client sent.
fn run_script(lines: &[String]) -> (Ended, LiveState, Vec<LiveState>, Vec<String>) {
    let mut remote = Remote(Client::new(Cursor::new(script(lines)), Vec::new()));
    let mut state = LiveState::default();
    let mut published = Vec::new();
    let stop = AtomicBool::new(false);
    let ended = follow(
        &mut remote,
        &hello_call(),
        "0.1.0",
        &stop,
        &mut state,
        &mut |s| {
            published.push(s.clone());
        },
    );
    let sent = String::from_utf8(remote.0.writer().clone()).unwrap();
    let methods = sent
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["method"].as_str().unwrap().to_owned()
        })
        .collect();
    (ended, state, published, methods)
}

fn names(state: &LiveState) -> Vec<String> {
    state
        .services
        .iter()
        .map(|s| format!("{} {}", s.name, s.state))
        .collect()
}

#[test]
fn the_conversation_is_hello_subscribe_status_and_then_the_events() {
    let (ended, state, published, methods) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "stopped")], Some(1)),
        service_event("nginx", "starting", 2),
        service_event("php-8.4.26", "running", 3),
    ]);
    assert!(matches!(ended, Ended::Lost));
    assert_eq!(methods, ["hello", "subscribe", "status"]);
    assert_eq!(names(&state), ["nginx starting", "php-8.4.26 running"]);
    assert_eq!(state.daemon.as_ref().unwrap().daemon, "0.1.0");
    // The first thing a screen is given is the whole picture, already connected.
    assert_eq!(published[0].connection, Connection::Connected);
    assert_eq!(names(&published[0]), ["nginx stopped"]);
    assert_eq!(published.len(), 3);
}

/// A dropped view stops at the next event and publishes nothing more, even if the daemon
/// keeps talking.
#[test]
fn a_stopped_view_publishes_nothing_more() {
    let mut remote = Remote(Client::new(
        Cursor::new(script(&[
            HELLO.to_owned(),
            SUBSCRIBED.to_owned(),
            status(&[("nginx", "stopped")], Some(1)),
            service_event("nginx", "starting", 2),
            service_event("nginx", "running", 3),
        ])),
        Vec::new(),
    ));
    let stop = AtomicBool::new(false);
    let mut published = Vec::new();
    let ended = follow(
        &mut remote,
        &hello_call(),
        "0.1.0",
        &stop,
        &mut LiveState::default(),
        &mut |s| {
            published.push(s.clone());
            stop.store(true, Ordering::Relaxed);
        },
    );
    assert!(matches!(ended, Ended::Stopped));
    assert_eq!(published.len(), 1);
    assert_eq!(names(&published[0]), ["nginx stopped"]);
}

/// An event newer than the picture can arrive before the answer that holds the picture: it is
/// kept and applied on top, and one the picture already includes is not applied again.
#[test]
fn an_event_newer_than_the_picture_that_arrives_before_it_is_kept() {
    let (_, state, published, methods) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        // Older than the picture (5) and newer than it: only the second one counts.
        service_event("nginx", "stopped", 4),
        service_event("nginx", "running", 6),
        status(&[("nginx", "starting")], Some(5)),
        // Already in the picture now: skipped without a word.
        service_event("nginx", "running", 6),
        service_event("nginx", "stopping", 7),
    ]);
    assert_eq!(methods, ["hello", "subscribe", "status"]);
    assert_eq!(names(&published[0]), ["nginx running"]);
    assert_eq!(names(&state), ["nginx stopping"]);
    assert_eq!(published.len(), 2);
}

#[test]
fn a_skipped_number_asks_for_the_state_again() {
    let (_, state, _, methods) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "running")], Some(6)),
        // 7 and 8 were lost.
        service_event("php-8.4.26", "running", 9),
        status_numbered(
            4,
            &[("nginx", "stopping"), ("php-8.4.26", "running")],
            Some(9),
        ),
        service_event("nginx", "stopped", 10),
    ]);
    assert_eq!(methods, ["hello", "subscribe", "status", "status"]);
    // The new picture already had event 9: it is not applied twice, and 10 goes on top.
    assert_eq!(names(&state), ["nginx stopped", "php-8.4.26 running"]);
}

#[test]
fn a_skipped_number_whose_picture_is_older_still_applies_the_event() {
    let (_, state, _, methods) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "running")], Some(1)),
        service_event("nginx", "stopping", 4),
        // The picture was taken before event 4 was sent: 4 goes on top of it.
        status_numbered(4, &[("nginx", "running")], Some(2)),
    ]);
    assert_eq!(methods, ["hello", "subscribe", "status", "status"]);
    assert_eq!(names(&state), ["nginx stopping"]);
}

#[test]
fn shutting_down_ends_the_conversation_and_nothing_after_it_counts() {
    let (ended, state, _, _) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "running")], Some(1)),
        service_event("nginx", "stopped", 2),
        event("shutting_down", 3),
        service_event("nginx", "running", 4),
    ]);
    assert!(matches!(ended, Ended::DaemonLeft));
    assert_eq!(names(&state), ["nginx stopped"]);
}

#[test]
fn reloading_is_between_reload_started_and_its_closing() {
    let (_, state, published, _) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[], Some(1)),
        event("reload_started", 2),
        event_with("reload_failed", "reason", "nginx -t failed", 3),
        event("reload_started", 4),
        event("reloaded", 5),
    ]);
    let reloading: Vec<bool> = published.iter().map(|s| s.reloading).collect();
    assert_eq!(reloading, [false, true, false, true, false]);
    assert_eq!(
        published[2].reload_error.as_deref(),
        Some("nginx -t failed")
    );
    // The next attempt forgets the old failure, and a good one leaves none.
    assert_eq!(published[3].reload_error, None);
    assert_eq!(state.reload_error, None);
}

#[test]
fn blocked_stays_until_a_configuration_is_applied() {
    let (_, state, published, _) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[], Some(1)),
        event_with("blocked", "reason", "close the home (icacls ...)", 2),
        service_event("nginx", "stopped", 3),
        event("reloaded", 4),
    ]);
    let blocked = Some("close the home (icacls ...)");
    assert_eq!(published[1].blocked.as_deref(), blocked);
    assert_eq!(published[2].blocked.as_deref(), blocked);
    assert_eq!(state.blocked, None);
}

#[test]
fn the_warnings_event_replaces_the_list_and_an_unknown_event_is_counted_not_fatal() {
    let warnings = |list: &str, seq: u64| {
        format!(
            r#"{{"jsonrpc":"2.0","method":"event","params":{{"type":"warnings","warnings":[{list}],"seq":{seq}}}}}"#
        )
    };
    let (ended, state, published, methods) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "running")], Some(1)),
        warnings(
            r#""the default php 8.3 in crisol.toml is not installed""#,
            2,
        ),
        // A newer daemon's event: read, counted (no gap at 4), nothing drawn.
        event_with("from_the_future", "x", "y", 3),
        warnings("", 4),
        service_event("nginx", "stopped", 5),
    ]);
    assert!(matches!(ended, Ended::Lost), "{ended:?}");
    assert_eq!(
        published[1].warnings,
        ["the default php 8.3 in crisol.toml is not installed"]
    );
    assert!(state.warnings.is_empty(), "{:?}", state.warnings);
    // No gap was seen, so `status` was asked once: the last state came from the events.
    assert_eq!(methods, ["hello", "subscribe", "status"]);
    assert_eq!(state.services[0].state, "stopped");
}

#[test]
fn a_gap_takes_a_reload_under_way_as_over() {
    let (_, state, _, _) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[], Some(1)),
        event("reload_started", 2),
        // 3, the closing, was lost.
        service_event("nginx", "running", 4),
        status_numbered(4, &[("nginx", "running")], Some(4)),
    ]);
    assert!(!state.reloading);
}

#[test]
fn a_daemon_of_another_version_warns_with_the_way_out_and_is_still_followed() {
    let (_, state, _, _) = run_script(&[
        HELLO.replace("0.1.0", "0.0.9"),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "running")], Some(1)),
    ]);
    let warning = state.version_warning.clone().unwrap();
    assert!(warning.detail.contains("0.0.9") && warning.detail.contains("0.1.0"));
    assert!(warning.fix.unwrap().starts_with("crisol shutdown"));
    assert_eq!(names(&state), ["nginx running"]);

    let (_, same, _, _) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[], Some(1)),
    ]);
    assert_eq!(same.version_warning, None);
}

#[test]
fn a_daemon_that_refuses_the_protocol_is_not_followed() {
    let (ended, state, published, methods) = run_script(&[REFUSAL.to_owned()]);
    match ended {
        Ended::Refused(message) => assert!(message.contains("protocol 4")),
        other => panic!("{other:?}"),
    }
    assert_eq!(methods, ["hello"]);
    assert_eq!(published, []);
    assert_eq!(state.services, []);
}

/// The scripted connections of a daemon that goes away and comes back, one per `connect`.
type Connections = Mutex<VecDeque<std::io::Result<Vec<u8>>>>;

fn config(connections: Connections) -> LiveConfig {
    let connections = Arc::new(connections);
    LiveConfig {
        connect: Arc::new(move || {
            let next = connections.lock().unwrap().pop_front();
            match next {
                Some(Ok(bytes)) => {
                    let client = Client::new(Cursor::new(bytes), Vec::new());
                    Ok(Box::new(Remote(client)) as Box<dyn Link>)
                }
                Some(Err(error)) => Err(error),
                None => Err(std::io::ErrorKind::NotFound.into()),
            }
        }),
        hello: hello_call(),
        version: "0.1.0".to_owned(),
        retry: Duration::from_millis(5),
    }
}

type History = Arc<Mutex<Vec<LiveState>>>;

/// Every state the view published.
fn recorded() -> (History, impl Fn(&LiveState) + Send + 'static) {
    let history = History::default();
    let sink = Arc::clone(&history);
    (history, move |state: &LiveState| {
        sink.lock().unwrap().push(state.clone());
    })
}

fn wait_until(history: &History, ready: impl Fn(&[LiveState]) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ready(&history.lock().unwrap()) {
        assert!(Instant::now() < end, "the view did not get there");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The connections the history went through, one entry per change.
fn connections(history: &[LiveState]) -> Vec<Connection> {
    let mut seen: Vec<Connection> = Vec::new();
    for state in history {
        if seen.last() != Some(&state.connection) {
            seen.push(state.connection.clone());
        }
    }
    seen
}

#[test]
fn the_view_follows_the_daemon_out_and_back_in() {
    let first = script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "running")], Some(1)),
        service_event("nginx", "stopped", 2),
        event("shutting_down", 3),
    ]);
    let second = script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[("nginx", "running"), ("mysql", "running")], Some(1)),
    ]);
    let (history, on_change) = recorded();
    let view = LiveView::start(
        config(Mutex::new(VecDeque::from([
            Ok(first),
            // The daemon is gone for a few attempts.
            Err(std::io::ErrorKind::NotFound.into()),
            Err(std::io::ErrorKind::NotFound.into()),
            Ok(second),
        ]))),
        on_change,
    );
    wait_until(&history, |h| {
        // (The second script ends too, and the view goes back to waiting: more can follow.)
        connections(h).starts_with(&[
            Connection::Connected,
            Connection::Disconnected,
            Connection::Connected,
        ])
    });
    drop(view);

    let history = history.lock().unwrap();
    // The first run ended with the daemon's last event, and what it knew is gone with it.
    let left = history
        .iter()
        .position(|s| s.connection == Connection::Disconnected)
        .unwrap();
    assert_eq!(history[left - 1].services[0].state, "stopped");
    assert_eq!(history[left], LiveState::default());
    // The second picture is whole: it does not carry anything from the first.
    let back = history.iter().position(|s| s.services.len() == 2).unwrap();
    assert_eq!(history[back].connection, Connection::Connected);
    assert_eq!(names(&history[back]), ["nginx running", "mysql running"]);
}

#[test]
fn the_view_says_when_the_pipe_is_not_the_users_and_keeps_looking() {
    let (history, on_change) = recorded();
    let view = LiveView::start(
        config(Mutex::new(VecDeque::from([
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the pipe belongs to another account",
            )),
            Ok(script(&[
                HELLO.to_owned(),
                SUBSCRIBED.to_owned(),
                status(&[], Some(1)),
            ])),
        ]))),
        on_change,
    );
    wait_until(&history, |h| {
        connections(h).contains(&Connection::Connected)
    });
    drop(view);
    let seen = connections(&history.lock().unwrap());
    assert!(matches!(&seen[0], Connection::Refused(m) if m.contains("another account")));
}

#[test]
fn a_view_with_no_daemon_waits_and_says_nothing() {
    let (history, on_change) = recorded();
    let view = LiveView::start(config(Mutex::new(VecDeque::new())), on_change);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(view.state(), LiveState::default());
    // It looked again and again; nothing changed, so nothing was said.
    assert!(history.lock().unwrap().is_empty());
    assert!(view
        .wait_for(Duration::from_millis(10), |s| s.connection
            == Connection::Connected)
        .is_none());
}

#[test]
fn a_daemon_of_another_protocol_is_shown_as_refused() {
    let (_history, on_change) = recorded();
    let view = LiveView::start(
        config(Mutex::new(VecDeque::from([Ok(script(&[
            REFUSAL.to_owned()
        ]))]))),
        on_change,
    );
    let state = view
        .wait_for(Duration::from_secs(10), |s| {
            matches!(s.connection, Connection::Refused(_))
        })
        .unwrap();
    assert!(matches!(&state.connection, Connection::Refused(m) if m.contains("protocol 4")));
    assert_eq!(state.services, []);
}

/// Review of F13.1 and F13.2, finding 1: a view that connects after the daemon was blocked, or
/// after a reload failed, sees it in the picture; and after a gap, the picture corrects what
/// the lost events would have said.
#[test]
fn what_still_holds_comes_with_the_picture_also_after_a_gap() {
    let late = r#"{"jsonrpc":"2.0","id":3,"result":{"type":"status","services":[],"warnings":[],"seq":7,"blocked":"close the home (icacls ...)","reload_error":"nginx -t said no"}}"#;
    let (_, state, published, _) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        late.to_owned(),
        // 8, the `reloaded` that lifted both, was lost.
        service_event("nginx", "running", 9),
        status_numbered(4, &[("nginx", "running")], Some(9)),
    ]);
    assert_eq!(
        published[0].blocked.as_deref(),
        Some("close the home (icacls ...)")
    );
    assert_eq!(
        published[0].reload_error.as_deref(),
        Some("nginx -t said no")
    );
    assert_eq!((state.blocked, state.reload_error), (None, None));
}

/// Nginx starting again makes earlier trouble moot, as the daemon counts it.
#[test]
fn a_new_start_of_nginx_clears_what_held_against_the_old_one() {
    let (_, state, _, _) = run_script(&[
        HELLO.to_owned(),
        SUBSCRIBED.to_owned(),
        status(&[], Some(1)),
        event_with("reload_failed", "reason", "could not start", 2),
        service_event("nginx", "starting", 3),
    ]);
    assert_eq!(state.reload_error, None);
}
