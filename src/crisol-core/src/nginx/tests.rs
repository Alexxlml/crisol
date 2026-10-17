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
        secure: false,
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
        tls: None,
        sites,
        pools,
        mail: None,
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
        tls: None,
        sites: &sites,
        pools: &pools,
        mail: None,
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
    write_nginx_config(
        &NginxInput {
            paths,
            nginx_dir: &dir,
            http_port: 80,
            tls: None,
            sites: &entries,
            pools: &pools,
            mail: None,
        },
        &crate::paths::fake::FakePrivateDirs::new(),
    )
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
    fs::write(docs_root.join("info.php"), "<?php echo 'SECRET-SOURCE';").unwrap();
    // The longest name a site may have, to be sure the server-names hash holds it.
    let long_name = "a".repeat(63);
    let long_root = paths.home().join("www").join(&long_name);
    fs::create_dir_all(&long_root).unwrap();
    fs::write(long_root.join("index.html"), "long-name").unwrap();
    let long_site = site(
        &long_name,
        &long_root.to_string_lossy(),
        DocRoot::Root,
        SiteKind::Parked,
    );
    let docs = site(
        "docs",
        &docs_root.to_string_lossy(),
        DocRoot::Root,
        SiteKind::Parked,
    );
    let sites = [
        NginxSite {
            site: &docs,
            php: None,
        },
        NginxSite {
            site: &long_site,
            php: None,
        },
    ];
    let pools = PhpPools::default();
    let done = write_nginx_config(
        &NginxInput {
            paths: &paths,
            nginx_dir: &installed.path,
            http_port: 19080,
            tls: None,
            sites: &sites,
            pools: &pools,
            mail: None,
        },
        &crate::paths::fake::FakePrivateDirs::new(),
    )
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
            // The same file by other names on Windows: never sent as text.
            ["/info.PHP", "/info.Php", "/info.php.", "/info.php%20"]
                .map(|path| (path, get("docs.test", path))),
            get(&format!("{long_name}.test"), "/"),
        )
    });
    let _ = nginx(&["-s", "stop"]);
    let _ = running.wait();

    let (page, php, stranger, aliases, long) = answers.expect("nginx did not start listening");
    assert!(
        page.starts_with("HTTP/1.1 200") && page.contains("hello-crisol"),
        "{page}"
    );
    assert!(php.starts_with("HTTP/1.1 503"), "{php}");
    assert_eq!(stranger, "", "an unknown host gets the connection closed");
    for (path, answer) in &aliases {
        assert!(
            !answer.contains("SECRET-SOURCE") && !answer.contains("<?php"),
            "{path} sent the source: {answer}"
        );
        assert!(answer.starts_with("HTTP/1.1 503"), "{path}: {answer}");
    }
    assert!(
        long.contains("long-name"),
        "a 63-character site name: {long}"
    );
}

// ---- review of phase 5 ------------------------------------------------------------

#[test]
fn php_is_matched_without_regard_to_case_and_with_the_endings_windows_ignores() {
    // On Windows `/wp-config.PHP`, `/wp-config.php.` and `/wp-config.php%20` are the same file
    // as `wp-config.php`; if the PHP location misses them, Nginx sends the source as text.
    let paths = home();
    let blog = site("blog", "D:/blog", DocRoot::WordPress, SiteKind::Linked);
    let pools = PhpPools::default().with("8.4.26", &[9100]);
    let with_php = render_for(
        &paths,
        &[NginxSite {
            site: &blog,
            php: Some("8.4.26"),
        }],
        &pools,
    );
    let without = render_for(
        &paths,
        &[NginxSite {
            site: &blog,
            php: None,
        }],
        &PhpPools::default(),
    );
    for conf in [
        &with_php.files["sites/blog.conf"],
        &without.files["sites/blog.conf"],
    ] {
        assert!(conf.contains(r#"location ~* "\.php[. ]*$" {"#), "{conf}");
        assert!(
            !conf.contains(r"location ~ \.php$"),
            "the case-sensitive form is gone"
        );
    }
}

#[test]
fn long_site_names_fit_in_the_server_names_hash() {
    let main = &render_for(&home(), &[], &PhpPools::default()).files["nginx.conf"];
    assert!(
        main.contains("server_names_hash_bucket_size 128;"),
        "{main}"
    );
}

fn staged_input<'a>(
    paths: &'a CrisolPaths,
    dir: &'a Path,
    sites: &'a [NginxSite<'a>],
    pools: &'a PhpPools,
) -> NginxInput<'a> {
    NginxInput {
        paths,
        nginx_dir: dir,
        http_port: 80,
        tls: None,
        sites,
        pools,
        mail: None,
    }
}

#[test]
fn a_staged_configuration_has_its_own_paths_and_leaves_the_real_one_alone() {
    let (_tmp, paths) = temp_home();
    let a = site("a", "D:/a", DocRoot::Root, SiteKind::Parked);
    let dir = nginx_dir();
    let pools = PhpPools::default();
    let one = [NginxSite {
        site: &a,
        php: None,
    }];
    write_nginx_config(
        &staged_input(&paths, &dir, &one, &pools),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let real = fs::read_to_string(paths.etc_for("nginx").unwrap().join("nginx.conf")).unwrap();

    // A second site is staged: the real files do not change, the staged ones point at themselves.
    let b = site("b", "D:/b", DocRoot::Root, SiteKind::Parked);
    let two = [
        NginxSite {
            site: &a,
            php: None,
        },
        NginxSite {
            site: &b,
            php: None,
        },
    ];
    let staged = stage_nginx_config(
        &staged_input(&paths, &dir, &two, &pools),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let etc = paths.etc_for("nginx").unwrap();
    assert_eq!(staged.main, etc.join(".next").join("nginx.conf"));
    assert_eq!(
        fs::read_to_string(etc.join("nginx.conf")).unwrap(),
        real,
        "the real main file is untouched"
    );
    assert!(!etc.join("sites").join("b.conf").exists());
    assert!(etc.join(".next").join("sites").join("b.conf").is_file());

    let text = normalised(&fs::read_to_string(&staged.main).unwrap()).replace('\\', "/");
    assert!(
        text.contains("/etc/nginx/.next/php_upstream.conf"),
        "{text}"
    );
    assert!(text.contains("/etc/nginx/.next/sites/*.conf"), "{text}");
    // The user's overrides are the real ones in both.
    assert!(text.contains("/etc/nginx/overrides/*.conf"), "{text}");
    assert!(!text.contains(".next/overrides"), "{text}");
}

#[test]
fn staging_replaces_whatever_was_staged_before() {
    let (_tmp, paths) = temp_home();
    let dir = nginx_dir();
    let pools = PhpPools::default();
    let a = site("a", "D:/a", DocRoot::Root, SiteKind::Parked);
    let b = site("b", "D:/b", DocRoot::Root, SiteKind::Parked);
    stage_nginx_config(
        &staged_input(
            &paths,
            &dir,
            &[NginxSite {
                site: &a,
                php: None,
            }],
            &pools,
        ),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let staged = stage_nginx_config(
        &staged_input(
            &paths,
            &dir,
            &[NginxSite {
                site: &b,
                php: None,
            }],
            &pools,
        ),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let sites = staged.main.parent().unwrap().join("sites");
    assert!(sites.join("b.conf").is_file() && !sites.join("a.conf").exists());
}

#[test]
fn the_disk_is_compared_with_what_would_be_written() {
    let (_tmp, paths) = temp_home();
    let dir = nginx_dir();
    let pools = PhpPools::default();
    let a = site("a", "D:/a", DocRoot::Root, SiteKind::Parked);
    let b = site("b", "D:/b", DocRoot::Root, SiteKind::Parked);
    let one = [NginxSite {
        site: &a,
        php: None,
    }];
    let two = [
        NginxSite {
            site: &a,
            php: None,
        },
        NginxSite {
            site: &b,
            php: None,
        },
    ];
    assert!(
        config_differs(&staged_input(&paths, &dir, &one, &pools)).unwrap(),
        "nothing yet"
    );
    write_nginx_config(
        &staged_input(&paths, &dir, &two, &pools),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(!config_differs(&staged_input(&paths, &dir, &two, &pools)).unwrap());
    assert!(
        config_differs(&staged_input(&paths, &dir, &one, &pools)).unwrap(),
        "b is stale"
    );
    fs::write(
        paths.etc_for("nginx").unwrap().join("nginx.conf"),
        "tampered",
    )
    .unwrap();
    assert!(config_differs(&staged_input(&paths, &dir, &two, &pools)).unwrap());
}

#[test]
fn pools_keep_away_from_the_reserved_ports() {
    let mut pools = PhpPools::default();
    let reserved: BTreeSet<u16> = [9102, 9105].into_iter().collect();
    assert!(pools.ensure("8.3.35", 9100, 2, &reserved));
    assert!(pools.ensure("8.4.26", 9100, 2, &reserved));
    assert!(pools.ensure("8.5.0", 9100, 2, &reserved));
    assert_eq!(pools.ports("8.3.35"), Some(&[9100, 9101][..]));
    // 9102-9103 clashes with 9102; 9104-9105 clashes with 9105; 9106-9107 is free.
    assert_eq!(pools.ports("8.4.26"), Some(&[9106, 9107][..]));
    assert_eq!(pools.ports("8.5.0"), Some(&[9108, 9109][..]));
}

// ---- HTTPS (F6.2) ------------------------------------------------------------------

fn secured(name: &str, docroot: &str) -> Site {
    Site {
        secure: true,
        ..site(name, docroot, DocRoot::Laravel, SiteKind::Parked)
    }
}

fn tls_files() -> (PathBuf, PathBuf) {
    let paths = home();
    (paths.site_cert(), paths.site_key())
}

fn render_tls(sites: &[NginxSite<'_>], tls: Option<(u16, &(PathBuf, PathBuf))>) -> RenderedNginx {
    let paths = home();
    let dir = nginx_dir();
    let pools = PhpPools::default().with("8.4.26", &[9100]);
    render(&NginxInput {
        paths: &paths,
        nginx_dir: &dir,
        http_port: 80,
        tls: tls.map(|(https_port, (cert, key))| NginxTls {
            cert,
            key,
            https_port,
        }),
        sites,
        pools: &pools,
        mail: None,
    })
}

#[test]
fn a_secured_site_gets_a_tls_block_and_its_http_block_only_redirects() {
    let shop = secured("shop", "D:/shop/public");
    let plain = site("blog", "D:/blog", DocRoot::Root, SiteKind::Parked);
    let files = tls_files();
    let rendered = render_tls(
        &[
            NginxSite {
                site: &shop,
                php: Some("8.4.26"),
            },
            NginxSite {
                site: &plain,
                php: None,
            },
        ],
        Some((443, &files)),
    );
    assert_eq!(rendered.warnings, Vec::<String>::new());
    for (name, text) in &rendered.files {
        if matches!(
            name.as_str(),
            "nginx.conf" | "sites/shop.conf" | "sites/blog.conf"
        ) {
            insta::assert_snapshot!(format!("tls_{}", name.replace('/', "_")), normalised(text));
        }
    }
}

#[test]
fn the_redirect_names_the_port_unless_it_is_443() {
    let shop = secured("shop", "D:/shop");
    let files = tls_files();
    let entry = [NginxSite {
        site: &shop,
        php: None,
    }];
    let standard = render_tls(&entry, Some((443, &files)));
    assert!(standard.files["sites/shop.conf"].contains("return 302 https://$host$request_uri;"));
    let custom = render_tls(&entry, Some((19443, &files)));
    let conf = &custom.files["sites/shop.conf"];
    assert!(
        conf.contains("return 302 https://$host:19443$request_uri;"),
        "{conf}"
    );
    assert!(conf.contains("listen 127.0.0.1:19443 ssl;"), "{conf}");
}

#[test]
fn the_tls_block_has_the_certificate_the_protocols_and_tells_php_it_is_https() {
    let shop = secured("shop", "D:/shop");
    let files = tls_files();
    let conf = render_tls(
        &[NginxSite {
            site: &shop,
            php: Some("8.4.26"),
        }],
        Some((443, &files)),
    )
    .files["sites/shop.conf"]
        .clone();
    let conf = normalised(&conf);
    for line in [
        "http2 on;",
        "ssl_certificate \"<home>/certs/sites.pem\";",
        "ssl_certificate_key \"<home>/certs/sites.key\";",
        "ssl_protocols TLSv1.2 TLSv1.3;",
        "fastcgi_param HTTPS on;",
    ] {
        assert!(conf.contains(line), "{line} in\n{conf}");
    }
    // The plain HTTP block comes first and only redirects: no root, no PHP.
    let (redirect, secure) = conf.split_once("# shop.test: parked site").unwrap();
    assert!(
        redirect.contains("return 302") && !redirect.contains("root"),
        "{redirect}"
    );
    assert!(
        !redirect.contains("fastcgi") && secure.contains("root"),
        "{conf}"
    );
}

#[test]
fn only_a_secured_site_makes_the_server_listen_with_tls_and_unknown_hosts_are_refused() {
    let files = tls_files();
    let plain = site("blog", "D:/blog", DocRoot::Root, SiteKind::Parked);
    let none = render_tls(
        &[NginxSite {
            site: &plain,
            php: None,
        }],
        Some((443, &files)),
    );
    let main = &none.files["nginx.conf"];
    assert!(
        !main.contains("ssl"),
        "no secured site, no TLS listener: {main}"
    );

    let shop = secured("shop", "D:/shop");
    let some = render_tls(
        &[NginxSite {
            site: &shop,
            php: None,
        }],
        Some((443, &files)),
    );
    let main = &some.files["nginx.conf"];
    assert!(
        main.contains("listen 127.0.0.1:443 ssl default_server;"),
        "{main}"
    );
    assert!(main.contains("ssl_reject_handshake on;"), "{main}");
}

#[test]
fn a_secured_site_without_a_certificate_is_served_over_http_with_a_warning() {
    let shop = secured("shop", "D:/shop");
    let rendered = render_tls(
        &[NginxSite {
            site: &shop,
            php: None,
        }],
        None,
    );
    assert_eq!(rendered.warnings.len(), 1, "{:?}", rendered.warnings);
    assert!(rendered.warnings[0].contains("shop.test"));
    let conf = &rendered.files["sites/shop.conf"];
    assert!(
        conf.contains("listen 127.0.0.1:80;") && !conf.contains("ssl"),
        "{conf}"
    );
}

#[test]
fn a_site_that_is_not_secured_is_unchanged_whether_or_not_there_is_a_certificate() {
    let plain = site("blog", "D:/blog", DocRoot::WordPress, SiteKind::Linked);
    let files = tls_files();
    let entry = [NginxSite {
        site: &plain,
        php: None,
    }];
    assert_eq!(
        render_tls(&entry, Some((443, &files))).files["sites/blog.conf"],
        render_tls(&entry, None).files["sites/blog.conf"]
    );
}

#[test]
fn a_certificate_path_nginx_would_misread_keeps_the_secured_site_out() {
    let shop = secured("shop", "D:/shop");
    let bad = (
        PathBuf::from("D:/odd$dir/sites.pem"),
        PathBuf::from("D:/k.key"),
    );
    let rendered = render_tls(
        &[NginxSite {
            site: &shop,
            php: None,
        }],
        Some((443, &bad)),
    );
    assert!(!rendered.files.contains_key("sites/shop.conf"));
    assert!(
        rendered.warnings.iter().any(|w| w.contains("not served")),
        "{:?}",
        rendered.warnings
    );
}

// ---- review of phase 7, finding 2: Nginx listens on [::1] too ------------------------------

/// Every `listen` on IPv4 loopback has its twin on `[::1]` with the same port and options, in
/// every file, and nothing listens anywhere else.
fn assert_listens_on_both_loopbacks(text: &str) {
    let listens: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("listen "))
        .collect();
    assert_ne!(listens.len(), 0);
    for line in &listens {
        assert!(
            line.starts_with("listen 127.0.0.1:") || line.starts_with("listen [::1]:"),
            "{line}"
        );
    }
    for line in listens
        .iter()
        .filter(|l| l.starts_with("listen 127.0.0.1:"))
    {
        let twin = line.replacen("127.0.0.1", "[::1]", 1);
        assert!(
            listens.contains(&twin.as_str()),
            "{line} has no twin\n{text}"
        );
    }
    let (v4, v6) = (
        listens.iter().filter(|l| l.contains("127.0.0.1")).count(),
        listens.iter().filter(|l| l.contains("[::1]")).count(),
    );
    assert_eq!(v4, v6, "{text}");
}

#[test]
fn every_listen_is_duplicated_on_ipv6_loopback_including_the_default_servers_and_tls() {
    let shop = secured("shop", "D:/shop/public");
    let plain = site("blog", "D:/blog", DocRoot::Root, SiteKind::Parked);
    let files = tls_files();
    for port in [443, 19443] {
        let rendered = render_tls(
            &[
                NginxSite {
                    site: &shop,
                    php: Some("8.4.26"),
                },
                NginxSite {
                    site: &plain,
                    php: None,
                },
            ],
            Some((port, &files)),
        );
        for name in ["nginx.conf", "sites/shop.conf", "sites/blog.conf"] {
            assert_listens_on_both_loopbacks(&rendered.files[name]);
        }
        let main = &rendered.files["nginx.conf"];
        assert!(
            main.contains(&format!("listen [::1]:{port} ssl default_server;")),
            "{main}"
        );
        assert!(main.contains("default_server;\n        server_name _;\n        return 444;"));
        // The secured site's HTTPS block takes the HTTPS port, its redirect the HTTP one.
        let conf = &rendered.files["sites/shop.conf"];
        assert!(
            conf.contains(&format!("listen [::1]:{port} ssl;")),
            "{conf}"
        );
        assert!(conf.contains("listen [::1]:80;"), "{conf}");
    }
}

#[test]
fn mailpit_is_a_name_of_nginx_that_forwards_with_the_name_the_browser_used() {
    let paths = home();
    let dir = nginx_dir();
    let pools = PhpPools::allocate([] as [&str; 0], 9100, 2);
    let route = MailRoute {
        domain: "mail.test".to_owned(),
        port: 8025,
    };
    let rendered = render(&NginxInput {
        paths: &paths,
        nginx_dir: &dir,
        http_port: 8080,
        tls: None,
        sites: &[],
        pools: &pools,
        mail: Some(&route),
    });
    let text = normalised(&rendered.files["sites/_mail.conf"]);
    assert!(text.contains("server_name mail.test;"), "{text}");
    assert!(text.contains("listen 127.0.0.1:8080;"), "{text}");
    assert!(text.contains("proxy_pass http://127.0.0.1:8025;"), "{text}");
    // The name is passed on (Mailpit answers only to it) and the live page (a WebSocket) works.
    assert!(text.contains("proxy_set_header Host $host;"), "{text}");
    assert!(
        text.contains("proxy_set_header Upgrade $http_upgrade;"),
        "{text}"
    );
    assert_eq!(rendered.warnings, Vec::<String>::new());
    assert!(
        rendered.files["nginx.conf"].contains("include "),
        "the main file includes sites/*.conf"
    );

    // Without Mailpit there is no such name: Nginx closes the connection for it.
    let without = render_for(&paths, &[], &pools);
    assert!(!without.files.contains_key("sites/_mail.conf"));

    // A name that is not a plain host name is not written.
    let odd = MailRoute {
        domain: "mail.test; evil".to_owned(),
        port: 8025,
    };
    let refused = render(&NginxInput {
        paths: &paths,
        nginx_dir: &dir,
        http_port: 80,
        tls: None,
        sites: &[],
        pools: &pools,
        mail: Some(&odd),
    });
    assert!(!refused.files.contains_key("sites/_mail.conf"));
    assert_eq!(refused.warnings.len(), 1, "{:?}", refused.warnings);
}
