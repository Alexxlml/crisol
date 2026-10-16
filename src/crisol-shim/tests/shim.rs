//! The shim, run for real: the built `crisol-shim.exe` is copied as `php.exe`, `node.exe`…
//! into a temporary `shims/` folder, next to a temporary `CRISOL_HOME` whose "installed"
//! versions are copies of `echo_args`, a program that prints what it received.
//!
//! Nothing outside the temporary folders is read or changed. `CRISOL_HOME` is set only on the
//! child processes, and `CRISOL_PHP`/`CRISOL_NODE`/`PHPRC` are removed from them.

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const EXE: &str = env!("CARGO_BIN_EXE_crisol-shim");

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// The compiled `echo_args` example: `cargo test` builds examples next to the test binaries.
fn echo_args() -> PathBuf {
    let tests = std::env::current_exe().unwrap();
    let debug = tests.parent().and_then(Path::parent).unwrap();
    let path = debug.join("examples").join(exe_name("echo_args"));
    assert!(path.is_file(), "{} was not built", path.display());
    path
}

struct Rig {
    _tmp: TempDir,
    home: PathBuf,
    shims: PathBuf,
    work: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let shims = tmp.path().join("shims");
        let work = tmp.path().join("work");
        for dir in [&home, &shims, &work] {
            fs::create_dir_all(dir).unwrap();
        }
        Self {
            _tmp: tmp,
            home,
            shims,
            work,
        }
    }

    /// "Installs" `tool` `version`: a folder whose `<program>` is the echo program.
    fn install(&self, tool: &str, version: &str, program: &str) -> PathBuf {
        let dir = self.home.join("bin").join(tool).join(version);
        fs::create_dir_all(&dir).unwrap();
        fs::copy(echo_args(), dir.join(program)).unwrap();
        dir
    }

    /// A copy of the shim called `name` (`php`, `node`…).
    fn shim(&self, name: &str) -> PathBuf {
        let path = self.shims.join(exe_name(name));
        fs::copy(EXE, &path).unwrap();
        path
    }

    fn command(&self, shim: &Path) -> Command {
        let mut command = Command::new(shim);
        command
            .current_dir(&self.work)
            .env("CRISOL_HOME", &self.home)
            .env_remove("CRISOL_PHP")
            .env_remove("CRISOL_NODE")
            .env_remove("PHPRC")
            .env_remove("CRISOL_PHPRC")
            .env_remove("NODE_EXTRA_CA_CERTS")
            .env_remove("ECHO_EXIT")
            .env_remove("ECHO_STDIN");
        command
    }

    fn run(&self, shim: &Path, args: &[&str]) -> Output {
        self.command(shim).args(args).output().unwrap()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn args_seen(output: &Output) -> Vec<String> {
    stdout(output)
        .lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .map(|l| l.split_once(':').unwrap().1.to_owned())
        .collect()
}

#[test]
fn it_runs_the_resolved_version_and_hands_over_the_arguments() {
    let rig = Rig::new();
    rig.install("php", "8.3.35", &exe_name("php"));
    rig.install("php", "8.4.26", &exe_name("php"));
    fs::write(rig.work.join(".php-version"), "8.3\n").unwrap();
    let php = rig.shim("php");

    let output = rig.run(&php, &["-v"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("8.3.35"), "the pinned version ran: {text}");
    assert_eq!(args_seen(&output), ["-v"]);
    assert!(text.contains(&format!("CWD:{}", rig.work.display())) || text.contains("CWD:"));
}

#[test]
fn arguments_with_spaces_quotes_and_odd_characters_arrive_unchanged() {
    let rig = Rig::new();
    rig.install("php", "8.4.26", &exe_name("php"));
    let php = rig.shim("php");

    let tricky = [
        "plain",
        "with space",
        "\"quoted\"",
        "say \"hi\" there",
        "trailing backslash\\",
        r"C:\path with space\file.php",
        "--opt=a b",
        "-r",
        "echo 'x';",
        "a&b|c>d<e",
        "100%",
        "(paren)",
        "ünïcödé",
        "",
        "two  spaces",
    ];
    let output = rig.run(&php, &tricky);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(args_seen(&output), tricky);
}

#[test]
fn the_exit_code_of_the_real_program_is_the_exit_code_of_the_shim() {
    let rig = Rig::new();
    rig.install("php", "8.4.26", &exe_name("php"));
    let php = rig.shim("php");

    for code in [0, 1, 3, 42, 255] {
        let output = rig
            .command(&php)
            .env("ECHO_EXIT", code.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "exit code {code}");
    }
}

#[test]
fn standard_input_and_output_are_connected_through() {
    let rig = Rig::new();
    rig.install("php", "8.4.26", &exe_name("php"));
    let php = rig.shim("php");

    let mut child = rig
        .command(&php)
        .env("ECHO_STDIN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello\nworld\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        stdout(&output).contains("STDIN:hello\\nworld\\n"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn the_working_directory_is_the_callers() {
    let rig = Rig::new();
    rig.install("node", "22.5.0", &exe_name("node"));
    let node = rig.shim("node");
    let sub = rig.work.join("sub dir");
    fs::create_dir(&sub).unwrap();

    let output = rig.command(&node).current_dir(&sub).output().unwrap();
    let text = stdout(&output);
    let cwd = text.lines().find_map(|l| l.strip_prefix("CWD:")).unwrap();
    assert!(cwd.ends_with("sub dir"), "{cwd}");
}

#[test]
fn the_version_follows_the_directory() {
    let rig = Rig::new();
    rig.install("php", "8.2.28", &exe_name("php"));
    rig.install("php", "8.4.26", &exe_name("php"));
    let php = rig.shim("php");
    let a = rig.work.join("a");
    let b = rig.work.join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join(".crisol.toml"), "php = \"8.2\"\n").unwrap();
    fs::write(b.join("composer.json"), r#"{"require":{"php":"^8.3"}}"#).unwrap();

    let ran = |dir: &Path| stdout(&rig.command(&php).current_dir(dir).output().unwrap());
    assert!(ran(&a).contains("8.2.28"));
    assert!(ran(&b).contains("8.4.26"));
}

#[test]
fn the_environment_variable_overrides_for_one_terminal() {
    let rig = Rig::new();
    rig.install("php", "8.2.28", &exe_name("php"));
    rig.install("php", "8.4.26", &exe_name("php"));
    fs::write(rig.work.join(".php-version"), "8.4\n").unwrap();
    let php = rig.shim("php");

    let output = rig.command(&php).env("CRISOL_PHP", "8.2").output().unwrap();
    assert!(stdout(&output).contains("8.2.28"));
}

#[test]
fn the_generated_php_ini_is_handed_to_php_unless_the_user_chose_one() {
    let rig = Rig::new();
    rig.install("php", "8.4.26", &exe_name("php"));
    let ini_dir = rig.home.join("etc").join("php").join("8.4.26");
    fs::create_dir_all(&ini_dir).unwrap();
    fs::write(ini_dir.join("php.ini"), "; generated\n").unwrap();
    let php = rig.shim("php");

    let phprc = |output: &Output| {
        stdout(output)
            .lines()
            .find_map(|l| l.strip_prefix("PHPRC:"))
            .unwrap()
            .to_owned()
    };
    let output = rig.run(&php, &[]);
    assert_eq!(PathBuf::from(phprc(&output)), ini_dir);

    let own = rig.command(&php).env("PHPRC", "D:/mine").output().unwrap();
    assert_eq!(phprc(&own), "D:/mine", "the user's own PHPRC is respected");
}

#[test]
fn node_is_told_to_trust_the_local_ca_only_when_there_is_one_and_the_user_has_not_chosen() {
    let rig = Rig::new();
    rig.install("node", "22.23.3", &exe_name("node"));
    rig.install("php", "8.4.26", &exe_name("php"));
    let node = rig.shim("node");
    let php = rig.shim("php");
    let seen = |output: &Output| echoed_var(output, "NODE_EXTRA_CA_CERTS");

    // No CA yet: nothing is set.
    assert_eq!(seen(&rig.run(&node, &[])), "-");

    let ca = rig.home.join("etc").join("node").join("ca.pem");
    fs::create_dir_all(ca.parent().unwrap()).unwrap();
    fs::write(
        &ca,
        "-----BEGIN CERTIFICATE-----
",
    )
    .unwrap();
    assert_eq!(PathBuf::from(seen(&rig.run(&node, &[]))), ca);

    // The user's own value wins, and PHP is not told anything.
    let own = rig
        .command(&node)
        .env("NODE_EXTRA_CA_CERTS", "D:/mine.pem")
        .output()
        .unwrap();
    assert_eq!(seen(&own), "D:/mine.pem");
    assert_eq!(seen(&rig.run(&php, &[])), "-");
}

/// What a child prints for the variable `name` (`PHPRC`, `CRISOL_PHPRC`).
fn echoed_var(output: &Output, name: &str) -> String {
    stdout(output)
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{name}:")))
        .unwrap()
        .to_owned()
}

#[test]
fn a_nested_shim_replaces_the_php_ini_its_outer_shim_set() {
    // Review finding 3: `php` run from a script of another project must read ITS version's
    // php.ini, not the one of the PHP that is running the script.
    let rig = Rig::new();
    let mut ini_dirs = Vec::new();
    for version in ["8.3.35", "8.4.26"] {
        rig.install("php", version, &exe_name("php"));
        let dir = rig.home.join("etc").join("php").join(version);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("php.ini"),
            "; generated
",
        )
        .unwrap();
        ini_dirs.push(dir);
    }
    let (project_a, project_b) = (rig.work.join("a"), rig.work.join("b"));
    for (dir, version) in [(&project_a, "8.4"), (&project_b, "8.3")] {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(".php-version"),
            format!(
                "{version}
"
            ),
        )
        .unwrap();
    }
    let php = rig.shim("php");

    // The outer shim, in project A: sets PHPRC and records that it was its own doing.
    let outer = rig.command(&php).current_dir(&project_a).output().unwrap();
    let outer_rc = echoed_var(&outer, "PHPRC");
    assert_eq!(PathBuf::from(&outer_rc), ini_dirs[1]);
    assert_eq!(echoed_var(&outer, "CRISOL_PHPRC"), outer_rc);

    // A shim started by that PHP inherits both variables and runs in project B (8.3).
    let nested = rig
        .command(&php)
        .current_dir(&project_b)
        .env("PHPRC", &outer_rc)
        .env("CRISOL_PHPRC", &outer_rc)
        .output()
        .unwrap();
    assert!(stdout(&nested).contains("8.3.35"));
    assert_eq!(PathBuf::from(echoed_var(&nested, "PHPRC")), ini_dirs[0]);

    // The user's own PHPRC still wins, even when an outer shim also set one.
    let own = rig
        .command(&php)
        .current_dir(&project_b)
        .env("PHPRC", "D:/mine")
        .env("CRISOL_PHPRC", &outer_rc)
        .output()
        .unwrap();
    assert_eq!(echoed_var(&own, "PHPRC"), "D:/mine");

    // A version with no generated php.ini must not inherit the outer version's.
    fs::remove_file(ini_dirs[0].join("php.ini")).unwrap();
    let bare = rig
        .command(&php)
        .current_dir(&project_b)
        .env("PHPRC", &outer_rc)
        .env("CRISOL_PHPRC", &outer_rc)
        .output()
        .unwrap();
    assert_eq!(echoed_var(&bare, "PHPRC"), "-");
    assert_eq!(echoed_var(&bare, "CRISOL_PHPRC"), "-");
}

#[test]
fn a_version_that_is_not_installed_is_an_error_that_says_what_is() {
    let rig = Rig::new();
    rig.install("php", "8.4.26", &exe_name("php"));
    fs::write(rig.work.join(".php-version"), "7.4\n").unwrap();
    let php = rig.shim("php");

    let output = rig.run(&php, &["-v"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.starts_with("crisol: "), "{text}");
    assert!(
        text.contains("7.4") && text.contains("8.4.26") && text.contains(".php-version"),
        "{text}"
    );
    assert_eq!(stdout(&output), "", "nothing ran");
}

#[test]
fn with_nothing_installed_it_says_so() {
    let rig = Rig::new();
    let node = rig.shim("node");
    let output = rig.run(&node, &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no node version is installed"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_damaged_installation_is_reported_not_guessed() {
    let rig = Rig::new();
    fs::create_dir_all(rig.home.join("bin").join("php").join("8.4.26")).unwrap();
    let php = rig.shim("php");
    let output = rig.run(&php, &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("damaged"), "{}", stderr(&output));
}

#[test]
fn node_commands_follow_the_node_version() {
    let rig = Rig::new();
    let dir = rig.install("node", "22.5.0", &exe_name("node"));
    // npm and npx are batch files on Windows; any file with the right name will do here.
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    assert!(dir.join(exe_name("node")).is_file());
    let output = rig.run(&rig.shim("npm"), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(npm),
        "npm.cmd is missing here: {}",
        stderr(&output)
    );
}

#[test]
fn composer_runs_the_phar_with_the_php_that_applies_to_the_directory() {
    let rig = Rig::new();
    rig.install("php", "8.2.28", &exe_name("php"));
    rig.install("php", "8.4.26", &exe_name("php"));
    let old = rig.home.join("bin").join("composer").join("2.7.0");
    let new = rig.home.join("bin").join("composer").join("2.10.3");
    for dir in [&old, &new] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("composer.phar"), "<?php").unwrap();
    }
    fs::write(rig.work.join(".php-version"), "8.2\n").unwrap();
    let composer = rig.shim("composer");

    let output = rig.run(&composer, &["install", "--no-dev", "a b"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("8.2.28"),
        "the PHP of the project ran: {text}"
    );
    let args = args_seen(&output);
    assert_eq!(args.len(), 4, "{args:?}");
    assert!(
        args[0].ends_with("composer.phar") && args[0].contains("2.10.3"),
        "the newest Composer is the one run: {args:?}"
    );
    assert_eq!(&args[1..], ["install", "--no-dev", "a b"]);
}

#[test]
fn composer_without_the_phar_installed_says_how_to_get_it() {
    let rig = Rig::new();
    rig.install("php", "8.4.26", &exe_name("php"));
    let output = rig.run(&rig.shim("composer"), &[]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("no composer version is installed"), "{text}");
    assert!(text.contains("crisol install composer"), "{text}");
}

#[test]
fn composer_without_a_php_says_so_too() {
    let rig = Rig::new();
    let dir = rig.home.join("bin").join("composer").join("2.10.3");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("composer.phar"), "<?php").unwrap();
    let output = rig.run(&rig.shim("composer"), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no php version is installed"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn run_under_its_own_name_it_explains_itself() {
    let rig = Rig::new();
    let own = rig.shims.join(exe_name("crisol-shim"));
    fs::copy(EXE, &own).unwrap();

    let version = rig.run(&own, &["--version"]);
    assert!(version.status.success());
    assert!(stdout(&version).starts_with("crisol-shim "));

    let output = rig.run(&own, &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("php, node, npm, npx or composer"));
}

/// The acceptance figure of F4.2: the shim adds well under ~20 ms to a command. Measured as
/// the median of many runs of the shim against the same program run directly, so that the
/// cost of starting a process cancels out.
///
/// ```text
/// cargo build --release --examples -p crisol-shim   # the stand-in program is an example
/// cargo test --release -p crisol-shim shim_overhead -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a timing benchmark: run it on request, in release mode"]
fn shim_overhead_is_small() {
    let rig = Rig::new();
    let dir = rig.install("php", "8.4.26", &exe_name("php"));
    fs::write(rig.work.join(".php-version"), "8.4\n").unwrap();
    // A realistic tree: the version file is a few directories up, and a git folder is not.
    let deep = rig.work.join("a").join("b").join("c").join("d");
    fs::create_dir_all(&deep).unwrap();
    let php = rig.shim("php");
    let direct = dir.join(exe_name("php"));

    let median = |program: &Path| {
        let mut times: Vec<f64> = (0..60)
            .map(|_| {
                let start = std::time::Instant::now();
                let output = rig.command(program).current_dir(&deep).output().unwrap();
                assert!(output.status.success());
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        times[times.len() / 2]
    };
    let _warm = median(&direct);
    let (plain, through) = (median(&direct), median(&php));
    let overhead = through - plain;
    println!("direct {plain:.1} ms, through the shim {through:.1} ms, overhead {overhead:.1} ms");
    assert!(overhead < 20.0, "the shim adds {overhead:.1} ms");
}

/// Installs real, already downloaded packages (`CRISOL_TEST_DOWNLOADS`) into the rig's
/// temporary home, through the real installer, and returns the paths.
fn install_downloaded(rig: &Rig, wanted: &[(&str, &str)]) -> crisol_core::paths::CrisolPaths {
    use crisol_core::catalog::Catalog;
    use crisol_core::fetch::fake::FakeFetcher;
    use crisol_core::install::{install, InstallOptions};

    let Some(folder) = std::env::var_os("CRISOL_TEST_DOWNLOADS").map(PathBuf::from) else {
        panic!("set CRISOL_TEST_DOWNLOADS to the folder with the downloaded files");
    };
    let catalog = Catalog::embedded().unwrap();
    let paths = crisol_core::paths::CrisolPaths::new(&rig.home).unwrap();
    let fetcher = FakeFetcher::new();
    for (service, version) in wanted {
        let package = catalog
            .packages()
            .iter()
            .find(|p| p.service == *service && p.version.to_string() == *version)
            .unwrap();
        let file = folder.join(package.url.rsplit('/').next().unwrap());
        assert!(file.is_file(), "{} is not there", file.display());
        fetcher.serve(&package.url, fs::read(&file).unwrap());
        install(
            package,
            &paths,
            &fetcher,
            &InstallOptions::default(),
            &mut |_| {},
        )
        .unwrap();
    }
    paths
}

/// The whole chain with the real thing: an official PHP is installed, its `php.ini` is
/// generated, and the shim runs it. Arguments with quotes arrive intact, the exit code comes
/// back, and the generated `php.ini` is the one PHP reads (no start-up warnings, the
/// extensions are loaded).
///
/// ```text
/// CRISOL_TEST_DOWNLOADS=<folder with php-8.4.26-nts-Win32-vs17-x64.zip> cargo test -p crisol-shim real_php -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS pointing at the downloaded official PHP archive (Windows)"]
fn real_php_runs_through_the_shim() {
    use crisol_core::phpini::{write_php_ini, PhpIniSettings};

    let rig = Rig::new();
    let paths = install_downloaded(&rig, &[("php", "8.4.26")]);
    write_php_ini(&paths, "8.4.26", &PhpIniSettings::default()).unwrap();
    let php = rig.shim("php");

    // Arguments with quotes and spaces come back exactly, and so does the exit code.
    let code = r"echo json_encode(array_slice($argv, 1)); exit(7);";
    let tricky = ["a b", "say \"hi\"", "back\\slash\\", "", "100%"];
    let mut args = vec!["-r", code, "--"];
    args.extend(tricky);
    let output = rig.run(&php, &args);
    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    let echoed: Vec<String> = serde_json_like(&stdout(&output));
    assert_eq!(echoed, tricky);

    // The generated php.ini is in use: every extension loads without a warning.
    let output = rig.run(&php, &["-m"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stderr(&output).trim(), "", "start-up warnings");
    let modules = stdout(&output).to_lowercase();
    for extension in ["curl", "mbstring", "openssl", "pdo_sqlite", "zend opcache"] {
        assert!(
            modules.lines().any(|l| l.trim() == extension),
            "{extension} in:\n{modules}"
        );
    }
    let output = rig.run(&php, &["--ini"]);
    let text = stdout(&output).replace('\\', "/").to_lowercase();
    assert!(
        text.contains("etc/php/8.4.26/php.ini"),
        "the generated file is loaded:\n{text}"
    );
}

/// PHP 7.4 (ADR-0012), the old line kept for old projects, with the generated `php.ini` and the
/// CA bundle: it starts without warnings, loads its extensions and cURL reaches an HTTPS site.
///
/// Needs the network (one GET to getcomposer.org) and, in `CRISOL_TEST_DOWNLOADS`,
/// `php-7.4.33-nts-Win32-vc15-x64.zip` and `cacert-2026-09-25.pem`.
///
/// ```text
/// CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-shim real_php_7_4 -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs the network and the downloaded PHP 7.4 archive and CA bundle (Windows)"]
fn real_php_7_4_runs_with_the_generated_ini_and_bundle() {
    use crisol_core::cacert::write_php_cacert;
    use crisol_core::phpini::{write_php_ini, PhpIniSettings};
    use crisol_platform::WindowsRootCertificates;

    let rig = Rig::new();
    let paths = install_downloaded(&rig, &[("php", "7.4.33"), ("cacert", "2026.9.25")]);
    write_php_cacert(&paths, &WindowsRootCertificates)
        .unwrap()
        .unwrap();
    let written = write_php_ini(&paths, "7.4.33", &PhpIniSettings::default()).unwrap();
    assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    let php = rig.shim("php");

    let output = rig.run(&php, &["-v"]);
    assert!(
        stdout(&output).starts_with("PHP 7.4.33"),
        "{}",
        stdout(&output)
    );

    let output = rig.run(&php, &["-m"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stderr(&output).trim(), "", "start-up warnings");
    let modules = stdout(&output).to_lowercase();
    for extension in ["curl", "mbstring", "openssl", "pdo_sqlite", "zend opcache"] {
        assert!(
            modules.lines().any(|l| l.trim() == extension),
            "{extension} in:
{modules}"
        );
    }

    let script = r"
        $c = curl_init('https://getcomposer.org/versions');
        curl_setopt_array($c, [CURLOPT_RETURNTRANSFER => true, CURLOPT_TIMEOUT => 30]);
        $body = curl_exec($c);
        echo $body === false ? 'FAIL: ' . curl_error($c) : 'OK ' . curl_getinfo($c, CURLINFO_HTTP_CODE);
    ";
    let output = rig.run(&php, &["-r", script]);
    assert_eq!(
        stdout(&output).trim(),
        "OK 200",
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let streams =
        r"echo @file_get_contents('https://getcomposer.org/versions') === false ? 'FAIL' : 'OK';";
    assert_eq!(stdout(&rig.run(&php, &["-r", streams])).trim(), "OK");
}

/// Reads a JSON array of strings, which is all the test needs (no JSON crate in this one).
fn serde_json_like(text: &str) -> Vec<String> {
    let body = text.trim().trim_start_matches('[').trim_end_matches(']');
    let mut out = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut item = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => item.push('\n'),
                    Some('/') => item.push('/'),
                    Some(other) => item.push(other),
                    None => break,
                },
                other => item.push(other),
            }
        }
        out.push(item);
    }
    out
}

/// The problem ADR-0011 exists for: with the generated CA bundle, PHP's cURL can talk HTTPS.
/// An official PHP and Mozilla's bundle are installed, the bundle is built together with the
/// roots of Windows, and `curl_exec` is run through the shim against a public site. The same
/// request without the bundle is made first, to show that it is the bundle that fixes it.
///
/// Needs the network (one GET to getcomposer.org) and, in `CRISOL_TEST_DOWNLOADS`, both
/// `php-8.4.26-nts-Win32-vs17-x64.zip` and `cacert-2026-09-25.pem`.
///
/// ```text
/// CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-shim real_curl -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs the network and the downloaded PHP archive and CA bundle (Windows)"]
fn real_curl_trusts_https_with_the_generated_bundle() {
    use crisol_core::cacert::write_php_cacert;
    use crisol_core::phpini::{write_php_ini, PhpIniSettings};
    use crisol_platform::WindowsRootCertificates;

    let rig = Rig::new();
    let paths = install_downloaded(&rig, &[("php", "8.4.26"), ("cacert", "2026.9.25")]);
    let php = rig.shim("php");
    let script = r"
        $c = curl_init('https://getcomposer.org/versions');
        curl_setopt_array($c, [CURLOPT_RETURNTRANSFER => true, CURLOPT_TIMEOUT => 30]);
        $body = curl_exec($c);
        echo $body === false ? 'FAIL: ' . curl_error($c) : 'OK ' . curl_getinfo($c, CURLINFO_HTTP_CODE);
    ";

    // 1. Without a bundle: the php.ini has no CA file and cURL cannot verify the site.
    let without = write_php_ini(&paths, "8.4.26", &PhpIniSettings::default()).unwrap();
    assert!(
        without.warnings.iter().any(|w| w.contains("CA bundle")),
        "{:?}",
        without.warnings
    );
    let output = rig.run(&php, &["-r", script]);
    assert!(
        stdout(&output).contains("FAIL"),
        "expected the known failure: {}",
        stdout(&output)
    );

    // 2. With it: Mozilla's bundle plus the roots of Windows, and the php.ini points at it.
    let bundle = write_php_cacert(&paths, &WindowsRootCertificates)
        .unwrap()
        .unwrap();
    assert_eq!(bundle.bundle_version, "2026.9.25");
    eprintln!(
        "bundle: {} certificate(s) added from Windows, warnings {:?}",
        bundle.added, bundle.warnings
    );
    let with = write_php_ini(&paths, "8.4.26", &PhpIniSettings::default()).unwrap();
    assert!(
        with.changed && with.warnings.is_empty(),
        "{:?}",
        with.warnings
    );
    let output = rig.run(&php, &["-r", script]);
    assert_eq!(
        stdout(&output).trim(),
        "OK 200",
        "{}{}",
        stdout(&output),
        stderr(&output)
    );

    // 3. PHP's own streams keep working with the same file.
    let streams =
        r"echo @file_get_contents('https://getcomposer.org/versions') === false ? 'FAIL' : 'OK';";
    assert_eq!(stdout(&rig.run(&php, &["-r", streams])).trim(), "OK");
}

/// Composer for real, through the shim: `composer.phar` runs on the resolved PHP and can talk
/// to Packagist over HTTPS **with and without** Crisol's CA bundle. That settles the open
/// question of ADR-0011: Composer ships its own certificates (`composer/ca-bundle`) and does
/// not need the bundle that PHP's plain cURL does.
///
/// Needs the network (a read-only query to packagist.org) and, in `CRISOL_TEST_DOWNLOADS`,
/// `php-8.4.26-nts-Win32-vs17-x64.zip`, `composer.phar` and `cacert-2026-09-25.pem`. Composer's
/// home and cache are pointed at the temporary folder: nothing of the user's is read or written.
///
/// ```text
/// CRISOL_TEST_DOWNLOADS=<folder> cargo test -p crisol-shim real_composer -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs the network and the downloaded PHP archive, composer.phar and CA bundle (Windows)"]
fn real_composer_runs_through_the_shim_with_and_without_the_ca_bundle() {
    use crisol_core::cacert::write_php_cacert;
    use crisol_core::phpini::{write_php_ini, PhpIniSettings};
    use crisol_platform::WindowsRootCertificates;

    let rig = Rig::new();
    let paths = install_downloaded(
        &rig,
        &[
            ("php", "8.4.26"),
            ("composer", "2.10.3"),
            ("cacert", "2026.9.25"),
        ],
    );
    let composer = rig.shim("composer");
    let composer_home = rig.work.join("composer-home");
    let run = |args: &[&str]| {
        rig.command(&composer)
            .env("COMPOSER_HOME", &composer_home)
            .env("COMPOSER_CACHE_DIR", composer_home.join("cache"))
            .env("COMPOSER_NO_INTERACTION", "1")
            .env("COMPOSER_DISABLE_NETWORK", "")
            .args(args)
            .output()
            .unwrap()
    };

    // It runs, on the PHP Crisol resolved, with the generated php.ini.
    write_php_ini(&paths, "8.4.26", &PhpIniSettings::default()).unwrap();
    let output = run(&["--version", "--no-ansi"]);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("Composer version 2.10.3"),
        "{}",
        stdout(&output)
    );
    // `show -p` lists the platform packages: which PHP, and with which extensions.
    let platform = run(&["show", "-p", "--no-ansi"]);
    assert!(
        platform.status.success(),
        "{}{}",
        stdout(&platform),
        stderr(&platform)
    );
    let text = stdout(&platform);
    assert!(
        text.lines()
            .any(|l| l.starts_with("php ") && l.contains("8.4.26")),
        "{text}"
    );
    assert!(
        text.contains("ext-curl") && text.contains("ext-openssl"),
        "{text}"
    );

    // HTTPS to Packagist, first with no CA bundle in the php.ini (cURL would fail there).
    let query = ["show", "--all", "--no-ansi", "psr/log"];
    let without = run(&query);
    eprintln!(
        "without the bundle: exit {:?}\n{}{}",
        without.status.code(),
        stdout(&without)
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n"),
        stderr(&without)
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        without.status.success(),
        "{}{}",
        stdout(&without),
        stderr(&without)
    );
    assert!(stdout(&without).contains("psr/log"), "{}", stdout(&without));

    // And with Crisol's bundle in place.
    write_php_cacert(&paths, &WindowsRootCertificates)
        .unwrap()
        .unwrap();
    write_php_ini(&paths, "8.4.26", &PhpIniSettings::default()).unwrap();
    fs::remove_dir_all(&composer_home).unwrap();
    let with = run(&query);
    assert!(with.status.success(), "{}{}", stdout(&with), stderr(&with));
    assert!(stdout(&with).contains("psr/log"), "{}", stdout(&with));
}

/// Review of phase 6, V1: a copy of the CA that other accounts could have replaced is not given to
/// Node. The folder is opened to "Authenticated Users" with `icacls`, like a drive root would,
/// inside this test's own temporary folder.
#[cfg(windows)]
#[test]
fn node_is_not_told_about_a_copy_other_accounts_can_modify() {
    let rig = Rig::new();
    rig.install("node", "22.23.3", &exe_name("node"));
    let node = rig.shim("node");
    let folder = rig.home.join("etc").join("node");
    fs::create_dir_all(&folder).unwrap();
    let ca = folder.join("ca.pem");
    fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
    let seen = |output: &Output| echoed_var(output, "NODE_EXTRA_CA_CERTS");
    assert_eq!(
        PathBuf::from(seen(&rig.run(&node, &[]))),
        ca,
        "private: passed on"
    );

    let icacls = |args: &[&str]| {
        let output = Command::new("icacls")
            .arg(&folder)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "icacls failed");
    };
    icacls(&["/grant", "*S-1-5-11:(OI)(CI)M"]);
    let open = rig.run(&node, &[]);
    assert_eq!(seen(&open), "-", "open to other accounts: not passed on");
    let warning = stderr(&open);
    assert_eq!(warning.lines().count(), 1, "one line only: {warning}");
    assert!(
        warning.contains("other accounts") && warning.contains("icacls"),
        "{warning}"
    );
    assert!(warning.contains("node"), "it names the path: {warning}");
    // For a folder the command carries the inheritance flags, names the user by SID (it must
    // work in PowerShell too, where %USERNAME% is not a thing), and has no cmd-only syntax.
    assert!(warning.contains("(OI)(CI)F"), "{warning}");
    assert!(warning.contains("/inheritance:r /grant:r"), "{warning}");
    assert!(
        warning.contains("*S-1-5-18:") && warning.contains("*S-1-5-32-544:"),
        "{warning}"
    );
    assert!(!warning.contains('%'), "{warning}");

    // Closed again, it is passed on again.
    icacls(&["/remove", "*S-1-5-11"]);
    assert_eq!(PathBuf::from(seen(&rig.run(&node, &[]))), ca);

    // Only the file open (the folder is private): the command is the file's, with no
    // inheritance flags.
    let output = Command::new("icacls")
        .arg(&ca)
        .args(["/grant", "*S-1-5-11:M"])
        .output()
        .unwrap();
    assert!(output.status.success(), "icacls on the file failed");
    let open = rig.run(&node, &[]);
    assert_eq!(seen(&open), "-");
    let warning = stderr(&open);
    assert_eq!(warning.lines().count(), 1, "one line only: {warning}");
    assert!(warning.contains("ca.pem"), "names the file: {warning}");
    assert!(warning.contains(":F\""), "{warning}");
    assert!(
        !warning.contains("(OI)"),
        "no inheritance flags on a file: {warning}"
    );
}
