//! What a client can ask for.

use serde::{Deserialize, Serialize};

/// The tools that have a default version and a per-project pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    /// PHP.
    Php,
    /// Node.js.
    Node,
}

/// An operation and its parameters. In the JSON it is `"method"` plus `"params"`.
///
/// Some operations only change files (`install`, `use`, `link`, …): a client that finds no
/// daemon may do them itself through the same core. The rest (`start`, `stop`, `status`, …) need
/// the daemon, which owns the running services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    /// First message of a connection: who is talking and which protocol it speaks.
    Hello {
        /// A name for logs (`crisol-cli 0.1.0`).
        client: String,
        /// The [`crate::PROTOCOL_VERSION`] the client speaks.
        protocol: u32,
    },
    /// The state of the services (all, or one by name).
    Status {
        /// `nginx`, `php-8.4.26`, …
        #[serde(default)]
        service: Option<String>,
    },
    /// Starts the services (all, or one).
    Start {
        /// Which one, or all.
        #[serde(default)]
        service: Option<String>,
    },
    /// Stops the services (all, or one).
    Stop {
        /// Which one, or all.
        #[serde(default)]
        service: Option<String>,
    },
    /// Stops and starts again.
    Restart {
        /// Which one, or all.
        #[serde(default)]
        service: Option<String>,
    },
    /// Stops everything the daemon started and exits.
    Shutdown,
    /// Installs a version.
    Install {
        /// `php`, `nginx`, `node`, …
        service: String,
        /// `8.4`, `8.4.26` or `latest`.
        version: String,
    },
    /// Removes an installed version.
    Uninstall {
        /// The tool.
        service: String,
        /// An exact version or a prefix matching exactly one.
        version: String,
    },
    /// What is installed, and optionally what could be.
    List {
        /// Also list what the catalog offers for this platform.
        #[serde(default)]
        available: bool,
    },
    /// Chooses a version: the default, or the one of a project folder.
    Use {
        /// PHP or Node.
        tool: Tool,
        /// What to choose (`8.4`, `22`, …).
        version: String,
        /// The project folder to pin instead of the global default.
        #[serde(default)]
        dir: Option<String>,
    },
    /// Serves every subfolder of this folder as a site.
    Park {
        /// An absolute path.
        path: String,
    },
    /// Stops serving the subfolders of a parked folder.
    Unpark {
        /// The path, as it was parked.
        path: String,
    },
    /// Serves one folder under a name.
    Link {
        /// The site name (`shop` for `shop.test`).
        name: String,
        /// The folder, absolute.
        path: String,
        /// The served subfolder, if not detected.
        #[serde(default)]
        docroot: Option<String>,
    },
    /// Stops serving a linked site.
    Unlink {
        /// The site name.
        name: String,
    },
    /// Every site.
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
    /// The end of a service's log.
    Logs {
        /// `nginx`, `php-8.4.26`, …
        service: String,
        /// How many lines from the end, when `from` is not given.
        #[serde(default = "default_lines")]
        lines: usize,
        /// Continue from this byte offset (the `next` of the previous chunk): what `logs -f`
        /// does, by asking again.
        #[serde(default)]
        from: Option<u64>,
    },
    /// Checks the installation and says what is wrong and how to fix it.
    Doctor,
    /// Asks the daemon to push [`crate::Event`]s on this connection.
    Subscribe,
}

fn default_lines() -> usize {
    50
}

impl Call {
    /// The name of the operation, as it is in the JSON.
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Status { .. } => "status",
            Self::Start { .. } => "start",
            Self::Stop { .. } => "stop",
            Self::Restart { .. } => "restart",
            Self::Shutdown => "shutdown",
            Self::Install { .. } => "install",
            Self::Uninstall { .. } => "uninstall",
            Self::List { .. } => "list",
            Self::Use { .. } => "use",
            Self::Park { .. } => "park",
            Self::Unpark { .. } => "unpark",
            Self::Link { .. } => "link",
            Self::Unlink { .. } => "unlink",
            Self::Sites => "sites",
            Self::Secure { .. } => "secure",
            Self::Unsecure { .. } => "unsecure",
            Self::Logs { .. } => "logs",
            Self::Doctor => "doctor",
            Self::Subscribe => "subscribe",
        }
    }

    /// Whether the operation writes in the Crisol home (so the home must exist, and be private,
    /// before it runs). The others only look, and answer on a home that is not there yet.
    #[must_use]
    pub fn writes(&self) -> bool {
        matches!(
            self,
            Self::Install { .. }
                | Self::Uninstall { .. }
                | Self::Use { .. }
                | Self::Park { .. }
                | Self::Unpark { .. }
                | Self::Link { .. }
                | Self::Unlink { .. }
                | Self::Secure { .. }
                | Self::Unsecure { .. }
                | Self::Start { .. }
                | Self::Restart { .. }
        )
    }

    /// Whether the operation needs the daemon (it acts on running services, or is about the
    /// connection itself). The others only change files.
    #[must_use]
    pub fn needs_daemon(&self) -> bool {
        matches!(
            self,
            Self::Hello { .. }
                | Self::Status { .. }
                | Self::Start { .. }
                | Self::Stop { .. }
                | Self::Restart { .. }
                | Self::Shutdown
                | Self::Subscribe
        )
    }
}
