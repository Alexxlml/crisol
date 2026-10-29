use super::*;
use std::collections::HashSet;

const SIGNED_CATALOG: &str = include_str!("testdata/signed-catalog.toml");
const SIGNATURE: &str = include_str!("testdata/test.minisig");
const PUBLIC_KEY: &str = include_str!("testdata/test.pub");

const WINDOWS: Target = Target {
    os: Os::Windows,
    arch: Arch::X86_64,
};
const LINUX: Target = Target {
    os: Os::Linux,
    arch: Arch::X86_64,
};

/// One well-formed entry; tests tweak a single field at a time. The hashes are fake.
fn entry(service: &str, version: &str) -> String {
    format!(
        "[[package]]\nservice = \"{service}\"\nversion = \"{version}\"\nos = \"windows\"\narch = \"x86_64\"\n\
         url = \"https://example.invalid/{service}-{version}.zip\"\n\
         sha256 = \"{}\"\nformat = \"zip\"\nexecutable = \"{service}.exe\"\n\n",
        "ab".repeat(32)
    )
}

fn catalog(entries: &[String]) -> String {
    format!("schema = 1\nserial = 3\n\n{}", entries.concat())
}

fn sample() -> Catalog {
    let linux = entry("php", "8.4.2").replace("windows", "linux");
    Catalog::parse(&catalog(&[
        entry("php", "8.3.12"),
        entry("php", "8.4.2"),
        entry("php", "8.4.10"),
        entry("php", "8.5.0-rc1"),
        linux,
        entry("nginx", "1.28.2"),
        entry("node", "22.11.0"),
    ]))
    .expect("the sample is valid")
}

fn invalid_field(text: &str) -> &'static str {
    match Catalog::parse(text) {
        Err(CatalogError::Invalid { field, .. }) => field,
        other => panic!("expected an invalid entry, got {other:?}"),
    }
}

// ---- the built-in catalog -------------------------------------------------------

#[test]
fn the_built_in_catalog_is_valid_and_has_a_serial() {
    let catalog = Catalog::embedded().expect("the built-in catalog must always parse");
    assert!(catalog.serial() >= 1);
    assert_eq!(catalog.warnings(), []);
    // F3.2 fills it in; every entry it ever gets is validated by this very test.
    for package in catalog.packages() {
        assert!(package.url.starts_with("https://"), "{}", package.label());
        assert_eq!(package.sha256.len(), 64, "{}", package.label());
    }
}

// ---- parsing --------------------------------------------------------------------

#[test]
fn a_package_is_read_with_every_field() {
    let text = "schema = 1\nserial = 9\ngenerated = \"2026-10-14\"\n\n[[package]]\n\
        service = \"nginx\"\nversion = \"1.28.2\"\nos = \"windows\"\narch = \"x86_64\"\n\
        url = \"https://nginx.org/download/nginx-1.28.2.zip\"\n\
        sha256 = \"ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD\"\n\
        format = \"zip\"\nroot = \"nginx-1.28.2\"\nexecutable = \"nginx.exe\"\nsize = 1802240\n";
    let catalog = Catalog::parse(text).unwrap();
    assert_eq!(
        (catalog.serial(), catalog.generated()),
        (9, Some("2026-10-14"))
    );
    let package = &catalog.packages()[0];
    assert_eq!(package.service, "nginx");
    assert_eq!(package.version.to_string(), "1.28.2");
    assert_eq!(package.target, WINDOWS);
    assert_eq!(package.url, "https://nginx.org/download/nginx-1.28.2.zip");
    assert_eq!(
        package.sha256, "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "lowercased"
    );
    assert_eq!(package.format, ArchiveFormat::Zip);
    assert_eq!(package.root.as_deref(), Some("nginx-1.28.2"));
    assert_eq!(package.executable, "nginx.exe");
    assert_eq!(package.size, Some(1_802_240));
    assert_eq!(package.host(), "nginx.org");
    assert_eq!(package.label(), "nginx 1.28.2 (windows-x86_64)");
}

#[test]
fn every_archive_format_is_understood() {
    for (text, format) in [
        ("zip", ArchiveFormat::Zip),
        ("tar.gz", ArchiveFormat::TarGz),
        ("tar.xz", ArchiveFormat::TarXz),
        ("file", ArchiveFormat::File),
    ] {
        let entry =
            entry("composer", "2.8.1").replace("format = \"zip\"", &format!("format = \"{text}\""));
        let catalog = Catalog::parse(&catalog(&[entry])).unwrap();
        assert_eq!(catalog.packages()[0].format, format);
    }
    let bad = entry("x", "1").replace("format = \"zip\"", "format = \"rar\"");
    assert!(matches!(
        Catalog::parse(&catalog(&[bad])),
        Err(CatalogError::Parse(_))
    ));
}

#[test]
fn a_catalog_without_packages_is_valid() {
    let catalog = Catalog::parse("schema = 1\nserial = 1\n").unwrap();
    assert_eq!(catalog.packages(), []);
    assert_eq!(catalog.services(), Vec::<&str>::new());
}

#[test]
fn only_schema_one_is_supported() {
    for schema in [0, 2, 99] {
        let err = Catalog::parse(&format!("schema = {schema}\nserial = 1\n")).unwrap_err();
        assert!(
            matches!(err, CatalogError::UnsupportedSchema(s) if s == schema),
            "{err}"
        );
    }
    assert!(
        matches!(Catalog::parse("serial = 1\n"), Err(CatalogError::Parse(_))),
        "schema is required"
    );
    assert!(
        matches!(Catalog::parse("schema = 1\n"), Err(CatalogError::Parse(_))),
        "serial is required"
    );
    assert!(matches!(
        Catalog::parse("not toml ="),
        Err(CatalogError::Parse(_))
    ));
}

#[test]
fn unknown_fields_are_warnings_not_errors() {
    let text = format!(
        "schema = 1\nserial = 1\nmotd = \"hi\"\n\n{}",
        entry("php", "8.4.2")
    )
    .replace("format = \"zip\"", "format = \"zip\"\nmirror = \"x\"");
    let catalog = Catalog::parse(&text).unwrap();
    assert_eq!(catalog.packages().len(), 1);
    let warned: Vec<String> = catalog.warnings().iter().map(ToString::to_string).collect();
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert!(warned.iter().any(|w| w.contains("motd")));
    assert!(warned.iter().any(|w| w.contains("mirror")));
}

#[test]
fn a_leading_byte_order_mark_is_ignored() {
    let text = format!("\u{feff}{}", catalog(&[entry("php", "8.4.2")]));
    assert_eq!(Catalog::parse(&text).unwrap().packages().len(), 1);
}

// ---- validation -----------------------------------------------------------------

#[test]
fn urls_must_be_plain_https() {
    let cases = [
        "http://example.invalid/a.zip",
        "ftp://example.invalid/a.zip",
        "file:///C:/a.zip",
        "//example.invalid/a.zip",
        "https://",
        "https:///a.zip",
        "https://user:pw@example.invalid/a.zip",
        "https://windows.php.net@evil.invalid/a.zip",
        "https://exa mple.invalid/a.zip",
        "https://example.invalid/a b.zip",
        "https://-bad.invalid/a.zip",
        "https://bad..invalid/a.zip",
        "https://.invalid/a.zip",
        "https://exam_ple.invalid/a.zip",
        "",
    ];
    for url in cases {
        let text =
            catalog(&[entry("php", "8.4.2").replace("https://example.invalid/php-8.4.2.zip", url)]);
        assert_eq!(invalid_field(&text), "url", "`{url}` must be rejected");
    }
    for url in [
        "https://example.invalid:8443/a.zip",
        "https://windows.php.net/downloads/a.zip?x=1#y",
    ] {
        let text =
            catalog(&[entry("php", "8.4.2").replace("https://example.invalid/php-8.4.2.zip", url)]);
        assert!(Catalog::parse(&text).is_ok(), "`{url}` is fine");
    }
}

#[test]
fn hashes_must_be_sixty_four_hex_digits() {
    let good = "ab".repeat(32);
    for hash in [
        "",
        "abc",
        &good[..63],
        &format!("{good}0"),
        &format!("{}g", &good[..63]),
        &format!(" {}", &good[..63]),
        &format!("sha256:{}", &good[..57]),
    ] {
        let text = catalog(&[entry("php", "8.4.2").replace(&good, hash)]);
        assert_eq!(invalid_field(&text), "sha256", "`{hash}` must be rejected");
    }
}

#[test]
fn paths_inside_the_archive_cannot_escape() {
    for path in [
        "",
        "/php.exe",
        "\\php.exe",
        "../php.exe",
        "bin/../../php.exe",
        "C:/php.exe",
        "bin//php.exe",
        "./php.exe",
        "a\\..\\b",
    ] {
        let text = catalog(&[entry("php", "8.4.2").replace(
            "executable = \"php.exe\"",
            &format!("executable = {path:?}"),
        )]);
        assert_eq!(
            invalid_field(&text),
            "executable",
            "`{path}` must be rejected"
        );

        let with_root = entry("php", "8.4.2").replace(
            "format = \"zip\"",
            &format!("format = \"zip\"\nroot = {path:?}"),
        );
        assert_eq!(
            invalid_field(&catalog(&[with_root])),
            "root",
            "`{path}` must be rejected as root"
        );
    }
}

#[test]
fn paths_are_normalized_to_forward_slashes() {
    let text = catalog(&[entry("mariadb", "11.4.4")
        .replace(
            "executable = \"mariadb.exe\"",
            "executable = \"bin\\\\mariadbd.exe\"",
        )
        .replace(
            "format = \"zip\"",
            "format = \"zip\"\nroot = \"mariadb-11.4.4-winx64\"",
        )]);
    let package = Catalog::parse(&text).unwrap().packages()[0].clone();
    assert_eq!(package.executable, "bin/mariadbd.exe");
}

#[test]
fn service_version_and_size_are_checked() {
    let upper = entry("PHP", "8.4.2");
    assert_eq!(invalid_field(&catalog(&[upper])), "service");
    for service in ["", "a b", "a/b", "../x", "ph.p"] {
        let text =
            catalog(&[entry("php", "8.4.2")
                .replace("service = \"php\"", &format!("service = {service:?}"))]);
        assert_eq!(invalid_field(&text), "service", "`{service}`");
    }
    for version in ["", "latest", "8..4", "v8.4", "8.4.x"] {
        let text =
            catalog(&[entry("php", "8.4.2")
                .replace("version = \"8.4.2\"", &format!("version = {version:?}"))]);
        assert_eq!(invalid_field(&text), "version", "`{version}`");
    }
    let zero = entry("php", "8.4.2").replace("format = \"zip\"", "format = \"zip\"\nsize = 0");
    assert_eq!(invalid_field(&catalog(&[zero])), "size");
}

#[test]
fn an_invalid_entry_is_named_in_the_error() {
    let text = catalog(&[
        entry("php", "8.4.2"),
        entry("nginx", "1.28.2").replace("https://", "http://"),
    ]);
    let message = Catalog::parse(&text).unwrap_err().to_string();
    assert!(
        message.contains("#2 (nginx 1.28.2)") && message.contains("`url`"),
        "{message}"
    );
}

#[test]
fn the_same_build_twice_is_refused_even_with_a_different_spelling() {
    let twice = catalog(&[entry("php", "8.4.2"), entry("php", "8.4.2")]);
    assert!(matches!(
        Catalog::parse(&twice),
        Err(CatalogError::Duplicate(_))
    ));
    let spelled = catalog(&[entry("php", "8.4"), entry("php", "8.4.0")]);
    assert!(
        matches!(Catalog::parse(&spelled), Err(CatalogError::Duplicate(_))),
        "8.4 is 8.4.0"
    );

    let other_platform = entry("php", "8.4.2").replace("windows", "linux");
    assert!(Catalog::parse(&catalog(&[entry("php", "8.4.2"), other_platform])).is_ok());
}

// ---- queries --------------------------------------------------------------------

#[test]
fn services_and_available_versions_are_sorted() {
    let catalog = sample();
    assert_eq!(catalog.services(), ["nginx", "node", "php"]);
    let versions: Vec<String> = catalog
        .available("php", WINDOWS)
        .iter()
        .map(|p| p.version.to_string())
        .collect();
    assert_eq!(
        versions,
        ["8.3.12", "8.4.2", "8.4.10", "8.5.0-rc1"],
        "numeric order, oldest first"
    );
    assert_eq!(catalog.available("php", LINUX).len(), 1);
    let mac = Target {
        os: Os::Macos,
        arch: Arch::Aarch64,
    };
    assert_eq!(catalog.available("php", mac).len(), 0);
    assert_eq!(catalog.available("ruby", WINDOWS).len(), 0);
}

#[test]
fn find_picks_the_highest_version_that_matches_the_request() {
    let catalog = sample();
    let version = |request: &str| {
        catalog
            .find("php", request, WINDOWS)
            .unwrap()
            .version
            .to_string()
    };
    assert_eq!(version("8.4"), "8.4.10", "8.4.10 beats 8.4.2 numerically");
    assert_eq!(version("8.3"), "8.3.12");
    assert_eq!(
        version("8"),
        "8.4.10",
        "a release wins over the 8.5 release candidate"
    );
    assert_eq!(version("8.4.2"), "8.4.2");
    assert_eq!(
        version("8.5.0-rc1"),
        "8.5.0-rc1",
        "a pre-release only when asked for exactly"
    );
}

#[test]
fn find_respects_the_platform() {
    let catalog = sample();
    assert_eq!(
        catalog
            .find("php", "8.4", LINUX)
            .unwrap()
            .version
            .to_string(),
        "8.4.2"
    );
    assert!(catalog.find("nginx", "1", LINUX).is_err());
}

#[test]
fn latest_ignores_prereleases() {
    let catalog = sample();
    assert_eq!(
        catalog.latest("php", WINDOWS).unwrap().version.to_string(),
        "8.4.10"
    );
    assert_eq!(
        catalog.latest("node", WINDOWS).unwrap().version.to_string(),
        "22.11.0"
    );
}

#[test]
fn a_missing_version_gives_a_readable_error() {
    let catalog = sample();
    let err = catalog.find("php", "7.4", WINDOWS).unwrap_err();
    assert_eq!(
        err.to_string(),
        "no php 7.4 for windows-x86_64 in the catalog"
    );
    let err = catalog.latest("ruby", WINDOWS).unwrap_err();
    assert_eq!(
        err.to_string(),
        "no ruby latest for windows-x86_64 in the catalog"
    );
    assert!(matches!(
        catalog.find("php", "8.x", WINDOWS),
        Err(CatalogError::BadRequest(_))
    ));
    // Only a pre-release exists: latest has nothing stable to offer.
    let only_rc = Catalog::parse(&catalog_text_with(&[entry("php", "8.5.0-rc1")])).unwrap();
    assert!(only_rc.latest("php", WINDOWS).is_err());
}

fn catalog_text_with(entries: &[String]) -> String {
    catalog(entries)
}

#[test]
fn the_current_platform_is_recognised() {
    let target = Target::current().expect("tests run on a supported platform");
    if cfg!(windows) {
        assert_eq!(target.os, Os::Windows);
    }
    assert_eq!(WINDOWS.to_string(), "windows-x86_64");
    assert_eq!(
        Target {
            os: Os::Macos,
            arch: Arch::Aarch64
        }
        .to_string(),
        "macos-aarch64"
    );
}

// ---- trust ----------------------------------------------------------------------

#[test]
fn a_correctly_signed_catalog_is_accepted() {
    let catalog = Catalog::parse_signed(SIGNED_CATALOG, SIGNATURE, PUBLIC_KEY).unwrap();
    assert_eq!(catalog.serial(), 7);
    assert_eq!(catalog.packages()[0].service, "php");
}

#[test]
fn the_bare_base64_key_works_too() {
    let bare = PUBLIC_KEY.lines().nth(1).unwrap();
    assert!(Catalog::parse_signed(SIGNED_CATALOG, SIGNATURE, bare).is_ok());
}

#[test]
fn any_change_to_a_signed_catalog_is_detected() {
    let tampered = [
        SIGNED_CATALOG.replace("8.4.21", "8.4.22"),
        SIGNED_CATALOG.replace("example.invalid", "evil.invalid"),
        SIGNED_CATALOG.replace("serial = 7", "serial = 8"),
        SIGNED_CATALOG.replace("aaaa", "bbbb"),
        format!("{SIGNED_CATALOG} "),
        format!("{SIGNED_CATALOG}\n"),
        SIGNED_CATALOG.replace('\n', "\r\n"),
        String::new(),
    ];
    for text in tampered {
        let err = Catalog::parse_signed(&text, SIGNATURE, PUBLIC_KEY).unwrap_err();
        assert!(matches!(err, CatalogError::Signature(_)), "{err}");
    }
}

#[test]
fn a_broken_signature_or_key_is_rejected_without_parsing() {
    let mangled = SIGNATURE.replacen("RUQG", "RUQH", 1);
    for (signature, key) in [
        (mangled.as_str(), PUBLIC_KEY),
        ("", PUBLIC_KEY),
        ("not a signature", PUBLIC_KEY),
        (SIGNATURE, ""),
        (SIGNATURE, "not a key"),
    ] {
        let err = Catalog::parse_signed(SIGNED_CATALOG, signature, key).unwrap_err();
        assert!(matches!(err, CatalogError::Signature(_)), "{err}");
    }
    // A valid signature does not rescue text that is not a catalog: signature first, then
    // parsing. Here the signature is wrong, so the garbage never gets parsed at all.
    let err = Catalog::parse_signed("schema = 99", SIGNATURE, PUBLIC_KEY).unwrap_err();
    assert!(matches!(err, CatalogError::Signature(_)), "{err}");
}

#[test]
fn without_a_configured_key_only_the_built_in_catalog_is_usable() {
    let err = Catalog::parse_signed_with(SIGNED_CATALOG, SIGNATURE, None).unwrap_err();
    assert!(matches!(err, CatalogError::NoTrustedKey), "{err}");
}

#[test]
fn the_project_public_key_is_a_valid_minisign_key() {
    let key = CATALOG_PUBLIC_KEY.expect("the project key is configured");
    assert!(
        minisign_verify::PublicKey::from_base64(key).is_ok(),
        "{key}"
    );
    assert!(key.starts_with("RW") && !key.contains(char::is_whitespace));
}

#[test]
fn a_catalog_signed_by_any_other_key_is_not_trusted() {
    // The fixture is signed by a disposable key, not by the project key.
    let err = Catalog::load_update(SIGNED_CATALOG, SIGNATURE, 0).unwrap_err();
    assert!(matches!(err, CatalogError::Signature(_)), "{err}");
}

#[test]
fn load_update_is_the_door_for_outside_catalogs_and_checks_signature_and_serial() {
    // The test catalog is correctly signed by the test key; its serial is whatever it says.
    let serial = Catalog::parse_signed(SIGNED_CATALOG, SIGNATURE, PUBLIC_KEY)
        .unwrap()
        .serial();

    let accepted = Catalog::load_update_with(SIGNED_CATALOG, SIGNATURE, Some(PUBLIC_KEY), serial);
    assert!(accepted.is_ok(), "same serial as the highest seen is fine");

    let replay = Catalog::load_update_with(SIGNED_CATALOG, SIGNATURE, Some(PUBLIC_KEY), serial + 1)
        .unwrap_err();
    assert!(
        matches!(replay, CatalogError::Rollback { offered, current } if offered == serial && current == serial + 1),
        "{replay}"
    );
    assert_eq!(
        replay.to_string(),
        format!(
            "catalog serial {serial} is older than the one in use ({})",
            serial + 1
        )
    );

    // (The fixture's serial is fixed and signed; the built-in catalog moves past it with every
    // new entry, so it is not compared with it.)

    let tampered = SIGNED_CATALOG.replace("serial", "serial ");
    let forged = Catalog::load_update_with(&tampered, SIGNATURE, Some(PUBLIC_KEY), 0);
    assert!(matches!(forged, Err(CatalogError::Signature(_))));

    let no_key = Catalog::load_update_with(SIGNED_CATALOG, SIGNATURE, None, 0);
    assert!(matches!(no_key, Err(CatalogError::NoTrustedKey)));
}

// ---- the contents of the built-in catalog (F3.2) --------------------------------

/// The only sites a built-in entry may download from.
const OFFICIAL_HOSTS: [&str; 10] = [
    "windows.php.net",
    "nginx.org",
    "nodejs.org",
    "getcomposer.org",
    "curl.se",
    "archive.mariadb.org",
    "cdn.mysql.com",
    "get.enterprisedb.com",
    "github.com",
    "fastdl.mongodb.org",
];

fn built_in() -> Catalog {
    Catalog::embedded().expect("the built-in catalog must parse")
}

#[test]
fn every_built_in_package_comes_from_an_official_host() {
    for package in built_in().packages() {
        assert!(
            OFFICIAL_HOSTS.contains(&package.host()),
            "{} is downloaded from {}, which is not an official source",
            package.label(),
            package.host()
        );
    }
}

#[test]
fn the_built_in_catalog_offers_the_planned_tools_for_windows() {
    let catalog = built_in();
    assert_eq!(
        catalog.services(),
        [
            "cacert",
            "composer",
            "garnet",
            "mailpit",
            "mariadb",
            "meilisearch",
            "memcached",
            "mongodb",
            "mysql",
            "nginx",
            "node",
            "php",
            "postgresql",
            "redis"
        ]
    );
    assert!(catalog.packages().iter().all(|p| p.target == WINDOWS));
    assert!(catalog.serial() >= 3);

    let resolve = |service: &str, request: &str| {
        catalog
            .find(service, request, WINDOWS)
            .unwrap()
            .version
            .to_string()
    };
    // The builds that fix PHP2Shell (CVE-2026-109618, CVE-2026-109619); the ones before are gone.
    assert_eq!(resolve("php", "8.3"), "8.3.36");
    assert_eq!(resolve("php", "8.4"), "8.4.27");
    assert_eq!(resolve("php", "8.5"), "8.5.12");
    assert_eq!(resolve("php", "8"), "8.5.12");
    for vulnerable in ["8.3.35", "8.4.26", "8.5.11"] {
        assert!(
            catalog.find("php", vulnerable, WINDOWS).is_err(),
            "{vulnerable}"
        );
    }
    assert_eq!(resolve("nginx", "1.28"), "1.28.3");
    assert_eq!(resolve("nginx", "1.30"), "1.30.5");
    assert_eq!(resolve("node", "22"), "22.23.3");
    assert_eq!(resolve("node", "24"), "24.22.0");
    assert_eq!(
        catalog
            .latest("composer", WINDOWS)
            .unwrap()
            .version
            .to_string(),
        "2.10.3"
    );
    // The only unsupported PHP is the one kept for old projects (ADR-0012).
    assert_eq!(resolve("php", "7.4"), "7.4.33");
    assert!(
        catalog.find("php", "8.2", WINDOWS).is_err()
            && catalog.find("php", "7.3", WINDOWS).is_err(),
        "other unsupported lines stay out"
    );
}

#[test]
fn built_in_archives_are_unpacked_the_way_each_provider_packages_them() {
    for package in built_in().packages() {
        let version = package.version.to_string();
        let (format, executable, root) = match package.service.as_str() {
            // PHP puts its files at the top of the zip.
            "php" => (ArchiveFormat::Zip, "php.exe", None),
            "nginx" => (
                ArchiveFormat::Zip,
                "nginx.exe",
                Some(format!("nginx-{version}")),
            ),
            "node" => (
                ArchiveFormat::Zip,
                "node.exe",
                Some(format!("node-v{version}-win-x64")),
            ),
            "composer" => (ArchiveFormat::File, "composer.phar", None),
            // Mozilla's CA bundle is one file (ADR-0011).
            "cacert" => (ArchiveFormat::File, "cacert.pem", None),
            // The databases: a folder named after the build, the server in `bin/`.
            "mariadb" => (
                ArchiveFormat::Zip,
                "bin/mariadbd.exe",
                Some(format!("mariadb-{version}-winx64")),
            ),
            "mailpit" => (ArchiveFormat::Zip, "mailpit.exe", None),
            // Redis for Windows (tporadowski) and Garnet: the files at the top of the zip, or
            // under the folder of the runtime it was built for.
            "redis" => (ArchiveFormat::Zip, "redis-server.exe", None),
            "garnet" => (ArchiveFormat::Zip, "net10.0/GarnetServer.exe", None),
            // The third-party build of Memcached (ADR-0017): one folder inside the zip.
            "memcached" => (
                ArchiveFormat::Zip,
                "bin/memcached.exe",
                Some("memcached-1.6.8-win64-mingw".to_owned()),
            ),
            "meilisearch" => (ArchiveFormat::File, "meilisearch-windows-amd64.exe", None),
            "mongodb" => (
                ArchiveFormat::Zip,
                "bin/mongod.exe",
                Some(format!("mongodb-win32-x86_64-windows-{version}")),
            ),
            "postgresql" => (
                ArchiveFormat::Zip,
                "bin/postgres.exe",
                Some("pgsql".to_owned()),
            ),
            "mysql" => (
                ArchiveFormat::Zip,
                "bin/mysqld.exe",
                Some(format!("mysql-{version}-winx64")),
            ),
            other => panic!("unexpected service {other}"),
        };
        assert_eq!(package.format, format, "{}", package.label());
        assert_eq!(package.executable, executable, "{}", package.label());
        assert_eq!(package.root, root, "{}", package.label());
    }
}

#[test]
fn built_in_php_urls_use_the_permanent_archives_folder() {
    // releases/ loses a version as soon as the next patch release appears; archives/ keeps all.
    for package in built_in().packages().iter().filter(|p| p.service == "php") {
        assert!(
            package
                .url
                .starts_with("https://windows.php.net/downloads/releases/archives/"),
            "{}",
            package.url
        );
        assert!(
            package.url.contains("-nts-"),
            "Crisol runs PHP through php-cgi: non-thread-safe"
        );
        assert!(package.url.ends_with("-x64.zip"), "{}", package.url);
    }
}

#[test]
fn the_ca_bundle_is_pinned_to_a_dated_url_and_its_version_is_that_date() {
    // `cacert.pem` without a date changes under our feet and would break the hash; the dated
    // copy never changes. The version is the date without leading zeros.
    let catalog = built_in();
    let bundle = catalog.latest("cacert", WINDOWS).unwrap();
    let date = bundle
        .url
        .strip_prefix("https://curl.se/ca/cacert-")
        .and_then(|rest| rest.strip_suffix(".pem"))
        .expect("a dated curl.se URL");
    let numbers: Vec<u64> = date.split('-').map(|n| n.parse().unwrap()).collect();
    assert_eq!(numbers.len(), 3, "{date}");
    assert_eq!(bundle.version.parts(), numbers.as_slice());
}

#[test]
fn built_in_packages_carry_a_realistic_size_and_distinct_hashes() {
    let catalog = built_in();
    let mut hashes = HashSet::new();
    for package in catalog.packages() {
        let size = package
            .size
            .expect("every built-in package states its size");
        // The CA bundle is a text file of a couple of hundred kilobytes; programs are bigger.
        let least = if package.service == "cacert" {
            100_000
        } else {
            1_000_000
        };
        assert!(size > least, "{} is {size} bytes", package.label());
        assert!(
            hashes.insert(package.sha256.clone()),
            "{} repeats a hash",
            package.label()
        );
    }
}

// ---- the recommended line and end-of-life versions (ADR-0012) -------------------------

#[test]
fn the_built_in_catalog_recommends_php_8_4_and_marks_7_4_as_unsupported() {
    let catalog = Catalog::embedded().unwrap();
    assert_eq!(catalog.recommended("php"), Some("8.4"));
    assert_eq!(catalog.recommended("node"), Some("24"));
    // The recommended line must be one the catalog can actually install.
    let target = Target {
        os: Os::Windows,
        arch: Arch::X86_64,
    };
    assert!(catalog.find("php", "8.4", target).is_ok());
    assert!(catalog.find("node", "24", target).is_ok());
    // Node 20 is only for old projects: it never is what "latest" means.
    let node20 = catalog.find("node", "20", target).unwrap();
    assert!(node20.end_of_life);
    assert_eq!(
        catalog.latest("node", target).unwrap().version.to_string(),
        "24.22.0"
    );

    let old = catalog.find("php", "7.4", target).unwrap();
    assert_eq!(old.version.to_string(), "7.4.33");
    assert!(old.end_of_life);
    assert!(catalog.is_end_of_life("php", &old.version));
    for package in catalog.packages().iter().filter(|p| {
        // PHP 7.4, Node 20 and the old builds of Redis 5 and Memcached 1.6.8 (ADR-0012, ADR-0017) are the
        // unsupported ones.
        let version = p.version.to_string();
        !matches!(
            (p.service.as_str(), version.split('.').next().unwrap_or("")),
            ("php", "7") | ("node", "20") | ("redis" | "memcached", _)
        )
    }) {
        assert!(
            !package.end_of_life,
            "{} is not end of life",
            package.label()
        );
    }
    // It is never what "latest" means.
    assert_eq!(
        catalog
            .latest("php", target)
            .unwrap()
            .version
            .to_string()
            .split('.')
            .next(),
        Some("8")
    );
}

#[test]
fn a_catalog_without_a_recommended_section_is_valid() {
    let catalog = sample();
    assert_eq!(catalog.recommended("php"), None);
    assert!(!catalog.is_end_of_life("php", &Version::parse("8.4.2").unwrap()));
}

#[test]
fn the_recommended_line_and_end_of_life_are_read_and_validated() {
    let eol = entry("php", "7.4.33").replace(
        "format =",
        "end_of_life = true
format =",
    );
    let text = format!(
        "schema = 1
serial = 3

[recommended]
php = \"8.4\"

{}{eol}{}",
        entry("php", "8.4.2"),
        entry("php", "8.5.1")
    );
    let catalog = Catalog::parse(&text).unwrap();
    assert_eq!(catalog.recommended("php"), Some("8.4"));
    assert!(catalog.is_end_of_life("php", &Version::parse("7.4.33").unwrap()));
    assert!(!catalog.is_end_of_life("php", &Version::parse("8.4.2").unwrap()));

    let target = Target {
        os: Os::Windows,
        arch: Arch::X86_64,
    };
    assert_eq!(
        catalog.latest("php", target).unwrap().version.to_string(),
        "8.5.1"
    );

    for bad in ["php = \"eight\"", "PHP = \"8.4\"", "php = 8.4"] {
        let text = format!(
            "schema = 1
serial = 3

[recommended]
{bad}
"
        );
        assert!(Catalog::parse(&text).is_err(), "{bad}");
    }
}

#[test]
fn latest_falls_back_to_an_unsupported_version_only_if_nothing_else_exists() {
    let eol = entry("php", "7.4.33").replace(
        "format =",
        "end_of_life = true
format =",
    );
    let catalog = Catalog::parse(&catalog(&[eol])).unwrap();
    let target = Target {
        os: Os::Windows,
        arch: Arch::X86_64,
    };
    assert_eq!(
        catalog.latest("php", target).unwrap().version.to_string(),
        "7.4.33"
    );
}

// ---- vulnerability advisories (ADR-0020) ----------------------------------------------

fn with_advisory(extra: &str) -> String {
    format!(
        "{}\n[[advisory]]\nid = \"PHP2Shell\"\nservice = \"php\"\nsummary = \"Remote code execution\"\n\
         url = \"https://example.invalid/a\"\n{extra}\n",
        catalog(&[entry("php", "8.4.26")])
    )
}

fn php2shell() -> Catalog {
    Catalog::parse(&with_advisory(
        "affects = [\"7.4\", \"8.3\", \"8.4\"]\nfixed_in = [\"8.3.36\", \"8.4.27\"]",
    ))
    .unwrap()
}

fn affected(catalog: &Catalog, service: &str, version: &str) -> bool {
    !catalog
        .advisories_for(service, &Version::parse(version).unwrap())
        .is_empty()
}

#[test]
fn an_advisory_affects_a_line_until_the_fix_of_that_same_line() {
    let catalog = php2shell();
    for vulnerable in ["8.3.35", "8.4.0", "8.4.26", "7.4.33"] {
        assert!(affected(&catalog, "php", vulnerable), "{vulnerable}");
    }
    for fixed in ["8.3.36", "8.3.40", "8.4.27", "8.4.99"] {
        assert!(!affected(&catalog, "php", fixed), "{fixed}");
    }
    // 8.4.27 fixes the 8.4 line, not 8.3; and a line the advisory does not name is untouched.
    assert!(affected(&catalog, "php", "8.3.0"));
    assert!(!affected(&catalog, "php", "8.5.1"));
    assert!(!affected(&catalog, "node", "8.4.26"));
}

#[test]
fn a_line_without_a_fixed_version_stays_affected() {
    let catalog = php2shell();
    let advisory = &catalog.advisories()[0];
    let old = Version::parse("7.4.33").unwrap();
    assert!(advisory.affects("php", &old));
    assert_eq!(advisory.fix_for(&old), None);
    assert_eq!(
        advisory
            .fix_for(&Version::parse("8.4.26").unwrap())
            .map(ToString::to_string),
        Some("8.4.27".to_owned())
    );
    assert_eq!(advisory.fix_for(&Version::parse("8.4.27").unwrap()), None);
}

#[test]
fn an_advisory_without_lines_covers_every_version_below_its_fix() {
    let catalog = Catalog::parse(&with_advisory("fixed_in = [\"8.4.27\"]")).unwrap();
    assert!(affected(&catalog, "php", "7.4.33"));
    assert!(affected(&catalog, "php", "8.4.26"));
    assert!(!affected(&catalog, "php", "8.4.27"));
    let nothing_fixed = Catalog::parse(&with_advisory("")).unwrap();
    assert!(affected(&nothing_fixed, "php", "9.9.9"));
}

#[test]
fn a_catalog_without_advisories_is_still_valid() {
    let catalog = Catalog::parse(&catalog(&[entry("php", "8.4.26")])).unwrap();
    assert_eq!(catalog.advisories(), []);
}

#[test]
fn malformed_advisories_are_rejected() {
    let reject = |text: String, field: &str| match Catalog::parse(&text) {
        Err(CatalogError::Invalid { field: got, .. }) => assert_eq!(got, field, "{text}"),
        other => panic!("expected `{field}` to be rejected, got {other:?}\n{text}"),
    };
    reject(
        with_advisory("").replace("https://example.invalid/a", "http://example.invalid/a"),
        "url",
    );
    reject(
        with_advisory("").replace(
            "https://example.invalid/a",
            "https://user@example.invalid/a",
        ),
        "url",
    );
    reject(
        with_advisory("").replace("service = \"php\"\nsummary", "service = \"PHP\"\nsummary"),
        "service",
    );
    reject(
        with_advisory("").replace("Remote code execution", "  "),
        "summary",
    );
    reject(with_advisory("").replace("PHP2Shell", "two words"), "id");
    reject(with_advisory("affects = [\"eight\"]"), "affects");
    reject(with_advisory("fixed_in = [\"8.4.x\"]"), "fixed_in");
}

#[test]
fn two_advisories_with_the_same_id_are_refused() {
    let text =
        with_advisory("") + &with_advisory("").replace(&catalog(&[entry("php", "8.4.26")]), "");
    assert!(
        matches!(Catalog::parse(&text), Err(CatalogError::Duplicate(what)) if what.contains("PHP2Shell"))
    );
}

#[test]
fn the_template_for_a_missing_version_is_the_highest_stable_build() {
    let catalog = sample();
    assert_eq!(
        catalog
            .template("php", WINDOWS)
            .unwrap()
            .version
            .to_string(),
        "8.4.10"
    );
    assert!(catalog.template("redis", WINDOWS).is_none());
}

#[test]
fn the_built_in_catalog_flags_php2shell_only_where_there_is_no_fixed_build() {
    let catalog = Catalog::embedded().unwrap();
    let advisory = catalog
        .advisories()
        .iter()
        .find(|a| a.id == "PHP2Shell")
        .expect("the built-in catalog carries the PHP2Shell advisory");
    // The builds the catalog offers are the fixed ones, except PHP 7.4, which has none.
    for package in catalog.packages().iter().filter(|p| p.service == "php") {
        let vulnerable = advisory.affects("php", &package.version);
        let seven_four = package.version.parts().first() == Some(&7);
        assert_eq!(vulnerable, seven_four, "{}", package.label());
    }
    // And the ones it replaced are flagged.
    for old in ["8.3.35", "8.4.26", "8.5.11"] {
        assert!(affected(&catalog, "php", old), "{old}");
    }
    for fixed in ["8.3.36", "8.4.27", "8.5.12"] {
        assert!(!affected(&catalog, "php", fixed), "{fixed}");
    }
    assert!(!affected(&catalog, "node", "22.23.3"));
}
