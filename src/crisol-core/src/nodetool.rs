//! How the Node shims start their programs (F10.2) and where Node keeps what it installs
//! globally (F10.3).
//!
//! `npm`, `npx`, `corepack`, `pnpm` and `yarn` are not programs but scripts that run on Node.
//! The files that start them in the Node ZIP are batch files (`npm.cmd`), and a batch file is a
//! poor way to start anything from a launcher: `cmd.exe` is one more process and, for `npm`, a
//! second `node` run only to ask where the global folder is; Rust refuses to pass a batch file an
//! argument with a line break; and the quoting rules of `cmd.exe` are not the ones of anything
//! else (`analisis/node-shims.md`). So the shim runs the script itself, the way the batch file
//! ends up doing: `node.exe <script> <arguments>`.
//!
//! The environment of the child is what keeps the versions apart (ADR-0018):
//!
//! - `npm_config_prefix` is `data/node/<version>/global`, so `npm install -g` writes there and
//!   not in `%APPDATA%\npm`, and a package installed for Node 22 is not seen by Node 24;
//! - `npm_config_cache` is `cache/npm`, so the cache is not `%LOCALAPPDATA%\npm-cache`;
//! - `COREPACK_HOME` is `cache/corepack`;
//! - the folder of the global programs and the folder of the Node itself go first in the child's
//!   `PATH`, so that a script, `npx` or a global program that says `node` gets the version that
//!   was chosen, not whichever `node` the terminal happens to find first.
//!
//! None of it is written anywhere: no `.npmrc`, no registry. A value the user already chose, in
//! the environment or in their own `.npmrc`, is left as it is.
//!
//! This module only decides **what** to run and with which environment. It touches nothing, so
//! it is tested without starting anything; the shim (`crisol-shim`) starts the process.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::paths::CrisolPaths;
use crate::resolve::Command;

/// The variable Corepack keeps its downloads in. Without it they go to `%LOCALAPPDATA%\node`.
pub const COREPACK_HOME: &str = "COREPACK_HOME";
/// The variable that makes npm use a global folder.
pub const NPM_PREFIX: &str = "npm_config_prefix";
/// The variable that makes npm use a cache folder.
pub const NPM_CACHE: &str = "npm_config_cache";
/// The variable that records the prefix a shim set. A shim that finds `npm_config_prefix` equal
/// to it knows the value came from an outer shim (the global folder of another Node version),
/// not from the user, so it puts its own: the same as `CRISOL_PHPRC` for `PHPRC`.
pub const OWN_NPM_PREFIX: &str = "CRISOL_NPM_PREFIX";

/// What the shim of a Node command starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLaunch {
    /// The program: the `node.exe` of the version that was chosen.
    pub program: PathBuf,
    /// The arguments before the user's own: the script.
    pub leading: Vec<OsString>,
    /// Variables to set in the child (the ones the user already set are not in the list).
    pub env: Vec<(OsString, OsString)>,
    /// Folders that go in front of the `PATH` of the child, first one first.
    pub path_prepend: Vec<PathBuf>,
}

/// Why a command cannot be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    /// A file of the installation is missing.
    #[error("`{}` is not there: the installation looks damaged", .0.display())]
    Damaged(PathBuf),
    /// The version has no Corepack.
    #[error(
        "Node {version} does not include Corepack (Node 25 and later left it out), so `{command}` is not available with it. \
         Use a version that has it (`crisol use node 24`), or install it for this version with `npm install --global corepack`"
    )]
    NoCorepack {
        /// The version.
        version: String,
        /// The command that was asked for.
        command: &'static str,
    },
}

/// Everything [`plan_launch`] needs to know.
pub struct LaunchInput<'a> {
    /// The command that was typed.
    pub command: Command,
    /// The installation of the version that was chosen.
    pub install: &'a Path,
    /// That version.
    pub version: &'a str,
    /// The Crisol home.
    pub paths: &'a CrisolPaths,
    /// Reads the environment of the shim.
    pub lookup: &'a dyn Fn(&str) -> Option<OsString>,
    /// The text of the user's own `.npmrc` (empty if there is none), to see what they chose.
    pub user_npmrc: &'a str,
}

impl std::fmt::Debug for LaunchInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchInput")
            .field("command", &self.command)
            .field("install", &self.install)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// The script (relative to the installation or to a global folder) that runs a command, if it
/// is one of those that are scripts.
fn script_of(command: Command) -> Option<&'static [&'static str]> {
    Some(match command {
        Command::Npm => &["node_modules", "npm", "bin", "npm-cli.js"],
        Command::Npx => &["node_modules", "npm", "bin", "npx-cli.js"],
        Command::Corepack => &["node_modules", "corepack", "dist", "corepack.js"],
        Command::Pnpm => &["node_modules", "corepack", "dist", "pnpm.js"],
        Command::Pnpx => &["node_modules", "corepack", "dist", "pnpx.js"],
        Command::Yarn => &["node_modules", "corepack", "dist", "yarn.js"],
        Command::Yarnpkg => &["node_modules", "corepack", "dist", "yarnpkg.js"],
        Command::Php | Command::Node | Command::Composer => return None,
    })
}

/// The name of a command as it is typed.
fn name_of(command: Command) -> &'static str {
    match command {
        Command::Php => "php",
        Command::Node => "node",
        Command::Npm => "npm",
        Command::Npx => "npx",
        Command::Composer => "composer",
        Command::Corepack => "corepack",
        Command::Pnpm => "pnpm",
        Command::Pnpx => "pnpx",
        Command::Yarn => "yarn",
        Command::Yarnpkg => "yarnpkg",
    }
}

fn is_corepack_family(command: Command) -> bool {
    matches!(
        command,
        Command::Corepack | Command::Pnpm | Command::Pnpx | Command::Yarn | Command::Yarnpkg
    )
}

/// Whether the environment has `name`, in either case (Windows ignores the case, others do not
/// and npm accepts both).
fn env_has(lookup: &dyn Fn(&str) -> Option<OsString>, name: &str) -> bool {
    lookup(name).is_some() || lookup(&name.to_ascii_uppercase()).is_some()
}

/// Whether the text of an `.npmrc` sets `key` (`prefix`, `cache`) on a line of its own. Comments
/// (`;` and `#`) do not count; spaces around the name and the `=` do not matter.
#[must_use]
pub fn npmrc_sets(text: &str, key: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        !line.starts_with([';', '#'])
            && line
                .split_once('=')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(key))
    })
}

/// What to start for `command` (any of the Node ones) in the installation `input.install`.
pub fn plan_launch(input: &LaunchInput<'_>) -> Result<NodeLaunch, LaunchError> {
    let LaunchInput {
        command,
        install,
        version,
        paths,
        lookup,
        user_npmrc,
    } = *input;
    let node = install.join(if cfg!(windows) {
        "node.exe"
    } else {
        "bin/node"
    });
    if !node.is_file() {
        return Err(LaunchError::Damaged(node));
    }
    let mut launch = NodeLaunch {
        program: node,
        leading: Vec::new(),
        env: Vec::new(),
        path_prepend: Vec::new(),
    };
    // The global folder is Crisol's unless the user chose their own prefix. One an outer shim
    // set (and recorded in `OWN_NPM_PREFIX`) is not the user's.
    let inherited = lookup(NPM_PREFIX)
        .or_else(|| lookup(&NPM_PREFIX.to_ascii_uppercase()))
        .is_some_and(|value| lookup(OWN_NPM_PREFIX).as_ref() == Some(&value));
    let users_own = (env_has(lookup, NPM_PREFIX) && !inherited) || npmrc_sets(user_npmrc, "prefix");
    let ours = paths.node_global(version).ok().filter(|_| !users_own);

    if let Some(parts) = script_of(command) {
        // A copy of npm (or of Corepack) installed in our global folder wins over the one in the
        // ZIP, which is what `npm.cmd` does for `npm install -g npm`. With a prefix of the
        // user's, the one in the ZIP is used.
        let under = |root: &Path| {
            parts
                .iter()
                .fold(root.to_path_buf(), |p, part| p.join(part))
        };
        let script = ours
            .as_deref()
            .map(under)
            .filter(|s| s.is_file())
            .unwrap_or_else(|| under(install));
        if !script.is_file() {
            return Err(if is_corepack_family(command) {
                LaunchError::NoCorepack {
                    version: version.to_owned(),
                    command: name_of(command),
                }
            } else {
                LaunchError::Damaged(script)
            });
        }
        launch.leading.push(script.into_os_string());
    }

    let mut set = |name: &str, value: &Path| {
        launch
            .env
            .push((name.into(), value.as_os_str().to_os_string()));
    };
    if is_corepack_family(command) && lookup(COREPACK_HOME).is_none() {
        if let Ok(home) = paths.cache_for("corepack") {
            set(COREPACK_HOME, &home);
        }
    }
    if matches!(command, Command::Node | Command::Npm | Command::Npx) {
        if let Some(global) = &ours {
            set(NPM_PREFIX, global);
            set(OWN_NPM_PREFIX, global);
        }
        if !env_has(lookup, NPM_CACHE) && !npmrc_sets(user_npmrc, "cache") {
            if let Ok(cache) = paths.cache_for("npm") {
                set(NPM_CACHE, &cache);
            }
        }
    }
    // The global programs of this version, then this version of Node itself.
    launch.path_prepend.extend(ours);
    launch.path_prepend.push(install.to_path_buf());
    Ok(launch)
}

/// What the shim of `corepack` says instead of running `corepack enable` or `corepack disable`,
/// or `None` if the arguments are for Corepack to run.
///
/// `corepack enable` writes `pnpm`, `yarn` and the like into the folder of the `node.exe` it
/// runs on (or into `--install-directory`), each pointing at **that** installation by a
/// relative path: the first Node version to switch away would leave them pointing at nothing
/// (or at the wrong version). Crisol has its own `pnpm`, `pnpx`, `yarn` and `yarnpkg` shims,
/// which pick the version of the project and need nothing to be enabled; so `enable` and
/// `disable` write nothing. A call with `--install-directory` is the user asking for the files
/// somewhere in particular and goes through.
#[must_use]
pub fn corepack_enable_note(args: &[OsString]) -> Option<String> {
    let mut words = args
        .iter()
        .filter(|a| !a.to_string_lossy().starts_with('-'));
    let verb = words.next()?.to_string_lossy().into_owned();
    if !matches!(verb.as_str(), "enable" | "disable") {
        return None;
    }
    let directory = |a: &OsString| {
        let text = a.to_string_lossy();
        text == "--install-directory" || text.starts_with("--install-directory=")
    };
    if args.iter().any(directory) {
        return None;
    }
    Some(format!(
        "`corepack {verb}` is not needed with Crisol and wrote nothing: `pnpm`, `pnpx`, `yarn` and \
         `yarnpkg` are already commands of Crisol's shims, they use the Node version of the project and \
         Corepack keeps what it downloads in Crisol's cache folder."
    ))
}

/// The npm commands (and their aliases) that install, remove or update programs.
const CHANGING: &[&str] = &[
    "install",
    "i",
    "add",
    "in",
    "ins",
    "inst",
    "insta",
    "instal",
    "isnt",
    "isnta",
    "isntal",
    "isntall",
    "uninstall",
    "un",
    "unlink",
    "remove",
    "rm",
    "r",
    "update",
    "up",
    "upgrade",
    "link",
    "ln",
    "dedupe",
    "ddp",
    "prune",
    "rebuild",
    "rb",
];
/// Other npm commands. They are here so that `npm view install -g` is read as `view`, and
/// so that the value of a flag (`--loglevel warn`) is not taken for the command.
const OTHERS: &[&str] = &[
    "ls",
    "list",
    "ll",
    "la",
    "view",
    "v",
    "info",
    "show",
    "root",
    "bin",
    "prefix",
    "config",
    "c",
    "get",
    "set",
    "run",
    "run-script",
    "rum",
    "urm",
    "exec",
    "x",
    "test",
    "t",
    "start",
    "stop",
    "restart",
    "publish",
    "pack",
    "outdated",
    "search",
    "s",
    "find",
    "cache",
    "help",
    "audit",
    "fund",
    "doctor",
    "explain",
    "why",
    "init",
    "create",
    "login",
    "logout",
    "whoami",
    "token",
    "team",
    "org",
    "owner",
    "access",
    "deprecate",
    "dist-tag",
    "ping",
    "pkg",
    "repo",
    "docs",
    "home",
    "bugs",
    "diff",
    "query",
    "sbom",
    "stars",
    "star",
    "unstar",
    "version",
    "completion",
    "edit",
    "explore",
    "hook",
    "profile",
    "set-script",
    "shrinkwrap",
    "unpublish",
    "install-test",
    "it",
    "install-ci-test",
    "cit",
    "ci",
    "clean-install",
    "ic",
];

/// The npm commands that change the global folder with or without `-g`: `npm link` in a
/// package links it there, bins included.
const ALWAYS_GLOBAL: &[&str] = &["link", "ln"];

/// Whether the arguments of an `npm` command are for the global folder: it is installing,
/// removing or updating programs there, and the shim must refresh the commands that stand for
/// them ([`crate::globals`]).
#[must_use]
pub fn changes_globals(args: &[OsString]) -> bool {
    let mut global = false;
    let mut always = false;
    let mut changing = None;
    let mut location_value = false;
    for arg in args.iter().map(|a| a.to_string_lossy()) {
        let word = arg.as_ref();
        if word == "--" {
            break;
        }
        if location_value {
            global |= word == "global";
            location_value = false;
            continue;
        }
        match word {
            "-g" | "--global" | "--global=true" | "-g=true" | "--location=global" => global = true,
            "--global=false" | "-g=false" | "--no-global" => global = false,
            "--location" => location_value = true,
            // Short flags can be glued together: `-gD`.
            short if short.starts_with('-') && !short.starts_with("--") => {
                global |= short[1..].contains('g');
            }
            flag if flag.starts_with('-') => {}
            other if changing.is_none() => {
                if CHANGING.contains(&other) {
                    changing = Some(true);
                    always = ALWAYS_GLOBAL.contains(&other);
                } else if OTHERS.contains(&other) {
                    changing = Some(false);
                }
            }
            _ => {}
        }
    }
    (global || always) && changing == Some(true)
}

/// Whether `name` is the name of a Node command a shim can stand for.
#[must_use]
pub fn is_node_command(name: &OsStr) -> bool {
    name.to_str()
        .and_then(Command::from_name)
        .is_some_and(|c| script_of(c).is_some() || c == Command::Node)
}

#[cfg(test)]
mod tests;
