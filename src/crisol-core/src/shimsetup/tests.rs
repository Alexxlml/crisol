use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::paths::fake::FakePrivateDirs;
use crate::userpath::fake::FakeUserPath;

fn home() -> (TempDir, CrisolPaths, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let exe = tmp.path().join("crisol-shim.exe");
    fs::write(&exe, b"MZ shim").unwrap();
    (tmp, paths, exe)
}

#[test]
fn installing_copies_the_shim_once_per_command_and_removing_takes_them_back() {
    let (_tmp, paths, exe) = home();
    let dirs = FakePrivateDirs::new();
    let done = install_shims(&paths, &exe, &dirs).unwrap();
    assert_eq!(done.written.len(), SHIM_COMMANDS.len());
    for name in [
        "node.exe",
        "npm.exe",
        "corepack.exe",
        "pnpm.exe",
        "yarn.exe",
    ] {
        assert!(done.written.contains(&name.to_owned()), "{name}");
    }
    assert!(shims_present(&paths).iter().all(|(_, there)| *there));

    let again = install_shims(&paths, &exe, &dirs).unwrap();
    assert!(again.written.is_empty() && again.failed.is_empty());
    assert_eq!(again.unchanged.len(), SHIM_COMMANDS.len());

    assert_eq!(
        remove_shims_files(&paths).unwrap().len(),
        SHIM_COMMANDS.len()
    );
    assert!(shims_present(&paths).iter().all(|(_, there)| !*there));
}

#[test]
fn a_shim_program_other_accounts_can_replace_is_not_copied_anywhere() {
    let (tmp, paths, exe) = home();
    let dirs = FakePrivateDirs::new();
    dirs.expose(tmp.path());
    let error = install_shims(&paths, &exe, &dirs).unwrap_err();
    assert!(matches!(
        error,
        ShimsError::Program(ProgramError::Exposed { .. })
    ));
    assert!(!paths.shims_dir().exists(), "nothing was written");
    let missing = install_shims(
        &paths,
        &tmp.path().join("none.exe"),
        &FakePrivateDirs::new(),
    );
    assert!(matches!(
        missing,
        Err(ShimsError::Program(ProgramError::Missing { .. }))
    ));
}

#[test]
fn the_path_entry_is_added_and_removed_only_on_command_and_leaves_the_rest_alone() {
    let (_tmp, paths, _exe) = home();
    let store = FakeUserPath::with_user(r"C:\Tools;%USERPROFILE%\bin");
    let before = store.user();
    assert_eq!(path_add(&store, &paths).unwrap(), PathChange::Added);
    assert_eq!(
        path_add(&store, &paths).unwrap(),
        PathChange::AlreadyPresent
    );
    assert_eq!(path_remove(&store, &paths).unwrap(), PathChange::Removed);
    assert_eq!(path_remove(&store, &paths).unwrap(), PathChange::NotPresent);
    assert_eq!(store.user(), before, "the value is as it was");
}

#[test]
fn the_machine_node_is_reported_as_winning_and_the_way_out_is_named() {
    let (_tmp, paths, _exe) = home();
    let store = FakeUserPath::with_user(r"C:\Tools");
    store.set_system(r"C:\Windows;C:\Program Files\nodejs");
    let exists = |p: &Path| p == Path::new(r"C:\Program Files\nodejs\node.exe");
    path_add(&store, &paths).unwrap();
    let status = path_status(&store, &paths, &exists).unwrap();
    assert!(status.in_path);
    assert_eq!(status.shadows.len(), 1);
    assert!(status.shadows[0].in_system_path);

    let text = describe_shadows(&status.shadows).join("\n");
    assert!(text.contains(r"C:\Program Files\nodejs\node.exe"), "{text}");
    assert!(
        text.contains("machine PATH") && text.contains("never changes"),
        "{text}"
    );
    assert!(text.contains("crisol env"), "{text}");
}

#[test]
fn something_earlier_in_the_user_path_is_told_apart_from_the_machine_path() {
    let (_tmp, paths, _exe) = home();
    let store = FakeUserPath::with_user(r"C:\Tools");
    let exists = |p: &Path| p == Path::new(r"C:\Tools\php.exe");
    // Not added yet: counted as if it were first, so nothing wins.
    assert_eq!(path_status(&store, &paths, &exists).unwrap().shadows, []);
    store.set_user(Some(crate::userpath::PathValue {
        text: format!(r"C:\Tools;{}", paths.shims_dir().display()),
        expandable: true,
    }));
    let shadows = path_status(&store, &paths, &exists).unwrap().shadows;
    let text = describe_shadows(&shadows).join("\n");
    assert!(
        text.contains("earlier in your user PATH") && text.contains("crisol path add"),
        "{text}"
    );
}

#[test]
fn the_shell_is_guessed_from_the_variables_the_shells_leave() {
    let with = |names: &'static [&'static str]| {
        move |name: &str| names.contains(&name).then(|| "x".to_owned())
    };
    assert_eq!(detect_shell(&with(&["SHELL", "PROMPT"])), Shell::Bash);
    assert_eq!(detect_shell(&with(&["PROMPT"])), Shell::Cmd);
    assert_eq!(detect_shell(&with(&[])), Shell::PowerShell);
}

#[test]
fn each_shell_gets_a_line_that_changes_only_that_terminal() {
    let shims = Path::new(r"D:\My Crisol\it's\shims");
    let power = env_script(shims, Shell::PowerShell);
    assert!(
        power.contains(r"'D:\My Crisol\it''s\shims;' + $env:Path"),
        "{power}"
    );
    assert!(power.contains("-ne"), "twice is the same as once: {power}");
    assert_eq!(
        env_script(shims, Shell::Cmd),
        "set \"PATH=D:\\My Crisol\\it's\\shims;%PATH%\"\n"
    );
    let bash = env_script(Path::new(r"D:\Crisol\shims"), Shell::Bash);
    assert!(bash.contains("'/d/Crisol/shims'"), "{bash}");
    assert!(bash.contains("export PATH="), "{bash}");
    let quoted = env_script(shims, Shell::Bash);
    assert!(quoted.contains(r"it'\''s"), "{quoted}");
}
