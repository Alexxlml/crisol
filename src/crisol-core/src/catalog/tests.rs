use super::*;

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
    let err = Catalog::parse_signed_trusted(SIGNED_CATALOG, SIGNATURE).unwrap_err();
    assert!(matches!(err, CatalogError::Signature(_)), "{err}");
}

#[test]
fn an_older_catalog_cannot_replace_a_newer_one() {
    let current = Catalog::parse("schema = 1\nserial = 5\n").unwrap();
    let older = Catalog::parse("schema = 1\nserial = 4\n").unwrap();
    let same = Catalog::parse("schema = 1\nserial = 5\n").unwrap();
    let newer = Catalog::parse("schema = 1\nserial = 6\n").unwrap();

    let err = older.check_not_older_than(&current).unwrap_err();
    assert!(
        matches!(
            err,
            CatalogError::Rollback {
                offered: 4,
                current: 5
            }
        ),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "catalog serial 4 is older than the one in use (5)"
    );
    assert!(same.check_not_older_than(&current).is_ok());
    assert!(newer.check_not_older_than(&current).is_ok());

    // And the signed fixture is newer than the built-in catalog.
    let signed = Catalog::parse_signed(SIGNED_CATALOG, SIGNATURE, PUBLIC_KEY).unwrap();
    assert!(signed
        .check_not_older_than(&Catalog::embedded().unwrap())
        .is_ok());
}
