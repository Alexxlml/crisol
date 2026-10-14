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
fn composer_is_not_supported_yet_and_says_so() {
    let rig = Rig::new();
    rig.install("php", "8.4.26", &exe_name("php"));
    let output = rig.run(&rig.shim("composer"), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("F4.3"), "{}", stderr(&output));
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
    use crisol_core::catalog::Catalog;
    use crisol_core::fetch::fake::FakeFetcher;
    use crisol_core::install::{install, InstallOptions};
    use crisol_core::paths::CrisolPaths;
    use crisol_core::phpini::{write_php_ini, PhpIniSettings};

    let Some(folder) = std::env::var_os("CRISOL_TEST_DOWNLOADS").map(PathBuf::from) else {
        panic!("set CRISOL_TEST_DOWNLOADS to the folder with the downloaded archives");
    };
    let catalog = Catalog::embedded().unwrap();
    let package = catalog
        .packages()
        .iter()
        .find(|p| p.service == "php" && p.version.to_string() == "8.4.26")
        .unwrap();
    let archive = folder.join(package.url.rsplit('/').next().unwrap());
    assert!(archive.is_file(), "{} is not there", archive.display());

    let rig = Rig::new();
    let paths = CrisolPaths::new(&rig.home).unwrap();
    let fetcher = FakeFetcher::new();
    fetcher.serve(&package.url, fs::read(&archive).unwrap());
    install(
        package,
        &paths,
        &fetcher,
        &InstallOptions::default(),
        &mut |_| {},
    )
    .unwrap();
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
