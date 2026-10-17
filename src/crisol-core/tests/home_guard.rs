//! ADR-0008, corrected 2026-10-16: a home that other accounts can write blocks everything that
//! runs or generates something from it, and only that. The permissions are judged through the
//! `PrivateDirs` double; nothing here depends on the operating system.

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crisol_core::ca::ensure_ca;
use crisol_core::cacert::{write_php_cacert, NoRoots};
use crisol_core::catalog::{Catalog, Target};
use crisol_core::fetch::fake::FakeFetcher;
use crisol_core::install::InstallOptions;
use crisol_core::manage::{
    install_version, list_available, list_installed, uninstall, Environment, ManageError,
};
use crisol_core::nginx::{
    stage_nginx_config, write_nginx_config, NginxError, NginxInput, PhpPools,
};
use crisol_core::paths::fake::FakePrivateDirs;
use crisol_core::paths::{require_private_home, CrisolPaths, HomeError};
use crisol_core::phpini::{write_php_ini, PhpIniError, PhpIniSettings};
use crisol_core::shims::install_shim_files;
use crisol_core::trust::fake::FakeTrustStore;
use crisol_core::trust::{trust_ca, TrustError, TrustStore};
use tempfile::TempDir;

/// A home with the folders Crisol runs or trusts, and a PHP in `bin/`.
fn home() -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    for dir in [
        paths.bin_dir(),
        paths.etc_dir(),
        paths.certs_dir(),
        paths.shims_dir(),
        paths.www_dir(),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    let php = paths.bin_for("php", "8.4.26").unwrap();
    fs::create_dir_all(php.join("ext")).unwrap();
    fs::write(
        php.join("php.ini-development"),
        "; base\n[PHP]\nmemory_limit = 128M\n",
    )
    .unwrap();
    (tmp, paths)
}

/// Every path under `root`, for "nothing changed" comparisons.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

fn open_home(paths: &CrisolPaths) -> FakePrivateDirs {
    let dirs = FakePrivateDirs::new();
    dirs.expose(paths.home());
    dirs
}

#[test]
fn an_open_home_or_runtime_folder_is_named_with_the_exact_command_to_close_it() {
    let (_tmp, paths) = home();
    let before = tree(paths.home());
    let cases = [
        paths.home().to_path_buf(),
        paths.bin_dir(),
        paths.etc_dir(),
        paths.certs_dir(),
        paths.shims_dir(),
        // Where the PHP of the sites runs from by default.
        paths.www_dir(),
    ];
    for open in cases {
        let dirs = FakePrivateDirs::new();
        dirs.expose(&open);
        let error = require_private_home(&paths, &dirs).unwrap_err();
        match &error {
            HomeError::Exposed { path, fix } => {
                assert_eq!(path, &open);
                assert!(fix.starts_with("icacls "), "{fix}");
                assert!(fix.contains("/inheritance:r /grant:r"), "{fix}");
                assert!(fix.contains(&open.display().to_string()), "{fix}");
            }
            other => panic!("{other}"),
        }
        let text = error.to_string();
        assert!(text.contains(&open.display().to_string()), "{text}");
        assert!(text.contains("will not go on"), "{text}");
        assert!(!text.contains("  "), "{text}");
        assert_eq!(dirs.created(), Vec::<PathBuf>::new(), "no folder was made");
    }
    assert_eq!(tree(paths.home()), before, "nothing was changed");
}

#[test]
fn a_private_home_passes_and_folders_that_do_not_exist_yet_are_skipped() {
    let (_tmp, paths) = home();
    require_private_home(&paths, &FakePrivateDirs::new()).unwrap();

    let tmp = TempDir::new().unwrap();
    let fresh = CrisolPaths::new(tmp.path().join("not-made-yet")).unwrap();
    let dirs = FakePrivateDirs::new();
    dirs.expose(fresh.home());
    require_private_home(&fresh, &dirs).unwrap();
}

#[test]
fn generating_php_ini_is_blocked_by_an_open_home() {
    let (_tmp, paths) = home();
    let dirs = open_home(&paths);
    let error = write_php_ini(&paths, "8.4.26", &PhpIniSettings::default(), &dirs).unwrap_err();
    assert!(matches!(error, PhpIniError::Home(_)), "{error}");
    assert!(!paths.php_ini("8.4.26").unwrap().exists());

    // Closed, the same call works.
    write_php_ini(
        &paths,
        "8.4.26",
        &PhpIniSettings::default(),
        &FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(paths.php_ini("8.4.26").unwrap().is_file());
}

#[test]
fn generating_the_php_ca_bundle_is_blocked_by_an_open_home() {
    let (_tmp, paths) = home();
    let error = write_php_cacert(&paths, &NoRoots, &open_home(&paths)).unwrap_err();
    assert!(matches!(error, PhpIniError::Home(_)), "{error}");
    assert!(!paths.php_cacert().exists());
}

#[test]
fn generating_the_nginx_configuration_is_blocked_by_an_open_home() {
    let (_tmp, paths) = home();
    let nginx_dir = paths.bin_for("nginx", "1.28.3").unwrap();
    let pools = PhpPools::default();
    let input = NginxInput {
        paths: &paths,
        nginx_dir: &nginx_dir,
        http_port: 19080,
        tls: None,
        sites: &[],
        pools: &pools,
        mail: None,
    };
    let dirs = open_home(&paths);
    let before = tree(paths.home());
    assert!(matches!(
        write_nginx_config(&input, &dirs),
        Err(NginxError::Home(_))
    ));
    assert!(matches!(
        stage_nginx_config(&input, &dirs),
        Err(NginxError::Home(_))
    ));
    assert_eq!(tree(paths.home()), before, "nothing was written");
    // Closed, it goes through.
    write_nginx_config(&input, &FakePrivateDirs::new()).unwrap();
}

#[test]
fn installing_and_uninstalling_are_blocked_by_an_open_home_and_listing_is_not() {
    let (_tmp, paths) = home();
    let catalog = Catalog::embedded().unwrap();
    let target = Target::current().unwrap();
    let fetcher = FakeFetcher::new();
    let env = Environment {
        paths: &paths,
        catalog: &catalog,
        target,
        fetcher: &fetcher,
    };
    let dirs = open_home(&paths);
    let before = tree(paths.home());

    let error = install_version(
        &env,
        "php",
        "8.5",
        &InstallOptions::default(),
        &mut |_| {},
        &dirs,
    )
    .unwrap_err();
    assert!(matches!(error, ManageError::Home(_)), "{error}");
    let error = uninstall(&paths, "php", "8.4.26", &dirs).unwrap_err();
    assert!(matches!(error, ManageError::Home(_)), "{error}");
    assert_eq!(
        tree(paths.home()),
        before,
        "nothing was installed or removed"
    );

    // Reading is always allowed: this is how `list` and `doctor` tell the user what is wrong.
    let installed = list_installed(&paths).unwrap();
    assert_eq!(installed.len(), 1);
    assert_ne!(list_available(&paths, &catalog, target).unwrap().len(), 0);

    // Closed, the uninstall goes through.
    uninstall(&paths, "php", "8.4.26", &FakePrivateDirs::new()).unwrap();
    assert_eq!(list_installed(&paths).unwrap().len(), 0);
}

#[test]
fn installing_the_shims_is_blocked_by_an_open_home() {
    let (tmp, paths) = home();
    let exe = tmp.path().join("crisol-shim.exe");
    fs::write(&exe, b"MZ not a real program").unwrap();
    let error = install_shim_files(&paths, &exe, &open_home(&paths)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("will not go on"), "{error}");
    assert_eq!(fs::read_dir(paths.shims_dir()).unwrap().count(), 0);
}

#[test]
fn trusting_the_ca_is_blocked_by_an_open_home() {
    let (_tmp, paths) = home();
    let ca = ensure_ca(&paths, &FakePrivateDirs::new(), "test", SystemTime::now())
        .unwrap()
        .ca;
    let store = FakeTrustStore::new();
    let error = trust_ca(&store, &ca, &paths, &open_home(&paths)).unwrap_err();
    assert!(matches!(error, TrustError::Home(_)), "{error}");
    assert_eq!(store.certificates().unwrap(), Vec::<Vec<u8>>::new());
    // Closed, it is trusted.
    trust_ca(&store, &ca, &paths, &FakePrivateDirs::new()).unwrap();
    assert_eq!(store.certificates().unwrap().len(), 1);
}

#[test]
fn a_home_that_is_another_accounts_says_each_person_needs_their_own() {
    let (_tmp, paths) = home();
    let dirs = FakePrivateDirs::new();
    dirs.make_foreign(paths.home());
    let error = require_private_home(&paths, &dirs).unwrap_err();
    assert!(matches!(error, HomeError::OtherAccount { .. }), "{error}");
    let text = error.to_string();
    assert!(
        text.contains("another account") && text.contains("their own home"),
        "{text}"
    );
    assert!(text.contains("CRISOL_HOME"), "{text}");
    assert!(!text.contains("  "), "{text}");

    // Starting the daemon in it says the same, and creates nothing in it.
    let before = tree(paths.home());
    let error = paths.ensure_private_layout(&dirs).unwrap_err();
    assert!(error.to_string().contains("their own home"), "{error}");
    assert_eq!(tree(paths.home()), before);
}
