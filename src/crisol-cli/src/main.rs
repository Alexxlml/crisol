use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

use crisol_cli::render::Style;
use crisol_cli::{run, Context};

/// How a stream should look (ADR-0028). The window's width comes from the console; in a
/// pseudo-terminal (Git Bash) from `COLUMNS` when the shell exports it, else 80.
fn style_of(stream: crisol_platform::Stream, is_terminal: bool) -> Style {
    if !is_terminal {
        return Style::plain();
    }
    let width = crisol_platform::console_width(stream)
        .or_else(|| std::env::var("COLUMNS").ok()?.trim().parse().ok())
        .unwrap_or(80);
    // One column short of the window: the classic console wraps as soon as a line fills the
    // last column, and the newline after it would then leave an empty line under each row.
    let width = width.saturating_sub(1);
    Style::detect(
        crisol_platform::enable_ansi(stream),
        // The convention (no-color.org) counts only a value that is not empty.
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        std::env::var("TERM").ok().as_deref(),
        false,
        width,
    )
}

/// Reads one line from the terminal: whether it is `yes`.
fn ask_yes() -> bool {
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("yes")
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let client = match crisol_client::Context::windows("crisol", env!("CARGO_PKG_VERSION")) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let context = Context {
        client,
        cwd: std::env::current_dir().unwrap_or_default(),
        env: Box::new(|name| std::env::var(name).ok()),
        parent_program: crisol_platform::parent_program(),
        env_vars: Box::new(|| std::env::vars_os().collect()),
        confirm: Box::new(|plan| {
            eprint!("{} ", crisol_cli::render::teardown_question(plan));
            let _ = std::io::stderr().flush();
            ask_yes()
        }),
        confirm_setup: Box::new(|plan| {
            eprint!("\n{} ", crisol_cli::render::setup_question(plan));
            let _ = std::io::stderr().flush();
            ask_yes()
        }),
        confirm_step: Box::new(|step| {
            eprint!("\n{}\n{} (yes/no) ", step.describe(), step.question());
            let _ = std::io::stderr().flush();
            ask_yes()
        }),
        follow_every: Duration::from_millis(500),
        follow_rounds: None,
        style: style_of(
            crisol_platform::Stream::Stdout,
            std::io::stdout().is_terminal(),
        ),
        err_style: style_of(
            crisol_platform::Stream::Stderr,
            std::io::stderr().is_terminal(),
        ),
    };
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let code = run(&arguments, &context, &mut stdout.lock(), &mut stderr.lock());
    let _ = std::io::stdout().flush();
    ExitCode::from(code)
}
