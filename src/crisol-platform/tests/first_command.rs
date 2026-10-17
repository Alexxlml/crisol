//! The first command of a new user, with real permissions on a temporary folder (review of the
//! manual verification of phase 8, M1). The folder's parent is opened to Authenticated Users with
//! `icacls`, as `C:\` is; nothing else on the machine is touched.

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::fs;
use std::process::Command;

use crisol_core::engine::Engine;
use crisol_core::paths::{require_private_home, CrisolPaths, PrivateDirs};
use crisol_platform::{default_deps, WindowsPrivateDirs};
use crisol_protocol::Call;

#[test]
fn park_as_the_first_command_leaves_a_private_home_under_an_open_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive-root");
    fs::create_dir(&drive).unwrap();
    let granted = Command::new("icacls")
        .arg(&drive)
        .args(["/grant", "*S-1-5-11:(OI)(CI)M"])
        .output()
        .unwrap();
    assert!(granted.status.success(), "icacls failed");
    // Anything made here the plain way is open, which is the trap.
    let plain = drive.join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(WindowsPrivateDirs.is_writable_by_others(&plain).unwrap());

    let paths = CrisolPaths::new(drive.join("Crisol")).unwrap();
    let engine = Engine::new(paths.clone(), default_deps().unwrap());
    engine.prepare_home().unwrap();
    let projects = drive.join("projects");
    fs::create_dir(&projects).unwrap();
    engine
        .handle(
            &Call::Park {
                path: projects.display().to_string(),
            },
            &mut |_| {},
        )
        .unwrap()
        .unwrap();

    assert!(!WindowsPrivateDirs
        .is_writable_by_others(paths.home())
        .unwrap());
    assert!(!WindowsPrivateDirs
        .is_writable_by_others(&paths.config_file())
        .unwrap());
    require_private_home(&paths, &WindowsPrivateDirs).unwrap();
}
