//! The command line, as `clap` sees it, and what each command asks the daemon.
//!
//! This is all the "logic" the CLI has: which words mean which [`Call`]. What a call does is the
//! core's business, and how its answer looks is [`crate::render`]'s.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use crisol_protocol::{Call, Tool};

/// Crisol: local PHP and Node development, without the pain.
#[derive(Debug, Parser)]
#[command(name = "crisol", version, propagate_version = true)]
pub struct Args {
    /// Print the answer as JSON (one object) instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Which tool a version is chosen for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ToolArg {
    /// PHP.
    Php,
    /// Node.js.
    Node,
}

/// The commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Starts the web stack (Nginx and PHP), starting the daemon if it is not running.
    Start {
        /// Only this service. The web stack starts as one, so this is only to get an answer
        /// that says so.
        service: Option<String>,
    },
    /// Stops the services (Nginx and PHP). The daemon keeps running: it watches your files and
    /// answers the DNS of the local names. `--daemon` closes it too.
    Stop {
        /// Close the daemon as well (the local names stop resolving). Same as `crisol shutdown`.
        #[arg(long, conflicts_with = "service")]
        daemon: bool,
        /// Only this service.
        service: Option<String>,
    },
    /// Stops the services and closes the daemon.
    Shutdown,
    /// Stops and starts the web stack again.
    Restart {
        /// Only this service.
        service: Option<String>,
    },
    /// Shows the state of the services.
    Status {
        /// Only this service.
        service: Option<String>,
    },
    /// Installs a version of a tool (`crisol install php 8.4`).
    Install {
        /// `php`, `nginx`, `node`, …
        service: String,
        /// `8.4`, `8.4.26` or `latest` (the highest). Without it, the version Crisol uses by
        /// default: the line the catalog recommends.
        #[arg(id = "requested", value_name = "VERSION")]
        version: Option<String>,
        /// Installs from an archive you already downloaded instead of the catalog: the way out
        /// when this `crisol.exe` does not list a version yet (needs the exact version and
        /// `--sha256`). Crisol marks it "not verified by Crisol".
        #[arg(long, value_name = "ZIP", requires = "sha256", requires = "requested")]
        from: Option<PathBuf>,
        /// The SHA-256 of that archive, from the provider's page. It is checked before
        /// anything is unpacked.
        #[arg(long, value_name = "HASH", requires = "from")]
        sha256: Option<String>,
    },
    /// Removes an installed version.
    Uninstall {
        /// The tool.
        service: String,
        /// The version (an exact one, or a prefix that matches exactly one).
        #[arg(id = "requested", value_name = "VERSION")]
        version: String,
    },
    /// Lists what is installed; with `--available`, also what could be.
    List {
        /// Also list what the catalog offers for this platform.
        #[arg(long)]
        available: bool,
    },
    /// Chooses a version: the default, or the one for a folder with `--local`.
    Use {
        /// The tool.
        #[arg(value_enum)]
        tool: ToolArg,
        /// What to choose (`8.4`, `22`, …).
        #[arg(id = "requested", value_name = "VERSION")]
        version: String,
        /// Pin it for this folder (`.crisol.toml`) instead of for everybody.
        #[arg(long, conflicts_with = "dir")]
        local: bool,
        /// Pin it for this folder.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Serves every folder inside this one as a site.
    Park {
        /// The folder; the current one if left out.
        path: Option<PathBuf>,
    },
    /// Stops serving the folders of a parked folder.
    Unpark {
        /// The folder; the current one if left out.
        path: Option<PathBuf>,
    },
    /// Serves one folder as a site.
    Link {
        /// The site name (`shop` for `shop.test`); the folder's name if left out.
        name: Option<String>,
        /// The folder; the current one if left out.
        path: Option<PathBuf>,
        /// The subfolder that is served, if it is not detected (`public`).
        #[arg(long)]
        docroot: Option<String>,
    },
    /// Stops serving a linked site.
    Unlink {
        /// The site name.
        name: String,
    },
    /// Lists the sites.
    Sites,
    /// Serves a site over HTTPS too.
    Secure {
        /// The site name.
        name: String,
    },
    /// Stops serving a site over HTTPS.
    Unsecure {
        /// The site name.
        name: String,
    },
    /// Shows the log of a service; with `-f`, keeps showing it as it grows.
    Logs {
        /// `nginx`, `php-8.4.26`, `crisold`, …
        service: String,
        /// How many lines from the end.
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Keep following the log.
        #[arg(short, long)]
        follow: bool,
    },
    /// Checks the installation and says what to fix.
    Doctor,
    /// Turns a data service on in `crisol.toml` (`enabled = true`); `crisol start` brings it up.
    Enable {
        /// `mysql`, `postgresql`, `redis`, …
        service: String,
    },
    /// Turns a data service off in `crisol.toml`; a running one keeps running until it is stopped.
    Disable {
        /// `mysql`, `postgresql`, `redis`, …
        service: String,
    },
    /// The databases and other services: what a project needs to connect to them.
    Db {
        /// `credentials <service>`.
        #[command(subcommand)]
        action: DbAction,
    },
    /// Sets up (or removes) the names of the local sites: needs your administrator rights, once.
    System {
        /// `install` or `uninstall`.
        #[command(subcommand)]
        action: SystemAction,
    },
    /// Starts the daemon when you sign in to Windows (or stops doing it).
    Autostart {
        /// `on`, `off` or `status` (the default).
        #[arg(value_enum, default_value = "status")]
        action: AutostartAction,
    },
    /// Puts the commands `php`, `node`, `npm`, `composer`… in Crisol's shims folder, or takes
    /// them out. They do nothing in a terminal until the folder is in your PATH (`crisol path add`).
    Shims {
        /// `install`, `remove`, `sync` or `status` (the default).
        #[arg(value_enum, default_value = "status")]
        action: ShimsAction,
    },
    /// Adds Crisol's shims folder to your user PATH, or takes it out. It changes nothing else of
    /// the PATH, and nothing in the machine PATH.
    Path {
        /// `add`, `remove` or `status` (the default).
        #[arg(value_enum, default_value = "status")]
        action: PathAction,
    },
    /// Prints the lines that put Crisol's shims first in the PATH of **one terminal**, without
    /// writing anything: `crisol env --shell powershell | Invoke-Expression` in PowerShell.
    Env {
        /// `powershell`, `cmd` or `bash`; by default the one you are in.
        #[arg(long, value_enum)]
        shell: Option<ShellArg>,
    },
    /// Opens a terminal where only Crisol decides which `php`, `node` or `mysql` runs: its
    /// shims and database clients first, the variables of other tools taken out, and your
    /// profile followed by Crisol's. `exit` takes you back. Nothing outside it changes.
    Shell {
        /// `pwsh`, `powershell`, `cmd` or `bash`; by default the one you are in.
        #[arg(long, value_enum)]
        shell: Option<TerminalArg>,
        /// Without your own profile (`$PROFILE`, `AutoRun`, `~/.bashrc`).
        #[arg(long)]
        clean: bool,
    },
}

/// What `crisol shims` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShimsAction {
    /// Copy the shims into the shims folder.
    Install,
    /// Delete them.
    Remove,
    /// Make the commands of the programs installed with `npm install --global` match what the
    /// Node versions have (`npm` does it by itself when it installs or removes one).
    Sync,
    /// Say which are there.
    Status,
}

/// What `crisol path` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PathAction {
    /// Put the shims folder first in the user PATH.
    Add,
    /// Take it out.
    Remove,
    /// Say whether it is there and what wins over it.
    Status,
}

/// The shell `crisol env` writes for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShellArg {
    /// PowerShell.
    Powershell,
    /// `cmd.exe`.
    Cmd,
    /// Git Bash and other POSIX shells.
    Bash,
}

/// The terminal `crisol shell` opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TerminalArg {
    /// PowerShell 7.
    Pwsh,
    /// Windows PowerShell 5.1.
    Powershell,
    /// `cmd.exe`.
    Cmd,
    /// Git Bash.
    Bash,
}

/// What `crisol system` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum SystemAction {
    /// Installs the privileged helper in Program Files and makes the DNS rule for the local
    /// sites. Windows asks for your permission once.
    Install,
    /// Removes the DNS rule, Crisol's block of the hosts file and the helper. Windows asks for
    /// your permission once.
    Uninstall,
}

/// What to do with the sign-in entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AutostartAction {
    /// Make the daemon start at sign-in.
    On,
    /// Stop it from doing so.
    Off,
    /// Say whether it does.
    Status,
}

/// What `crisol db` does.
#[derive(Debug, Subcommand)]
pub enum DbAction {
    /// Shows what a project needs to connect to a service, as lines for its `.env`: the user,
    /// the host, the port and the password Crisol generated for it (`mysql`, `postgresql`,
    /// `redis`, `meilisearch`, …).
    Credentials {
        /// The service.
        service: String,
    },
    /// Makes a new password for a service without losing its data, with the service running
    /// (`ALTER USER`, `ALTER ROLE`, `CONFIG SET`…), and keeps it where `credentials` shows it.
    /// Garnet and Meilisearch read theirs only when they start: stop them first. Update the
    /// `.env` of your projects afterwards.
    ResetPassword {
        /// The service.
        service: String,
    },
}

/// What a command comes to: one call, or following a log.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Make this call and show the answer.
    Call(Call),
    /// Show a log and keep asking for what is added.
    Follow {
        /// The service.
        service: String,
        /// Lines from the end to start with.
        lines: usize,
    },
    /// `system`: the one-time setup that needs an administrator.
    System(SystemAction),
    /// `autostart`: a setting of the user, not something the daemon does.
    Autostart(AutostartAction),
    /// `shims`: the files in the shims folder, which belong to the user as well.
    Shims(ShimsAction),
    /// `path`: the user PATH.
    Path(PathAction),
    /// `env`: the lines for one terminal.
    Env(Option<ShellArg>),
    /// Open a `crisol shell`.
    Shell {
        /// The terminal asked for.
        shell: Option<TerminalArg>,
        /// Without the user's profile.
        clean: bool,
    },
}

/// Parses the arguments (the first is the program name).
pub fn parse<I, T>(args: I) -> Result<Args, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    Args::try_parse_from(args)
}

/// An absolute version of `path` relative to `cwd`, without touching the file system and
/// without the `\\?\` prefix `canonicalize` would add.
fn absolute(path: Option<&Path>, cwd: &Path) -> String {
    let joined = match path {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => cwd.join(path),
        None => cwd.to_path_buf(),
    };
    // `.` and `..` are resolved by name (a link in the path is not followed).
    let mut parts = Vec::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !matches!(
                    parts.last(),
                    Some(std::path::Component::RootDir | std::path::Component::Prefix(_)) | None
                ) {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect::<PathBuf>().display().to_string()
}

fn name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

impl Command {
    /// What this command asks for. `cwd` is the folder the command ran in.
    #[must_use]
    pub fn plan(self, cwd: &Path) -> Plan {
        let call = match self {
            Self::Start { service } => Call::Start { service },
            Self::Restart { service } => Call::Restart { service },
            Self::Status { service } => Call::Status { service },
            Self::Stop {
                daemon: true,
                service: _,
            }
            | Self::Shutdown => Call::Shutdown,
            Self::Stop {
                daemon: false,
                service,
            } => Call::Stop { service },
            Self::Install {
                service,
                version,
                from,
                sha256,
            } => Call::Install {
                service,
                version,
                from: from.map(|path| absolute(Some(&path), cwd)),
                sha256,
            },
            Self::Uninstall { service, version } => Call::Uninstall { service, version },
            Self::List { available } => Call::List { available },
            Self::Use {
                tool,
                version,
                local,
                dir,
            } => Call::Use {
                tool: match tool {
                    ToolArg::Php => Tool::Php,
                    ToolArg::Node => Tool::Node,
                },
                version,
                dir: (local || dir.is_some()).then(|| absolute(dir.as_deref(), cwd)),
            },
            Self::Park { path } => Call::Park {
                path: absolute(path.as_deref(), cwd),
            },
            Self::Unpark { path } => Call::Unpark {
                path: absolute(path.as_deref(), cwd),
            },
            Self::Link {
                name,
                path,
                docroot,
            } => {
                let path = absolute(path.as_deref(), cwd);
                Call::Link {
                    name: name.unwrap_or_else(|| name_of(&path)),
                    path,
                    docroot,
                }
            }
            Self::Unlink { name } => Call::Unlink { name },
            Self::Sites => Call::Sites,
            Self::Secure { name } => Call::Secure { name },
            Self::Unsecure { name } => Call::Unsecure { name },
            Self::Logs {
                service,
                lines,
                follow,
            } => {
                if follow {
                    return Plan::Follow { service, lines };
                }
                Call::Logs {
                    service,
                    lines,
                    from: None,
                }
            }
            Self::Doctor => Call::Doctor,
            Self::Enable { service } => Call::Enable { service },
            Self::Disable { service } => Call::Disable { service },
            Self::Db {
                action: DbAction::Credentials { service },
            } => Call::Credentials { service },
            Self::Db {
                action: DbAction::ResetPassword { service },
            } => Call::ResetPassword { service },
            Self::Autostart { action } => return Plan::Autostart(action),
            Self::System { action } => return Plan::System(action),
            Self::Shims { action } => return Plan::Shims(action),
            Self::Path { action } => return Plan::Path(action),
            Self::Env { shell } => return Plan::Env(shell),
            Self::Shell { shell, clean } => return Plan::Shell { shell, clean },
        };
        Plan::Call(call)
    }
}

#[cfg(test)]
mod tests;
