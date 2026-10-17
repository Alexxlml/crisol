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
}
