//! What Rust does with the arguments when the program is a batch file (`npm.cmd`, `npx.cmd`).
//!
//! The pending test of phase 4. Since Rust 1.77.2 the standard library quotes the arguments of a
//! batch file with the rules of `cmd.exe` (`CVE-2024-24576`) and refuses the ones it cannot make
//! safe. This fixes what that means for `%`, `^`, `"`, spaces and line breaks, with a stand-in
//! `.cmd` that hands `%*` to a program that prints what it receives. It is why the Node shims
//! run `node.exe <script>` and never the batch files (`crisol_core::nodetool`); the same
//! arguments through the shim itself are tested in `shim.rs`.

// Test code: a failed `unwrap` is the failure report.
#![allow(clippy::unwrap_used)]
#![cfg(windows)]

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

fn echo_args() -> PathBuf {
    let tests = std::env::current_exe().unwrap();
    let debug = tests.parent().and_then(Path::parent).unwrap();
    debug.join("examples").join("echo_args.exe")
}

/// The arguments the program behind a `.cmd` received, or why Rust did not start it.
fn through_cmd(arg: &str) -> Result<Vec<String>, std::io::Error> {
    let tmp = tempfile::TempDir::new().unwrap();
    let cmd = tmp.path().join("probe.cmd");
    fs::write(&cmd, format!("@\"{}\" %*\r\n", echo_args().display())).unwrap();
    let output = Command::new(&cmd).arg(arg).output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("ARG:"))
        .map(|l| l.split_once(':').unwrap().1.to_owned())
        .collect())
}

#[test]
fn percent_caret_quotes_spaces_and_shell_characters_survive_a_batch_file() {
    for arg in [
        "plain",
        "with space",
        "50%",
        "%PATH%",
        "%%",
        "a^b",
        "^",
        "say \"hi\"",
        "q\"",
        "a&b",
        "a|b",
        "<x>",
        "!bang!",
        "trailing\\",
    ] {
        assert_eq!(through_cmd(arg).unwrap(), [arg], "{arg:?}");
    }
}

#[test]
fn an_argument_with_a_line_break_is_refused_with_an_error_that_says_what_it_is() {
    for arg in ["line one\nline two", "carriage\rreturn"] {
        let error = through_cmd(arg).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{arg:?}");
        assert_eq!(error.to_string(), "batch file arguments are invalid");
    }
}
