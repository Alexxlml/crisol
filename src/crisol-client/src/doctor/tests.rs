use tempfile::TempDir;

use super::*;

fn home() -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("Crisol")).unwrap();
    (tmp, paths)
}

fn check(status: CheckStatus, fix: Option<&str>, action: Option<CheckAction>) -> Check {
    Check {
        name: "a check".to_owned(),
        status,
        detail: "what was found".to_owned(),
        fix: fix.map(str::to_owned),
        action,
    }
}

fn button(action: &CheckAction) -> Option<DoctorButton> {
    let (_tmp, paths) = home();
    doctor_button(&paths, action)
}

#[test]
fn a_finding_without_an_action_has_no_button_and_keeps_its_words() {
    let (_tmp, paths) = home();
    let view = doctor_view(
        vec![
            check(CheckStatus::Ok, None, None),
            check(CheckStatus::Warning, Some("do this"), None),
            check(CheckStatus::Problem, None, None),
            check(CheckStatus::Problem, None, None),
        ],
        &paths,
    );
    assert_eq!((view.problems, view.warnings), (2, 1));
    assert!(view.rows.iter().all(|row| row.button.is_none()));
    assert_eq!(view.rows[1].fix.as_deref(), Some("do this"));
    assert_eq!(view.rows[1].detail, "what was found");
}

#[test]
fn an_install_says_what_it_installs_and_asks_for_no_second_click() {
    let exact = button(&CheckAction::install("php", Some("8.4.27"))).unwrap();
    assert_eq!(exact.label.key, "doctor.act.install");
    assert_eq!(exact.label.args["service"], "php");
    assert_eq!(exact.label.args["version"], "8.4.27");
    assert_eq!(exact.kind, ButtonKind::Run);
    assert!(!exact.confirm);

    let recommended = button(&CheckAction::install("nginx", None)).unwrap();
    assert_eq!(recommended.label.key, "doctor.act.install_recommended");
    assert_eq!(recommended.label.args["service"], "nginx");
}

#[test]
fn an_install_from_an_archive_of_ones_own_is_never_a_button() {
    let from = |from: Option<&str>, sha256: Option<&str>| CheckAction::Calls {
        calls: vec![Call::Install {
            service: "php".to_owned(),
            version: Some("7.4.33".to_owned()),
            from: from.map(str::to_owned),
            sha256: sha256.map(str::to_owned),
        }],
    };
    let (_tmp, paths) = home();
    for action in [
        from(Some(r"C:\Downloads\php.zip"), Some(&"a".repeat(64))),
        from(Some(r"C:\Downloads\php.zip"), None),
        from(None, Some(&"a".repeat(64))),
    ] {
        assert!(doctor_button(&paths, &action).is_none(), "{action:?}");
    }
    // Not offered, so not carried out either, whatever the page sends.
    assert!(doctor_button(&paths, &from(None, None)).is_some());
}

#[test]
fn removing_a_version_asks_for_a_second_click_and_says_it_reinstalls() {
    let reinstall = button(&CheckAction::reinstall("php", "8.4.28")).unwrap();
    assert_eq!(reinstall.label.key, "doctor.act.reinstall");
    assert_eq!(reinstall.label.args["version"], "8.4.28");
    assert!(reinstall.confirm, "the first call removes something");
}

#[test]
fn what_asks_windows_for_a_permission_asks_for_a_second_click() {
    let confirm = |op| button(&CheckAction::Local { op }).unwrap().confirm;
    assert!(confirm(LocalOp::Trust));
    assert!(confirm(LocalOp::UntrustStale));
    assert!(confirm(LocalOp::SystemInstall));
    assert!(!confirm(LocalOp::AutostartOff));
    assert!(!confirm(LocalOp::PathAdd));
}

#[test]
fn start_is_one_click_and_an_odd_list_of_calls_gets_a_plain_label() {
    let start = button(&CheckAction::start()).unwrap();
    assert_eq!(start.label.key, "doctor.act.start");
    let odd = button(&CheckAction::Calls {
        calls: vec![Call::Sites],
    })
    .unwrap();
    assert_eq!(odd.label.key, "doctor.act.calls");
    // A list with nothing in it is nothing to press.
    assert!(button(&CheckAction::Calls { calls: vec![] }).is_none());
}

#[test]
fn only_a_secure_address_is_ever_opened() {
    let link = |url: &str| {
        button(&CheckAction::Link {
            url: url.to_owned(),
        })
    };
    assert!(link("https://aka.ms/vs/17/release/vc_redist.x64.exe").is_some());
    for no in [
        "http://aka.ms/x",
        "file:///C:/Windows/notepad.exe",
        "calc.exe",
        "https://",
        "https://a.test/ --flag",
        "",
    ] {
        assert!(link(no).is_none(), "{no:?}");
    }
}

#[test]
fn a_command_to_copy_and_a_log_are_done_by_the_page_and_carry_their_text() {
    let copy = button(&CheckAction::Copy {
        text: "ipconfig /flushdns".to_owned(),
    })
    .unwrap();
    assert_eq!(copy.kind, ButtonKind::Copy);
    assert_eq!(copy.text.as_deref(), Some("ipconfig /flushdns"));
    let log = button(&CheckAction::ShowLog {
        service: "mysql".to_owned(),
    })
    .unwrap();
    assert_eq!(log.kind, ButtonKind::ShowLog);
    assert_eq!(log.text.as_deref(), Some("mysql"));
    assert!(button(&CheckAction::Copy {
        text: String::new()
    })
    .is_none());
    assert!(button(&CheckAction::ShowLog {
        service: String::new()
    })
    .is_none());
}

#[test]
fn explorer_is_only_pointed_inside_the_home() {
    let (_tmp, paths) = home();
    let home = paths.home().to_path_buf();
    let reveal = |path: String| doctor_button(&paths, &CheckAction::Reveal { path });
    assert!(reveal(paths.config_file().display().to_string()).is_some());
    assert!(
        reveal(home.display().to_string()).is_some(),
        "the home itself"
    );
    assert!(reveal(home.join("logs").join("nginx").display().to_string()).is_some());
    // Another case of the same letters is the same folder to Windows.
    assert!(inside_home(
        &paths,
        &home.join("LOGS").display().to_string().to_uppercase()
    ));
    // A `..` walks out of the home, and the neighbour that shares a prefix is not inside it.
    let sneaky = format!("{}\\..\\Other", home.display());
    assert!(reveal(sneaky).is_none());
    let neighbour = format!("{}-other\\crisol.toml", home.display());
    assert!(reveal(neighbour).is_none());
    for no in [
        "",
        "crisol.toml",
        r"..\crisol.toml",
        r"C:\Windows\System32\cmd.exe",
    ] {
        assert!(reveal(no.to_owned()).is_none(), "{no:?}");
    }
}

#[test]
fn a_button_carries_the_very_action_it_was_made_from() {
    let (_tmp, paths) = home();
    let action = CheckAction::Local {
        op: LocalOp::PathAdd,
    };
    let view = doctor_view(
        vec![check(
            CheckStatus::Warning,
            Some("crisol path add"),
            Some(action.clone()),
        )],
        &paths,
    );
    assert_eq!(view.rows[0].button.as_ref().unwrap().action, action);
    // And it travels to the page as JSON the page can send back.
    let json = serde_json::to_value(&view.rows[0]).unwrap();
    assert_eq!(json["button"]["action"]["kind"], "local");
    assert_eq!(json["button"]["action"]["op"], "path_add");
    assert_eq!(json["button"]["kind"], "run");
    let back: CheckAction = serde_json::from_value(json["button"]["action"].clone()).unwrap();
    assert_eq!(back, action);
}
