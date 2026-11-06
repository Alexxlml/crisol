//! The formatted output (ADR-0028), at a fixed width and with the colour codes stripped.

use crisol_protocol::{
    Check, CredentialsInfo, DoneInfo, EnvLine, ErrorCode, ServiceInfo, SiteInfo, StatusInfo,
    VersionInfo, VersionList,
};

use super::*;

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' || c == 'K' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn check(name: &str, status: CheckStatus, detail: &str, fix: Option<&str>) -> Check {
    Check {
        name: name.to_owned(),
        status,
        detail: detail.to_owned(),
        fix: fix.map(str::to_owned),
    }
}

#[test]
fn style_needs_a_terminal_that_takes_codes_and_nothing_that_says_no() {
    let detect = |ansi, no_color, term: Option<&str>, plain| {
        Style::detect(ansi, no_color, term, plain, 100).formatted
    };
    assert!(detect(true, false, Some("xterm"), false));
    assert!(detect(true, false, None, false), "Windows sets no TERM");
    assert!(
        !detect(false, false, None, false),
        "console refused the mode"
    );
    assert!(!detect(true, true, None, false), "NO_COLOR");
    assert!(!detect(true, false, Some("dumb"), false));
    assert!(!detect(true, false, None, true), "--plain");
}

#[test]
fn plain_style_gives_the_text_of_always() {
    let r = Reply::Doctor {
        checks: vec![check("PATH", CheckStatus::Ok, "fine", None)],
    };
    assert_eq!(reply_styled(&r, &Style::plain()), reply(&r));
}

#[test]
fn doctor_is_a_boxed_table_with_the_fix_inside_the_cell_and_a_summary() {
    let r = Reply::Doctor {
        checks: vec![
            check(
                "home permissions",
                CheckStatus::Ok,
                "only you can write to it",
                None,
            ),
            check(
                "Visual C++ runtime",
                CheckStatus::Warning,
                "not installed",
                Some("https://aka.ms/vs/17/release/vc_redist.x64.exe"),
            ),
            check("php", CheckStatus::Problem, "8.4.26 cannot start", None),
        ],
    };
    let text = strip_ansi(&reply_styled(&r, &Style::formatted(60)));
    let expected = "\
┌────────┬────────────────────┬────────────────────────────┐
│ Status │ Check              │ Detail                     │
├────────┼────────────────────┼────────────────────────────┤
│ ✓ ok   │ home permissions   │ only you can write to it   │
├────────┼────────────────────┼────────────────────────────┤
│ ! warn │ Visual C++ runtime │ not installed              │
│        │                    │ fix:                       │
│        │                    │ https://aka.ms/vs/17/relea │
│        │                    │ se/vc_redist.x64.exe       │
├────────┼────────────────────┼────────────────────────────┤
│ ✗ fail │ php                │ 8.4.26 cannot start        │
└────────┴────────────────────┴────────────────────────────┘
1 ok · 1 warning · 1 problem
";
    assert_eq!(text, expected);
    for line in text.lines().take(12) {
        assert_eq!(line.chars().count(), 60, "{line}");
    }
}

#[test]
fn a_long_word_is_cut_without_losing_a_character() {
    assert_eq!(
        table::wrap("abcdefghij kl", 4),
        ["abcd", "efgh", "ij", "kl"]
    );
    assert_eq!(table::wrap("one two three", 7), ["one two", "three"]);
    assert_eq!(table::wrap("a\nb", 5), ["a", "b"]);
    assert_eq!(table::wrap("", 5), [""]);
}

#[test]
fn a_narrow_window_gets_the_list_with_marks() {
    let r = Reply::Doctor {
        checks: vec![check(
            "php",
            CheckStatus::Problem,
            "cannot start",
            Some("reinstall"),
        )],
    };
    let text = strip_ansi(&reply_styled(&r, &Style::formatted(40)));
    assert_eq!(
        text,
        "✗ fail php: cannot start\n    fix: reinstall\n0 ok · 0 warnings · 1 problem\n"
    );
}

#[test]
fn colour_is_there_for_a_problem() {
    let r = Reply::Doctor {
        checks: vec![check("php", CheckStatus::Problem, "cannot start", None)],
    };
    let text = reply_styled(&r, &Style::formatted(80));
    assert!(text.contains("\x1b[31m"), "red for a problem");
    assert!(text.contains("\x1b[0m"));
}

#[test]
fn status_sites_and_versions_come_in_tables() {
    let status = Reply::Status(StatusInfo {
        services: vec![ServiceInfo {
            name: "nginx".to_owned(),
            state: "failed".to_owned(),
            detail: Some("port 80 is already in use by herd.exe".to_owned()),
        }],
        warnings: vec!["be careful".to_owned()],
        seq: None,
        blocked: None,
        reload_error: None,
    });
    let text = strip_ansi(&reply_styled(&status, &Style::formatted(80)));
    assert!(text.contains("│ Service │ State  │ Detail"), "{text}");
    assert!(
        text.contains("│ nginx   │ failed │ port 80 is already in use by herd.exe"),
        "{text}"
    );
    assert!(text.ends_with("! be careful\n"), "{text}");

    let sites = Reply::Sites {
        sites: vec![SiteInfo {
            name: "blog".to_owned(),
            domain: "blog.test".to_owned(),
            root: "D:/blog".to_owned(),
            docroot: "D:/blog/public".to_owned(),
            kind: "linked".to_owned(),
            secure: true,
        }],
        warnings: vec![],
    };
    let text = strip_ansi(&reply_styled(&sites, &Style::formatted(80)));
    assert!(
        text.contains("│ https  │ blog.test │ linked │ D:/blog/public"),
        "{text}"
    );

    let version = VersionInfo {
        service: "php".to_owned(),
        version: "7.4.33".to_owned(),
        installed: true,
        recommended: false,
        end_of_life: true,
        unverified: false,
        advisories: vec!["PHP2Shell".to_owned()],
    };
    let list = Reply::Versions(VersionList {
        installed: vec![version.clone()],
        available: vec![version],
    });
    let text = strip_ansi(&reply_styled(&list, &Style::formatted(80)));
    assert!(text.starts_with("Installed\n┌"), "{text}");
    assert!(
        text.contains("│ php  │ 7.4.33  │ end of life, vulnerable: PHP2Shell"),
        "{text}"
    );
    assert!(text.contains("Available\n"), "{text}");
    assert!(text.contains("installed, end of life"), "{text}");
}

#[test]
fn messages_get_a_tick_warnings_a_bang_and_errors_a_cross() {
    let done = Reply::Done(DoneInfo {
        message: "Everything is stopped".to_owned(),
        changed: true,
        warnings: vec!["names stop resolving".to_owned()],
    });
    let text = strip_ansi(&reply_styled(&done, &Style::formatted(80)));
    assert_eq!(text, "✓ Everything is stopped\n! names stop resolving\n");

    let failure = RpcError::new(ErrorCode::Io, "disk is full");
    assert_eq!(
        strip_ansi(&error_styled(&failure, &Style::formatted(80))),
        "✗ disk is full\n"
    );
    assert_eq!(error_styled(&failure, &Style::plain()), error(&failure));
    // Credentials are made to be copied: never dressed.
    let credentials = Reply::Credentials(CredentialsInfo {
        service: "mysql".to_owned(),
        lines: vec![EnvLine {
            key: "DB_PASSWORD".to_owned(),
            value: "x".to_owned(),
        }],
        notes: vec![],
    });
    assert_eq!(
        reply_styled(&credentials, &Style::formatted(80)),
        reply(&credentials)
    );
}

#[test]
fn progress_in_place_redraws_one_line_and_only_when_the_percentage_moves() {
    let event = |done| Event::Progress {
        what: "nginx 1.30.5".to_owned(),
        done,
        total: Some(1000),
        from_file: false,
    };
    let mut last = None;
    let first = progress_in_place(&event(500), &mut last).unwrap();
    assert!(first.starts_with("\r\x1b[2K"));
    assert_eq!(
        strip_ansi(&first).trim_start_matches('\r'),
        "Downloading nginx 1.30.5  [██████████░░░░░░░░░░] 50% (500 B)"
    );
    assert!(
        progress_in_place(&event(501), &mut last).is_none(),
        "same percentage"
    );
    assert!(progress_in_place(&event(510), &mut last).is_some());
    assert!(
        progress_in_place(&event(2000), &mut last)
            .unwrap()
            .contains("100%"),
        "never past 100"
    );
}
