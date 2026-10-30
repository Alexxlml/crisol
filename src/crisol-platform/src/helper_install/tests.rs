// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]

use std::net::IpAddr;
use std::sync::Arc;

use crisol_core::dns::fake::FakeNrpt;
use crisol_core::dns::{enable, NrptRule, DEFAULT_DNS_ADDR};
use crisol_core::hosts::{DirectHosts, HostsManager, MemoryHosts};
use crisol_core::paths::fake::FakePrivateDirs;
use tempfile::TempDir;

use super::*;

const PROGRAM: &[u8] = b"MZ pretend this is crisol-elevate.exe";

/// A "Program Files" and a program to install, both in a temporary folder.
fn world() -> (TempDir, PathBuf, PathBuf, Arc<FakePrivateDirs>) {
    let tmp = tempfile::tempdir().unwrap();
    let program_files = tmp.path().join("Program Files");
    fs::create_dir(&program_files).unwrap();
    let source = tmp.path().join("build").join("crisol-elevate.exe");
    fs::create_dir(source.parent().unwrap()).unwrap();
    fs::write(&source, PROGRAM).unwrap();
    (tmp, program_files, source, Arc::new(FakePrivateDirs::new()))
}

#[test]
fn installing_copies_the_program_writes_its_manifest_and_verifies_it() {
    let (_tmp, program_files, source, dirs) = world();
    let installed = install_into(&program_files, &source, dirs.clone()).unwrap();
    assert_eq!(
        installed,
        program_files.join("Crisol").join("crisol-elevate.exe")
    );
    assert_eq!(fs::read(&installed).unwrap(), PROGRAM);

    // The manifest says which program it is without running it.
    let text = fs::read_to_string(program_files.join("Crisol").join(MANIFEST_FILE)).unwrap();
    let manifest = Manifest::parse(&text).unwrap();
    assert_eq!(manifest, Manifest::of(HELPER_VERSION, PROGRAM));
    HelperLocation::at(installed, dirs).verify().unwrap();

    // Nothing is left half done.
    let mut names: Vec<String> = fs::read_dir(program_files.join("Crisol"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["crisol-elevate.exe", MANIFEST_FILE]);
}

#[test]
fn installing_again_replaces_the_program_and_the_manifest_together() {
    let (_tmp, program_files, source, dirs) = world();
    install_into(&program_files, &source, dirs.clone()).unwrap();
    fs::write(&source, b"MZ the next build").unwrap();
    let installed = install_into(&program_files, &source, dirs.clone()).unwrap();
    assert_eq!(fs::read(&installed).unwrap(), b"MZ the next build");
    HelperLocation::at(installed, dirs).verify().unwrap();
}

#[test]
fn nothing_is_copied_into_a_folder_other_accounts_can_write() {
    let (_tmp, program_files, source, dirs) = world();
    dirs.make_user_writable(&program_files.join("Crisol"));
    let error = install_into(&program_files, &source, dirs).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("not administrators"), "{error}");
    assert!(!program_files
        .join("Crisol")
        .join("crisol-elevate.exe")
        .exists());
    assert!(!program_files.join("Crisol").join(MANIFEST_FILE).exists());
}

#[test]
fn a_missing_source_installs_nothing() {
    let (tmp, program_files, _source, dirs) = world();
    let error = install_into(&program_files, &tmp.path().join("nope.exe"), dirs).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!program_files.join("Crisol").join(MANIFEST_FILE).exists());
}

// ---- removing ------------------------------------------------------------------------------

fn crisol_rule() -> NrptRule {
    NrptRule {
        namespace: ".test".to_owned(),
        name_server: IpAddr::V4(DEFAULT_DNS_ADDR),
        comment: "Crisol".to_owned(),
    }
}

#[test]
fn removal_takes_out_the_rule_and_the_block_and_nothing_else() {
    let other = NrptRule {
        namespace: ".corp.example".to_owned(),
        name_server: "10.0.0.1".parse().unwrap(),
        comment: String::new(),
    };
    let nrpt = FakeNrpt::with(vec![other.clone()]);
    enable(&nrpt, "test", IpAddr::V4(DEFAULT_DNS_ADDR)).unwrap();
    let file = MemoryHosts::new("127.0.0.1 localhost\r\n");
    let hosts = DirectHosts::new(&file);
    hosts.set(&["a.test".to_owned()]).unwrap();

    let (report, failures) = remove_system_changes(&nrpt, &hosts, "test");
    assert_eq!(failures, Vec::<String>::new());
    assert_eq!(nrpt.current(), vec![other], "back to how it was");
    assert_eq!(file.content(), "127.0.0.1 localhost\r\n");
    assert_eq!(report.steps.len(), 2, "{report:?}");
    assert!(
        report.steps[0].contains("Removed the DNS rule"),
        "{report:?}"
    );
    assert!(report.steps[1].contains("block"), "{report:?}");
}

#[test]
fn removal_leaves_a_rule_of_another_tool_and_says_so() {
    let mut foreign = crisol_rule();
    foreign.comment = "corporate VPN".to_owned();
    let nrpt = FakeNrpt::with(vec![foreign.clone()]);
    let file = MemoryHosts::new("");
    let (report, failures) = remove_system_changes(&nrpt, &DirectHosts::new(&file), "test");
    assert_eq!(failures, Vec::<String>::new());
    assert_eq!(nrpt.current(), vec![foreign]);
    assert!(report.steps[0].contains("not Crisol's"), "{report:?}");
}

#[test]
fn removal_with_nothing_installed_is_not_an_error() {
    let nrpt = FakeNrpt::new();
    let file = MemoryHosts::new("127.0.0.1 localhost\r\n");
    let (report, failures) = remove_system_changes(&nrpt, &DirectHosts::new(&file), "test");
    assert_eq!(failures, Vec::<String>::new());
    assert!(
        report.steps[0].contains("no DNS rule of Crisol"),
        "{report:?}"
    );
    assert_eq!(file.writes(), 0);
}

#[test]
fn a_step_that_fails_is_reported_and_the_other_still_happens() {
    let nrpt = FakeNrpt::new();
    nrpt.fail_with("access denied");
    let file = MemoryHosts::new("# BEGIN CRISOL\r\n127.0.0.1 a.test\r\n# END CRISOL\r\n");
    let (report, failures) = remove_system_changes(&nrpt, &DirectHosts::new(&file), "test");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("access denied"), "{failures:?}");
    assert_eq!(file.content(), "", "the block went anyway");
    assert_eq!(report.steps.len(), 1);
}

#[test]
fn the_installation_folder_goes_except_the_program_that_is_running() {
    let (_tmp, program_files, source, dirs) = world();
    let installed = install_into(&program_files, &source, dirs).unwrap();
    let folder = program_files.join("Crisol");
    fs::create_dir(folder.join("leftover-folder")).unwrap();
    fs::write(folder.join("leftover-folder").join("x"), "x").unwrap();

    // Running from the installed copy: everything else goes, and the folder is reported.
    let left = remove_installation(&program_files, Some(&installed)).unwrap();
    assert_eq!(left, Some(folder.clone()));
    let names: Vec<String> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["crisol-elevate.exe"]);

    // Nothing running from it: the folder goes too.
    assert_eq!(remove_installation(&program_files, None).unwrap(), None);
    assert!(!folder.exists());
    // And again with nothing there.
    assert_eq!(remove_installation(&program_files, None).unwrap(), None);
}

#[test]
fn installing_and_removing_leave_the_program_files_folder_as_it_was() {
    let (_tmp, program_files, source, dirs) = world();
    let before: Vec<_> = fs::read_dir(&program_files)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    install_into(&program_files, &source, dirs).unwrap();
    remove_installation(&program_files, None).unwrap();
    let after: Vec<_> = fs::read_dir(&program_files)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn the_exit_codes_of_the_elevated_step_mean_one_thing_each() {
    use std::collections::BTreeSet;
    let all = [
        SetupFailure::Failed,
        SetupFailure::Refused,
        SetupFailure::Folder,
        SetupFailure::Copy,
        SetupFailure::Verify,
        SetupFailure::Rule,
        SetupFailure::ForeignRule,
        SetupFailure::NotInstalledCopy,
        SetupFailure::SourceOpen,
    ];
    let codes: BTreeSet<u8> = all.iter().map(|f| f.code()).collect();
    assert_eq!(codes.len(), all.len(), "no two failures share a code");
    for failure in all {
        assert_eq!(
            SetupFailure::from_code(u32::from(failure.code())),
            Some(failure)
        );
        assert!(!failure.message().contains("  "), "{failure:?}");
    }
    assert_eq!(SetupFailure::from_code(0), None);
    assert_eq!(SetupFailure::from_code(740), None);
}

#[test]
fn the_launcher_refuses_a_bad_suffix_or_a_missing_program_before_any_prompt() {
    let setup = WindowsSetup::new(r"C:\no\such\crisol-elevate.exe");
    let error = setup.install("com").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
    let error = setup.install("test").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    assert!(error.to_string().contains("next to crisol.exe"), "{error}");
}

/// The command a refused program carries.
fn fix_of(error: &io::Error) -> String {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ProgramError>())
    {
        Some(ProgramError::Exposed { fix, .. }) => fix.clone(),
        other => panic!("not a refused program: {other:?}"),
    }
}

#[test]
fn a_program_other_accounts_can_replace_is_not_installed() {
    let (_tmp, program_files, source, dirs) = world();
    dirs.expose(source.parent().unwrap());
    let error = install_into(&program_files, &source, dirs).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        error
            .get_ref()
            .is_some_and(<dyn std::error::Error + Send + Sync>::is::<ProgramError>),
        "{error}"
    );
    assert!(fix_of(&error).contains("(OI)(CI)"), "{error}");
    assert!(!program_files.join("Crisol").exists(), "nothing was made");
}

#[test]
fn the_manifest_hashes_the_bytes_that_were_read_and_written() {
    let (_tmp, program_files, source, dirs) = world();
    let installed = install_into(&program_files, &source, dirs).unwrap();
    let text = fs::read_to_string(program_files.join("Crisol").join(MANIFEST_FILE)).unwrap();
    assert_eq!(
        Manifest::parse(&text).unwrap().sha256,
        crisol_core::setup::sha256_hex(&fs::read(&installed).unwrap())
    );
}

#[test]
fn the_launcher_shows_no_prompt_for_a_program_others_can_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("crisol-elevate.exe");
    fs::write(&source, PROGRAM).unwrap();
    let dirs = Arc::new(FakePrivateDirs::new());
    dirs.expose(tmp.path());
    let error = WindowsSetup::with_dirs(&source, dirs)
        .install("test")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    assert!(fix_of(&error).starts_with("icacls "), "{error}");
    assert!(
        !error.to_string().contains("icacls"),
        "one line, the fix apart: {error}"
    );
}

#[test]
fn waiting_for_the_helper_folder_ends_when_it_goes_or_when_time_is_up() {
    let tmp = TempDir::new().unwrap();
    let folder = tmp.path().join("Crisol");
    fs::create_dir(&folder).unwrap();
    assert!(!wait_until_gone(&folder, Duration::from_millis(300)));
    let removing = folder.clone();
    let remover = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        fs::remove_dir(&removing).unwrap();
    });
    assert!(wait_until_gone(&folder, Duration::from_secs(10)));
    remover.join().unwrap();
    assert!(wait_until_gone(
        &tmp.path().join("never there"),
        Duration::ZERO
    ));
}

#[test]
fn programs_scheduled_for_deletion_go_a_moment_later() {
    let tmp = TempDir::new().unwrap();
    let files: Vec<PathBuf> = ["crisol.exe", "crisol shim.exe"]
        .iter()
        .map(|name| tmp.path().join(name))
        .collect();
    for file in &files {
        fs::write(file, "MZ").unwrap();
    }
    schedule_delete(&files).unwrap();
    assert!(
        files[0].exists(),
        "not at once: the program is still running"
    );
    for file in &files {
        assert!(
            wait_until_gone(file, Duration::from_secs(20)),
            "{}",
            file.display()
        );
    }
    assert!(schedule_delete(&[]).is_ok());
    let odd = schedule_delete(&[tmp.path().join("100%.exe")]).unwrap_err();
    assert_eq!(odd.kind(), io::ErrorKind::InvalidInput);
}
