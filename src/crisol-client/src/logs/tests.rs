use crisol_protocol::ServiceInfo;

use super::*;

fn chunk(lines: &[&str], next: u64) -> LogChunk {
    LogChunk {
        lines: lines.iter().map(|l| (*l).to_owned()).collect(),
        next,
        path: r"C:\Crisol\logs\nginx\runtime.log".to_owned(),
    }
}

fn running(names: &[&str]) -> LiveState {
    LiveState {
        services: names
            .iter()
            .map(|name| ServiceInfo {
                name: (*name).to_owned(),
                state: "running".to_owned(),
                detail: None,
            })
            .collect(),
        ..LiveState::default()
    }
}

#[test]
fn the_services_come_in_the_daemons_order_and_the_daemons_own_log_last() {
    assert_eq!(
        log_services(&running(&["nginx", "php-8.4.26", "mysql"])),
        ["nginx", "php-8.4.26", "mysql", "crisold"]
    );
}

#[test]
fn without_a_daemon_the_daemons_log_can_still_be_read() {
    assert_eq!(log_services(&LiveState::default()), ["crisold"]);
}

#[test]
fn the_daemons_log_is_not_listed_twice() {
    assert_eq!(log_services(&running(&["crisold"])), ["crisold"]);
}

#[test]
fn the_first_read_asks_for_the_end_and_the_next_ones_for_what_came_after() {
    assert_eq!(
        log_call("nginx", None),
        Call::Logs {
            service: "nginx".to_owned(),
            lines: FIRST_LINES,
            from: None
        }
    );
    assert_eq!(
        log_call("nginx", Some(900)),
        Call::Logs {
            service: "nginx".to_owned(),
            lines: FIRST_LINES,
            from: Some(900)
        }
    );
}

#[test]
fn the_first_read_starts_the_window_over() {
    let update = log_update("nginx", None, chunk(&["a", "b"], 4));
    assert!(update.replace);
    assert_eq!(update.lines, ["a", "b"]);
    assert_eq!(update.next, 4);
    assert_eq!(update.keep, KEEP_LINES);
}

#[test]
fn a_following_read_adds_to_the_window() {
    let update = log_update("nginx", Some(4), chunk(&["c"], 6));
    assert!(!update.replace);
    assert_eq!(update.lines, ["c"]);
    assert_eq!(update.next, 6);
}

#[test]
fn a_following_read_with_nothing_new_changes_nothing() {
    let update = log_update("nginx", Some(6), chunk(&[], 6));
    assert!(!update.replace);
    assert_eq!(update.lines.len(), 0);
    assert_eq!(update.next, 6);
}

#[test]
fn a_file_shorter_than_where_the_last_read_ended_was_rotated() {
    // The daemon reads a rotated file from its start, so the new `next` is behind `from`.
    let update = log_update("nginx", Some(5000), chunk(&["fresh"], 6));
    assert!(update.replace);
    assert_eq!(update.lines, ["fresh"]);
}
