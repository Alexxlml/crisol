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
    /// Stops everything and the daemon. With `--keep-daemon`, only the services.
    Stop {
        /// Stop the services but leave the daemon running (the local names keep resolving).
        #[arg(long)]
        keep_daemon: bool,
        /// Only this service.
        service: Option<String>,
    },
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
        /// `8.4`, `8.4.26` or `latest` (the default).
        #[arg(id = "requested", value_name = "VERSION", default_value = "latest")]
        version: String,
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
    /// `stop`: shut down, or stop the services only.
    Stop {
        /// Leave the daemon running.
        keep_daemon: bool,
        /// A single service, if named.
        service: Option<String>,
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
                keep_daemon,
                service,
            } => {
                return Plan::Stop {
                    keep_daemon,
                    service,
                }
            }
            Self::Install { service, version } => Call::Install { service, version },
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
        };
        Plan::Call(call)
    }
}

#[cfg(test)]
mod tests;
