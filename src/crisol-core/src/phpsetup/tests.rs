use std::fs;

use tempfile::TempDir;

use super::*;
use crate::cacert::{NoRoots, BUNDLE_FILE, BUNDLE_SERVICE};
use crate::paths::fake::FakePrivateDirs;
use crate::phpini::BASE_FILE;

const EXCERPT: &str = include_str!("../phpini/testdata/php.ini-development.excerpt");

fn home(versions: &[&str], bundle: bool) -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    for version in versions {
        let install = paths.bin_for("php", version).unwrap();
        fs::create_dir_all(install.join("ext")).unwrap();
        fs::write(install.join(BASE_FILE), EXCERPT).unwrap();
        fs::write(install.join("ext").join("php_curl.dll"), "MZ").unwrap();
    }
    if bundle {
        let dir = paths.bin_for(BUNDLE_SERVICE, "2026.9.25").unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(BUNDLE_FILE), "## Mozilla\n").unwrap();
    }
    (tmp, paths)
}

fn versions(list: &[&str]) -> Vec<String> {
    list.iter().map(|v| (*v).to_owned()).collect()
}

#[test]
fn the_bundle_is_written_before_the_ini_so_the_ini_names_it() {
    let (_tmp, paths) = home(&["8.4.26", "8.3.35"], true);
    let result = refresh_php(
        &paths,
        &PhpConfig::default(),
        &versions(&["8.4.26", "8.3.35"]),
        &NoRoots,
        &FakePrivateDirs::new(),
    );
    assert_eq!(result.ready, ["8.4.26", "8.3.35"]);
    assert_eq!(result.changed, ["8.4.26", "8.3.35"]);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert!(paths.php_cacert().is_file());
    for version in ["8.4.26", "8.3.35"] {
        let ini = fs::read_to_string(paths.php_ini(version).unwrap()).unwrap();
        assert!(ini.contains("curl.cainfo"), "{ini}");
        assert!(ini.contains("extension=curl"), "{ini}");
    }
}

#[test]
fn a_second_run_touches_nothing() {
    let (_tmp, paths) = home(&["8.4.26"], true);
    let run = || {
        refresh_php(
            &paths,
            &PhpConfig::default(),
            &versions(&["8.4.26"]),
            &NoRoots,
            &FakePrivateDirs::new(),
        )
    };
    run();
    let again = run();
    assert_eq!(again.changed, Vec::<String>::new());
    assert_eq!(again.ready, ["8.4.26"]);
}

#[test]
fn without_the_bundle_the_ini_has_no_cainfo_and_the_warning_comes_once() {
    let (_tmp, paths) = home(&["8.4.26"], false);
    let result = refresh_php(
        &paths,
        &PhpConfig::default(),
        &versions(&["8.4.26"]),
        &NoRoots,
        &FakePrivateDirs::new(),
    );
    assert_eq!(result.ready, ["8.4.26"]);
    assert!(!paths.php_cacert().exists());
    let ini = fs::read_to_string(paths.php_ini("8.4.26").unwrap()).unwrap();
    assert!(!ini.contains("curl.cainfo = "), "{ini}");
    assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    assert!(result.warnings[0].contains("crisol install cacert"));
}

#[test]
fn a_change_of_time_zone_changes_only_the_ini() {
    let (_tmp, paths) = home(&["8.4.26"], true);
    let list = versions(&["8.4.26"]);
    let dirs = FakePrivateDirs::new();
    refresh_php(&paths, &PhpConfig::default(), &list, &NoRoots, &dirs);
    let php = PhpConfig {
        timezone: Some("Europe/Madrid".to_owned()),
        ..PhpConfig::default()
    };
    let result = refresh_php(&paths, &php, &list, &NoRoots, &dirs);
    assert_eq!(result.changed, ["8.4.26"]);
    let ini = fs::read_to_string(paths.php_ini("8.4.26").unwrap()).unwrap();
    assert!(ini.contains("date.timezone = \"Europe/Madrid\""), "{ini}");
}

#[test]
fn a_version_that_is_not_installed_is_a_warning_and_the_rest_go_on() {
    let (_tmp, paths) = home(&["8.4.26"], true);
    let result = refresh_php(
        &paths,
        &PhpConfig::default(),
        &versions(&["7.4.33", "8.4.26"]),
        &NoRoots,
        &FakePrivateDirs::new(),
    );
    assert_eq!(result.ready, ["8.4.26"]);
    assert!(result.warnings.iter().any(|w| w.contains("7.4.33")));
}

#[test]
fn the_installed_versions_are_listed() {
    let (_tmp, paths) = home(&["8.4.26", "8.3.35"], true);
    let mut found = installed_php_versions(&paths);
    found.sort();
    assert_eq!(found, ["8.3.35", "8.4.26"]);
}

struct CodeSigningOnly;

impl crate::cacert::RootCertificates for CodeSigningOnly {
    fn trusted_roots(&self) -> std::io::Result<Vec<crate::cacert::SystemRoot>> {
        Ok(vec![crate::cacert::SystemRoot {
            der: vec![1, 2, 3],
            usages: Some(vec!["1.3.6.1.5.5.7.3.3".to_owned()]),
            partially_distrusted: false,
        }])
    }

    fn disallowed_roots(&self) -> std::io::Result<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

#[test]
fn roots_left_out_of_the_bundle_are_a_note_not_a_warning() {
    // W1 of the review of phase 9: `crisol start` shows the warnings, and this is not
    // something the user can act on.
    let (_tmp, paths) = home(&["8.4.26"], true);
    let result = refresh_php(
        &paths,
        &PhpConfig::default(),
        &versions(&["8.4.26"]),
        &CodeSigningOnly,
        &FakePrivateDirs::new(),
    );
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
    assert!(result.notes[0].starts_with("1 system root(s) were left out"));
}
