use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use crisol_cli::{run, Context, Link, Remote};
use crisol_core::ipc::pipe_name;
use crisol_platform::{connect_to_daemon, default_deps, resolve_paths, spawn_daemon};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let paths = match resolve_paths() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let pipe = pipe_name(paths.home());
    // `crisold.exe` is installed next to `crisol.exe`.
    let daemon = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("crisold.exe")));
    let context = Context {
        cwd: std::env::current_dir().unwrap_or_default(),
        paths,
        deps: Box::new(default_deps),
        connect: Box::new(move || {
            let client = connect_to_daemon(&pipe)?;
            Ok(Box::new(Remote(client)) as Box<dyn Link>)
        }),
        spawn_daemon: Box::new(move || match &daemon {
            Some(path) if path.is_file() => spawn_daemon(path),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "crisold.exe is not next to crisol.exe",
            )),
        }),
        daemon_wait: Duration::from_secs(15),
        follow_every: Duration::from_millis(500),
        follow_rounds: None,
    };
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let code = run(&arguments, &context, &mut stdout.lock(), &mut stderr.lock());
    let _ = std::io::stdout().flush();
    ExitCode::from(code)
}
