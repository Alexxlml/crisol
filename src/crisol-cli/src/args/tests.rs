// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]

use super::*;

fn plan(words: &[&str]) -> Plan {
    let mut arguments = vec!["crisol"];
    arguments.extend_from_slice(words);
    parse(arguments)
        .unwrap()
        .command
        .plan(Path::new(r"D:\work\shop"))
}

fn call(words: &[&str]) -> Call {
    match plan(words) {
        Plan::Call(call) => call,
        other => panic!("{other:?}"),
    }
}

#[test]
fn each_command_is_one_call() {
    assert_eq!(call(&["start"]), Call::Start { service: None });
    assert_eq!(
        call(&["restart", "nginx"]),
        Call::Restart {
            service: Some("nginx".to_owned())
        }
    );
    assert_eq!(call(&["status"]), Call::Status { service: None });
    assert_eq!(
        call(&["install", "php", "8.4"]),
        Call::Install {
            service: "php".to_owned(),
            version: "8.4".to_owned()
        }
    );
    assert_eq!(
        call(&["install", "nginx"]),
        Call::Install {
            service: "nginx".to_owned(),
            version: "latest".to_owned()
        }
    );
    assert_eq!(
        call(&["uninstall", "php", "7.4"]),
        Call::Uninstall {
            service: "php".to_owned(),
            version: "7.4".to_owned()
        }
    );
    assert_eq!(call(&["list"]), Call::List { available: false });
    assert_eq!(
        call(&["list", "--available"]),
        Call::List { available: true }
    );
    assert_eq!(call(&["sites"]), Call::Sites);
    assert_eq!(
        call(&["secure", "shop"]),
        Call::Secure {
            name: "shop".to_owned()
        }
    );
    assert_eq!(
        call(&["unsecure", "shop"]),
        Call::Unsecure {
            name: "shop".to_owned()
        }
    );
    assert_eq!(
        call(&["unlink", "shop"]),
        Call::Unlink {
            name: "shop".to_owned()
        }
    );
    assert_eq!(call(&["doctor"]), Call::Doctor);
}

#[test]
fn use_is_global_unless_a_folder_is_named() {
    let global = call(&["use", "php", "8.4"]);
    assert_eq!(
        global,
        Call::Use {
            tool: Tool::Php,
            version: "8.4".to_owned(),
            dir: None
        }
    );
    let local = call(&["use", "node", "22", "--local"]);
    assert_eq!(
        local,
        Call::Use {
            tool: Tool::Node,
            version: "22".to_owned(),
            dir: Some(r"D:\work\shop".to_owned())
        }
    );
    let named = call(&["use", "php", "8.4", "--dir", r"E:\other\app"]);
    assert!(matches!(named, Call::Use { dir: Some(d), .. } if d == r"E:\other\app"));
    assert!(parse(["crisol", "use", "php", "8", "--local", "--dir", "x"]).is_err());
}

#[test]
fn folders_default_to_where_the_command_ran_and_are_made_absolute() {
    assert_eq!(
        call(&["park"]),
        Call::Park {
            path: r"D:\work\shop".to_owned()
        }
    );
    assert_eq!(
        call(&["unpark", "."]),
        Call::Unpark {
            path: r"D:\work\shop".to_owned()
        }
    );
    assert_eq!(
        call(&["park", "sub"]),
        Call::Park {
            path: r"D:\work\shop\sub".to_owned()
        }
    );
    assert_eq!(
        call(&["park", r"..\sibling"]),
        Call::Park {
            path: r"D:\work\sibling".to_owned()
        }
    );
    assert_eq!(
        call(&["park", r"..\..\..\..\up"]),
        Call::Park {
            path: r"D:\up".to_owned()
        }
    );
    assert_eq!(
        call(&["park", r"E:\abs"]),
        Call::Park {
            path: r"E:\abs".to_owned()
        }
    );
}

#[test]
fn link_takes_its_name_from_the_folder_when_none_is_given() {
    assert_eq!(
        call(&["link"]),
        Call::Link {
            name: "shop".to_owned(),
            path: r"D:\work\shop".to_owned(),
            docroot: None
        }
    );
    assert_eq!(
        call(&["link", "store", "--docroot", "public"]),
        Call::Link {
            name: "store".to_owned(),
            path: r"D:\work\shop".to_owned(),
            docroot: Some("public".to_owned())
        }
    );
    // A name that is a folder name is lower-cased: site names are.
    assert!(
        matches!(call(&["link", "x", r"E:\Some\MyApp"]), Call::Link { path, .. } if path == r"E:\Some\MyApp")
    );
    let derived = plan_with_cwd(&["link"], r"D:\work\MyShop");
    assert!(matches!(derived, Plan::Call(Call::Link { name, .. }) if name == "myshop"));
}

fn plan_with_cwd(words: &[&str], cwd: &str) -> Plan {
    let mut arguments = vec!["crisol"];
    arguments.extend_from_slice(words);
    parse(arguments).unwrap().command.plan(Path::new(cwd))
}

#[test]
fn stop_and_logs_are_plans_of_their_own() {
    // `stop` stops the services and nothing else; closing the daemon is said out loud.
    assert_eq!(call(&["stop"]), Call::Stop { service: None });
    assert_eq!(call(&["stop", "--daemon"]), Call::Shutdown);
    assert_eq!(call(&["shutdown"]), Call::Shutdown);
    assert!(parse(["crisol", "stop", "--keep-daemon"]).is_err());
    assert!(parse(["crisol", "stop", "--daemon", "nginx"]).is_err());
    assert_eq!(
        call(&["logs", "nginx"]),
        Call::Logs {
            service: "nginx".to_owned(),
            lines: 50,
            from: None
        }
    );
    assert_eq!(
        call(&["logs", "nginx", "-n", "5"]),
        Call::Logs {
            service: "nginx".to_owned(),
            lines: 5,
            from: None
        }
    );
    assert_eq!(
        plan(&["logs", "nginx", "-f", "-n", "7"]),
        Plan::Follow {
            service: "nginx".to_owned(),
            lines: 7
        }
    );
}

#[test]
fn json_is_a_flag_of_every_command_wherever_it_is_written() {
    for words in [
        vec!["--json", "list"],
        vec!["list", "--json"],
        vec!["install", "php", "--json"],
        vec!["logs", "nginx", "--json"],
    ] {
        let mut arguments = vec!["crisol"];
        arguments.extend(words);
        assert!(parse(arguments).unwrap().json);
    }
    assert!(!parse(["crisol", "list"]).unwrap().json);
}

#[test]
fn every_call_the_cli_can_make_is_one_the_protocol_knows() {
    // A round trip through the wire format for each command of the table above.
    for call in [
        call(&["use", "php", "8.4", "--local"]),
        call(&["link", "x", "--docroot", "public"]),
        call(&["logs", "nginx"]),
    ] {
        let line =
            crisol_protocol::to_line(&crisol_protocol::Request::new(1, call.clone())).unwrap();
        let back: crisol_protocol::Request = crisol_protocol::from_line(&line).unwrap();
        assert_eq!(back.call, call);
    }
}
