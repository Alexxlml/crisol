//! What the daemon answers.

use serde::{Deserialize, Serialize};

/// The result of a [`crate::Call`]. In the JSON it is `{"type": "...", ...fields}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// Answer to `hello`.
    Hello(HelloInfo),
    /// Answer to `status`.
    Status(StatusInfo),
    /// Something was done (start, stop, link, secure, park, …).
    Done(DoneInfo),
    /// A version was installed.
    Installed(InstalledInfo),
    /// A version was removed.
    Removed(RemovedInfo),
    /// Answer to `list`.
    Versions(VersionList),
    /// A version was chosen.
    Used(UsedInfo),
    /// Answer to `sites`.
    Sites {
        /// Every site.
        sites: Vec<SiteInfo>,
        /// Folders and entries left out, with why.
        warnings: Vec<String>,
    },
    /// Answer to `logs`.
    Logs(LogChunk),
    /// Answer to `doctor`.
    Doctor {
        /// Each check, in order.
        checks: Vec<Check>,
    },
    /// Answer to `credentials`.
    Credentials(CredentialsInfo),
}

/// How to connect to a service, as the lines of a project's `.env`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialsInfo {
    /// The service.
    pub service: String,
    /// `KEY=value`, in order.
    pub lines: Vec<EnvLine>,
    /// Things to know.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// One line of an `.env`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvLine {
    /// `DB_PASSWORD`.
    pub key: String,
    /// Its value.
    pub value: String,
}

/// The daemon introduces itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloInfo {
    /// The daemon's version.
    pub daemon: String,
    /// The protocol it speaks.
    pub protocol: u32,
    /// Its Crisol home.
    pub home: String,
}

/// One service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// `nginx`, `php-8.4.26`, …
    pub name: String,
    /// `stopped`, `starting`, `running`, `stopping` or `failed`.
    pub state: String,
    /// Why, for a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The services and what the daemon has to say about them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusInfo {
    /// The services, in a stable order.
    pub services: Vec<ServiceInfo>,
    /// Things to tell the user.
    pub warnings: Vec<String>,
    /// The number of the last event sent before this picture was taken: it already includes
    /// that one and every earlier one. Absent when nothing numbers the events (the CLI
    /// answering by itself, with no daemon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The last `blocked` that still holds: Crisol will not change the running web stack until
    /// a folder is closed (ADR-0025, extension). Absent when nothing is blocked, and from a
    /// daemon older than the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    /// The last `reload_failed` that still holds: the new web configuration was not applied
    /// and the previous one goes on. Absent when the last one was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_error: Option<String>,
}

/// "Done", with a sentence and anything worth adding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoneInfo {
    /// What happened, ready to show.
    pub message: String,
    /// Whether anything actually changed (a repeated command changes nothing).
    pub changed: bool,
    /// Things to tell the user.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// An installed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledInfo {
    /// The tool.
    pub service: String,
    /// The version.
    pub version: String,
    /// `bin/<service>/<version>/`.
    pub path: String,
    /// It already was; nothing was downloaded.
    pub already: bool,
    /// The version no longer receives security fixes.
    pub end_of_life: bool,
    /// Bytes downloaded.
    pub downloaded: u64,
    /// Installed with `--from`: Crisol did not verify the archive (ADR-0020).
    #[serde(default)]
    pub unverified: bool,
    /// Known vulnerabilities of this version (ADR-0020).
    #[serde(default)]
    pub advisories: Vec<AdvisoryInfo>,
}

/// A known vulnerability that affects a version (ADR-0020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryInfo {
    /// Short name (`PHP2Shell`).
    pub id: String,
    /// One sentence on what is wrong.
    pub summary: String,
    /// Where to read more.
    pub url: String,
    /// The version that fixes it for this line, if there is one.
    #[serde(default)]
    pub fixed_in: Option<String>,
}

/// A removed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedInfo {
    /// The tool.
    pub service: String,
    /// The version.
    pub version: String,
}

/// A version, installed or offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Independent yes/no facts on the wire (a JSON object of booleans), not states of one machine.
#[allow(clippy::struct_excessive_bools)]
pub struct VersionInfo {
    /// The tool.
    pub service: String,
    /// The version.
    pub version: String,
    /// Installed on this machine.
    pub installed: bool,
    /// The one used when nothing else is chosen.
    #[serde(default)]
    pub recommended: bool,
    /// No longer receives security fixes.
    #[serde(default)]
    pub end_of_life: bool,
    /// Installed with `--from`, not verified by Crisol (ADR-0020).
    #[serde(default)]
    pub unverified: bool,
    /// Ids of the known vulnerabilities that affect it (ADR-0020).
    #[serde(default)]
    pub advisories: Vec<String>,
}

/// The answer to `list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionList {
    /// What is installed.
    pub installed: Vec<VersionInfo>,
    /// What the catalog offers, when asked for.
    #[serde(default)]
    pub available: Vec<VersionInfo>,
}

/// The result of choosing a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsedInfo {
    /// The file that holds the choice.
    pub file: String,
    /// What was written.
    pub request: String,
    /// The installed version it selects now.
    pub selects: String,
    /// What was there before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// The file was created or changed.
    pub changed: bool,
    /// Things to tell the user.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// A site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteInfo {
    /// `shop`.
    pub name: String,
    /// `shop.test`.
    pub domain: String,
    /// The project folder.
    pub root: String,
    /// The folder served.
    pub docroot: String,
    /// `parked` or `linked`.
    pub kind: String,
    /// Also served over HTTPS.
    pub secure: bool,
}

/// A piece of a log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogChunk {
    /// The lines, without newlines.
    pub lines: Vec<String>,
    /// Ask again with `from` set to this to get what comes next.
    pub next: u64,
    /// The file the lines come from.
    pub path: String,
}

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Fine.
    Ok,
    /// Works, but the user should know.
    Warning,
    /// Broken.
    Problem,
}

/// One line of `doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// A short name (`home permissions`).
    pub name: String,
    /// How it came out.
    pub status: CheckStatus,
    /// What was found.
    pub detail: String,
    /// The exact thing to do, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// What a button can do for `fix` (ADR-0021 §5). The CLI does not show it; `--json` carries
    /// it. An action this version does not know reads as `None`, so an older client keeps
    /// `doctor` and only loses the button.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient"
    )]
    pub action: Option<CheckAction>,
}

impl Check {
    /// Whether the button does what `fix` says (ADR-0021 section 5): a check without an action
    /// always agrees. For the ones with a `crisol` command (`calls`, `local`, `show_log`) `fix`
    /// is that command; a command to copy is in `fix`; a link or a path is in `fix` or in the
    /// `detail`. The tests of the core and of the platform assert this for every check they
    /// build, so the text and the button cannot drift apart.
    #[must_use]
    pub fn action_agrees_with_fix(&self) -> bool {
        let fix = self.fix.as_deref().unwrap_or_default();
        let mentioned = |text: &str| fix.contains(text) || self.detail.contains(text);
        match &self.action {
            None => true,
            Some(
                action @ (CheckAction::Calls { .. }
                | CheckAction::Local { .. }
                | CheckAction::ShowLog { .. }),
            ) => action.command().as_deref() == self.fix.as_deref(),
            Some(CheckAction::Copy { text }) => fix.contains(text.as_str()),
            Some(CheckAction::Link { url }) => mentioned(url),
            // The file is named by its path, or by its name (`in crisol.toml, set …`).
            Some(CheckAction::Reveal { path }) => {
                mentioned(path)
                    || std::path::Path::new(path)
                        .file_name()
                        .is_some_and(|name| mentioned(&name.to_string_lossy()))
            }
        }
    }

    /// The same check with a button.
    #[must_use]
    pub fn with_action(mut self, action: CheckAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// What a button of `doctor` does. A closed list: nothing here runs a program a reply names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckAction {
    /// Calls to the daemon, one after another; the first that fails stops the rest.
    Calls {
        /// The calls, in order.
        calls: Vec<crate::Call>,
    },
    /// An operation of the core that runs in the client's process, so that the dialogs of
    /// Windows (and the UAC) show in front of the person.
    Local {
        /// Which one.
        op: LocalOp,
    },
    /// A web page (`https` only).
    Link {
        /// The address.
        url: String,
    },
    /// The log viewer on this service.
    ShowLog {
        /// `nginx`, `mysql`, …
        service: String,
    },
    /// A command to copy. Never run.
    Copy {
        /// The text.
        text: String,
    },
    /// Opens Explorer with this file or folder selected. Only paths inside the home.
    Reveal {
        /// An absolute path.
        path: String,
    },
}

impl CheckAction {
    /// `crisol install <service> [<version>]`: without a version, the line Crisol recommends.
    #[must_use]
    pub fn install(service: &str, version: Option<&str>) -> Self {
        Self::Calls {
            calls: vec![install_call(service, version)],
        }
    }

    /// `crisol uninstall <service> <version> && crisol install <service> <version>`.
    #[must_use]
    pub fn reinstall(service: &str, version: &str) -> Self {
        Self::Calls {
            calls: vec![
                crate::Call::Uninstall {
                    service: service.to_owned(),
                    version: version.to_owned(),
                },
                install_call(service, Some(version)),
            ],
        }
    }

    /// `crisol start`.
    #[must_use]
    pub fn start() -> Self {
        Self::Calls {
            calls: vec![crate::Call::Start { service: None }],
        }
    }

    /// The `crisol` command that does the same as the button, written as the CLI takes it, for
    /// the actions that have one. `fix` says this command: the tests compare the two so that
    /// they cannot drift apart.
    #[must_use]
    pub fn command(&self) -> Option<String> {
        match self {
            Self::Calls { calls } => calls
                .iter()
                .map(call_command)
                .collect::<Option<Vec<_>>>()
                .map(|all| all.join(" && ")),
            Self::Local { op } => Some(
                match op {
                    LocalOp::Trust => "crisol trust",
                    LocalOp::UntrustStale => "crisol untrust --stale",
                    LocalOp::AutostartOff => "crisol autostart off",
                    LocalOp::PathAdd => "crisol path add",
                    LocalOp::SystemInstall => "crisol system install",
                }
                .to_owned(),
            ),
            Self::ShowLog { service } => Some(format!("crisol logs {service}")),
            Self::Copy { text } => Some(text.clone()),
            Self::Link { .. } | Self::Reveal { .. } => None,
        }
    }
}

fn install_call(service: &str, version: Option<&str>) -> crate::Call {
    crate::Call::Install {
        service: service.to_owned(),
        version: version.map(str::to_owned),
        from: None,
        sha256: None,
    }
}

fn call_command(call: &crate::Call) -> Option<String> {
    use crate::Call;
    Some(match call {
        Call::Start { service: None } => "crisol start".to_owned(),
        Call::Install {
            service,
            version: Some(version),
            from: None,
            ..
        } => format!("crisol install {service} {version}"),
        Call::Install {
            service,
            version: None,
            from: None,
            ..
        } => format!("crisol install {service}"),
        Call::Uninstall { service, version } => format!("crisol uninstall {service} {version}"),
        _ => return None,
    })
}

/// The operations of the core a client runs in its own process for a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalOp {
    /// `crisol trust`.
    Trust,
    /// `crisol untrust --stale`.
    UntrustStale,
    /// `crisol autostart off`.
    AutostartOff,
    /// `crisol path add`.
    PathAdd,
    /// `crisol system install` (asks for administrator permission).
    SystemInstall,
}

/// Reads an optional value, and gives `None` when it is not one this version knows.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}
