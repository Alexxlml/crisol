//! The versions screen: what is installed and what could be, which button each row offers, and
//! the progress of an installation (F13.5).
//!
//! The daemon says what is installed and what the catalog offers (`list`); this puts both in one
//! list per service, marks the version the shims use when nothing else is chosen, decides the
//! buttons of every row and turns a click into the call the command line makes. The page only
//! draws it.

use std::collections::BTreeMap;

use crisol_core::config::Config;
use crisol_core::resolve::{resolve, ResolveContext, Source, Tool as CoreTool};
use crisol_core::version::Version;
use crisol_protocol::{Call, ErrorCode, Event, Reply, RpcError, Tool, VersionInfo, VersionList};
use serde::{Deserialize, Serialize};

use crate::connect;
use crate::context::Context;

/// A button of a version's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionAction {
    /// `install <service> <version>`: only what the catalog offers and is not installed.
    Install,
    /// `uninstall <service> <version>`.
    Uninstall,
    /// `use <tool> <version>`: the version for everywhere; only PHP and Node have one.
    Use,
}

/// The tool a service's version is chosen for, if it is one (`php` and `node`).
#[must_use]
pub fn tool_of(service: &str) -> Option<Tool> {
    match service {
        "php" => Some(Tool::Php),
        "node" => Some(Tool::Node),
        _ => None,
    }
}

/// Which buttons a row offers. Something that is not installed can only be installed. Something
/// installed can be removed, and chosen as the default when it is PHP or Node and is not already.
/// `chosen` is "the version in use, and `crisol.toml` agrees": the version in use only because
/// the one in `[defaults]` was removed can still be chosen, which puts `crisol.toml` right.
#[must_use]
pub fn version_actions(service: &str, installed: bool, chosen: bool) -> Vec<VersionAction> {
    if !installed {
        return vec![VersionAction::Install];
    }
    let mut actions = Vec::new();
    if tool_of(service).is_some() && !chosen {
        actions.push(VersionAction::Use);
    }
    actions.push(VersionAction::Uninstall);
    actions
}

/// One version, as a list draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
// Independent facts a page draws as separate marks, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
pub struct VersionRow {
    /// The tool (`php`).
    pub service: String,
    /// The exact version (`8.4.26`).
    pub version: String,
    /// On this machine.
    pub installed: bool,
    /// The catalog's recommended line.
    pub recommended: bool,
    /// The version the shims run when nothing else is chosen.
    pub default: bool,
    /// No longer receives security fixes.
    pub end_of_life: bool,
    /// Installed with `--from`, not verified by Crisol (ADR-0020).
    pub unverified: bool,
    /// Ids of the known vulnerabilities that affect it (ADR-0020).
    pub advisories: Vec<String>,
    /// The buttons that change something.
    pub actions: Vec<VersionAction>,
}

/// The versions of one tool, the highest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceVersions {
    /// The tool (`php`).
    pub service: String,
    /// Its versions.
    pub rows: Vec<VersionRow>,
    /// The version `[defaults]` asks for when it is no longer installed (`7.4`): the shims use
    /// another one, and say so every time they run, until one is chosen.
    pub missing_default: Option<String>,
}

/// The version a tool runs when nothing else is chosen, as [`versions_view`] is told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InUse {
    /// The exact installed version (`8.4.27`).
    pub version: String,
    /// What `[defaults]` asks for, when it is not installed and `version` stands in for it.
    pub missing_default: Option<String>,
}

impl InUse {
    /// The version in use, as `crisol.toml` asks (or as the catalog recommends without one).
    #[must_use]
    pub fn chosen(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            missing_default: None,
        }
    }
}

/// What the versions screen draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionsView {
    /// Each tool, PHP and Node first.
    pub services: Vec<ServiceVersions>,
}

/// The order tools are listed in: the ones the shims run, then the web server, then the rest
/// alphabetically.
fn rank(service: &str) -> (usize, &str) {
    let first = ["php", "node", "nginx", "composer"]
        .iter()
        .position(|name| *name == service)
        .unwrap_or(usize::MAX);
    (first, service)
}

/// Puts what is installed together with what the catalog offers. A version installed with
/// `--from` is not in the catalog and is listed anyway. `default_of` says which installed
/// version a tool runs when nothing else is chosen.
#[must_use]
pub fn versions_view(
    list: VersionList,
    default_of: &dyn Fn(&str) -> Option<InUse>,
) -> VersionsView {
    let mut rows: BTreeMap<String, Vec<VersionRow>> = BTreeMap::new();
    let mut add = |info: VersionInfo, installed_here: bool| {
        let list = rows.entry(info.service.clone()).or_default();
        if let Some(known) = list.iter_mut().find(|row| row.version == info.version) {
            // The catalog and the disk both know it: what is installed is what is on the disk.
            known.installed |= installed_here;
            known.unverified |= info.unverified;
            return;
        }
        list.push(VersionRow {
            installed: installed_here,
            default: false,
            actions: Vec::new(),
            service: info.service,
            version: info.version,
            recommended: info.recommended,
            end_of_life: info.end_of_life,
            unverified: info.unverified,
            advisories: info.advisories,
        });
    };
    for info in list.installed {
        add(info, true);
    }
    for info in list.available {
        let installed = info.installed;
        add(info, installed);
    }
    let mut services: Vec<ServiceVersions> = rows
        .into_iter()
        .map(|(service, mut rows)| {
            let in_use = default_of(&service);
            let missing_default = in_use.as_ref().and_then(|u| u.missing_default.clone());
            for row in &mut rows {
                row.default =
                    row.installed && in_use.as_ref().is_some_and(|u| u.version == row.version);
                let chosen = row.default && missing_default.is_none();
                row.actions = version_actions(&row.service, row.installed, chosen);
            }
            // The highest first; a version that does not parse (it should not happen) goes last.
            rows.sort_by(
                |a, b| match (Version::parse(&a.version), Version::parse(&b.version)) {
                    (Ok(a), Ok(b)) => b.cmp(&a),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => a.version.cmp(&b.version),
                },
            );
            ServiceVersions {
                service,
                rows,
                missing_default,
            }
        })
        .collect();
    services.sort_by(|a, b| rank(&a.service).cmp(&rank(&b.service)));
    VersionsView { services }
}

/// Asks what is installed and what is offered, and which installed version PHP and Node run by
/// default. Listing starts nothing: the daemon answers, or this process when there is none.
pub fn fetch_versions(context: &Context) -> Result<VersionsView, RpcError> {
    let list = match connect::call(context, &Call::List { available: true }, &mut |_| {})? {
        Reply::Versions(list) => list,
        other => {
            return Err(RpcError::new(
                ErrorCode::Internal,
                format!("unexpected answer to `list`: {other:?}"),
            ))
        }
    };
    let deps = (context.deps)().map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?;
    let env = |_: &str| None;
    // A `crisol.toml` that does not load leaves nothing to compare with; the shims would fail
    // the same way, and `doctor` says why.
    let defaults = Config::load_from(&context.paths)
        .map(|loaded| loaded.config.defaults)
        .ok();
    let default_of = |service: &str| -> Option<InUse> {
        let (tool, asked) = match service {
            "php" => (CoreTool::Php, defaults.as_ref().and_then(|d| d.php.clone())),
            "node" => (
                CoreTool::Node,
                defaults.as_ref().and_then(|d| d.node.clone()),
            ),
            _ => return None,
        };
        // The very rule the shims follow, asked from the home itself: no project, and no
        // `CRISOL_PHP` of this process.
        let resolved = resolve(
            tool,
            &ResolveContext {
                paths: &context.paths,
                cwd: context.paths.home(),
                ceiling: Some(context.paths.home()),
                owners: deps.owners.as_ref(),
                env: &env,
                catalog: &deps.catalog,
            },
        )
        .ok()?;
        Some(InUse {
            version: resolved.resolved.version.version.to_string(),
            missing_default: missing_default(asked, &resolved.resolved.source),
        })
    };
    Ok(versions_view(list, &default_of))
}

/// What `[defaults]` asks for (`asked`), when the resolution passed over it. Without a project
/// that happens only when the version is not installed (`resolve`, step 5): the source is then
/// the catalog's line or the highest version, which a written default never is.
#[must_use]
fn missing_default(asked: Option<String>, source: &Source) -> Option<String> {
    let stood_in = matches!(
        source,
        Source::CatalogRecommendation | Source::HighestInstalled
    );
    asked.filter(|_| stood_in)
}

/// A click of the versions screen, as the page sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum VersionAct {
    /// Install an exact version of the catalog.
    Install {
        /// The tool.
        service: String,
        /// The exact version.
        version: String,
    },
    /// Remove an installed version.
    Uninstall {
        /// The tool.
        service: String,
        /// The exact version.
        version: String,
    },
    /// Choose the version for everywhere.
    Use {
        /// The tool.
        service: String,
        /// The exact version.
        version: String,
    },
}

impl VersionAct {
    /// The call this click makes: the same one the command line makes. `use` for something that
    /// has no version to choose (`nginx`) is refused here, not sent.
    pub fn call(&self) -> Result<Call, RpcError> {
        match self {
            Self::Install { service, version } => Ok(Call::Install {
                service: service.clone(),
                version: Some(version.clone()),
                from: None,
                sha256: None,
            }),
            Self::Uninstall { service, version } => Ok(Call::Uninstall {
                service: service.clone(),
                version: version.clone(),
            }),
            Self::Use { service, version } => {
                let tool = tool_of(service).ok_or_else(|| {
                    RpcError::new(
                        ErrorCode::InvalidParams,
                        format!("there is no version of `{service}` to choose; only php and node"),
                    )
                })?;
                Ok(Call::Use {
                    tool,
                    version: version.clone(),
                    dir: None,
                })
            }
        }
    }

    /// What is being installed, as the progress events name it (`php 8.4.26`), when this is an
    /// installation.
    #[must_use]
    pub fn installing(&self) -> Option<String> {
        match self {
            Self::Install { service, version } => Some(format!("{service} {version}")),
            _ => None,
        }
    }
}

/// How far a download or installation is, as a bar draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Progress {
    /// What is being installed (`php 8.4.26`).
    pub what: String,
    /// Bytes so far.
    pub done: u64,
    /// Bytes in total, when the server said.
    pub total: Option<u64>,
    /// The bytes come from a file the person gave, not the network.
    pub from_file: bool,
    /// Whole percent, when the total is known.
    pub percent: Option<u8>,
}

/// Decides which progress events reach the page: one per percent, and one per megabyte when the
/// total is not known, so a fast download does not flood the window with thousands of events.
#[derive(Debug, Default)]
pub struct ProgressGate {
    last: Option<(String, u64)>,
}

impl ProgressGate {
    /// A gate that has let nothing through yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The progress `event` stands for, or `None` when it is another event or says nothing new.
    pub fn pass(&mut self, event: &Event) -> Option<Progress> {
        let Event::Progress {
            what,
            done,
            total,
            from_file,
        } = event
        else {
            return None;
        };
        let percent = total
            .filter(|total| *total > 0)
            .map(|total| u8::try_from((done.saturating_mul(100) / total).min(100)).unwrap_or(100));
        // The step of the gate: a percent, or a megabyte without a total.
        let step = percent.map_or(done / (1 << 20), u64::from);
        let finished = total.is_some_and(|total| done >= &total);
        let same = self
            .last
            .as_ref()
            .is_some_and(|(last_what, last_step)| last_what == what && *last_step == step);
        if same && !finished {
            return None;
        }
        self.last = Some((what.clone(), step));
        Some(Progress {
            what: what.clone(),
            done: *done,
            total: *total,
            from_file: *from_file,
            percent,
        })
    }
}

/// Makes the call of `act`, telling `on_progress` how far an installation is. The installation
/// runs in the daemon, which finishes it (or deletes what it staged) whatever becomes of this
/// window: closing it does not cancel anything.
pub fn run_version_act(
    context: &Context,
    act: &VersionAct,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Reply, RpcError> {
    let call = act.call()?;
    let mut gate = ProgressGate::new();
    connect::call_with_events(context, &call, &mut |_| {}, &mut |event| {
        if let Some(progress) = gate.pass(&event) {
            on_progress(progress);
        }
    })
}

#[cfg(test)]
mod tests;
