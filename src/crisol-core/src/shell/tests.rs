use std::fs;

use tempfile::TempDir;

use super::*;

/// A home with all the shims in place.
fn home() -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("my home")).unwrap();
    let shims = paths.shims_dir();
    fs::create_dir_all(&shims).unwrap();
    for command in SHIM_COMMANDS {
        fs::write(shims.join(shim_file_name(command)), b"MZ").unwrap();
    }
    (tmp, paths)
}

#[test]
fn every_quote_powershell_reads_is_doubled() {
    assert_eq!(powershell_quote(r"C:\Crisol"), r"'C:\Crisol'");
    assert_eq!(powershell_quote("O'Brien"), "'O''Brien'");
    assert_eq!(powershell_quote("O’Brien"), "'O’’Brien'");
    assert_eq!(powershell_quote("a‘b‚c‛d"), "'a‘‘b‚‚c‛‛d'");
    assert_eq!(powershell_quote("\"x\" `y` $z"), "'\"x\" `y` $z'");
    assert_eq!(powershell_quote(""), "''");
}

fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(n, v)| (OsString::from(n), OsString::from(v)))
        .collect()
}

fn plan(
    paths: &CrisolPaths,
    terminal: Terminal,
    clean: bool,
    vars: &[(OsString, OsString)],
) -> Result<ShellLaunch, ShellError> {
    let front = vec![paths.shims_dir()];
    plan_shell(&ShellInput {
        paths,
        terminal,
        program: Path::new(r"C:\x\term.exe"),
        clean,
        front: &front,
        env: vars,
        cwd: None,
    })
}

fn set_of<'a>(launch: &'a ShellLaunch, name: &str) -> Option<&'a OsString> {
    launch.set.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

fn decode(encoded: &OsStr) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.to_string_lossy().as_bytes())
        .unwrap();
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).unwrap()
}

#[test]
fn the_variables_of_other_tools_are_taken_out_in_any_case() {
    for name in [
        "PHPRC",
        "phprc",
        "PHP_INI_SCAN_DIR",
        "NODE_PATH",
        "NODE_OPTIONS",
        "npm_config_prefix",
        "NPM_CONFIG_CACHE",
        "CRISOL_PHPRC",
        "CRISOL_NPM_PREFIX",
    ] {
        assert!(is_removed(name), "{name}");
    }
    for name in [
        "PATH",
        "COMPOSER_HOME",
        "NODE",
        "npm",
        "npm_config",
        "CRISOL_HOME",
    ] {
        assert!(!is_removed(name), "{name}");
    }
}

#[test]
fn the_terminal_is_the_asked_one_then_the_parent_then_the_first_installed() {
    let all = |t: Terminal| Some(PathBuf::from(format!(r"C:\{}.exe", t.name())));
    let (t, _, note) = choose_terminal(Some(Terminal::Cmd), None, Some("pwsh.exe"), &all).unwrap();
    assert_eq!((t, note), (Terminal::Cmd, None));
    let (t, _, note) = choose_terminal(
        None,
        None,
        Some(r"C:\Program Files\Git\usr\bin\bash.exe"),
        &all,
    )
    .unwrap();
    assert_eq!((t, note), (Terminal::Bash, None));
    let (t, _, _) = choose_terminal(None, None, Some("powershell.exe"), &all).unwrap();
    assert_eq!(t, Terminal::WindowsPowerShell);

    // Started from an editor: the first one installed, and it says so.
    let no_pwsh = |t: Terminal| (t != Terminal::Pwsh).then(|| PathBuf::from(r"C:\t.exe"));
    let (t, _, note) = choose_terminal(None, None, Some("Code.exe"), &no_pwsh).unwrap();
    assert_eq!(t, Terminal::WindowsPowerShell);
    assert!(note.unwrap().contains("--shell"));

    // The one asked for (or the one it came from) must exist: no silent swap.
    let error = choose_terminal(Some(Terminal::Pwsh), None, None, &no_pwsh).unwrap_err();
    assert!(matches!(error, ShellError::NotFound("pwsh")), "{error}");
    assert!(matches!(
        choose_terminal(None, None, None, &|_| None).unwrap_err(),
        ShellError::NoTerminal
    ));
}

#[test]
fn the_path_has_the_front_folders_first_and_keeps_the_rest_once() {
    let (_tmp, paths) = home();
    let shims = paths.shims_dir();
    let current = format!(
        r"C:\laragon\bin\php;{};C:\Program Files\Git\cmd",
        shims.display()
    );
    let launch = plan(&paths, Terminal::Cmd, false, &env(&[("Path", &current)])).unwrap();
    let path = set_of(&launch, "PATH")
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        path,
        format!(
            r"{};C:\laragon\bin\php;C:\Program Files\Git\cmd",
            shims.display()
        )
    );
    assert_eq!(set_of(&launch, INSIDE).unwrap(), "1");
}

#[test]
fn the_variables_of_other_tools_are_removed_from_the_terminal() {
    let (_tmp, paths) = home();
    let vars = env(&[
        ("PHPRC", r"C:\laragon\bin\php"),
        ("npm_config_prefix", r"C:\x"),
        ("COMPOSER_HOME", r"C:\y"),
    ]);
    let launch = plan(&paths, Terminal::Pwsh, false, &vars).unwrap();
    assert_eq!(launch.remove, ["PHPRC", "npm_config_prefix"]);
}

#[test]
fn powershell_gets_its_profile_on_the_command_line_and_no_profile_with_clean() {
    let (_tmp, paths) = home();
    let launch = plan(&paths, Terminal::Pwsh, false, &[]).unwrap();
    let args: Vec<String> = launch
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(&args[..3], ["-NoLogo", "-NoExit", "-EncodedCommand"]);
    let script = decode(&launch.args[3]);
    assert!(script.contains("(crisol) "), "{script}");
    assert!(script.contains("WindowTitle = 'Crisol'"), "{script}");
    assert!(script.contains("my home"), "{script}");
    assert!(
        !paths.etc_dir().join("shell").exists(),
        "no script file for PowerShell"
    );

    let clean = plan(&paths, Terminal::Pwsh, true, &[]).unwrap();
    assert_eq!(clean.args[1], "-NoProfile");
}

#[test]
fn windows_powershell_drops_the_module_folders_of_powershell_7() {
    let (_tmp, paths) = home();
    let modules = r"C:\Users\me\Documents\PowerShell\Modules;C:\Program Files\PowerShell\7\Modules;C:\Program Files\WindowsPowerShell\Modules;C:\WINDOWS\system32\WindowsPowerShell\v1.0\Modules";
    let launch = plan(
        &paths,
        Terminal::WindowsPowerShell,
        false,
        &env(&[("PSModulePath", modules)]),
    )
    .unwrap();
    assert_eq!(
        set_of(&launch, "PSModulePath").unwrap(),
        r"C:\Program Files\WindowsPowerShell\Modules;C:\WINDOWS\system32\WindowsPowerShell\v1.0\Modules"
    );
    let only_7 = plan(
        &paths,
        Terminal::WindowsPowerShell,
        false,
        &env(&[("PSModulePath", r"C:\Program Files\PowerShell\7\Modules")]),
    )
    .unwrap();
    assert!(only_7.remove.iter().any(|n| n == "PSModulePath"));
    // PowerShell 7 sorts its own out.
    let pwsh = plan(
        &paths,
        Terminal::Pwsh,
        false,
        &env(&[("PSModulePath", modules)]),
    )
    .unwrap();
    assert!(set_of(&pwsh, "PSModulePath").is_none());
}

#[test]
fn cmd_runs_its_profile_after_autorun_and_skips_autorun_with_clean() {
    let (_tmp, paths) = home();
    let launch = plan(&paths, Terminal::Cmd, false, &[]).unwrap();
    let file = paths.etc_dir().join("shell").join("crisol.cmd");
    assert_eq!(
        launch.args,
        [OsString::from("/k"), file.clone().into_os_string()]
    );
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("@set \"PHPRC=\""), "{text}");
    assert!(text.contains("@set \"PROMPT=(crisol) %PROMPT%\""), "{text}");
    assert!(
        !text.to_lowercase().contains("echo off"),
        "echo off would hide the prompt"
    );
    let clean = plan(&paths, Terminal::Cmd, true, &[]).unwrap();
    assert_eq!(clean.args[0], "/d");
}

#[test]
fn bash_reads_crisols_rcfile_which_loads_the_users_unless_clean() {
    let (_tmp, paths) = home();
    let launch = plan(&paths, Terminal::Bash, false, &[]).unwrap();
    assert_eq!(launch.args[0], "--rcfile");
    assert!(!launch.args[1].to_string_lossy().contains('\\'));
    assert_eq!(set_of(&launch, "CHERE_INVOKING").unwrap(), "1");
    assert!(set_of(&launch, "CRISOL_SHELL_CLEAN").is_none());
    let text = fs::read_to_string(paths.etc_dir().join("shell").join("crisol.bashrc")).unwrap();
    assert!(text.contains(". ~/.bashrc"), "{text}");
    assert!(text.contains("PS1=\"(crisol) $PS1\""), "{text}");
    assert!(text.contains("/my home/shims'"), "{text}");
    let clean = plan(&paths, Terminal::Bash, true, &[]).unwrap();
    assert_eq!(set_of(&clean, "CRISOL_SHELL_CLEAN").unwrap(), "1");
}

#[test]
fn every_profile_takes_back_the_names_an_alias_or_macro_of_the_user_had_taken() {
    let front = [PathBuf::from(r"C:\Crisol\shims")];
    let power = profile_text(Terminal::Pwsh, &front);
    assert!(
        power.contains("Alias:$__crisol_name") && power.contains("Function:$__crisol_name"),
        "{power}"
    );
    let cmd = profile_text(Terminal::Cmd, &front);
    assert!(
        cmd.contains("@doskey php=\r\n") && cmd.contains("@doskey psql=\r\n"),
        "{cmd}"
    );
    // Herd writes `alias php="php.bat"` into ~/.bash_profile.
    let bash = profile_text(Terminal::Bash, &front);
    assert!(bash.contains("unalias \"$__crisol_v\""), "{bash}");
    for name in ["php", "node", "npm", "composer", "mysql", "redis-cli"] {
        assert!(taken_back().contains(&name), "{name}");
    }
}

#[test]
fn a_shell_inside_a_shell_or_without_shims_is_refused() {
    let (_tmp, paths) = home();
    assert!(matches!(
        plan(&paths, Terminal::Cmd, false, &env(&[(INSIDE, "1")])),
        Err(ShellError::AlreadyInside)
    ));
    fs::remove_file(paths.shims_dir().join(shim_file_name("node"))).unwrap();
    assert!(matches!(
        plan(&paths, Terminal::Cmd, false, &[]),
        Err(ShellError::NoShims)
    ));
}

#[test]
fn the_clients_of_the_installed_databases_go_after_the_shims_the_enabled_first() {
    let (_tmp, paths) = home();
    for (service, version, client) in [
        ("mysql", "8.4.11", "bin"),
        ("mariadb", "11.8.9", "bin"),
        ("redis", "5.0.14.1", ""),
        ("mailpit", "1.31.6", ""),
    ] {
        let dir = paths.bin_for(service, version).unwrap().join(client);
        fs::create_dir_all(dir).unwrap();
    }
    let config = Config::parse("[services.mariadb]\nenabled = true\n")
        .unwrap()
        .config;
    let front = front_folders(&paths, &config);
    let shown: Vec<String> = front
        .iter()
        .map(|f| {
            f.strip_prefix(paths.home())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        shown,
        [
            "shims",
            "bin/mariadb/11.8.9/bin",
            "bin/mysql/8.4.11/bin",
            "bin/redis/5.0.14.1"
        ]
    );
}

#[test]
fn the_terminal_it_was_typed_in_beats_the_chosen_one_and_what_was_asked_beats_both() {
    let all = |t: Terminal| Some(PathBuf::from(format!(r"C:\{}.exe", t.name())));
    let bash = Some(r"C:\Program Files\Git\usr\bin\bash.exe");
    // Typed in Git Bash: a Git Bash, not a pwsh inside it.
    let (t, _, note) = choose_terminal(None, Some(Terminal::Pwsh), bash, &all).unwrap();
    assert_eq!((t, note), (Terminal::Bash, None));
    let (t, _, _) = choose_terminal(Some(Terminal::Cmd), Some(Terminal::Pwsh), bash, &all).unwrap();
    assert_eq!(t, Terminal::Cmd);
    // Not started from a terminal (the GUI, an editor): the chosen one.
    let (t, _, note) = choose_terminal(None, Some(Terminal::Pwsh), None, &all).unwrap();
    assert_eq!((t, note), (Terminal::Pwsh, None));
    let (t, _, note) = choose_terminal(None, Some(Terminal::Cmd), Some("Code.exe"), &all).unwrap();
    assert_eq!((t, note), (Terminal::Cmd, None));
}

#[test]
fn a_chosen_terminal_that_is_gone_is_said_and_the_automatic_one_opens() {
    let no_pwsh = |t: Terminal| (t != Terminal::Pwsh).then(|| PathBuf::from(r"C:\t.exe"));
    // From a terminal: that one opens, and the choice is not even looked at.
    let (t, _, note) =
        choose_terminal(None, Some(Terminal::Pwsh), Some("cmd.exe"), &no_pwsh).unwrap();
    assert_eq!((t, note), (Terminal::Cmd, None));
    // From nowhere: the first installed, and why.
    let (t, _, note) = choose_terminal(None, Some(Terminal::Pwsh), None, &no_pwsh).unwrap();
    assert_eq!(t, Terminal::WindowsPowerShell);
    let note = note.unwrap();
    assert!(
        note.contains("pwsh") && note.contains("opening powershell"),
        "{note}"
    );
}

#[test]
fn terminals_are_known_by_the_name_the_configuration_writes() {
    for terminal in Terminal::ALL {
        assert_eq!(Terminal::from_name(terminal.name()), Some(terminal));
    }
    assert_eq!(Terminal::from_name("wt"), None);
    assert_eq!(Terminal::from_name("PWSH"), None);
}

#[test]
fn the_preference_is_written_with_the_comments_kept_and_can_be_taken_out() {
    let (_tmp, paths) = home();
    let original = "# my settings\n[sites]\ntld = \"test\" # keep me\n";
    fs::write(paths.config_file(), original).unwrap();

    assert!(save_terminal_preference(&paths, Some(Terminal::Bash)).unwrap());
    let text = fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.starts_with(original), "{text}");
    assert!(
        text.contains("[shell]") && text.contains("terminal = \"bash\""),
        "{text}"
    );
    let config = Config::load_from(&paths).unwrap().config;
    assert_eq!(config.preferred_terminal(), Some(Terminal::Bash));

    // The same choice again changes nothing.
    assert!(!save_terminal_preference(&paths, Some(Terminal::Bash)).unwrap());
    // Automatic takes the key out and leaves the rest.
    assert!(save_terminal_preference(&paths, None).unwrap());
    let text = fs::read_to_string(paths.config_file()).unwrap();
    assert!(!text.contains("terminal"), "{text}");
    assert!(text.contains("# keep me"));
    assert_eq!(
        Config::load_from(&paths)
            .unwrap()
            .config
            .preferred_terminal(),
        None
    );
}

#[test]
fn a_name_that_is_not_a_terminal_is_refused_when_the_file_is_loaded() {
    let error = Config::parse("[shell]\nterminal = \"wt\"\n").unwrap_err();
    assert!(error.to_string().contains("shell.terminal"), "{error}");
    assert_eq!(
        Config::parse("[shell]\nterminal = \"cmd\"\n")
            .unwrap()
            .config
            .preferred_terminal(),
        Some(Terminal::Cmd)
    );
}

#[test]
fn the_folder_to_open_in_goes_to_the_launch() {
    let (_tmp, paths) = home();
    let front = vec![paths.shims_dir()];
    let folder = Path::new(r"D:\Proyectos\tienda");
    let launch = plan_shell(&ShellInput {
        paths: &paths,
        terminal: Terminal::Pwsh,
        program: Path::new(r"C:\x\pwsh.exe"),
        clean: false,
        front: &front,
        env: &[],
        cwd: Some(folder),
    })
    .unwrap();
    assert_eq!(launch.cwd.as_deref(), Some(folder));
    assert_eq!(plan(&paths, Terminal::Pwsh, false, &[]).unwrap().cwd, None);
}
