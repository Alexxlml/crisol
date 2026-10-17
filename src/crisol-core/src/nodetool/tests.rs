use std::fs;

use tempfile::TempDir;

use super::*;

fn install(with_corepack: bool) -> (TempDir, CrisolPaths, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let dir = paths.bin_for("node", "24.22.0").unwrap();
    fs::create_dir_all(dir.join("node_modules/npm/bin")).unwrap();
    fs::write(dir.join("node.exe"), "MZ").unwrap();
    fs::write(dir.join("node_modules/npm/bin/npm-cli.js"), "").unwrap();
    fs::write(dir.join("node_modules/npm/bin/npx-cli.js"), "").unwrap();
    if with_corepack {
        fs::create_dir_all(dir.join("node_modules/corepack/dist")).unwrap();
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
    plan_with(command, dir, paths, &none, "")
}

fn plan_with(
    command: Command,
    dir: &Path,
    paths: &CrisolPaths,
    lookup: &dyn Fn(&str) -> Option<OsString>,
    user_npmrc: &str,
) -> Result<NodeLaunch, LaunchError> {
    plan_launch(&LaunchInput {
        command,
        install: dir,
        version: "24.22.0",
        paths,
        lookup,
        user_npmrc,
    })
}

fn env_of(launch: &NodeLaunch, name: &str) -> Option<PathBuf> {
    launch
        .env
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| PathBuf::from(v))
}

#[test]
fn node_runs_as_it_is_with_its_own_folder_in_front_of_the_path() {
    let (_tmp, paths, dir) = install(true);
    let launch = plan(Command::Node, &dir, &paths).unwrap();
    assert_eq!(launch.program, dir.join("node.exe"));
    assert_eq!(launch.leading, Vec::<OsString>::new());
    assert_eq!(
        launch.path_prepend,
        [paths.node_global("24.22.0").unwrap(), dir.clone()]
    );
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
        assert_eq!(
            env_of(&launch, COREPACK_HOME),
            None,
            "npm needs no Corepack"
        );
    }
}

#[test]
fn npm_gets_a_global_folder_per_version_and_a_cache_of_its_own() {
    let (_tmp, paths, dir) = install(true);
    for command in [Command::Node, Command::Npm, Command::Npx] {
        let launch = plan(command, &dir, &paths).unwrap();
        assert_eq!(
            env_of(&launch, NPM_PREFIX),
            Some(paths.home().join("data/node/24.22.0/global")),
            "{command:?}"
        );
        assert_eq!(
            env_of(&launch, NPM_CACHE),
            Some(paths.home().join("cache").join("npm"))
        );
    }
    // Another version has another folder.
    let other = plan_launch(&LaunchInput {
        command: Command::Npm,
        install: &dir,
        version: "22.23.3",
        paths: &paths,
        lookup: &none,
        user_npmrc: "",
    })
    .unwrap();
    assert_eq!(
        env_of(&other, NPM_PREFIX),
        Some(paths.home().join("data/node/22.23.3/global"))
    );
}

#[test]
fn what_the_user_chose_is_left_alone_in_the_environment_and_in_their_npmrc() {
    let (_tmp, paths, dir) = install(true);
    let mine = |name: &str| (name == "NPM_CONFIG_PREFIX").then(|| OsString::from("D:\\mine"));
    let launch = plan_with(Command::Npm, &dir, &paths, &mine, "").unwrap();
    assert_eq!(env_of(&launch, NPM_PREFIX), None);
    assert!(
        env_of(&launch, NPM_CACHE).is_some(),
        "the cache is still ours"
    );
    assert!(
        !launch
            .path_prepend
            .contains(&paths.node_global("24.22.0").unwrap()),
        "their prefix is not ours to put in the PATH"
    );
    assert!(launch.path_prepend.contains(&dir));

    let rc = "; a comment\nregistry = https://registry.example/\nprefix=D:\\npm-global\ncache = D:\\npm-cache\n";
    let launch = plan_with(Command::Npm, &dir, &paths, &none, rc).unwrap();
    assert_eq!(
        (env_of(&launch, NPM_PREFIX), env_of(&launch, NPM_CACHE)),
        (None, None)
    );

    // A comment or another key that looks the same does not count.
    let rc = "# prefix=D:\\x\nprefixes = 1\nregistry=https://example/cache=1\n";
    let launch = plan_with(Command::Npm, &dir, &paths, &none, rc).unwrap();
    assert!(env_of(&launch, NPM_PREFIX).is_some() && env_of(&launch, NPM_CACHE).is_some());
}

#[test]
fn a_copy_of_npm_installed_globally_wins_over_the_one_of_the_zip() {
    let (_tmp, paths, dir) = install(true);
    let global = paths
        .node_global("24.22.0")
        .unwrap()
        .join("node_modules")
        .join("npm")
        .join("bin");
    fs::create_dir_all(&global).unwrap();
    fs::write(global.join("npm-cli.js"), "").unwrap();
    let launch = plan(Command::Npm, &dir, &paths).unwrap();
    assert_eq!(launch.leading, [global.join("npm-cli.js").into_os_string()]);
    // npx has none of its own there: the one of the ZIP.
    let launch = plan(Command::Npx, &dir, &paths).unwrap();
    assert!(launch.leading[0].to_string_lossy().contains("24.22.0"));
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
            env_of(&launch, COREPACK_HOME),
            Some(paths.cache_for("corepack").unwrap())
        );
        assert_eq!(env_of(&launch, NPM_PREFIX), None, "they do not read npm's");
    }
    let mine = |name: &str| (name == COREPACK_HOME).then(|| OsString::from("D:\\mine"));
    let launch = plan_with(Command::Pnpm, &dir, &paths, &mine, "").unwrap();
    assert_eq!(env_of(&launch, COREPACK_HOME), None, "the user's stays");
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

    // Corepack installed globally for this version is what `npm install --global corepack` is for.
    let dist = paths
        .node_global("24.22.0")
        .unwrap()
        .join("node_modules")
        .join("corepack")
        .join("dist");
    fs::create_dir_all(&dist).unwrap();
    fs::write(dist.join("yarn.js"), "").unwrap();
    let launch = plan(Command::Yarn, &dir, &paths).unwrap();
    assert_eq!(launch.leading, [dist.join("yarn.js").into_os_string()]);
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
fn only_the_npm_commands_that_change_the_global_programs_count() {
    for list in [
        &["install", "-g", "typescript"][..],
        &["i", "--global", "typescript"],
        &["-g", "install", "typescript"],
        &["install", "typescript", "--location=global"],
        &["install", "typescript", "--location", "global"],
        &["uninstall", "-g", "typescript"],
        &["rm", "-g", "typescript"],
        &["update", "-g"],
        &["up", "-g"],
        &["link", "-g"],
        &["install", "-gD", "typescript"],
        &["--loglevel", "warn", "install", "-g", "typescript"],
    ] {
        assert!(changes_globals(&args(list)), "{list:?}");
    }
    for list in [
        &["install", "typescript"][..],
        &["install", "--location=project", "typescript"],
        &["list", "-g"],
        &["ls", "--global"],
        &["root", "-g"],
        &["run", "build", "--", "-g"],
        &["config", "get", "prefix"],
        &["view", "install", "-g"],
        &[],
    ] {
        assert!(!changes_globals(&args(list)), "{list:?}");
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
