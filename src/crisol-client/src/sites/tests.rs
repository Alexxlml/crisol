use std::fs;
use std::path::PathBuf;

use serde_json::json;
use tempfile::TempDir;

use super::*;

fn config(text: &str) -> Config {
    Config::parse(text).expect("valid").config
}

fn site(name: &str, kind: &str, secure: bool) -> SiteInfo {
    SiteInfo {
        name: name.to_owned(),
        domain: format!("{name}.test"),
        root: format!(r"D:\Proyectos\{name}"),
        docroot: format!(r"D:\Proyectos\{name}\public"),
        kind: kind.to_owned(),
        secure,
    }
}

fn home() -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().expect("dir");
    let paths = CrisolPaths::new(tmp.path().join("home")).expect("a home");
    (tmp, paths)
}

#[test]
fn the_address_uses_https_when_secured_and_the_port_only_when_it_is_not_the_usual_one() {
    let usual = config("");
    assert_eq!(site_url("shop.test", false, &usual), "http://shop.test/");
    assert_eq!(site_url("shop.test", true, &usual), "https://shop.test/");
    let test_home = config("[services.nginx]\nhttp_port = 19080\nhttps_port = 19443\n");
    assert_eq!(
        site_url("shop.test", false, &test_home),
        "http://shop.test:19080/"
    );
    assert_eq!(
        site_url("shop.test", true, &test_home),
        "https://shop.test:19443/"
    );
}

#[test]
fn every_address_is_one_the_browser_may_be_sent_to() {
    for secure in [false, true] {
        let url = site_url(
            "shop.test",
            secure,
            &config("[services.nginx]\nhttp_port = 8080\n"),
        );
        assert!(crisol_platform::is_web_address(&url), "{url}");
    }
}

#[test]
fn a_site_offers_the_opposite_of_its_https_and_unlink_only_if_it_was_linked() {
    use SiteAction::{Secure, Unlink, Unsecure};
    assert_eq!(site_actions("parked", false), [Secure]);
    assert_eq!(site_actions("parked", true), [Unsecure]);
    assert_eq!(site_actions("linked", false), [Secure, Unlink]);
    assert_eq!(site_actions("linked", true), [Unsecure, Unlink]);
}

#[test]
fn the_default_parked_folder_has_nothing_to_unpark() {
    let (_tmp, paths) = home();
    let rows = parked_rows(&paths, &config(""));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entry, "www");
    assert_eq!(rows[0].path, paths.home().join("www").display().to_string());
    assert!(!rows[0].removable);
}

#[test]
fn parked_folders_say_whether_they_are_there_and_give_the_entry_unpark_needs() {
    let (tmp, paths) = home();
    let real = tmp.path().join("Proyectos");
    fs::create_dir_all(&real).expect("folder");
    let gone: PathBuf = tmp.path().join("borrada");
    let text = format!(
        "[sites]\nparked = [\"www\", {:?}, {:?}]\n",
        real.to_str().expect("utf-8"),
        gone.to_str().expect("utf-8")
    );
    let rows = parked_rows(&paths, &config(&text));
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.removable));
    // The relative entry is shown resolved but unparked as it was written.
    assert_eq!(rows[0].entry, "www");
    assert_eq!(rows[0].path, paths.home().join("www").display().to_string());
    assert!(rows[1].exists && !rows[2].exists);
    assert_eq!(rows[1].entry, real.display().to_string());
}

#[test]
fn the_view_puts_the_daemons_sites_with_the_configuration() {
    let (_tmp, paths) = home();
    let config = config("[sites]\ntld = \"dev\"\n[services.nginx]\nhttp_port = 19080\n");
    let view = sites_view(
        vec![site("shop", "linked", false), site("blog", "parked", true)],
        vec!["`x` is not a folder".to_owned()],
        &paths,
        &config,
    );
    assert_eq!(view.tld, "dev");
    assert_eq!(view.warnings, ["`x` is not a folder"]);
    assert_eq!(view.sites[0].url, "http://shop.test:19080/");
    assert_eq!(
        view.sites[0].actions,
        [SiteAction::Secure, SiteAction::Unlink]
    );
    assert_eq!(view.sites[1].url, "https://blog.test/");
    assert_eq!(view.sites[1].actions, [SiteAction::Unsecure]);
    // What the daemon said is passed on.
    assert_eq!(view.sites[1].docroot, r"D:\Proyectos\blog\public");
}

#[test]
fn the_suggested_name_is_one_the_core_accepts() {
    for (folder, name) in [
        (r"D:\Proyectos\Tienda", "tienda"),
        (r"D:\Proyectos\mi tienda_2", "mi-tienda-2"),
        (r"D:\Proyectos\--Mi  App!!", "mi-app"),
        // A letter with an accent is not one the core accepts: it is dropped, not guessed.
        (r"D:\Proyectos\ÁrbolDeNavidad", "rboldenavidad"),
        (r"D:\Proyectos\___", ""),
        (r"D:\", ""),
    ] {
        assert_eq!(suggested_site_name(Path::new(folder)), name, "{folder}");
    }
    let long = format!(r"D:\{}", "a".repeat(100));
    assert_eq!(suggested_site_name(Path::new(&long)).len(), 63);
}

#[test]
fn each_click_makes_the_call_the_command_line_makes() {
    let act = |value: serde_json::Value| -> Call {
        serde_json::from_value::<Act>(value).expect("an act").call()
    };
    assert_eq!(
        act(json!({"do": "service", "action": "restart", "name": "nginx"})),
        Call::Restart {
            service: Some("nginx".to_owned())
        }
    );
    assert_eq!(
        act(json!({"do": "all", "toggle": "stop_all"})),
        Call::Stop { service: None }
    );
    assert_eq!(
        act(json!({"do": "all", "toggle": "start_all"})),
        Call::Start { service: None }
    );
    assert_eq!(
        act(json!({"do": "site", "action": "secure", "name": "shop"})),
        Call::Secure {
            name: "shop".to_owned()
        }
    );
    assert_eq!(
        act(json!({"do": "site", "action": "unlink", "name": "shop"})),
        Call::Unlink {
            name: "shop".to_owned()
        }
    );
    assert_eq!(
        act(json!({"do": "link", "name": "shop", "path": r"D:\Proyectos\shop"})),
        Call::Link {
            name: "shop".to_owned(),
            path: r"D:\Proyectos\shop".to_owned(),
            docroot: None
        }
    );
    assert_eq!(
        act(json!({"do": "park", "path": r"D:\Proyectos"})),
        Call::Park {
            path: r"D:\Proyectos".to_owned()
        }
    );
    assert_eq!(
        act(json!({"do": "unpark", "entry": "www"})),
        Call::Unpark {
            path: "www".to_owned()
        }
    );
    assert_eq!(
        act(json!({"do": "reset_password", "service": "mysql"})),
        Call::ResetPassword {
            service: "mysql".to_owned()
        }
    );
}

#[test]
fn a_click_the_page_invents_is_refused() {
    for bad in [
        json!({"do": "format_disk"}),
        json!({"do": "service", "action": "reboot", "name": "nginx"}),
        json!({"do": "site", "action": "start", "name": "shop"}),
        json!({"do": "link", "name": "shop"}),
    ] {
        assert!(serde_json::from_value::<Act>(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn the_sites_view_json_is_what_the_page_reads() {
    let (_tmp, paths) = home();
    let view = sites_view(
        vec![site("shop", "linked", false)],
        Vec::new(),
        &paths,
        &config(""),
    );
    let value = serde_json::to_value(&view).expect("json");
    assert_eq!(
        value["sites"][0],
        json!({
            "name": "shop",
            "domain": "shop.test",
            "root": r"D:\Proyectos\shop",
            "docroot": r"D:\Proyectos\shop\public",
            "kind": "linked",
            "secure": false,
            "url": "http://shop.test/",
            "actions": ["secure", "unlink"],
        })
    );
    assert_eq!(value["tld"], json!("test"));
    assert_eq!(value["parked"][0]["removable"], json!(false));
}
