use super::*;

fn info(service: &str, version: &str, installed: bool) -> VersionInfo {
    VersionInfo {
        service: service.to_owned(),
        version: version.to_owned(),
        installed,
        recommended: false,
        end_of_life: false,
        unverified: false,
        advisories: Vec::new(),
    }
}

fn list(installed: Vec<VersionInfo>, available: Vec<VersionInfo>) -> VersionList {
    VersionList {
        installed,
        available,
    }
}

fn nothing(_: &str) -> Option<String> {
    None
}

fn progress(what: &str, done: u64, total: Option<u64>) -> Event {
    Event::Progress {
        what: what.to_owned(),
        done,
        total,
        from_file: false,
    }
}

#[test]
fn what_is_not_installed_can_only_be_installed() {
    use VersionAction::Install;
    assert_eq!(version_actions("php", false, false), vec![Install]);
    assert_eq!(version_actions("nginx", false, false), vec![Install]);
}

#[test]
fn an_installed_php_or_node_can_be_chosen_unless_it_already_is() {
    use VersionAction::{Uninstall, Use};
    assert_eq!(version_actions("php", true, false), vec![Use, Uninstall]);
    assert_eq!(version_actions("node", true, false), vec![Use, Uninstall]);
    assert_eq!(version_actions("php", true, true), vec![Uninstall]);
}

#[test]
fn a_tool_without_a_version_to_choose_is_only_removed() {
    use VersionAction::Uninstall;
    for service in ["nginx", "mysql", "composer", "redis"] {
        assert_eq!(version_actions(service, true, false), vec![Uninstall]);
    }
}

#[test]
fn the_catalog_and_the_disk_make_one_row_per_version() {
    let view = versions_view(
        list(
            vec![info("php", "8.4.26", true)],
            vec![info("php", "8.4.26", true), info("php", "8.3.35", false)],
        ),
        &nothing,
    );
    assert_eq!(view.services.len(), 1);
    let rows = &view.services[0].rows;
    assert_eq!(rows.len(), 2);
    assert!(rows[0].installed && rows[0].version == "8.4.26");
    assert!(!rows[1].installed && rows[1].version == "8.3.35");
}

#[test]
fn a_version_installed_from_a_file_is_listed_though_the_catalog_lacks_it() {
    let mut from_file = info("php", "7.4.34", true);
    from_file.unverified = true;
    let view = versions_view(
        list(vec![from_file], vec![info("php", "8.4.26", false)]),
        &nothing,
    );
    let rows = &view.services[0].rows;
    let old = rows.iter().find(|r| r.version == "7.4.34").expect("listed");
    assert!(old.installed && old.unverified);
}

#[test]
fn the_highest_version_comes_first_by_number_not_by_text() {
    let view = versions_view(
        list(
            Vec::new(),
            vec![
                info("node", "9.11.2", false),
                info("node", "22.23.4", false),
                info("node", "24.22.2", false),
                info("node", "20.20.2", false),
            ],
        ),
        &nothing,
    );
    let versions: Vec<_> = view.services[0]
        .rows
        .iter()
        .map(|r| r.version.as_str())
        .collect();
    assert_eq!(versions, ["24.22.2", "22.23.4", "20.20.2", "9.11.2"]);
}

#[test]
fn the_tools_come_in_a_fixed_order() {
    let view = versions_view(
        list(
            Vec::new(),
            vec![
                info("redis", "5.0.14.1", false),
                info("nginx", "1.30.5", false),
                info("node", "24.22.2", false),
                info("mysql", "8.4.11", false),
                info("php", "8.4.27", false),
                info("composer", "2.10.3", false),
            ],
        ),
        &nothing,
    );
    let order: Vec<_> = view.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(
        order,
        ["php", "node", "nginx", "composer", "mysql", "redis"]
    );
}

#[test]
fn only_the_installed_version_that_is_the_default_is_marked() {
    let default = |service: &str| (service == "php").then(|| "8.4.27".to_owned());
    let view = versions_view(
        list(
            vec![info("php", "8.4.27", true), info("php", "8.3.36", true)],
            vec![info("php", "8.5.12", false)],
        ),
        &default,
    );
    let rows = &view.services[0].rows;
    let marked: Vec<_> = rows
        .iter()
        .filter(|r| r.default)
        .map(|r| r.version.as_str())
        .collect();
    assert_eq!(marked, ["8.4.27"]);
    // The default is not offered to be chosen again, the others installed are.
    let of = |version: &str| {
        &rows
            .iter()
            .find(|r| r.version == version)
            .expect("row")
            .actions
    };
    assert_eq!(of("8.4.27"), &vec![VersionAction::Uninstall]);
    assert_eq!(
        of("8.3.36"),
        &vec![VersionAction::Use, VersionAction::Uninstall]
    );
    assert_eq!(of("8.5.12"), &vec![VersionAction::Install]);
}

#[test]
fn a_default_that_is_not_installed_marks_nothing() {
    let default = |_: &str| Some("8.4.27".to_owned());
    let view = versions_view(
        list(Vec::new(), vec![info("php", "8.4.27", false)]),
        &default,
    );
    assert!(!view.services[0].rows[0].default);
}

#[test]
fn what_the_daemon_says_of_a_version_reaches_the_row() {
    let mut old = info("php", "7.4.33", true);
    old.end_of_life = true;
    old.advisories = vec!["PHP2Shell".to_owned()];
    let mut rec = info("php", "8.4.27", false);
    rec.recommended = true;
    let view = versions_view(list(vec![old], vec![rec]), &nothing);
    let rows = &view.services[0].rows;
    let recommended = &rows[0];
    assert!(recommended.recommended && !recommended.end_of_life);
    let old = &rows[1];
    assert!(old.end_of_life);
    assert_eq!(old.advisories, ["PHP2Shell"]);
}

#[test]
fn a_click_makes_the_call_the_command_line_makes() {
    let install = VersionAct::Install {
        service: "php".to_owned(),
        version: "8.4.27".to_owned(),
    };
    assert_eq!(
        install.call().expect("a call"),
        Call::Install {
            service: "php".to_owned(),
            version: Some("8.4.27".to_owned()),
            from: None,
            sha256: None,
        }
    );
    let uninstall = VersionAct::Uninstall {
        service: "node".to_owned(),
        version: "20.20.2".to_owned(),
    };
    assert_eq!(
        uninstall.call().expect("a call"),
        Call::Uninstall {
            service: "node".to_owned(),
            version: "20.20.2".to_owned(),
        }
    );
    let choose = VersionAct::Use {
        service: "node".to_owned(),
        version: "24.22.2".to_owned(),
    };
    assert_eq!(
        choose.call().expect("a call"),
        Call::Use {
            tool: Tool::Node,
            version: "24.22.2".to_owned(),
            dir: None,
        }
    );
}

#[test]
fn choosing_a_version_of_something_that_has_none_is_refused_before_it_is_sent() {
    let act = VersionAct::Use {
        service: "nginx".to_owned(),
        version: "1.30.5".to_owned(),
    };
    let error = act.call().expect_err("refused");
    assert_eq!(error.code, ErrorCode::InvalidParams);
}

#[test]
fn only_an_installation_has_progress_and_it_is_named_as_the_events_name_it() {
    let install = VersionAct::Install {
        service: "php".to_owned(),
        version: "8.4.27".to_owned(),
    };
    assert_eq!(install.installing().as_deref(), Some("php 8.4.27"));
    let remove = VersionAct::Uninstall {
        service: "php".to_owned(),
        version: "8.4.27".to_owned(),
    };
    assert_eq!(remove.installing(), None);
}

#[test]
fn the_page_is_sent_a_progress_event_per_percent_not_one_per_chunk() {
    let mut gate = ProgressGate::new();
    let total = Some(1000);
    let mut passed = 0;
    for done in 0..=1000 {
        if gate.pass(&progress("php 8.4.27", done, total)).is_some() {
            passed += 1;
        }
    }
    // 0 % … 100 %.
    assert_eq!(passed, 101);
}

#[test]
fn the_last_event_always_passes_and_says_one_hundred() {
    let mut gate = ProgressGate::new();
    let _ = gate.pass(&progress("php 8.4.27", 999, Some(1000)));
    let last = gate
        .pass(&progress("php 8.4.27", 1000, Some(1000)))
        .expect("passes");
    assert_eq!(last.percent, Some(100));
    assert_eq!(last.done, 1000);
}

#[test]
fn without_a_total_there_is_no_percent_and_an_event_passes_per_megabyte() {
    let mut gate = ProgressGate::new();
    let first = gate.pass(&progress("php 8.4.27", 10, None)).expect("first");
    assert_eq!(first.percent, None);
    assert!(gate.pass(&progress("php 8.4.27", 500, None)).is_none());
    assert!(gate
        .pass(&progress("php 8.4.27", (1 << 20) + 5, None))
        .is_some());
}

#[test]
fn another_installation_is_not_mistaken_for_the_one_before() {
    let mut gate = ProgressGate::new();
    assert!(gate
        .pass(&progress("php 8.4.27", 500, Some(1000)))
        .is_some());
    assert!(gate
        .pass(&progress("php 8.4.27", 505, Some(1000)))
        .is_none());
    // The same percent, in another installation, is news.
    assert!(gate
        .pass(&progress("node 24.22.2", 500, Some(1000)))
        .is_some());
}

#[test]
fn events_that_are_not_progress_pass_for_nothing() {
    let mut gate = ProgressGate::new();
    assert_eq!(gate.pass(&Event::Reloaded), None);
}

#[test]
fn copying_a_file_says_so() {
    let mut gate = ProgressGate::new();
    let event = Event::Progress {
        what: "php 7.4.34".to_owned(),
        done: 10,
        total: Some(100),
        from_file: true,
    };
    assert!(gate.pass(&event).expect("passes").from_file);
}
