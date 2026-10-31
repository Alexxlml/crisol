use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use crisol_cli::{run, Context, Link, Remote};
use crisol_core::programs::{is_winget_package, winget_package_folders};
use crisol_platform::{
    connect_to_daemon, default_deps, pipe_name_for_user, resolve_paths, spawn_daemon,
    RegistryAutostart, RegistryUserPath, WindowsPrivateDirs, WindowsSetup,
};

/// Reads one line from the terminal: whether it is `yes`.
fn ask_yes() -> bool {
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("yes")
}

#[allow(clippy::too_many_lines)] // wires every dependency of the context, top to bottom
fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let paths = match resolve_paths() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let pipe = match pipe_name_for_user(paths.home()) {
        Ok(pipe) => pipe,
        Err(error) => {
            eprintln!("error: cannot tell which account this is: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The programs come together in one folder. `crisol` may be reached through a link (winget
    // puts one in its links folder), so the folder is that of the real file.
    let folder = crisol_platform::real_program_folder();
    let daemon = folder.as_ref().map(|dir| dir.join("crisold.exe"));
    let winget = folder.as_deref().is_some_and(|dir| {
        is_winget_package(
            dir,
            &winget_package_folders(&|name| std::env::var(name).ok()),
        )
    });
    let reserved_name = crisol_platform::path_program_guard(&paths);
    let context = Context {
        cwd: std::env::current_dir().unwrap_or_default(),
        paths,
        deps: Box::new(default_deps),
        connect: Box::new(move || {
            let client = connect_to_daemon(&pipe)?;
            Ok(Box::new(Remote(client)) as Box<dyn Link>)
        }),
        setup: Box::new(WindowsSetup::new(
            daemon
                .as_ref()
                .and_then(|d| d.parent())
                .map(|dir| dir.join("crisol-elevate.exe"))
                .unwrap_or_default(),
        )),
        autostart: Box::new(RegistryAutostart::new()),
        daemon_exe: daemon.clone(),
        user_path: Box::new(RegistryUserPath::new()),
        trust_store: Box::new(|| {
            crisol_platform::WindowsTrustStore::current_user_root()
                .map(|store| Box::new(store) as Box<dyn crisol_core::trust::TrustStore>)
        }),
        shim_exe: folder.as_ref().map(|dir| dir.join("crisol-shim.exe")),
        reserved_name: Box::new(reserved_name),
        env: Box::new(|name| std::env::var(name).ok()),
        parent_program: crisol_platform::parent_program(),
        env_vars: Box::new(|| std::env::vars_os().collect()),
        find_terminal: Box::new(crisol_platform::find_terminal),
        run_terminal: Box::new(crisol_platform::run_terminal),
        confirm: Box::new(|plan| {
            eprint!("{} ", crisol_cli::render::teardown_question(plan));
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer).is_ok()
                && answer.trim().eq_ignore_ascii_case("yes")
        }),
        confirm_setup: Box::new(|plan| {
            eprint!(
                "
{} ",
                crisol_cli::render::setup_question(plan)
            );
            let _ = std::io::stderr().flush();
            ask_yes()
        }),
        confirm_step: Box::new(|step| {
            eprint!(
                "
{}
{} (yes/no) ",
                step.describe(),
                step.question()
            );
            let _ = std::io::stderr().flush();
            ask_yes()
        }),
        // The programs that came together; `uninstall --all` deletes the ones that are there,
        // unless winget installed them: then winget owns them.
        programs: folder
            .as_ref()
            .filter(|_| !winget)
            .map(|dir| {
                [
                    "crisol.exe",
                    "crisold.exe",
                    "crisol-shim.exe",
                    "crisol-elevate.exe",
                ]
                .iter()
                .map(|name| dir.join(name))
                .filter(|path| path.is_file())
                .collect()
            })
            .unwrap_or_default(),
        winget,
        remove_programs: Box::new(crisol_platform::schedule_delete),
        dirs: std::sync::Arc::new(WindowsPrivateDirs::new()),
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
