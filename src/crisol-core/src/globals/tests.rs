use std::fs;

use tempfile::TempDir;

use super::*;

/// A home with Node 22 and 24 installed and a shim program to copy.
fn home() -> (TempDir, CrisolPaths, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    for version in ["22.23.3", "24.22.0"] {
        fs::create_dir_all(paths.bin_for("node", version).unwrap()).unwrap();
    }
    let shim = tmp.path().join("crisol-shim.exe");
    fs::write(&shim, b"MZ shim one").unwrap();
    (tmp, paths, shim)
}

/// What `npm install -g` leaves in a global folder for a program: three files.
fn install_global(paths: &CrisolPaths, version: &str, name: &str) {
    let prefix = paths.node_global(version).unwrap();
    fs::create_dir_all(&prefix).unwrap();
    for file in [
        name.to_owned(),
        format!("{name}.cmd"),
        format!("{name}.ps1"),
    ] {
        fs::write(prefix.join(file), "x").unwrap();
    }
}

fn uninstall_global(paths: &CrisolPaths, version: &str, name: &str) {
    let prefix = paths.node_global(version).unwrap();
    for file in [
        name.to_owned(),
        format!("{name}.cmd"),
        format!("{name}.ps1"),
    ] {
        fs::remove_file(prefix.join(file)).unwrap();
    }
}

fn names(list: &[String]) -> Vec<&str> {
    list.iter().map(String::as_str).collect()
}

#[test]
fn the_programs_of_a_global_folder_are_the_cmd_files_npm_leaves() {
    let (_tmp, paths, _shim) = home();
    install_global(&paths, "24.22.0", "tsc");
    install_global(&paths, "24.22.0", "tsserver");
    let prefix = paths.node_global("24.22.0").unwrap();
    fs::create_dir_all(prefix.join("node_modules").join("typescript")).unwrap();
    assert_eq!(global_commands(&prefix), ["tsc", "tsserver"]);
    assert_eq!(
        global_commands(&paths.node_global("22.23.3").unwrap()),
        Vec::<String>::new()
    );
}

#[test]
fn only_plain_names_that_are_not_crisols_own_can_be_commands() {
    for good in ["tsc", "ts-node", "http-server", "pm2", "node-gyp", "a.b"] {
        assert!(valid_name(good), "{good}");
    }
    for bad in [
        "",
        ".hidden",
        "npm",
        "NPM",
        "node",
        "pnpm",
        "yarn",
        "corepack",
        "php",
        "composer",
        "crisol-shim",
        "crisol",
        "Crisol",
        "crisold",
        "crisol-elevate",
        "con",
        "nul",
        "a b",
        "a;b",
        "a%b",
        "a/b",
        "ends.",
        "x$",
    ] {
        assert!(!valid_name(bad), "{bad:?}");
    }
    assert!(!valid_name(&"a".repeat(65)));
}

#[test]
fn syncing_makes_one_shim_per_program_of_any_installed_version_and_lists_them() {
    let (_tmp, paths, shim) = home();
    install_global(&paths, "24.22.0", "tsc");
    install_global(&paths, "22.23.3", "tsc");
    install_global(&paths, "22.23.3", "pm2");
    // npm itself and a name nobody can run are not commands.
    install_global(&paths, "24.22.0", "npm");
    install_global(&paths, "24.22.0", "bad name");

    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert_eq!(names(&done.added), ["pm2", "tsc"]);
    assert_eq!(done.removed, Vec::<String>::new());
    assert_eq!(done.skipped.len(), 1, "{:?}", done.skipped);
    assert_eq!(done.skipped[0].0, "bad name");
    for name in ["tsc", "pm2"] {
        let file = paths.shims_dir().join(shim_file_name(name));
        assert_eq!(fs::read(file).unwrap(), b"MZ shim one");
        assert!(is_global_shim(&paths, name));
    }
    assert!(!is_global_shim(&paths, "npm") && !is_global_shim(&paths, "bad name"));
    assert_eq!(shimmed_globals(&paths), ["pm2", "tsc"]);

    // Doing it again changes nothing.
    let again = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert!(again.added.is_empty() && again.removed.is_empty());
}

#[test]
fn a_program_that_is_gone_from_every_version_loses_its_shim_and_the_list_goes_when_empty() {
    let (_tmp, paths, shim) = home();
    install_global(&paths, "24.22.0", "tsc");
    install_global(&paths, "22.23.3", "tsc");
    sync_global_shims(&paths, &shim, &|_| false).unwrap();

    uninstall_global(&paths, "24.22.0", "tsc");
    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert!(done.removed.is_empty(), "Node 22 still has it");

    uninstall_global(&paths, "22.23.3", "tsc");
    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert_eq!(names(&done.removed), ["tsc"]);
    assert!(!paths.shims_dir().join("tsc.exe").exists());
    assert!(!paths.shims_dir().join(MANIFEST).exists());
    assert!(!is_global_shim(&paths, "tsc"));
}

#[test]
fn a_program_of_a_node_version_that_is_not_installed_has_no_shim() {
    let (_tmp, paths, shim) = home();
    install_global(&paths, "20.20.2", "gulp");
    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert_eq!(done.added, Vec::<String>::new());
}

#[test]
fn a_file_with_that_name_that_is_not_ours_is_never_overwritten_or_deleted() {
    let (_tmp, paths, shim) = home();
    fs::create_dir_all(paths.shims_dir()).unwrap();
    let mine = paths.shims_dir().join("tsc.exe");
    fs::write(&mine, b"MZ somebody else's tsc").unwrap();
    install_global(&paths, "24.22.0", "tsc");
    install_global(&paths, "24.22.0", "pm2");

    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert_eq!(names(&done.added), ["pm2"]);
    assert_eq!(done.skipped[0].0, "tsc");
    assert!(done.skipped[0].1.contains("not Crisol's"));
    assert_eq!(fs::read(&mine).unwrap(), b"MZ somebody else's tsc");

    // Not in the list, so it is not removed when the program goes either.
    uninstall_global(&paths, "24.22.0", "tsc");
    sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert!(mine.is_file());
}

#[test]
fn a_new_shim_program_replaces_the_copies_of_the_old_one() {
    let (tmp, paths, shim) = home();
    install_global(&paths, "24.22.0", "tsc");
    sync_global_shims(&paths, &shim, &|_| false).unwrap();
    fs::write(&shim, b"MZ shim two").unwrap();
    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert!(done.added.is_empty(), "it was already there");
    assert_eq!(
        fs::read(paths.shims_dir().join("tsc.exe")).unwrap(),
        b"MZ shim two"
    );
    drop(tmp);
}

#[test]
fn removing_takes_the_listed_shims_and_nothing_else() {
    let (_tmp, paths, shim) = home();
    install_global(&paths, "24.22.0", "tsc");
    sync_global_shims(&paths, &shim, &|_| false).unwrap();
    let stranger = paths.shims_dir().join("my-tool.exe");
    fs::write(&stranger, "x").unwrap();

    assert_eq!(remove_global_shims(&paths).unwrap(), ["tsc"]);
    assert!(!paths.shims_dir().join("tsc.exe").exists());
    assert!(!paths.shims_dir().join(MANIFEST).exists());
    assert!(stranger.is_file());
    assert_eq!(remove_global_shims(&paths).unwrap(), Vec::<String>::new());
}

#[test]
fn a_program_is_found_only_under_the_version_that_has_it_and_the_message_says_where_it_is() {
    let (_tmp, paths, _shim) = home();
    install_global(&paths, "22.23.3", "pm2");
    assert!(find_global(&paths, "pm2", "22.23.3").is_some());
    assert!(find_global(&paths, "pm2", "24.22.0").is_none());
    assert!(find_global(&paths, "../pm2", "22.23.3").is_none());
    assert_eq!(installed_under(&paths, "pm2"), ["22.23.3"]);

    let message = not_installed_message(&paths, "pm2", "24.22.0");
    assert!(message.contains("`pm2` is not installed for Node 24.22.0, only for Node 22.23.3"));
    assert!(
        message.contains("crisol use node 22.23.3") && message.contains("npm install --global")
    );
    let gone = not_installed_message(&paths, "gulp", "24.22.0");
    assert!(gone.contains("is gone now"), "{gone}");
}

#[test]
fn a_program_called_like_one_in_the_path_gets_no_command() {
    let (_tmp, paths, shim) = home();
    install_global(&paths, "24.22.0", "ping");
    install_global(&paths, "24.22.0", "tsc");
    let done = sync_global_shims(&paths, &shim, &|name| name == "ping").unwrap();
    assert_eq!(names(&done.added), ["tsc"]);
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(done.skipped[0].0, "ping");
    assert!(done.skipped[0].1.contains("already in your PATH"));
    assert!(!paths.shims_dir().join("ping.exe").exists());
}

#[test]
fn names_that_differ_only_in_case_are_one_command_that_stays_while_either_is_there() {
    let (_tmp, paths, shim) = home();
    install_global(&paths, "24.22.0", "MyTool");
    install_global(&paths, "22.23.3", "mytool");
    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert_eq!(names(&done.added), ["mytool"]);
    assert!(is_global_shim(&paths, "MyTool") && is_global_shim(&paths, "mytool"));
    uninstall_global(&paths, "24.22.0", "MyTool");
    let done = sync_global_shims(&paths, &shim, &|_| false).unwrap();
    assert!(done.removed.is_empty(), "{done:?}");
    assert!(paths.shims_dir().join(shim_file_name("mytool")).is_file());
}
