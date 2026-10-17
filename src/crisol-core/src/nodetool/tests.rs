use std::fs;

use tempfile::TempDir;

use super::*;

fn install(with_corepack: bool) -> (TempDir, CrisolPaths, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let dir = paths.bin_for("node", "24.22.0").unwrap();
    fs::create_dir_all(dir.join("node_modules").join("npm").join("bin")).unwrap();
    fs::write(dir.join("node.exe"), "MZ").unwrap();
    fs::write(dir.join("node_modules/npm/bin/npm-cli.js"), "").unwrap();
    fs::write(dir.join("node_modules/npm/bin/npx-cli.js"), "").unwrap();
    if with_corepack {
        fs::create_dir_all(dir.join("node_modules").join("corepack").join("dist")).unwrap();
        for name in ["corepack", "pnpm", "pnpx", "yarn", "yarnpkg"] {
            fs::write(
                dir.join(format!("node_modules/corepack/dist/{name}.js")),
                "",
            )
            .unwrap();
        }
    }
    (tmp, paths, dir)
}

fn none(_: &str) -> Option<OsString> {
    None
}

fn plan(command: Command, dir: &Path, paths: &CrisolPaths) -> Result<NodeLaunch, LaunchError> {
    plan_launch(command, dir, "24.22.0", paths, &none)
}

#[test]
fn node_runs_as_it_is() {
    let (_tmp, paths, dir) = install(true);
    let launch = plan(Command::Node, &dir, &paths).unwrap();
    assert_eq!(launch.program, dir.join("node.exe"));
    assert!(launch.leading.is_empty() && launch.env.is_empty());
}

#[test]
fn npm_and_npx_run_their_scripts_on_this_versions_node_without_a_batch_file() {
    let (_tmp, paths, dir) = install(true);
    for (command, script) in [(Command::Npm, "npm-cli.js"), (Command::Npx, "npx-cli.js")] {
        let launch = plan(command, &dir, &paths).unwrap();
        assert_eq!(launch.program, dir.join("node.exe"));
        assert_eq!(
            launch.leading,
            [dir.join("node_modules")
                .join("npm")
                .join("bin")
                .join(script)
                .into_os_string()]
        );
        assert!(launch.env.is_empty(), "npm needs nothing of Corepack");
    }
}

#[test]
fn corepack_and_the_managers_keep_their_downloads_in_the_cache_unless_the_user_chose() {
    let (_tmp, paths, dir) = install(true);
    for (command, script) in [
        (Command::Corepack, "corepack.js"),
        (Command::Pnpm, "pnpm.js"),
        (Command::Pnpx, "pnpx.js"),
        (Command::Yarn, "yarn.js"),
        (Command::Yarnpkg, "yarnpkg.js"),
    ] {
        let launch = plan(command, &dir, &paths).unwrap();
        assert_eq!(
            launch.leading,
            [dir.join("node_modules")
                .join("corepack")
                .join("dist")
                .join(script)
                .into_os_string()]
        );
        assert_eq!(
            launch.env,
            [(
                OsString::from(COREPACK_HOME),
                paths.cache_for("corepack").unwrap().into_os_string()
            )]
        );
    }
    let mine = |name: &str| (name == COREPACK_HOME).then(|| OsString::from("D:\\mine"));
    let launch = plan_launch(Command::Pnpm, &dir, "24.22.0", &paths, &mine).unwrap();
    assert!(launch.env.is_empty(), "the user's COREPACK_HOME stays");
}

#[test]
fn a_node_without_corepack_says_so_and_says_what_to_do() {
    let (_tmp, paths, dir) = install(false);
    let error = plan(Command::Yarn, &dir, &paths).unwrap_err();
    assert!(matches!(error, LaunchError::NoCorepack { .. }));
    let text = error.to_string();
    assert!(
        text.contains("Node 24.22.0 does not include Corepack"),
        "{text}"
    );
    assert!(
        text.contains("`yarn`") && text.contains("crisol use node"),
        "{text}"
    );
    // npm is not affected.
    assert!(plan(Command::Npm, &dir, &paths).is_ok());
}

#[test]
fn a_damaged_installation_names_the_missing_file() {
    let (_tmp, paths, dir) = install(true);
    fs::remove_file(dir.join("node_modules/npm/bin/npm-cli.js")).unwrap();
    let error = plan(Command::Npm, &dir, &paths).unwrap_err();
    assert!(error.to_string().contains("npm-cli.js"), "{error}");
    fs::remove_file(dir.join("node.exe")).unwrap();
    assert!(matches!(
        plan(Command::Node, &dir, &paths),
        Err(LaunchError::Damaged(_))
    ));
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn corepack_enable_and_disable_are_explained_and_write_nothing() {
    for verb in ["enable", "disable"] {
        let note = corepack_enable_note(&args(&[verb])).expect(verb);
        assert!(
            note.contains(verb) && note.contains("wrote nothing"),
            "{note}"
        );
    }
    assert!(corepack_enable_note(&args(&["--verbose", "enable", "pnpm"])).is_some());
}

#[test]
fn the_other_corepack_commands_and_an_explicit_folder_go_through() {
    for list in [
        &["prepare", "pnpm@9", "--activate"][..],
        &["use", "yarn@4"],
        &["--version"],
        &[],
        &["enable", "--install-directory", "D:\\bin"],
        &["disable", "--install-directory=D:\\bin"],
    ] {
        assert_eq!(corepack_enable_note(&args(list)), None, "{list:?}");
    }
}

#[test]
fn only_the_node_commands_are_node_commands() {
    for name in [
        "node", "npm", "npx", "corepack", "pnpm", "pnpx", "yarn", "yarnpkg",
    ] {
        assert!(is_node_command(OsStr::new(name)), "{name}");
    }
    for name in ["php", "composer", "python", ""] {
        assert!(!is_node_command(OsStr::new(name)), "{name}");
    }
}
