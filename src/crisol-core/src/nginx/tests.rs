use std::fs;

use tempfile::TempDir;

use super::*;
use crate::sites::SiteKind;

fn home() -> CrisolPaths {
    CrisolPaths::new(if cfg!(windows) {
        r"C:\Crisol"
    } else {
        "/crisol"
    })
    .unwrap()
}

fn site(name: &str, docroot: &str, rule: DocRoot, kind: SiteKind) -> Site {
    Site {
        name: name.to_owned(),
        domain: format!("{name}.test"),
        root: PathBuf::from(docroot),
        docroot: PathBuf::from(docroot),
        docroot_rule: rule,
        kind,
    }
}

fn nginx_dir() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        r"C:\Crisol\bin\nginx\1.28.3"
    } else {
        "/crisol/bin/nginx/1.28.3"
    })
}

/// The home is written with forward slashes and the platform's prefix; snapshots keep one.
fn normalised(text: &str) -> String {
    text.replace("C:/Crisol", "<home>")
        .replace("/crisol", "<home>")
}

fn render_for(paths: &CrisolPaths, sites: &[NginxSite<'_>], pools: &PhpPools) -> RenderedNginx {
    let dir = nginx_dir();
    render(&NginxInput {
        paths,
        nginx_dir: &dir,
        http_port: 80,
        sites,
        pools,
    })
}

#[test]
fn the_generated_files_are_the_ones_reviewed() {
    let paths = home();
    let shop = site(
        "shop",
        "D:/code/shop/public",
        DocRoot::Laravel,
        SiteKind::Parked,
    );
    let blog = site("blog", "D:/code/blog", DocRoot::WordPress, SiteKind::Linked);
    let static_only = site("docs", "D:/code/docs", DocRoot::Root, SiteKind::Parked);
    let pools = PhpPools::allocate(["8.4.26", "8.3.35"], 9100, 2);
    let rendered = render_for(
        &paths,
        &[
            NginxSite {
                site: &shop,
                php: Some("8.4.26"),
            },
            NginxSite {
                site: &blog,
                php: Some("8.3.35"),
            },
            NginxSite {
                site: &static_only,
                php: None,
            },
        ],
        &pools,
    );
    assert_eq!(rendered.warnings, Vec::<String>::new());
    assert_eq!(
        rendered
            .files
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "nginx.conf",
            "php_upstream.conf",
            "sites/blog.conf",
            "sites/docs.conf",
            "sites/shop.conf"
        ]
    );
    for (name, text) in &rendered.files {
        insta::assert_snapshot!(format!("file_{}", name.replace('/', "_")), normalised(text));
    }
}

#[test]
fn only_the_pools_that_a_site_uses_get_an_upstream() {
    let paths = home();
    let shop = site("shop", "D:/s", DocRoot::Root, SiteKind::Parked);
    let pools = PhpPools::allocate(["8.3.35", "8.4.26"], 9100, 2);
    let rendered = render_for(
        &paths,
        &[NginxSite {
            site: &shop,
            php: Some("8.4.26"),
        }],
        &pools,
    );
    let upstream = &rendered.files["php_upstream.conf"];
    assert!(upstream.contains("upstream php_8_4_26"), "{upstream}");
    assert!(upstream.contains("127.0.0.1:9102;") && upstream.contains("127.0.0.1:9103;"));
    assert!(!upstream.contains("php_8_3_35"));
}

#[test]
fn a_php_version_without_workers_gives_503_not_a_broken_upstream() {
    let paths = home();
    let shop = site("shop", "D:/s", DocRoot::Root, SiteKind::Parked);
    let rendered = render_for(
        &paths,
        &[NginxSite {
            site: &shop,
            php: Some("8.4.26"),
        }],
        &PhpPools::default(),
    );
    let conf = &rendered.files["sites/shop.conf"];
    assert!(conf.contains("has no workers running") && conf.contains("return 503;"));
    assert!(!conf.contains("fastcgi_pass"));
    assert!(!rendered.files["php_upstream.conf"].contains("upstream php_"));
}

#[test]
fn a_path_nginx_would_misread_keeps_its_site_out_and_the_rest_in() {
    let paths = home();
    let good = site("good", "D:/good", DocRoot::Root, SiteKind::Parked);
    let dollar = site("dollar", "D:/price$list", DocRoot::Root, SiteKind::Parked);
    let quote_in = site("quote", "D:/a\"b", DocRoot::Root, SiteKind::Parked);
    let rendered = render_for(
        &paths,
        &[
            NginxSite {
                site: &good,
                php: None,
            },
            NginxSite {
                site: &dollar,
                php: None,
            },
            NginxSite {
                site: &quote_in,
                php: None,
            },
        ],
        &PhpPools::default(),
    );
    assert!(rendered.files.contains_key("sites/good.conf"));
    assert!(!rendered.files.contains_key("sites/dollar.conf"));
    assert!(!rendered.files.contains_key("sites/quote.conf"));
    assert_eq!(rendered.warnings.len(), 2, "{:?}", rendered.warnings);
    assert!(rendered.warnings.iter().all(|w| w.contains("not served")));
    // And the main file is still there.
    assert!(rendered.files.contains_key("nginx.conf"));
}

#[test]
fn backslashes_become_slashes_and_spaces_are_fine() {
    let paths = home();
    let spaced = site(
        "spaced",
        r"D:\My Code\site",
        DocRoot::Root,
        SiteKind::Linked,
    );
    let rendered = render_for(
        &paths,
        &[NginxSite {
            site: &spaced,
            php: None,
        }],
        &PhpPools::default(),
    );
    assert!(rendered.files["sites/spaced.conf"].contains("root \"D:/My Code/site\";"));
}

#[test]
fn the_users_override_folders_are_included_inside_http_and_inside_each_site() {
    let paths = home();
    let shop = site("shop", "D:/s", DocRoot::Root, SiteKind::Parked);
    let rendered = render_for(
        &paths,
        &[NginxSite {
            site: &shop,
            php: None,
        }],
        &PhpPools::default(),
    );
    let main = normalised(&rendered.files["nginx.conf"]);
    assert!(
        main.contains("include \"<home>/etc/nginx/overrides/*.conf\";"),
        "{main}"
    );
    let conf = normalised(&rendered.files["sites/shop.conf"]);
    assert!(
        conf.contains("include \"<home>/etc/nginx/overrides/sites/shop/*.conf\";"),
        "{conf}"
    );
}

#[test]
fn the_port_is_in_the_files_that_listen_and_nowhere_else_is_it_decided() {
    let paths = home();
    let shop = site("shop", "D:/s", DocRoot::Root, SiteKind::Parked);
    let dir = nginx_dir();
    let pools = PhpPools::default();
    let sites = [NginxSite {
        site: &shop,
        php: None,
    }];
    let rendered = render(&NginxInput {
        paths: &paths,
        nginx_dir: &dir,
        http_port: 19080,
        sites: &sites,
        pools: &pools,
    });
    for name in ["nginx.conf", "sites/shop.conf"] {
        assert!(
            rendered.files[name].contains("listen 127.0.0.1:19080"),
            "{name}"
        );
    }
}

#[test]
fn the_same_input_gives_the_same_text() {
    let paths = home();
    let a = site("a", "D:/a", DocRoot::Root, SiteKind::Parked);
    let b = site("b", "D:/b", DocRoot::Root, SiteKind::Parked);
    let pools = PhpPools::allocate(["8.4.26"], 9100, 4);
    let one = render_for(
        &paths,
        &[
            NginxSite {
                site: &a,
                php: Some("8.4.26"),
            },
            NginxSite {
                site: &b,
                php: Some("8.4.26"),
            },
        ],
        &pools,
    );
    let two = render_for(
        &paths,
        &[
            NginxSite {
                site: &a,
                php: Some("8.4.26"),
            },
            NginxSite {
                site: &b,
                php: Some("8.4.26"),
            },
        ],
        &pools,
    );
    assert_eq!(one, two);
}

#[test]
fn a_site_listed_twice_is_served_once() {
    let paths = home();
    let a = site("a", "D:/a", DocRoot::Root, SiteKind::Parked);
    let rendered = render_for(
        &paths,
        &[
            NginxSite {
                site: &a,
                php: None,
            },
            NginxSite {
                site: &a,
                php: None,
            },
        ],
        &PhpPools::default(),
    );
    assert_eq!(rendered.warnings.len(), 1);
}

// ---- pools ------------------------------------------------------------------------

#[test]
fn pools_get_consecutive_ports_in_version_order_whatever_the_input_order() {
    let pools = PhpPools::allocate(["8.4.26", "8.3.35", "8.4.26"], 9100, 3);
    assert_eq!(pools.ports("8.3.35"), Some(&[9100, 9101, 9102][..]));
    assert_eq!(pools.ports("8.4.26"), Some(&[9103, 9104, 9105][..]));
    assert_eq!(pools.ports("7.4.33"), None);
    assert_eq!(PhpPools::upstream("8.4.26"), "php_8_4_26");
    assert_eq!(PhpPools::upstream("8.5.0-rc1"), "php_8_5_0_rc1");
}

#[test]
fn pools_never_go_past_the_last_port() {
    let pools = PhpPools::allocate(["1.0.0", "2.0.0"], 65534, 2);
    assert_eq!(pools.ports("1.0.0"), Some(&[65534, 65535][..]));
    assert_eq!(pools.ports("2.0.0"), None);
    let none = PhpPools::allocate(["1.0.0"], 65535, 2);
    assert_eq!(none.ports("1.0.0"), None);
    // Zero workers still means one.
    assert_eq!(
        PhpPools::allocate(["1.0.0"], 9100, 0).ports("1.0.0"),
        Some(&[9100][..])
    );
}

// ---- writing ----------------------------------------------------------------------

fn written(paths: &CrisolPaths, names: &[&str]) -> WrittenNginx {
    let sites: Vec<Site> = names
        .iter()
        .map(|n| site(n, &format!("D:/{n}"), DocRoot::Root, SiteKind::Parked))
        .collect();
    let entries: Vec<NginxSite<'_>> = sites
        .iter()
        .map(|s| NginxSite { site: s, php: None })
        .collect();
    let dir = nginx_dir();
    let pools = PhpPools::default();
    write_nginx_config(&NginxInput {
        paths,
        nginx_dir: &dir,
        http_port: 80,
        sites: &entries,
        pools: &pools,
    })
    .unwrap()
}

fn temp_home() -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    (tmp, paths)
}

#[test]
fn writing_creates_the_files_and_the_folders_nginx_needs() {
    let (_tmp, paths) = temp_home();
    let done = written(&paths, &["a", "b"]);
    assert_eq!(
        done.main,
        paths.etc_for("nginx").unwrap().join("nginx.conf")
    );
    assert_eq!(done.changed.len(), 4);
    assert!(done.removed.is_empty() && done.warnings.is_empty());
    assert!(paths.logs_for("nginx").unwrap().is_dir());
    assert!(paths.state_dir().join("nginx").is_dir());
    assert!(paths
        .etc_for("nginx")
        .unwrap()
        .join("sites")
        .join("a.conf")
        .is_file());
}

#[test]
fn writing_again_changes_nothing_and_a_new_site_changes_only_its_own_file() {
    let (_tmp, paths) = temp_home();
    written(&paths, &["a"]);
    let nginx_conf = paths.etc_for("nginx").unwrap().join("nginx.conf");
    let stamp = fs::metadata(&nginx_conf).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    assert_eq!(written(&paths, &["a"]).changed, Vec::<PathBuf>::new());
    let more = written(&paths, &["a", "b"]);
    let names: Vec<_> = more
        .changed
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["b.conf"],
        "only the new site: the others are the same text"
    );
    assert_eq!(
        fs::metadata(&nginx_conf).unwrap().modified().unwrap(),
        stamp
    );
}

#[test]
fn the_file_of_a_site_that_is_gone_is_deleted_and_nothing_else_is() {
    let (_tmp, paths) = temp_home();
    written(&paths, &["a", "b"]);
    let overrides = paths.overrides_for("nginx").unwrap();
    fs::create_dir_all(overrides.join("sites").join("b")).unwrap();
    fs::write(overrides.join("sites").join("b").join("x.conf"), "# mine").unwrap();
    fs::write(overrides.join("extra.conf"), "# mine too").unwrap();

    let done = written(&paths, &["a"]);
    assert_eq!(done.removed.len(), 1);
    assert!(done.removed[0].ends_with("b.conf"));
    let sites = paths.etc_for("nginx").unwrap().join("sites");
    assert!(sites.join("a.conf").is_file() && !sites.join("b.conf").exists());
    assert!(overrides.join("sites").join("b").join("x.conf").is_file());
    assert!(
        overrides.join("extra.conf").is_file(),
        "the user's files stay"
    );
}

#[test]
fn a_hand_edited_generated_file_is_put_back() {
    let (_tmp, paths) = temp_home();
    let done = written(&paths, &["a"]);
    fs::write(&done.main, "tampered").unwrap();
    assert_eq!(
        written(&paths, &["a"]).changed,
        std::slice::from_ref(&done.main)
    );
    assert!(fs::read_to_string(&done.main)
        .unwrap()
        .contains("Generated by Crisol"));
}

// ---- the real Nginx ---------------------------------------------------------------

/// The generated configuration is accepted by the real Nginx (`nginx -t`), and a server started
/// from it on a test port serves a static site, answers 503 for PHP without workers and closes
/// the connection for a host that is not a site.
///
/// Needs `nginx-1.28.3.zip` in `CRISOL_TEST_DOWNLOADS`. Nginx runs from a temporary home on
/// port 19080 and is stopped at the end.
///
/// ```text
/// CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-core real_nginx -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS with the official Nginx archive (Windows)"]
#[allow(clippy::too_many_lines)]
fn real_nginx_accepts_the_generated_configuration_and_serves_from_it() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::process::Command;

    use crate::catalog::Catalog;
    use crate::fetch::fake::FakeFetcher;
    use crate::install::{install, InstallOptions};

    let folder = PathBuf::from(
        std::env::var_os("CRISOL_TEST_DOWNLOADS").expect("set CRISOL_TEST_DOWNLOADS"),
    );
    let (_tmp, paths) = temp_home();
    let catalog = Catalog::embedded().unwrap();
    let package = catalog
        .packages()
        .iter()
        .find(|p| p.service == "nginx" && p.version.to_string() == "1.28.3")
        .unwrap();
    let fetcher = FakeFetcher::new();
    fetcher.serve(
        &package.url,
        fs::read(folder.join("nginx-1.28.3.zip")).unwrap(),
    );
    let installed = install(
        package,
        &paths,
        &fetcher,
        &InstallOptions::default(),
        &mut |_| {},
    )
    .unwrap();

    let docs_root = paths.home().join("www").join("docs");
    fs::create_dir_all(&docs_root).unwrap();
    fs::write(docs_root.join("index.html"), "hello-crisol").unwrap();
    fs::write(docs_root.join("info.php"), "<?php").unwrap();
    let docs = site(
        "docs",
        &docs_root.to_string_lossy(),
        DocRoot::Root,
        SiteKind::Parked,
    );
    let sites = [NginxSite {
        site: &docs,
        php: None,
    }];
    let pools = PhpPools::default();
    let done = write_nginx_config(&NginxInput {
        paths: &paths,
        nginx_dir: &installed.path,
        http_port: 19080,
        sites: &sites,
        pools: &pools,
    })
    .unwrap();
    assert_eq!(done.warnings, Vec::<String>::new());

    let nginx = |args: &[&str]| {
        Command::new(installed.path.join("nginx.exe"))
            .arg("-c")
            .arg(&done.main)
            .arg("-p")
            .arg(&installed.path)
            .args(args)
            .output()
            .unwrap()
    };
    let test = nginx(&["-t"]);
    let report = String::from_utf8_lossy(&test.stderr).into_owned();
    assert!(test.status.success(), "nginx -t said:\n{report}");
    assert!(report.contains("test is successful"), "{report}");

    let get = |host: &str, path: &str| -> String {
        let mut stream = TcpStream::connect("127.0.0.1:19080").unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut body = String::new();
        let _ = stream.read_to_string(&mut body);
        body
    };

    let mut running = Command::new(installed.path.join("nginx.exe"))
        .arg("-c")
        .arg(&done.main)
        .arg("-p")
        .arg(&installed.path)
        .spawn()
        .unwrap();
    let up = (0..50).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        TcpStream::connect("127.0.0.1:19080").is_ok()
    });
    let answers = up.then(|| {
        (
            get("docs.test", "/"),
            get("docs.test", "/info.php"),
            get("nobody.test", "/"),
        )
    });
    let _ = nginx(&["-s", "stop"]);
    let _ = running.wait();

    let (page, php, stranger) = answers.expect("nginx did not start listening");
    assert!(
        page.starts_with("HTTP/1.1 200") && page.contains("hello-crisol"),
        "{page}"
    );
    assert!(php.starts_with("HTTP/1.1 503"), "{php}");
    assert_eq!(stranger, "", "an unknown host gets the connection closed");
}
