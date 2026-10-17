use std::fmt::Write as _;
use std::io::{Cursor, Write};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

use super::*;
use crate::cacert::NoRoots;
use crate::catalog::{Arch, Os};
use crate::dbservice::NoAdmin;
use crate::fetch::fake::FakeFetcher;
use crate::paths::fake::FakePrivateDirs;
use crate::ports::fake::FakePortProbe;
use crate::process::fake::FakeSupervisor;
use crate::project::TrustAllOwners;
use crate::web::AlwaysEffective;

const WINDOWS: Target = Target {
    os: Os::Windows,
    arch: Arch::X86_64,
};

fn zip_of(executable: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file(executable, options).unwrap();
    writer.write_all(b"MZ binary").unwrap();
    writer.finish().unwrap().into_inner()
}

fn url_of(service: &str, version: &str) -> String {
    format!("https://example.invalid/{service}-{version}.zip")
}

/// A catalog (and a fetcher that serves it) offering these builds.
fn world(builds: &[(&str, &str)]) -> (Catalog, FakeFetcher) {
    let fetcher = FakeFetcher::new();
    let mut text = String::from("schema = 1\nserial = 1\n");
    for (service, version) in builds {
        let bytes = zip_of(&format!("{service}.exe"));
        let digest = Sha256::digest(&bytes);
        let hash = digest.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        });
        let _ = write!(
            text,
            "\n[[package]]\nservice = \"{service}\"\nversion = \"{version}\"\nos = \"windows\"\n\
             arch = \"x86_64\"\nurl = \"{}\"\nsha256 = \"{hash}\"\nformat = \"zip\"\n\
             executable = \"{service}.exe\"\nsize = {}\n",
            url_of(service, version),
            bytes.len()
        );
        fetcher.serve(&url_of(service, version), bytes);
    }
    (Catalog::parse(&text).unwrap(), fetcher)
}

struct Rig {
    _dir: TempDir,
    engine: Engine,
    dirs: Arc<FakePrivateDirs>,
}

struct Marked(Vec<Check>);

impl SystemChecks for Marked {
    fn checks(&self, _paths: &CrisolPaths) -> Vec<Check> {
        self.0.clone()
    }
}

impl Rig {
    fn new(builds: &[(&str, &str)]) -> Self {
        Self::with_system(builds, Arc::new(NoSystemChecks))
    }

    fn with_system(builds: &[(&str, &str)], system: Arc<dyn SystemChecks>) -> Self {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let (catalog, fetcher) = world(builds);
        let dirs = Arc::new(FakePrivateDirs::new());
        let deps = Deps {
            supervisor: Arc::new(FakeSupervisor::new()),
            probe: Arc::new(FakePortProbe::new()),
            dirs: dirs.clone(),
            fetcher: Arc::new(fetcher),
            owners: Arc::new(TrustAllOwners),
            env: Arc::new(|_| None),
            system,
            roots: Arc::new(NoRoots),
            config_probe: Arc::new(AlwaysEffective),
            db_admin: Arc::new(NoAdmin),
            catalog,
            target: WINDOWS,
        };
        Self {
            _dir: dir,
            engine: Engine::new(paths, deps),
            dirs,
        }
    }

    fn call(&self, call: &Call) -> Result<Reply, RpcError> {
        self.engine
            .handle(call, &mut |_| {})
            .expect("a file operation")
    }

    fn paths(&self) -> &CrisolPaths {
        self.engine.paths()
    }

    fn install(&self, service: &str, version: &str) -> Reply {
        self.call(&Call::Install {
            service: service.to_owned(),
            version: version.to_owned(),
        })
        .unwrap()
    }

    fn folder(&self, name: &str) -> PathBuf {
        let dir = self.paths().home().join("projects").join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

fn done(reply: Reply) -> DoneInfo {
    match reply {
        Reply::Done(info) => info,
        other => panic!("{other:?}"),
    }
}

fn sites(reply: Reply) -> Vec<SiteInfo> {
    match reply {
        Reply::Sites { sites, .. } => sites,
        other => panic!("{other:?}"),
    }
}

fn checks(reply: Reply) -> Vec<Check> {
    match reply {
        Reply::Doctor { checks } => checks,
        other => panic!("{other:?}"),
    }
}

// ---- which calls are the engine's ----------------------------------------------------------

#[test]
fn the_calls_about_running_services_are_left_to_the_daemon() {
    let rig = Rig::new(&[]);
    for call in [
        Call::Hello {
            client: "t".to_owned(),
            protocol: 1,
        },
        Call::Status { service: None },
        Call::Start { service: None },
        Call::Stop { service: None },
        Call::Restart { service: None },
        Call::Shutdown,
        Call::Subscribe,
    ] {
        assert!(call.needs_daemon());
        assert!(
            rig.engine.handle(&call, &mut |_| {}).is_none(),
            "{}",
            call.method()
        );
    }
    // And the other way around: whatever does not need the daemon is answered here.
    for call in [
        Call::List { available: true },
        Call::Sites,
        Call::Doctor,
        Call::Unlink {
            name: "x".to_owned(),
        },
    ] {
        assert!(!call.needs_daemon());
        assert!(
            rig.engine.handle(&call, &mut |_| {}).is_some(),
            "{}",
            call.method()
        );
    }
}

// ---- versions ------------------------------------------------------------------------------

#[test]
fn install_list_and_uninstall_agree_with_what_is_on_disk() {
    let rig = Rig::new(&[("php", "8.4.26"), ("php", "7.4.33"), ("nginx", "1.28.3")]);
    let installed = rig.install("php", "8.4");
    match installed {
        Reply::Installed(info) => {
            assert_eq!(
                (info.service.as_str(), info.version.as_str()),
                ("php", "8.4.26")
            );
            assert!(!info.already);
            assert!(info.path.ends_with("8.4.26"), "{}", info.path);
            assert!(info.downloaded > 0);
        }
        other => panic!("{other:?}"),
    }
    // Installing again downloads nothing and says so.
    match rig.install("php", "8.4.26") {
        Reply::Installed(info) => {
            assert!(info.already);
            assert_eq!(info.downloaded, 0);
        }
        other => panic!("{other:?}"),
    }

    let Reply::Versions(list) = rig.call(&Call::List { available: true }).unwrap() else {
        panic!("not a list");
    };
    assert_eq!(list.installed.len(), 1);
    assert_eq!(list.available.len(), 3);
    let marked = |service: &str, version: &str| {
        list.available
            .iter()
            .find(|v| v.service == service && v.version == version)
            .unwrap()
            .installed
    };
    assert!(marked("php", "8.4.26"));
    assert!(!marked("php", "7.4.33"));
    // Without `available` the catalog is not listed.
    let Reply::Versions(short) = rig.call(&Call::List { available: false }).unwrap() else {
        panic!("not a list");
    };
    assert_eq!(short.available, Vec::new());

    let Reply::Removed(removed) = rig
        .call(&Call::Uninstall {
            service: "php".to_owned(),
            version: "8.4".to_owned(),
        })
        .unwrap()
    else {
        panic!("not removed");
    };
    assert_eq!(
        (removed.service.as_str(), removed.version.as_str()),
        ("php", "8.4.26")
    );
    let Reply::Versions(after) = rig.call(&Call::List { available: false }).unwrap() else {
        panic!("not a list");
    };
    assert_eq!(after.installed, Vec::new());
}

#[test]
fn install_reports_download_progress_through_events() {
    let rig = Rig::new(&[("nginx", "1.28.3")]);
    let mut events = Vec::new();
    rig.engine
        .handle(
            &Call::Install {
                service: "nginx".to_owned(),
                version: "latest".to_owned(),
            },
            &mut |e| events.push(e),
        )
        .unwrap()
        .unwrap();
    assert!(
        events.iter().any(|e| matches!(e, Event::Progress { what, done, .. } if what == "nginx latest" && *done > 0)),
        "{events:?}"
    );
}

#[test]
fn version_errors_have_codes_a_client_can_act_on() {
    let rig = Rig::new(&[("php", "8.4.26")]);
    let code = |call: Call| rig.call(&call).unwrap_err().code;
    assert_eq!(
        code(Call::Install {
            service: "php".to_owned(),
            version: "9.9".to_owned()
        }),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Call::Install {
            service: "php".to_owned(),
            version: "not-a-version".to_owned()
        }),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Call::Uninstall {
            service: "php".to_owned(),
            version: "8.4".to_owned()
        }),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Call::Uninstall {
            service: "php".to_owned(),
            version: "!!".to_owned()
        }),
        ErrorCode::InvalidParams
    );
    assert_eq!(
        code(Call::Use {
            tool: crisol_protocol::Tool::Php,
            version: "8.4".to_owned(),
            dir: None
        }),
        ErrorCode::NotFound
    );
}

#[test]
fn an_open_home_blocks_install_and_uninstall_with_the_command_to_close_it() {
    let rig = Rig::new(&[("php", "8.4.26")]);
    rig.install("php", "8.4.26");
    rig.dirs.expose(rig.paths().home());
    for call in [
        Call::Install {
            service: "php".to_owned(),
            version: "8.4".to_owned(),
        },
        Call::Uninstall {
            service: "php".to_owned(),
            version: "8.4.26".to_owned(),
        },
    ] {
        let error = rig.call(&call).unwrap_err();
        assert_eq!(error.code, ErrorCode::HomeExposed, "{call:?}");
        assert!(error.message.contains("will not go on"), "{error}");
        assert!(
            !error.message.contains("icacls"),
            "the command is apart: {error}"
        );
        assert!(
            error
                .fix
                .as_deref()
                .is_some_and(|f| f.starts_with("icacls ")),
            "{error:?}"
        );
        assert!(!error.message.contains("  "), "{error}");
    }
    // Listing is read only and still works, which is how the user finds out what is wrong.
    assert!(rig.call(&Call::List { available: false }).is_ok());
    assert!(rig.call(&Call::Sites).is_ok());
}

#[test]
fn use_writes_the_default_or_the_pin_of_a_folder() {
    let rig = Rig::new(&[("php", "8.4.26"), ("php", "7.4.33"), ("node", "22.23.3")]);
    rig.install("php", "8.4.26");
    rig.install("php", "7.4.33");
    rig.install("node", "22.23.3");

    let Reply::Used(global) = rig
        .call(&Call::Use {
            tool: crisol_protocol::Tool::Php,
            version: "7.4".to_owned(),
            dir: None,
        })
        .unwrap()
    else {
        panic!("not used");
    };
    assert_eq!(global.selects, "php 7.4.33");
    assert!(global.changed);
    assert!(global.file.ends_with("crisol.toml"), "{}", global.file);

    let project = rig.folder("shop");
    let Reply::Used(local) = rig
        .call(&Call::Use {
            tool: crisol_protocol::Tool::Node,
            version: "22".to_owned(),
            dir: Some(project.display().to_string()),
        })
        .unwrap()
    else {
        panic!("not used");
    };
    assert_eq!(local.selects, "node 22.23.3");
    assert!(project.join(".crisol.toml").is_file());

    // The same again changes nothing.
    let Reply::Used(again) = rig
        .call(&Call::Use {
            tool: crisol_protocol::Tool::Node,
            version: "22".to_owned(),
            dir: Some(project.display().to_string()),
        })
        .unwrap()
    else {
        panic!("not used");
    };
    assert!(!again.changed);
    // A folder that is not there is not found.
    assert_eq!(
        rig.call(&Call::Use {
            tool: crisol_protocol::Tool::Node,
            version: "22".to_owned(),
            dir: Some(project.join("nope").display().to_string()),
        })
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
}

// ---- sites -----------------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)]
fn park_link_secure_and_their_opposites_edit_only_their_own_lines() {
    let rig = Rig::new(&[]);
    // The user's own comments in both files must survive every edit.
    fs::write(
        rig.paths().config_file(),
        "# my config\n[sites]\ntld = \"test\"\n",
    )
    .unwrap();
    fs::write(rig.paths().sites_file(), "# my sites\n").unwrap();

    let projects = rig.folder("projects");
    fs::create_dir_all(projects.join("blog")).unwrap();
    fs::create_dir_all(projects.join("docs")).unwrap();
    let alone = rig.folder("alone");

    assert!(
        done(
            rig.call(&Call::Park {
                path: projects.display().to_string()
            })
            .unwrap()
        )
        .changed
    );
    assert!(
        !done(
            rig.call(&Call::Park {
                path: projects.display().to_string()
            })
            .unwrap()
        )
        .changed
    );
    assert!(
        done(
            rig.call(&Call::Link {
                name: "shop".to_owned(),
                path: alone.display().to_string(),
                docroot: None
            })
            .unwrap()
        )
        .changed
    );

    let listed = sites(rig.call(&Call::Sites).unwrap());
    let names: Vec<(&str, &str)> = listed
        .iter()
        .map(|s| (s.name.as_str(), s.kind.as_str()))
        .collect();
    assert_eq!(
        names,
        [("blog", "parked"), ("docs", "parked"), ("shop", "linked")]
    );
    assert!(listed
        .iter()
        .all(|s| s.domain.rsplit('.').next() == Some("test") && !s.secure));

    assert!(
        done(
            rig.call(&Call::Secure {
                name: "blog".to_owned()
            })
            .unwrap()
        )
        .changed
    );
    assert!(
        !done(
            rig.call(&Call::Secure {
                name: "blog".to_owned()
            })
            .unwrap()
        )
        .changed
    );
    let listed = sites(rig.call(&Call::Sites).unwrap());
    assert!(listed.iter().find(|s| s.name == "blog").unwrap().secure);
    assert!(
        done(
            rig.call(&Call::Unsecure {
                name: "blog".to_owned()
            })
            .unwrap()
        )
        .changed
    );

    assert!(
        done(
            rig.call(&Call::Unlink {
                name: "shop".to_owned()
            })
            .unwrap()
        )
        .changed
    );
    assert!(
        done(
            rig.call(&Call::Unpark {
                path: projects.display().to_string()
            })
            .unwrap()
        )
        .changed
    );
    assert_eq!(sites(rig.call(&Call::Sites).unwrap()), Vec::new());

    let config = fs::read_to_string(rig.paths().config_file()).unwrap();
    assert!(config.starts_with("# my config\n"), "{config}");
    assert!(config.contains("tld = \"test\""), "{config}");
    let sites_file = fs::read_to_string(rig.paths().sites_file()).unwrap();
    assert!(sites_file.starts_with("# my sites\n"), "{sites_file}");
}

#[test]
fn parking_the_default_folder_keeps_it_when_another_is_added() {
    let rig = Rig::new(&[]);
    let projects = rig.folder("projects");
    rig.call(&Call::Park {
        path: projects.display().to_string(),
    })
    .unwrap();
    let config = fs::read_to_string(rig.paths().config_file()).unwrap();
    assert!(
        config.contains("www") && config.contains("projects"),
        "{config}"
    );
    // The default `www` folder still serves its sites.
    fs::create_dir_all(rig.paths().www_dir().join("old")).unwrap();
    let names: Vec<String> = sites(rig.call(&Call::Sites).unwrap())
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["old"]);
    // Taking the folder out brings the default back as it was.
    rig.call(&Call::Unpark {
        path: projects.display().to_string(),
    })
    .unwrap();
    assert!(rig.call(&Call::Sites).is_ok());
}

#[test]
fn site_errors_say_what_is_wrong() {
    let rig = Rig::new(&[]);
    let code = |call: Call| rig.call(&call).unwrap_err();
    assert_eq!(
        code(Call::Park {
            path: "relative/dir".to_owned()
        })
        .code,
        ErrorCode::InvalidParams
    );
    assert_eq!(
        code(Call::Park {
            path: rig.paths().home().join("nope").display().to_string()
        })
        .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Call::Unpark {
            path: "D:/never".to_owned()
        })
        .code,
        ErrorCode::NotFound
    );
    let folder = rig.folder("a");
    assert_eq!(
        code(Call::Link {
            name: "Bad Name!".to_owned(),
            path: folder.display().to_string(),
            docroot: None
        })
        .code,
        ErrorCode::InvalidParams
    );
    assert_eq!(
        code(Call::Unlink {
            name: "ghost".to_owned()
        })
        .code,
        ErrorCode::NotFound
    );
    let secure = code(Call::Secure {
        name: "ghost".to_owned(),
    });
    assert_eq!(secure.code, ErrorCode::NotFound);
    assert!(secure.message.contains("crisol sites"), "{secure}");
    for error in [
        secure,
        code(Call::Park {
            path: "relative".to_owned(),
        }),
    ] {
        assert!(!error.message.contains("  "), "{error}");
    }
}

// ---- logs ------------------------------------------------------------------------------------

fn write_log(rig: &Rig, service: &str, text: &str) -> PathBuf {
    let path = rig.paths().runtime_log(service).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

fn chunk(reply: Reply) -> LogChunk {
    match reply {
        Reply::Logs(chunk) => chunk,
        other => panic!("{other:?}"),
    }
}

#[test]
fn logs_give_the_end_and_then_only_what_was_added() {
    let rig = Rig::new(&[]);
    let path = write_log(&rig, "nginx", "one\ntwo\nthree\nfour\n");
    let call = |lines, from| Call::Logs {
        service: "nginx".to_owned(),
        lines,
        from,
    };
    let last = chunk(rig.call(&call(2, None)).unwrap());
    assert_eq!(last.lines, ["three", "four"]);
    assert_eq!(last.next, 19);
    assert!(last.path.ends_with("runtime.log"));

    // Nothing new yet.
    let nothing = chunk(rig.call(&call(2, Some(last.next))).unwrap());
    assert_eq!(nothing.lines, Vec::<String>::new());
    assert_eq!(nothing.next, last.next);

    // A line is added; an unfinished one is not returned until it is finished.
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    write!(file, "five\nsix par").unwrap();
    let more = chunk(rig.call(&call(2, Some(last.next))).unwrap());
    assert_eq!(more.lines, ["five"]);
    writeln!(file, "tial").unwrap();
    let rest = chunk(rig.call(&call(2, Some(more.next))).unwrap());
    assert_eq!(rest.lines, ["six partial"]);

    // The file was rotated (it is shorter than the offset): it is read from the start.
    fs::write(&path, "fresh\n").unwrap();
    let rotated = chunk(rig.call(&call(2, Some(rest.next))).unwrap());
    assert_eq!(rotated.lines, ["fresh"]);
}

#[test]
fn a_big_log_gives_whole_lines_from_its_end() {
    let rig = Rig::new(&[]);
    let mut text = String::new();
    for i in 0..30_000 {
        let _ = writeln!(text, "line number {i}");
    }
    write_log(&rig, "nginx", &text);
    let tail = chunk(
        rig.call(&Call::Logs {
            service: "nginx".to_owned(),
            lines: 3,
            from: None,
        })
        .unwrap(),
    );
    assert_eq!(
        tail.lines,
        [
            "line number 29997",
            "line number 29998",
            "line number 29999"
        ]
    );
}

#[test]
fn a_service_without_a_log_or_with_a_bad_name_is_an_error() {
    let rig = Rig::new(&[]);
    let call = |service: &str| Call::Logs {
        service: service.to_owned(),
        lines: 5,
        from: None,
    };
    assert_eq!(
        rig.call(&call("nginx")).unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        rig.call(&call("../../etc")).unwrap_err().code,
        ErrorCode::InvalidParams
    );
}

// ---- doctor ----------------------------------------------------------------------------------

#[test]
fn doctor_reports_each_check_and_adds_the_ones_of_the_system() {
    let extra = Check {
        name: "elevated helper".to_owned(),
        status: CheckStatus::Warning,
        detail: "not installed".to_owned(),
        fix: Some("crisol system install".to_owned()),
    };
    let rig = Rig::with_system(
        &[("nginx", "1.28.3"), ("php", "8.4.26")],
        Arc::new(Marked(vec![extra.clone()])),
    );
    let fresh = checks(rig.call(&Call::Doctor).unwrap());
    let by_name = |name: &str| fresh.iter().find(|c| c.name == name).unwrap();
    assert_eq!(by_name("home permissions").status, CheckStatus::Ok);
    assert_eq!(by_name("configuration").status, CheckStatus::Ok);
    assert_eq!(by_name("installed tools").status, CheckStatus::Warning);
    assert_eq!(
        by_name("installed tools").fix.as_deref(),
        Some("crisol install nginx")
    );
    assert_eq!(by_name("shims").status, CheckStatus::Warning);
    assert_eq!(by_name("sites").status, CheckStatus::Ok);
    assert_eq!(
        fresh.last(),
        Some(&extra),
        "the system's checks come last, as given"
    );

    rig.install("nginx", "1.28.3");
    rig.install("php", "8.4.26");
    let installed = checks(rig.call(&Call::Doctor).unwrap());
    let tools = installed
        .iter()
        .find(|c| c.name == "installed tools")
        .unwrap();
    assert_eq!(tools.status, CheckStatus::Ok);
    assert!(tools.detail.contains("nginx 1.28.3") && tools.detail.contains("php 8.4.26"));
}

#[test]
fn doctor_names_the_open_folder_and_the_command_and_an_invalid_config() {
    let rig = Rig::new(&[]);
    rig.dirs.expose(&rig.paths().bin_dir());
    fs::write(rig.paths().config_file(), "[sites\n").unwrap();
    let all = checks(rig.call(&Call::Doctor).unwrap());
    let home = all.iter().find(|c| c.name == "home permissions").unwrap();
    assert_eq!(home.status, CheckStatus::Problem);
    assert!(home.detail.contains("bin"), "{}", home.detail);
    assert!(home
        .fix
        .as_deref()
        .is_some_and(|f| f.starts_with("icacls ")));
    let config = all.iter().find(|c| c.name == "configuration").unwrap();
    assert_eq!(config.status, CheckStatus::Problem);
    assert!(config
        .fix
        .as_deref()
        .is_some_and(|f| f.contains("crisol.toml")));
}

#[test]
fn doctor_warns_that_an_enabled_memcached_has_no_login_and_says_nothing_otherwise() {
    let rig = Rig::new(&[]);
    let named = |rig: &Rig| {
        checks(rig.call(&Call::Doctor).unwrap())
            .into_iter()
            .find(|c| c.name == "memcached")
    };
    assert_eq!(named(&rig), None);
    fs::write(
        rig.paths().config_file(),
        "[services.memcached]
enabled = true
",
    )
    .unwrap();
    let check = named(&rig).expect("an enabled Memcached is mentioned");
    assert_eq!(check.status, CheckStatus::Warning);
    assert!(
        check.detail.contains("no password") && check.detail.contains("PHP sessions"),
        "{}",
        check.detail
    );
}

#[test]
fn doctor_says_nothing_of_the_ca_bundle_when_nothing_was_left_out() {
    let rig = Rig::new(&[]);
    let found = checks(rig.call(&Call::Doctor).unwrap())
        .into_iter()
        .any(|c| c.name == "php ca bundle");
    assert!(!found);
}

#[test]
fn doctor_lists_a_service_with_data_in_two_series() {
    let rig = Rig::new(&[]);
    let named = |rig: &Rig| {
        checks(rig.call(&Call::Doctor).unwrap())
            .into_iter()
            .find(|c| c.name == "data series")
    };
    for series in ["10.11", "11.8"] {
        let data = rig
            .paths()
            .data_for("mariadb", series)
            .unwrap()
            .join("mysql");
        fs::create_dir_all(data).unwrap();
        assert!(series != "11.8" || named(&rig).is_some());
    }
    let check = named(&rig).unwrap();
    assert_eq!(check.status, CheckStatus::Warning);
    assert!(
        check
            .detail
            .contains("data/mariadb/10.11 and data/mariadb/11.8"),
        "{}",
        check.detail
    );
}

#[test]
fn every_message_has_no_stretch_of_spaces() {
    let rig = Rig::new(&[]);
    rig.dirs.expose(rig.paths().home());
    for check in checks(rig.call(&Call::Doctor).unwrap()) {
        assert!(!check.detail.contains("  "), "{check:?}");
    }
}

// ---- enable and disable ------------------------------------------------------------------

fn done_info(reply: Reply) -> DoneInfo {
    match reply {
        Reply::Done(info) => info,
        other => panic!("{other:?}"),
    }
}

#[test]
fn enable_and_disable_change_one_value_and_keep_the_rest_of_the_file() {
    let rig = Rig::new(&[]);
    let original = "# my setup
[sites]
tld = \"test\" # keep me

[services.redis]
port = 6380 # mine
";
    fs::write(rig.paths().config_file(), original).unwrap();

    let on = done_info(
        rig.call(&Call::Enable {
            service: "redis".to_owned(),
        })
        .unwrap(),
    );
    assert!(on.changed && on.warnings.is_empty(), "{on:?}");
    let text = fs::read_to_string(rig.paths().config_file()).unwrap();
    assert!(text.contains("# my setup") && text.contains("tld = \"test\" # keep me"));
    assert!(text.contains("port = 6380 # mine"), "{text}");
    assert!(text.contains("enabled = true"), "{text}");

    let again = done_info(
        rig.call(&Call::Enable {
            service: "redis".to_owned(),
        })
        .unwrap(),
    );
    assert!(!again.changed, "{again:?}");
    assert_eq!(fs::read_to_string(rig.paths().config_file()).unwrap(), text);

    let off = done_info(
        rig.call(&Call::Disable {
            service: "redis".to_owned(),
        })
        .unwrap(),
    );
    assert!(
        off.changed && off.message.contains("crisol stop redis"),
        "{off:?}"
    );
    let text = fs::read_to_string(rig.paths().config_file()).unwrap();
    assert!(
        text.contains("enabled = false") && text.contains("# mine"),
        "{text}"
    );

    // Disabling what was never enabled writes nothing, not even the file.
    let fresh = Rig::new(&[]);
    let none = done_info(
        fresh
            .call(&Call::Disable {
                service: "mysql".to_owned(),
            })
            .unwrap(),
    );
    assert!(!none.changed);
    assert!(!fresh.paths().config_file().exists());
}

#[test]
fn enabling_an_alternative_or_a_service_on_a_taken_port_warns_and_still_writes() {
    let rig = Rig::new(&[]);
    fs::write(
        rig.paths().config_file(),
        "[services.mysql]
enabled = true
[services.redis]
enabled = true
",
    )
    .unwrap();
    let mariadb = done_info(
        rig.call(&Call::Enable {
            service: "mariadb".to_owned(),
        })
        .unwrap(),
    );
    assert!(mariadb.changed);
    assert!(
        mariadb.warnings.iter().any(|w| w.contains("alternatives")),
        "{mariadb:?}"
    );
    let garnet = done_info(
        rig.call(&Call::Enable {
            service: "garnet".to_owned(),
        })
        .unwrap(),
    );
    assert!(
        garnet.warnings.iter().any(|w| w.contains("alternatives")),
        "{garnet:?}"
    );
    // Something that has nothing to do with them says nothing about them.
    let mail = done_info(
        rig.call(&Call::Enable {
            service: "mailpit".to_owned(),
        })
        .unwrap(),
    );
    assert!(mail.warnings.is_empty(), "{mail:?}");
}

#[test]
fn enabling_something_that_is_not_a_data_service_is_refused_and_writes_nothing() {
    let rig = Rig::new(&[]);
    let error = rig
        .call(&Call::Enable {
            service: "nginx".to_owned(),
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidParams);
    assert!(!rig.paths().config_file().exists());
}

#[test]
fn installing_php_leaves_its_php_ini_for_the_shim_and_a_missing_bundle_is_no_error() {
    let rig = Rig::new(&[("php", "8.4.26")]);
    rig.install("php", "8.4.26");
    // The fake build is only an executable: give it what a real one ships.
    let install = rig.paths().bin_for("php", "8.4.26").unwrap();
    fs::create_dir_all(install.join("ext")).unwrap();
    fs::write(
        install.join("php.ini-development"),
        "[PHP]\n;extension=curl\n",
    )
    .unwrap();
    fs::write(install.join("ext").join("php_curl.dll"), "MZ").unwrap();
    assert!(!rig.paths().php_ini("8.4.26").unwrap().exists());

    // Same version again: nothing downloaded, but the generated files are brought up to date.
    match rig.install("php", "8.4.26") {
        Reply::Installed(info) => assert!(info.already),
        other => panic!("{other:?}"),
    }
    let ini = fs::read_to_string(rig.paths().php_ini("8.4.26").unwrap()).unwrap();
    assert!(ini.contains("extension=curl"), "{ini}");
    // The catalog of this test has no bundle: PHP is installed all the same.
    assert!(!rig.paths().php_cacert().exists());
}
