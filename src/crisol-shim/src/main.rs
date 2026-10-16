//! The shim: `php.exe`, `node.exe`, `npm`, `npx` and `composer` in `shims/` are all this one
//! program (F4.2). It works out which version applies in the current directory and runs the
//! real binary with the same arguments, input, output and exit code.
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
use crisol_core::paths::CrisolPaths;
use crisol_core::project::FileOwners;
use crisol_core::resolve::{
    resolve, resolve_composer, Command, ResolveContext, ResolveError, COMPOSER_PHAR,
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
    let Some(command) = invoked_as() else {
        return not_a_shim(&args);
    };
    match launch(command, &args) {
        Ok(code) => code,
        Err(message) => {
            report(&message);
            FAILURE
        }
    }
}

/// The command this executable stands for, from its own file name (`php.exe` → `php`). The
/// real path is used rather than `argv[0]`, which may be relative or found through `PATH`.
fn invoked_as() -> Option<Command> {
    let exe = std::env::current_exe().ok()?;
    Command::from_name(exe.file_stem()?.to_str()?)
}

fn not_a_shim(args: &[OsString]) -> i32 {
    if args.first().is_some_and(|a| a == "--version") {
        println!("crisol-shim {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    report("this program only works under the name of a command: php, node, npm, npx or composer");
    USAGE
}

fn report(message: &str) {
    // A closed stderr is not worth failing over.
    let _ = writeln!(io::stderr(), "crisol: {message}");
}

fn launch(command: Command, args: &[OsString]) -> Result<i32, String> {
    let paths = crisol_platform::resolve_paths().map_err(|e| e.to_string())?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let owners = file_owners();
    let lookup = |name: &str| std::env::var(name).ok();
    let catalog =
        Catalog::embedded().map_err(|e| format!("the built-in catalog is broken: {e}"))?;
    let resolution = resolve(
        command.tool(),
        &ResolveContext {
            paths: &paths,
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

    let install = &resolution.resolved.version.path;
    let (program, leading) = program_for(command, install, &paths)?;
    let mut process = Process::new(&program);
    process.args(leading).args(args);
    match php_ini_for(
        command,
        &paths,
        &resolution.resolved.version.version.to_string(),
    ) {
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

    // Ctrl+C reaches the child and the shim alike; only the child should die.
    let _ = crisol_platform::survive_ctrl_c();
    let status = process
        .status()
        .map_err(|e| format!("cannot run `{}`: {e}", program.display()))?;
    // A program ended by a signal has no code; 1 is the conventional "it failed".
    Ok(status.code().unwrap_or(FAILURE))
}

fn explain(error: ResolveError) -> String {
    match error {
        ResolveError::NotInstalled { .. } | ResolveError::NothingInstalled(_) => {
            format!("{error}. Install one with `crisol install`")
        }
        other => other.to_string(),
    }
}

/// The executable that runs `command` inside an installation, and the arguments that go
/// before the user's own. For everything but Composer there are none; Composer is a PHAR, so
/// it runs as `php composer.phar …` with the PHP that was resolved.
fn program_for(
    command: Command,
    install: &Path,
    paths: &CrisolPaths,
) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let name = match command {
        Command::Php | Command::Composer => "php",
        Command::Node => "node",
        Command::Npm => "npm",
        Command::Npx => "npx",
    };
    let file = match (cfg!(windows), command) {
        (true, Command::Npm | Command::Npx) => format!("{name}.cmd"),
        (true, _) => format!("{name}.exe"),
        (false, _) => name.to_owned(),
    };
    let program = install.join(file);
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

/// Node, npm and npx trust the local CA (`certs/ca.pem`, ADR-0013) so that a request to
/// `https://shop.test` works from JavaScript too, whether or not the CA is in the system's
/// store. Only when the CA exists, and never over a value the user already set.
fn node_ca_for(command: Command, paths: &CrisolPaths) -> Option<PathBuf> {
    let for_node = matches!(command, Command::Node | Command::Npm | Command::Npx);
    let ca = paths.ca_cert();
    (for_node && std::env::var_os(NODE_CA_VAR).is_none() && ca.is_file()).then_some(ca)
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
