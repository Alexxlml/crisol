use crisol_protocol::{
    AdvisoryInfo, Check, DoneInfo, HelloInfo, InstalledInfo, LogChunk, RemovedInfo, ServiceInfo,
    SiteInfo, StatusInfo, UsedInfo, VersionInfo, VersionList,
};

use super::*;

#[test]
fn done_prints_the_sentence_and_each_warning_on_its_own_line() {
    let text = reply(&Reply::Done(DoneInfo {
        message: "Everything is stopped".to_owned(),
        changed: true,
        warnings: vec!["names stop resolving".to_owned(), "another".to_owned()],
    }));
    assert_eq!(
        text,
        "Everything is stopped\nwarning: names stop resolving\nwarning: another\n"
    );
}

#[test]
fn status_is_a_table_with_the_reason_of_a_failure() {
    let text = reply(&Reply::Status(StatusInfo {
        services: vec![
            ServiceInfo {
                name: "nginx".to_owned(),
                state: "failed".to_owned(),
                detail: Some("port 80 is already in use by herd.exe".to_owned()),
            },
            ServiceInfo {
                name: "php-8.4.26".to_owned(),
                state: "running".to_owned(),
                detail: None,
            },
        ],
        warnings: vec![],
    }));
    assert_eq!(
        text,
        "nginx       failed  (port 80 is already in use by herd.exe)\nphp-8.4.26  running\n"
    );
    let empty = reply(&Reply::Status(StatusInfo {
        services: vec![],
        warnings: vec![],
    }));
    assert!(empty.contains("crisol install nginx"), "{empty}");
}

#[test]
fn installing_removing_and_choosing_read_as_sentences() {
    let installed = reply(&Reply::Installed(InstalledInfo {
        service: "php".to_owned(),
        version: "7.4.33".to_owned(),
        path: r"C:\Crisol\bin\php\7.4.33".to_owned(),
        already: false,
        end_of_life: true,
        downloaded: 35_103_568,
        unverified: false,
        advisories: Vec::new(),
    }));
    assert!(
        installed.starts_with(
            "Installed php 7.4.33 in C:\\Crisol\\bin\\php\\7.4.33 (33.5 MB downloaded)\n"
        ),
        "{installed}"
    );
    assert!(
        installed.contains("warning: php 7.4.33 no longer receives security fixes"),
        "{installed}"
    );
    let already = reply(&Reply::Installed(InstalledInfo {
        service: "php".to_owned(),
        version: "8.4.26".to_owned(),
        path: "p".to_owned(),
        already: true,
        end_of_life: false,
        downloaded: 0,
        unverified: false,
        advisories: Vec::new(),
    }));
    assert_eq!(already, "php 8.4.26 was already installed (p)\n");
    assert_eq!(
        reply(&Reply::Removed(RemovedInfo {
            service: "php".to_owned(),
            version: "8.4.26".to_owned()
        })),
        "Removed php 8.4.26\n"
    );
    let used = reply(&Reply::Used(UsedInfo {
        file: "crisol.toml".to_owned(),
        request: "8.4".to_owned(),
        selects: "php 8.4.26".to_owned(),
        previous: None,
        changed: false,
        warnings: vec!["CRISOL_PHP overrides it".to_owned()],
    }));
    assert!(
        used.starts_with(
            "8.4 now selects php 8.4.26 (written to crisol.toml); it already said so\n"
        ),
        "{used}"
    );
    assert!(
        used.ends_with("warning: CRISOL_PHP overrides it\n"),
        "{used}"
    );
}

#[test]
fn lists_mark_what_is_installed_recommended_and_old() {
    let version = |v: &str, installed, recommended, end_of_life| VersionInfo {
        service: "php".to_owned(),
        version: v.to_owned(),
        installed,
        recommended,
        end_of_life,
        unverified: false,
        advisories: Vec::new(),
    };
    let text = reply(&Reply::Versions(VersionList {
        installed: vec![version("8.4.26", true, true, false)],
        available: vec![
            version("8.4.26", true, true, false),
            version("7.4.33", false, false, true),
        ],
    }));
    assert_eq!(
        text,
        "Installed:\n  php 8.4.26  (recommended)\nAvailable:\n  php 8.4.26  [installed]  (recommended)\n  php 7.4.33  (end of life)\n"
    );
    let nothing = reply(&Reply::Versions(VersionList {
        installed: vec![],
        available: vec![],
    }));
    assert_eq!(nothing, "Installed:\n  (nothing yet)\n");
}

#[test]
fn sites_show_the_scheme_the_domain_and_where_they_come_from() {
    let site = |name: &str, secure, kind: &str| SiteInfo {
        name: name.to_owned(),
        domain: format!("{name}.test"),
        root: format!("D:/{name}"),
        docroot: format!("D:/{name}/public"),
        kind: kind.to_owned(),
        secure,
    };
    let text = reply(&Reply::Sites {
        sites: vec![site("shop", true, "linked"), site("blog", false, "parked")],
        warnings: vec!["skipped x".to_owned()],
    });
    assert_eq!(
        text,
        "https  shop.test  D:/shop/public  [linked]\nhttp   blog.test  D:/blog/public  [parked]\nwarning: skipped x\n"
    );
    assert!(reply(&Reply::Sites {
        sites: vec![],
        warnings: vec![]
    })
    .contains("crisol park"));
}

#[test]
fn doctor_marks_each_check_and_puts_the_fix_under_it() {
    let text = reply(&Reply::Doctor {
        checks: vec![
            Check {
                name: "PATH".to_owned(),
                status: CheckStatus::Ok,
                detail: "fine".to_owned(),
                fix: None,
            },
            Check {
                name: "home permissions".to_owned(),
                status: CheckStatus::Problem,
                detail: "open".to_owned(),
                fix: Some("icacls x".to_owned()),
            },
            Check {
                name: "shims".to_owned(),
                status: CheckStatus::Warning,
                detail: "missing".to_owned(),
                fix: None,
            },
        ],
    });
    assert_eq!(
        text,
        "ok       PATH: fine\nPROBLEM  home permissions: open\n         fix: icacls x\nwarning  shims: missing\n"
    );
}

#[test]
fn logs_and_hello_are_plain() {
    assert_eq!(
        reply(&Reply::Logs(LogChunk {
            lines: vec!["a".to_owned(), "b".to_owned()],
            next: 4,
            path: "p".to_owned()
        })),
        "a\nb\n"
    );
    let hello = reply(&Reply::Hello(HelloInfo {
        daemon: "0.1.0".to_owned(),
        protocol: 1,
        home: "C:\\Crisol".to_owned(),
    }));
    assert_eq!(hello, "crisold 0.1.0 (protocol 1) in C:\\Crisol\n");
}

#[test]
fn sizes_read_the_way_people_say_them() {
    assert_eq!(bytes(0), "0 B");
    assert_eq!(bytes(1023), "1023 B");
    assert_eq!(bytes(1024), "1.0 KB");
    assert_eq!(bytes(1_572_864), "1.5 MB");
    assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
}

#[test]
fn an_error_is_one_line_and_the_fix_another() {
    assert_eq!(
        error(&RpcError::new(
            crisol_protocol::ErrorCode::NotFound,
            "no such site"
        )),
        "error: no such site\n"
    );
    assert_eq!(
        error(&RpcError::new(crisol_protocol::ErrorCode::HomeExposed, "open").with_fix("icacls x")),
        "error: open\n  fix: icacls x\n"
    );
}

#[test]
fn progress_prints_a_line_every_ten_percent_and_at_the_end() {
    let event = |done| Event::Progress {
        what: "php 8.4".to_owned(),
        done,
        total: Some(1000),
    };
    let mut last = None;
    let lines: Vec<String> = (0..=100)
        .filter_map(|p| progress(&event(p * 10), &mut last))
        .collect();
    assert!(lines.len() >= 10 && lines.len() <= 12, "{lines:?}");
    assert!(lines[0].starts_with("Downloading php 8.4: 0%"), "{lines:?}");
    assert!(lines.last().unwrap().contains("100%"), "{lines:?}");

    // Without a total, one line says it started and no more.
    let mut last = None;
    let unknown = Event::Progress {
        what: "x".to_owned(),
        done: 5,
        total: None,
    };
    assert_eq!(
        progress(&unknown, &mut last).as_deref(),
        Some("Downloading x...")
    );
    assert_eq!(progress(&unknown, &mut last), None);
    assert_eq!(progress(&Event::Reloaded, &mut last), None);
}

#[test]
fn an_install_from_a_file_and_known_vulnerabilities_are_said_out_loud() {
    let advisory = |fixed_in: Option<&str>| AdvisoryInfo {
        id: "PHP2Shell".to_owned(),
        summary: "Remote code execution".to_owned(),
        url: "https://example.org/a".to_owned(),
        fixed_in: fixed_in.map(str::to_owned),
    };
    let info = |version: &str, unverified, advisories| {
        reply(&Reply::Installed(InstalledInfo {
            service: "php".to_owned(),
            version: version.to_owned(),
            path: "p".to_owned(),
            already: false,
            end_of_life: false,
            downloaded: 1024,
            unverified,
            advisories,
        }))
    };
    let from_file = info("8.4.28", true, Vec::new());
    assert!(
        from_file.starts_with("Installed php 8.4.28 in p (1.0 KB copied from the file)\n"),
        "{from_file}"
    );
    assert!(
        from_file.contains("warning: php 8.4.28 was not verified by Crisol"),
        "{from_file}"
    );
    let fixable = info("8.4.26", false, vec![advisory(Some("8.4.27"))]);
    assert!(
        fixable.contains("known vulnerability, PHP2Shell: Remote code execution (fixed in php 8.4.27: `crisol install php 8.4.27`; https://example.org/a)"),
        "{fixable}"
    );
    let unfixable = info("7.4.33", false, vec![advisory(None)]);
    assert!(unfixable.contains("there is no fixed build"), "{unfixable}");
    let clean = info("8.4.27", false, Vec::new());
    assert!(!clean.contains("warning"), "{clean}");
}

#[test]
fn a_list_marks_unverified_and_vulnerable_versions() {
    let text = reply(&Reply::Versions(VersionList {
        installed: vec![
            VersionInfo {
                service: "php".to_owned(),
                version: "8.4.26".to_owned(),
                installed: true,
                recommended: false,
                end_of_life: false,
                unverified: false,
                advisories: vec!["PHP2Shell".to_owned()],
            },
            VersionInfo {
                service: "php".to_owned(),
                version: "8.4.28".to_owned(),
                installed: true,
                recommended: false,
                end_of_life: false,
                unverified: true,
                advisories: Vec::new(),
            },
        ],
        available: vec![],
    }));
    assert_eq!(
        text,
        "Installed:\n  php 8.4.26  (vulnerable: PHP2Shell)\n  php 8.4.28  (not verified by Crisol)\n"
    );
}
