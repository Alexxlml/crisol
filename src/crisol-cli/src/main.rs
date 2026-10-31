use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use crisol_cli::{run, Context};

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
        find_terminal: Box::new(crisol_platform::find_terminal),
        run_terminal: Box::new(crisol_platform::run_terminal),
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
    };
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let code = run(&arguments, &context, &mut stdout.lock(), &mut stderr.lock());
    let _ = std::io::stdout().flush();
    ExitCode::from(code)
}
