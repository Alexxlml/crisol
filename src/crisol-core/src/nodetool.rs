//! How the Node shims start their programs (F10.2).
//!
//! `npm`, `npx`, `corepack`, `pnpm` and `yarn` are not programs but scripts that run on Node.
//! The files that start them in the Node ZIP are batch files (`npm.cmd`), and a batch file is a
//! poor way to start anything from a launcher: `cmd.exe` is one more process and, for `npm`, a
//! second `node` run only to ask where the global folder is; Rust refuses to pass a batch file an
//! argument with a line break; and the quoting rules of `cmd.exe` are not the ones of anything
//! else (`analisis/node-shims.md`). So the shim runs the script itself, the way the batch file
//! ends up doing: `node.exe <script> <arguments>`.
//!
//! This module only decides **what** to run and with which environment. It touches nothing, so
//! it is tested without starting anything; the shim (`crisol-shim`) starts the process.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::paths::CrisolPaths;
use crate::resolve::Command;

/// The variable Corepack keeps its downloads in. Without it they go to `%LOCALAPPDATA%\node`.
pub const COREPACK_HOME: &str = "COREPACK_HOME";

/// What the shim of a Node command starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLaunch {
    /// The program: the `node.exe` of the version that was chosen.
    pub program: PathBuf,
    /// The arguments before the user's own: the script.
    pub leading: Vec<OsString>,
    /// Variables to set in the child (the ones the user already set are not in the list).
    pub env: Vec<(OsString, OsString)>,
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

/// The script (relative to the installation) that runs a command, if it is one of those that
/// are scripts.
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

/// What to start for `command` (any of the Node ones) in the installation `install` of
/// `version`. `lookup` reads the environment of the shim.
pub fn plan_launch(
    command: Command,
    install: &Path,
    version: &str,
    paths: &CrisolPaths,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Result<NodeLaunch, LaunchError> {
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
    };
    if let Some(parts) = script_of(command) {
        let script: PathBuf = parts
            .iter()
            .fold(install.to_path_buf(), |path, part| path.join(part));
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
    if is_corepack_family(command) && lookup(COREPACK_HOME).is_none() {
        if let Ok(home) = paths.cache_for("corepack") {
            launch
                .env
                .push((COREPACK_HOME.into(), home.into_os_string()));
        }
    }
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

/// Whether `name` is the name of a Node command a shim can stand for.
#[must_use]
pub fn is_node_command(name: &OsStr) -> bool {
    name.to_str()
        .and_then(Command::from_name)
        .is_some_and(|c| script_of(c).is_some() || c == Command::Node)
}

#[cfg(test)]
mod tests;
