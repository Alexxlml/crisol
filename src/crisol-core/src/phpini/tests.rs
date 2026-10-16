//! Tests of the `php.ini` generator. The excerpt in `testdata/` is made of lines copied from
//! the `php.ini-development` of PHP 8.4.26, with the CRLF line endings PHP ships.

use std::time::Duration;

use tempfile::TempDir;

use super::*;

const EXCERPT: &str = include_str!("testdata/php.ini-development.excerpt");
const EXT_DIR: &str = r"C:\Crisol\bin\php\8.4.26\ext";
const OVERRIDES_DIR: &str = r"C:\Crisol\etc\php\overrides";
const LOG: &str = r"C:\Crisol\logs\php\php_errors.log";

/// What a standard 8.4 build ships (the extensions of the excerpt that have a DLL).
const SHIPPED: [&str; 24] = [
    "bz2",
    "curl",
    "exif",
    "ffi",
    "fileinfo",
    "ftp",
    "gd",
    "gettext",
    "gmp",
    "intl",
    "ldap",
    "mbstring",
    "mysqli",
    "openssl",
    "opcache",
    "pdo_mysql",
    "pdo_pgsql",
    "pdo_sqlite",
    "pgsql",
    "soap",
    "sockets",
    "sodium",
    "sqlite3",
    "zip",
];

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn render_with(
    base: &str,
    available: &[&str],
    settings: &PhpIniSettings,
    overrides: &[(String, String)],
) -> GeneratedPhpIni {
    render_php_ini(&PhpIniInput {
        version: "8.4.26",
        base,
        extension_dir: Path::new(EXT_DIR),
        available: &set(available),
        error_log: Some(Path::new(LOG)),
        ca_file: None,
        overrides,
        overrides_dir: Path::new(OVERRIDES_DIR),
        settings,
    })
    .unwrap()
}

fn render(base: &str) -> GeneratedPhpIni {
    render_with(base, &SHIPPED, &PhpIniSettings::default(), &[])
}

/// The lines of `text` that are not in `base`, and the other way round: what changed.
fn changes(base: &str, text: &str) -> String {
    let before: Vec<&str> = base.lines().collect();
    let after: Vec<&str> = text.lines().collect();
    let removed = before
        .iter()
        .filter(|l| !after.contains(l))
        .map(|l| format!("- {l}\n"));
    let added = after
        .iter()
        .filter(|l| !before.contains(l))
        .map(|l| format!("+ {l}\n"));
    removed.chain(added).collect()
}

#[test]
fn the_generated_file_for_the_excerpt_is_the_one_reviewed() {
    insta::assert_snapshot!(render(EXCERPT).text);
}

#[test]
fn only_the_intended_lines_change() {
    let generated = render(EXCERPT);
    insta::assert_snapshot!(changes(EXCERPT, &generated.text));
}

#[test]
fn extensions_are_switched_on_only_when_the_build_ships_them() {
    let shipped_without_gd: Vec<&str> = SHIPPED.iter().copied().filter(|e| *e != "gd").collect();
    let generated = render_with(
        EXCERPT,
        &shipped_without_gd,
        &PhpIniSettings::default(),
        &[],
    );
    assert!(generated.text.contains("\r\nextension=curl\r\n"));
    assert!(
        generated.text.contains("\r\n;extension=gd\r\n"),
        "no DLL, no extension: the line stays commented"
    );
    assert_eq!(generated.skipped, ["gd", "xsl"]);
    assert!(!generated.enabled.contains(&"gd".to_owned()));
}

#[test]
fn extensions_that_are_not_requested_stay_off() {
    let text = render(EXCERPT).text;
    for off in [
        "ffi",
        "ldap",
        "odbc",
        "tidy",
        "snmp",
        "shmop",
        "pdo_firebird",
    ] {
        assert!(
            text.contains(&format!(";extension={off}")),
            "`{off}` must stay commented"
        );
        assert!(!text.contains(&format!("\r\nextension={off}")), "{off}");
    }
}

#[test]
fn a_comment_after_the_extension_name_is_kept() {
    let text = render(EXCERPT).text;
    assert!(
        text.contains("\r\nextension=exif      ; Must be after mbstring as it depends on it\r\n")
    );
}

#[test]
fn the_prose_that_mentions_extension_lines_is_left_alone() {
    let text = render(EXCERPT).text;
    for prose in [
        ";   extension=modulename",
        ";   extension=mysqli",
        ";   extension=/path/to/extension/mysqli.so",
    ] {
        assert!(text.contains(&format!("\r\n{prose}\r\n")), "{prose}");
    }
}

#[test]
fn opcache_is_a_zend_extension_and_only_when_shipped() {
    let on = render(EXCERPT);
    assert!(on.text.contains("\r\nzend_extension=opcache\r\n"));
    assert!(on.enabled.contains(&"opcache".to_owned()));

    let without = render_with(EXCERPT, &["curl"], &PhpIniSettings::default(), &[]);
    assert!(without.text.contains("\r\n;zend_extension=opcache\r\n"));

    let disabled = PhpIniSettings {
        opcache: false,
        ..PhpIniSettings::default()
    };
    let off = render_with(EXCERPT, &SHIPPED, &disabled, &[]);
    assert!(off.text.contains("\r\n;zend_extension=opcache\r\n"));
}

#[test]
fn extension_dir_replaces_the_windows_line_not_the_unix_one() {
    let text = render(EXCERPT).text;
    assert!(text.contains("\r\n;extension_dir = \"./\"\r\n"));
    assert!(
        text.contains("\r\nextension_dir = \"C:/Crisol/bin/php/8.4.26/ext\"\r\n"),
        "forward slashes, so no backslash sits before a quote"
    );
    assert!(!text.contains(";extension_dir = \"ext\""));
}

#[test]
fn limits_and_the_log_are_set() {
    let text = render(EXCERPT).text;
    for line in [
        "max_execution_time = 120",
        "memory_limit = 512M",
        "post_max_size = 128M",
        "upload_max_filesize = 128M",
        "error_log = \"C:/Crisol/logs/php/php_errors.log\"",
    ] {
        assert!(text.contains(&format!("\r\n{line}\r\n")), "{line}");
    }
    assert!(
        text.contains(";error_log = syslog"),
        "the other commented error_log line is not touched"
    );
}

#[test]
fn the_ca_bundle_is_set_for_curl_only_and_only_when_there_is_one() {
    let none = render(EXCERPT).text;
    assert!(none.contains("\r\n;curl.cainfo =\r\n"));
    assert!(none.contains("\r\n;openssl.cafile=\r\n"));

    let bundle = Path::new(r"C:\Crisol\etc\php\cacert.pem");
    let text = render_php_ini(&PhpIniInput {
        version: "8.4.26",
        base: EXCERPT,
        extension_dir: Path::new(EXT_DIR),
        available: &set(&SHIPPED),
        error_log: None,
        ca_file: Some(bundle),
        overrides: &[],
        overrides_dir: Path::new(OVERRIDES_DIR),
        settings: &PhpIniSettings::default(),
    })
    .unwrap()
    .text;
    let line = "curl.cainfo = \"C:/Crisol/etc/php/cacert.pem\"";
    assert!(text.contains(&format!("\r\n{line}\r\n")), "{line}");
    assert!(
        text.contains("\r\n;openssl.cafile=\r\n"),
        "openssl.cafile stays as PHP ships it (the streams use the live Windows store)"
    );
    assert!(
        !text.contains(";curl.cainfo"),
        "the shipped lines are replaced, not duplicated"
    );
}

#[test]
fn a_user_override_of_the_ca_file_has_the_last_word() {
    let overrides = vec![(
        "10-ca.ini".to_owned(),
        "curl.cainfo = \"D:/corp/ca.pem\"\n".to_owned(),
    )];
    let text = render_php_ini(&PhpIniInput {
        version: "8.4.26",
        base: EXCERPT,
        extension_dir: Path::new(EXT_DIR),
        available: &set(&SHIPPED),
        error_log: None,
        ca_file: Some(Path::new("C:/Crisol/etc/php/cacert.pem")),
        overrides: &overrides,
        overrides_dir: Path::new(OVERRIDES_DIR),
        settings: &PhpIniSettings::default(),
    })
    .unwrap()
    .text;
    let ours = text.find("curl.cainfo = \"C:/Crisol").unwrap();
    let theirs = text.find("curl.cainfo = \"D:/corp/ca.pem\"").unwrap();
    assert!(
        ours < theirs,
        "PHP keeps the last assignment, and overrides come last"
    );
}

#[test]
fn the_time_zone_is_set_only_when_configured() {
    let unset = render(EXCERPT).text;
    assert!(unset.contains("\r\n;date.timezone =\r\n"));

    let zoned = PhpIniSettings {
        timezone: Some("Europe/Madrid".to_owned()),
        ..PhpIniSettings::default()
    };
    let text = render_with(EXCERPT, &SHIPPED, &zoned, &[]).text;
    assert!(text.contains("\r\ndate.timezone = \"Europe/Madrid\"\r\n"));
    assert!(!text.contains(";date.timezone"));
}

#[test]
fn what_the_shipped_file_has_no_line_for_is_added_in_its_own_block() {
    // A build whose file lacks the extension block and the settings.
    let bare = "; just a comment\r\n[PHP]\r\n";
    let generated = render_with(
        bare,
        &["curl", "opcache"],
        &PhpIniSettings {
            extensions: vec!["curl".to_owned(), "gd".to_owned()],
            timezone: Some("UTC".to_owned()),
            ..PhpIniSettings::default()
        },
        &[],
    );
    let added = generated
        .text
        .split("; ---- Added by Crisol")
        .nth(1)
        .expect("a block of additions");
    for line in [
        "extension=curl",
        "zend_extension=opcache",
        "memory_limit = 512M",
        "date.timezone = \"UTC\"",
        "extension_dir = \"C:/Crisol/bin/php/8.4.26/ext\"",
    ] {
        assert!(added.contains(line), "{line} in {added}");
    }
    assert!(!added.contains("extension=gd"), "gd has no DLL");
}

#[test]
fn line_endings_are_kept_whatever_they_are() {
    let crlf = render(EXCERPT).text;
    assert!(
        !crlf.replace("\r\n", "").contains('\n'),
        "every line break of a CRLF file stays CRLF, header and additions included"
    );

    let lf_base = EXCERPT.replace("\r\n", "\n");
    let lf = render(&lf_base).text;
    assert!(!lf.contains('\r'), "an LF file stays LF");
    assert_eq!(
        lf.replace('\n', "\r\n"),
        crlf,
        "and the content is the same either way"
    );
}

#[test]
fn a_file_without_a_final_newline_gets_one() {
    let text = render("[PHP]\r\nmemory_limit = 128M").text;
    assert!(text.contains("\r\nmemory_limit = 512M\r\n"));
}

#[test]
fn overrides_come_last_in_the_order_given_with_the_file_line_endings() {
    let overrides = vec![
        (
            "10-dev.ini".to_owned(),
            "memory_limit = 1G\nxdebug.mode=off".to_owned(),
        ),
        (
            "20-mine.ini".to_owned(),
            "display_errors = Off\r\n".to_owned(),
        ),
    ];
    let text = render_with(EXCERPT, &SHIPPED, &PhpIniSettings::default(), &overrides).text;
    let first = text.find("; ---- Override: 10-dev.ini ----").unwrap();
    let second = text.find("; ---- Override: 20-mine.ini ----").unwrap();
    assert!(first < second);
    assert!(text.ends_with("display_errors = Off\r\n"));
    assert!(text.contains("memory_limit = 1G\r\nxdebug.mode=off\r\n"));
    assert!(first > text.find("\r\nmemory_limit = 512M\r\n").unwrap());
}

#[test]
fn the_header_says_what_the_file_is_and_where_to_put_changes() {
    let text = render(EXCERPT).text;
    let head: Vec<&str> = text.lines().take(4).collect();
    assert_eq!(
        head[0],
        "; php.ini for PHP 8.4.26 generated by Crisol from php.ini-development."
    );
    assert!(head[1].contains("Do not edit"));
    assert!(head[2].contains("overrides"));
}

#[test]
fn the_same_input_gives_the_same_text() {
    assert_eq!(render(EXCERPT), render(EXCERPT));
}

#[test]
fn settings_that_cannot_be_written_safely_are_refused() {
    let base = PhpIniSettings::default();
    for (settings, name) in [
        (
            PhpIniSettings {
                memory_limit: "lots".into(),
                ..base.clone()
            },
            "memory_limit",
        ),
        (
            PhpIniSettings {
                post_max_size: "8 M".into(),
                ..base.clone()
            },
            "post_max_size",
        ),
        (
            PhpIniSettings {
                upload_max_filesize: String::new(),
                ..base.clone()
            },
            "upload_max_filesize",
        ),
        (
            PhpIniSettings {
                timezone: Some("UTC\"\r\nevil = 1".into()),
                ..base.clone()
            },
            "date.timezone",
        ),
        (
            PhpIniSettings {
                timezone: Some(String::new()),
                ..base.clone()
            },
            "date.timezone",
        ),
        (
            PhpIniSettings {
                extensions: vec!["curl\nx".into()],
                ..base.clone()
            },
            "extensions",
        ),
    ] {
        let error = render_php_ini(&PhpIniInput {
            version: "8.4.26",
            base: EXCERPT,
            extension_dir: Path::new(EXT_DIR),
            available: &set(&SHIPPED),
            error_log: None,
            ca_file: None,
            overrides: &[],
            overrides_dir: Path::new(OVERRIDES_DIR),
            settings: &settings,
        })
        .unwrap_err();
        assert!(
            matches!(&error, PhpIniError::InvalidSetting { name: n, .. } if *n == name),
            "{name}: {error}"
        );
    }
    // -1 (unlimited) and plain numbers are fine.
    let fine = PhpIniSettings {
        memory_limit: "-1".into(),
        post_max_size: "0".into(),
        ..base
    };
    assert!(render_with(EXCERPT, &SHIPPED, &fine, &[])
        .text
        .contains("memory_limit = -1"));
}

#[test]
fn a_path_with_a_quote_is_refused() {
    let error = render_php_ini(&PhpIniInput {
        version: "8.4.26",
        base: EXCERPT,
        extension_dir: Path::new("C:/odd\"dir/ext"),
        available: &set(&SHIPPED),
        error_log: None,
        ca_file: None,
        overrides: &[],
        overrides_dir: Path::new(OVERRIDES_DIR),
        settings: &PhpIniSettings::default(),
    })
    .unwrap_err();
    assert!(
        matches!(error, PhpIniError::InvalidSetting { .. }),
        "{error}"
    );
}

// ---- reading the installation -------------------------------------------------------

#[test]
fn installed_extensions_come_from_the_dll_names() {
    let dir = TempDir::new().unwrap();
    for name in [
        "php_curl.dll",
        "php_opcache.dll",
        "PHP_GD.DLL",
        "libpq.dll",
        "readme.txt",
    ] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let found = installed_extensions(dir.path()).unwrap();
    assert!(found.contains("curl") && found.contains("opcache") && found.contains("gd"));
    assert!(!found.contains("readme"));
    assert!(installed_extensions(&dir.path().join("nope"))
        .unwrap()
        .is_empty());
}

#[test]
fn overrides_are_read_in_name_order_and_only_ini_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("20-b.ini"), "b=1").unwrap();
    fs::write(dir.path().join("10-A.INI"), "a=1").unwrap();
    fs::write(dir.path().join("notes.txt"), "no").unwrap();
    fs::create_dir(dir.path().join("sub.ini")).unwrap();

    let found = read_overrides(dir.path()).unwrap();
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["10-A.INI", "20-b.ini"]);
    assert_eq!(read_overrides(&dir.path().join("nope")).unwrap(), []);
}

// ---- writing the file ---------------------------------------------------------------

struct Home {
    _tmp: TempDir,
    paths: CrisolPaths,
}

fn home_with_php(version: &str) -> Home {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let install = paths.bin_for("php", version).unwrap();
    fs::create_dir_all(install.join("ext")).unwrap();
    fs::write(install.join(BASE_FILE), EXCERPT).unwrap();
    for dll in ["php_curl.dll", "php_mbstring.dll", "php_opcache.dll"] {
        fs::write(install.join("ext").join(dll), "MZ").unwrap();
    }
    Home { _tmp: tmp, paths }
}

#[test]
fn the_file_is_written_where_the_layout_says() {
    let home = home_with_php("8.4.26");
    let written = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();

    assert_eq!(written.path, home.paths.php_ini("8.4.26").unwrap());
    assert!(written
        .path
        .ends_with(Path::new("etc").join("php").join("8.4.26").join("php.ini")));
    assert!(written.changed);
    assert_eq!(written.enabled, ["curl", "mbstring", "opcache"]);
    assert!(written.skipped.contains(&"gd".to_owned()));

    let text = fs::read_to_string(&written.path).unwrap();
    let ext = home.paths.bin_for("php", "8.4.26").unwrap().join("ext");
    let ext = ext.to_string_lossy().replace('\\', "/");
    assert!(
        text.contains(&format!("extension_dir = \"{ext}\"")),
        "{text}"
    );
    let log = home.paths.logs_for("php").unwrap().join("php_errors.log");
    let log = log.to_string_lossy().replace('\\', "/");
    assert!(text.contains(&format!("error_log = \"{log}\"")));
}

#[test]
fn the_file_points_at_the_ca_bundle_when_it_exists_and_warns_when_it_does_not() {
    let home = home_with_php("8.4.26");
    let without = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert_eq!(without.warnings.len(), 1, "{:?}", without.warnings);
    assert!(without.warnings[0].contains("CA bundle"));
    let text = fs::read_to_string(&without.path).unwrap();
    assert!(
        text.contains(";curl.cainfo ="),
        "no bundle, no directive: {text}"
    );

    fs::create_dir_all(home.paths.php_cacert().parent().unwrap()).unwrap();
    fs::write(home.paths.php_cacert(), "bundle").unwrap();
    let with = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(with.changed, "the bundle appearing changes the file");
    assert!(with.warnings.is_empty(), "{:?}", with.warnings);
    let text = fs::read_to_string(&with.path).unwrap();
    let bundle = home.paths.php_cacert().to_string_lossy().replace('\\', "/");
    assert!(
        text.contains(&format!("curl.cainfo = \"{bundle}\"")),
        "{text}"
    );
    assert!(
        text.lines().all(|l| !l.starts_with("openssl.cafile")),
        "no active openssl.cafile: {text}"
    );
}

#[test]
fn generating_twice_does_not_touch_the_file_the_second_time() {
    let home = home_with_php("8.4.26");
    let first = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let stamp = fs::metadata(&first.path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(50));

    let second = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(!second.changed);
    assert_eq!(
        fs::metadata(&second.path).unwrap().modified().unwrap(),
        stamp
    );
}

#[test]
fn a_new_override_changes_the_file_and_the_overrides_are_never_modified() {
    let home = home_with_php("8.4.26");
    write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();

    let overrides = home.paths.overrides_for("php").unwrap();
    fs::create_dir_all(&overrides).unwrap();
    let mine = overrides.join("10-mine.ini");
    fs::write(&mine, "memory_limit = 2G\n").unwrap();

    let again = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(again.changed);
    let text = fs::read_to_string(&again.path).unwrap();
    assert!(text.trim_end().ends_with("memory_limit = 2G"), "{text}");
    assert_eq!(fs::read_to_string(&mine).unwrap(), "memory_limit = 2G\n");
}

#[test]
fn a_hand_edited_generated_file_is_put_back() {
    let home = home_with_php("8.4.26");
    let written = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    fs::write(&written.path, "; tampered\n").unwrap();

    let again = write_php_ini(
        &home.paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(
        again.changed,
        "etc/ is derived: it is regenerated, not trusted"
    );
    assert!(fs::read_to_string(&again.path)
        .unwrap()
        .contains("generated by Crisol"));
}

#[test]
fn versions_that_are_not_installed_or_not_valid_are_errors() {
    let home = home_with_php("8.4.26");
    let settings = PhpIniSettings::default();
    assert!(matches!(
        write_php_ini(&home.paths, "8.3.35", &settings, &crate::paths::fake::FakePrivateDirs::new()),
        Err(PhpIniError::NotInstalled(v)) if v == "8.3.35"
    ));
    assert!(matches!(
        write_php_ini(
            &home.paths,
            "../x",
            &settings,
            &crate::paths::fake::FakePrivateDirs::new()
        ),
        Err(PhpIniError::Paths(_))
    ));

    fs::remove_file(home.paths.bin_for("php", "8.4.26").unwrap().join(BASE_FILE)).unwrap();
    assert!(matches!(
        write_php_ini(
            &home.paths,
            "8.4.26",
            &settings,
            &crate::paths::fake::FakePrivateDirs::new()
        ),
        Err(PhpIniError::NoBaseFile(_))
    ));
}

// ---- the real PHP builds ------------------------------------------------------------

/// Installs each official PHP archive found in `CRISOL_TEST_DOWNLOADS`, generates its
/// `php.ini`, and asks the real `php.exe` (with `-c`, so nothing outside the temporary home
/// is read) to list its modules. It proves that every extension Crisol switched on loads with
/// no start-up warning, which is the acceptance test of F4.1.
///
/// ```text
/// CRISOL_TEST_DOWNLOADS=<folder with php-*-nts-Win32-*-x64.zip> cargo test -p crisol-core real_php -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS pointing at the downloaded official PHP archives (Windows)"]
#[allow(clippy::too_many_lines)]
fn real_php_builds_load_every_extension_crisol_enables() {
    use crate::catalog::Catalog;
    use crate::fetch::fake::FakeFetcher;
    use crate::install::{install, InstallOptions};

    let Some(folder) = std::env::var_os("CRISOL_TEST_DOWNLOADS").map(PathBuf::from) else {
        panic!("set CRISOL_TEST_DOWNLOADS to the folder with the downloaded archives");
    };
    let catalog = Catalog::embedded().unwrap();
    let mut tried = 0;
    for package in catalog.packages().iter().filter(|p| p.service == "php") {
        let file = folder.join(package.url.rsplit('/').next().unwrap());
        if !file.is_file() {
            eprintln!(
                "skipping {}: {} is not there",
                package.label(),
                file.display()
            );
            continue;
        }
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        let fetcher = FakeFetcher::new();
        fetcher.serve(&package.url, fs::read(&file).unwrap());
        install(
            package,
            &paths,
            &fetcher,
            &InstallOptions::default(),
            &mut |_| {},
        )
        .unwrap();

        let version = package.version.to_string();
        let written = write_php_ini(
            &paths,
            &version,
            &PhpIniSettings::default(),
            &crate::paths::fake::FakePrivateDirs::new(),
        )
        .unwrap();
        let php = paths.bin_for("php", &version).unwrap().join("php.exe");
        let output = std::process::Command::new(&php)
            .arg("-c")
            .arg(&written.path)
            .arg("-m")
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let modules = String::from_utf8_lossy(&output.stdout).to_lowercase();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{}: php -m failed: {stderr}",
            package.label()
        );
        assert_eq!(stderr.trim(), "", "{}: start-up warnings", package.label());
        assert!(
            !modules.contains("warning"),
            "{}: {modules}",
            package.label()
        );

        for extension in &written.enabled {
            let listed = if extension == "opcache" {
                "zend opcache"
            } else {
                extension
            };
            assert!(
                modules.lines().any(|l| l.trim() == listed),
                "{}: `{extension}` is enabled but php -m does not list it:\n{modules}",
                package.label()
            );
        }
        // The extensions the plan names must be among them on every version.
        for must in [
            "curl",
            "fileinfo",
            "gd",
            "intl",
            "mbstring",
            "openssl",
            "pdo_mysql",
            "pdo_pgsql",
            "pdo_sqlite",
            "sqlite3",
            "zip",
        ] {
            assert!(
                written.enabled.iter().any(|e| e == must),
                "{}: {must}",
                package.label()
            );
        }
        insta::assert_snapshot!(
            format!("real_php_{version}"),
            format!(
                "enabled: {}\nskipped: {}",
                written.enabled.join(" "),
                written.skipped.join(" ")
            )
        );
        eprintln!(
            "ok {} ({} extensions)",
            package.label(),
            written.enabled.len()
        );
        tried += 1;
    }
    assert!(tried > 0, "no PHP archive found in {}", folder.display());
}

#[test]
fn a_repeated_commented_extension_line_is_activated_only_once() {
    // Review finding 11: PHP warns "Module already loaded" for a duplicate `extension=`.
    let base = ";extension=curl\r\n;extension=mbstring\r\n;extension=curl\r\n;zend_extension=opcache\r\n;zend_extension=opcache\r\n";
    let text = render(base).text;
    let count = |needle: &str| text.lines().filter(|l| l.trim() == needle).count();
    assert_eq!(count("extension=curl"), 1, "{text}");
    assert_eq!(count("extension=mbstring"), 1, "{text}");
    assert_eq!(count("zend_extension=opcache"), 1, "{text}");
    assert_eq!(
        count(";extension=curl"),
        1,
        "the second line stays commented"
    );
}
