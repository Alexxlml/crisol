//! The shim: `php.exe`, `node.exe`, `npm`, `npx`, `composer`, `corepack`, `pnpm`, `pnpx`,
//! `yarn` and `yarnpkg` in `shims/` (and the one of every global program, `tsc.exe`) are all
//! this one program (F4.2, F10.2, F10.3). It works out which version applies in the current
//! directory and runs the real binary with the same arguments, input, output and exit code.
//!
//! It has no logic of its own: choosing the version is `crisol_core::resolve`, and the rules
//! about whose files to trust are the ones of ADR-0010. What is left here is what only a
//! launcher does: tell which command it was called as, build the command line, wait, and
//! pass on the exit code. No async runtime and no network code: it runs on every `php` and
//! has to start in milliseconds.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command as Process};

use crisol_core::catalog::Catalog;
use crisol_core::globals::{find_global, is_global_shim, not_installed_message, sync_global_shims};
use crisol_core::nodetool::{
    changes_globals, corepack_enable_note, plan_launch, LaunchInput, NodeLaunch, NPM_PREFIX,
};
use crisol_core::paths::CrisolPaths;
use crisol_core::project::FileOwners;
use crisol_core::resolve::{
    resolve, resolve_composer, Command, Resolution, ResolveContext, ResolveError, Tool,
    COMPOSER_PHAR,
};

/// The exit code when the shim itself cannot do its job (no version, no binary).
const FAILURE: i32 = 1;
/// The exit code when the program is not run through a shim name.
const USAGE: i32 = 2;

fn main() {
    process::exit(run());
}

fn run() -> i32 {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let outcome = match invoked_as() {
        Some(Invoked::Core(command)) => launch(command, &args),
        Some(Invoked::Global(name)) => launch_global(&name, &args),
        None => return not_a_shim(&args),
    };
    match outcome {
        Ok(code) => code,
        Err(message) => {
            report(&message);
            FAILURE
        }
    }
}

/// What this executable stands for.
enum Invoked {
    /// One of Crisol's own commands.
    Core(Command),
    /// A program installed with `npm install --global`: its name is in the list of `shims/`.
    Global(String),
}

/// The command this executable stands for, from its own file name (`php.exe` → `php`). The
/// real path is used rather than `argv[0]`, which may be relative or found through `PATH`.
fn invoked_as() -> Option<Invoked> {
    let exe = std::env::current_exe().ok()?;
    let name = exe.file_stem()?.to_str()?.to_owned();
    if let Some(command) = Command::from_name(&name) {
        return Some(Invoked::Core(command));
    }
    // Any other name is only a global program if it is in the shims folder and in its list;
    // a copy of this program anywhere else with a made-up name does nothing.
    let paths = crisol_platform::resolve_paths().ok()?;
    let in_shims = exe
        .parent()
        .is_some_and(|dir| same_folder(dir, &paths.shims_dir()));
    (in_shims && is_global_shim(&paths, &name)).then_some(Invoked::Global(name))
}

fn same_folder(a: &Path, b: &Path) -> bool {
    let normal = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    normal(a) == normal(b)
}

fn not_a_shim(args: &[OsString]) -> i32 {
    if args.first().is_some_and(|a| a == "--version") {
        println!("crisol-shim {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    report(
        "this program only works under the name of a command: php, node, npm, npx, composer, corepack, pnpm, pnpx, yarn or yarnpkg",
    );
    USAGE
}

fn report(message: &str) {
    // A closed stderr is not worth failing over.
    let _ = writeln!(io::stderr(), "crisol: {message}");
}

/// The version that applies in the current directory, with the warnings shown.
fn resolve_here(tool: Tool, paths: &CrisolPaths) -> Result<Resolution, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let owners = file_owners();
    let lookup = |name: &str| std::env::var(name).ok();
    let catalog =
        Catalog::embedded().map_err(|e| format!("the built-in catalog is broken: {e}"))?;
    let resolution = resolve(
        tool,
        &ResolveContext {
            paths,
            cwd: &cwd,
            ceiling: None,
            owners: owners.as_ref(),
            env: &lookup,
            catalog: &catalog,
        },
    )
    .map_err(explain)?;
    for warning in &resolution.warnings {
        report(&format!("warning: {warning}"));
    }
    Ok(resolution)
}

fn launch(command: Command, args: &[OsString]) -> Result<i32, String> {
    if command == Command::Corepack {
        if let Some(note) = corepack_enable_note(args) {
            report(&note);
            return Ok(0);
        }
    }
    let paths = crisol_platform::resolve_paths().map_err(|e| e.to_string())?;
    let resolution = resolve_here(command.tool(), &paths)?;

    let install = &resolution.resolved.version.path;
    let version = resolution.resolved.version.version.to_string();
    let (program, leading, extra_env, path_prepend) = if command.tool() == Tool::Node {
        let launch = node_launch(command, install, &version, &paths)?;
        (
            launch.program,
            launch.leading,
            launch.env,
            launch.path_prepend,
        )
    } else {
        let (program, leading) = program_for(command, install, &paths)?;
        (
            program,
            leading.into_iter().map(PathBuf::into_os_string).collect(),
            Vec::new(),
            Vec::new(),
        )
    };
    let mut process = Process::new(&program);
    process.args(leading).args(args).envs(extra_env);
    prepend_to_path(&mut process, &path_prepend);
    match php_ini_for(command, &paths, &version) {
        IniChoice::Set(ini) => {
            process.env("PHPRC", &ini).env(OWN_PHPRC, &ini);
        }
        IniChoice::Clear => {
            process.env_remove("PHPRC").env_remove(OWN_PHPRC);
        }
        IniChoice::Leave => {}
    }
    if let Some(ca) = node_ca_for(command, &paths) {
        process.env(NODE_CA_VAR, ca);
    }

    let code = run_child(&mut process, &program)?;
    // `npm install --global` just changed which programs there are: their commands follow.
    if command == Command::Npm && code == 0 && changes_globals(args) {
        sync_globals(&paths);
    }
    Ok(code)
}

/// A program installed with `npm install --global`, run from its `.cmd` in the global folder
/// of the Node version that applies here, with that Node first in its `PATH`.
fn launch_global(name: &str, args: &[OsString]) -> Result<i32, String> {
    let paths = crisol_platform::resolve_paths().map_err(|e| e.to_string())?;
    let resolution = resolve_here(Tool::Node, &paths)?;
    let install = &resolution.resolved.version.path;
    let version = resolution.resolved.version.version.to_string();
    let launch = node_launch(Command::Node, install, &version, &paths)?;
    let Some(program) = find_global(&paths, name, &version) else {
        // Without Crisol's prefix the global programs are in the user's own folder.
        if !launch.env.iter().any(|(n, _)| n == NPM_PREFIX) {
            return Err(format!(
                "`{name}` is a command for Crisol's global folder of Node {version}, but your own npm prefix is in use \
                 (`npm_config_prefix`, or `prefix` in your .npmrc), so global programs go there. Run the one in that \
                 folder; `crisol shims sync` removes this command once no Node version of Crisol has it"
            ));
        }
        return Err(not_installed_message(&paths, name, &version));
    };
    let mut process = Process::new(&program);
    process.args(args).envs(launch.env);
    prepend_to_path(&mut process, &launch.path_prepend);
    if let Some(ca) = node_ca_for(Command::Node, &paths) {
        process.env(NODE_CA_VAR, ca);
    }
    run_child(&mut process, &program).map_err(|message| {
        // Rust will not give a batch file an argument with a line break.
        if message.contains("batch file arguments are invalid") {
            format!(
                "`{name}` is started through `{}`, a batch file, and cmd.exe cannot carry an argument with a line break: \
                 pass that text another way (a file, or standard input)",
                program.display()
            )
        } else {
            message
        }
    })
}

/// What to start for a Node command, with the file of the user's `.npmrc` taken into account.
fn node_launch(
    command: Command,
    install: &Path,
    version: &str,
    paths: &CrisolPaths,
) -> Result<NodeLaunch, String> {
    let npmrc = user_npmrc();
    plan_launch(&LaunchInput {
        command,
        install,
        version,
        paths,
        lookup: &|name| std::env::var_os(name),
        user_npmrc: &npmrc,
    })
    .map_err(|e| e.to_string())
}

/// The text of the user's `.npmrc` (`NPM_CONFIG_USERCONFIG`, else `~/.npmrc`), empty if there is
/// none. It is only read, to see whether the user chose their own `prefix` or `cache`.
fn user_npmrc() -> String {
    let path = std::env::var_os("NPM_CONFIG_USERCONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|home| PathBuf::from(home).join(".npmrc"))
        });
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Puts `folders` in front of the `PATH` of `process`.
fn prepend_to_path(process: &mut Process, folders: &[PathBuf]) {
    if folders.is_empty() {
        return;
    }
    let current = std::env::var_os("PATH").unwrap_or_default();
    let joined = std::env::join_paths(
        folders
            .iter()
            .cloned()
            .chain(std::env::split_paths(&current)),
    );
    // A folder with a `;` in it cannot be in a `PATH`: the child keeps the one it had.
    if let Ok(joined) = joined {
        process.env("PATH", joined);
    }
}

/// Runs the child and gives its exit code.
fn run_child(process: &mut Process, program: &Path) -> Result<i32, String> {
    // Ctrl+C reaches the child and the shim alike; only the child should die.
    let _ = crisol_platform::survive_ctrl_c();
    let status = process
        .status()
        .map_err(|e| format!("cannot run `{}`: {e}", program.display()))?;
    // A program ended by a signal has no code; 1 is the conventional "it failed".
    Ok(status.code().unwrap_or(FAILURE))
}

/// Brings the shims of the global programs up to date. It never changes the exit code of npm.
fn sync_globals(paths: &CrisolPaths) {
    let Ok(me) = std::env::current_exe() else {
        return;
    };
    let reserved = crisol_platform::path_program_guard(paths);
    match sync_global_shims(paths, &me, &reserved) {
        Ok(done) => {
            for (name, why) in done.skipped {
                report(&format!("warning: no command `{name}`: {why}"));
            }
        }
        Err(error) => report(&format!(
            "warning: could not update the commands of the global programs: {error}"
        )),
    }
}

fn explain(error: ResolveError) -> String {
    match error {
        ResolveError::NotInstalled { .. } | ResolveError::NothingInstalled(_) => {
            format!("{error}. Install one with `crisol install`")
        }
        other => other.to_string(),
    }
}

/// The executable that runs `command` (PHP or Composer; the Node ones are `nodetool`) inside an
/// installation, and the arguments that go before the user's own. Composer is a PHAR, so it
/// runs as `php composer.phar …` with the PHP that was resolved.
fn program_for(
    command: Command,
    install: &Path,
    paths: &CrisolPaths,
) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let program = install.join(if cfg!(windows) { "php.exe" } else { "php" });
    if !program.is_file() {
        return Err(format!(
            "`{}` is not there: the installation looks damaged",
            program.display()
        ));
    }
    if command != Command::Composer {
        return Ok((program, Vec::new()));
    }
    let composer = resolve_composer(paths)
        .map_err(|e| format!("{e}. Install it with `crisol install composer`"))?;
    let phar = composer.path.join(COMPOSER_PHAR);
    if !phar.is_file() {
        return Err(format!(
            "`{}` is not there: the installation looks damaged",
            phar.display()
        ));
    }
    Ok((program, vec![phar]))
}

/// The variable Node reads to trust extra certificate authorities.
const NODE_CA_VAR: &str = "NODE_EXTRA_CA_CERTS";

/// Node, npm and npx trust the local CA so that a request to `https://shop.test` works from
/// JavaScript too, whether or not the CA is in the system's store (ADR-0013). They are pointed
/// at `etc/node/ca.pem`, the copy the core writes from a CA it has validated, never at
/// `certs/ca.pem`. Only when that copy exists, never over a value the user already set, and only
/// if neither the copy nor its folder can be modified by other accounts: otherwise one line on
/// stderr says which and how to fix it, and Node is told nothing.
fn node_ca_for(command: Command, paths: &CrisolPaths) -> Option<PathBuf> {
    if command.tool() != Tool::Node || std::env::var_os(NODE_CA_VAR).is_some() {
        return None;
    }
    let ca = paths.node_ca().ok()?;
    if !ca.is_file() {
        return None;
    }
    let folder = ca.parent().map(Path::to_path_buf).unwrap_or_default();
    if let Some(open) = [folder, ca.clone()].into_iter().find(|p| open_to_others(p)) {
        report(&format!(
            "warning: `{}` can be modified by other accounts, so Node was not told to trust the \
             local CA. Remove their write access (this works in cmd and in PowerShell: {}) and \
             run `crisol trust` again",
            open.display(),
            fix_command(&open)
        ));
        return None;
    }
    Some(ca)
}

/// The `icacls` command that makes `path` private again, for the message. A folder and a file
/// get different ones (the inheritance flags only mean something on a folder).
#[cfg(windows)]
fn fix_command(path: &Path) -> String {
    match crisol_platform::current_user_sid() {
        Ok(sid) => crisol_core::paths::private_acl_fix_command(path, path.is_dir(), &sid),
        Err(_) => format!(
            "remove write access for other accounts on `{}`",
            path.display()
        ),
    }
}

#[cfg(not(windows))]
fn fix_command(path: &Path) -> String {
    format!("chmod go-w \"{}\"", path.display())
}

/// Whether other accounts of this computer can write to `path`. If that cannot be found out it
/// counts as open: the copy is only trusted when it is known to be private.
#[cfg(windows)]
fn open_to_others(path: &Path) -> bool {
    use crisol_core::paths::PrivateDirs;
    crisol_platform::WindowsPrivateDirs
        .is_writable_by_others(path)
        .unwrap_or(true)
}

/// Unix homes are private by default (`~/.crisol` and the `umask`); no check yet.
#[cfg(not(windows))]
fn open_to_others(_path: &Path) -> bool {
    false
}

/// The variable that records the `PHPRC` this shim set. A shim that finds a `PHPRC` equal to
/// it knows the value came from an outer shim, not from the user, so it may replace it: the
/// `php` of another project run from inside a PHP script must not read the outer version's
/// `php.ini` and `extension_dir`.
const OWN_PHPRC: &str = "CRISOL_PHPRC";

/// What to do with `PHPRC` in the child.
enum IniChoice {
    /// Point it at the generated `php.ini` of this version.
    Set(PathBuf),
    /// It was inherited from an outer shim and this version has no generated file: remove it.
    Clear,
    /// Not ours to touch (the user's own, or nothing to set).
    Leave,
}

/// The generated `php.ini` of this version, if it exists and the user has not chosen their
/// own with `PHPRC`. PHP then reads it instead of looking for one next to `php.exe`.
fn php_ini_for(command: Command, paths: &CrisolPaths, version: &str) -> IniChoice {
    if !matches!(command, Command::Php | Command::Composer) {
        return IniChoice::Leave;
    }
    let inherited = match std::env::var_os("PHPRC") {
        Some(current) if std::env::var_os(OWN_PHPRC).as_ref() != Some(&current) => {
            return IniChoice::Leave;
        }
        Some(_) => true,
        None => false,
    };
    let generated = paths
        .php_ini(version)
        .ok()
        .filter(|ini| ini.is_file())
        .and_then(|ini| ini.parent().map(Path::to_path_buf));
    match (generated, inherited) {
        (Some(dir), _) => IniChoice::Set(dir),
        (None, true) => IniChoice::Clear,
        (None, false) => IniChoice::Leave,
    }
}

#[cfg(windows)]
fn file_owners() -> Box<dyn FileOwners> {
    Box::new(crisol_platform::WindowsFileOwners::new())
}

#[cfg(not(windows))]
fn file_owners() -> Box<dyn FileOwners> {
    // Ownership is only checked on Windows so far.
    Box::new(crisol_core::project::TrustAllOwners)
}
