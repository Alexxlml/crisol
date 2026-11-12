//! The diagnostic screen (F13.7, ADR-0021 §5): what `doctor` found, and the button each finding
//! offers.
//!
//! The core says what is wrong and, next to the `fix` text, an action a button can take. This
//! decides which of them become a button, how it is called (the page only translates the
//! words), whether it asks for a second click, and carries the action out. It never runs a
//! program a reply names: the actions are a closed list, and the page sends back the very action
//! it was given, which is checked again before anything happens.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::SystemTime;

use crisol_core::autostart;
use crisol_core::paths::CrisolPaths;
use crisol_core::setup::suffix_of;
use crisol_core::shimsetup;
use crisol_core::trust::{prepare_trust, trust_ca, untrust_local, AddOutcome, TrustError};
use crisol_core::userpath::PathChange;
use crisol_protocol::{Call, Check, CheckAction, CheckStatus, ErrorCode, LocalOp, Reply, RpcError};
use serde::Serialize;

use crate::connect;
use crate::context::Context;
use crate::versions::{Progress, ProgressGate};

/// A sentence of the page: its key in the dictionaries, and the words that go in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Words {
    /// The key (`doctor.act.install`).
    pub key: &'static str,
    /// The values for `{service}`, `{version}`… that the sentence names.
    pub args: BTreeMap<&'static str, String>,
}

impl Words {
    fn new(key: &'static str) -> Self {
        Self {
            key,
            args: BTreeMap::new(),
        }
    }

    fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.args.insert(name, value.into());
        self
    }
}

/// What the page does with a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonKind {
    /// Asks the app to carry it out (`doctor_act`).
    Run,
    /// Copies `text` to the clipboard, like the credentials screen does.
    Copy,
    /// Goes to the log viewer on `service`.
    ShowLog,
}

/// The button of a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorButton {
    /// What it says.
    pub label: Words,
    /// What the page does with it.
    pub kind: ButtonKind,
    /// The click only arms it: a second one does it. True when the action removes something or
    /// asks Windows for a permission (§5.3).
    pub confirm: bool,
    /// For [`ButtonKind::Copy`], the command to copy; for [`ButtonKind::ShowLog`], the service.
    pub text: Option<String>,
    /// The action itself, which the page sends back when the button is pressed.
    pub action: CheckAction,
}

/// One line of `doctor`, as the screen draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorRow {
    /// A short name (`PATH`).
    pub name: String,
    /// How it came out.
    pub status: CheckStatus,
    /// What was found, in the core's words.
    pub detail: String,
    /// The exact thing to do (what the command line would say), shown under the button so
    /// that nothing is done without being seen first.
    pub fix: Option<String>,
    /// The button, when there is one that is safe to offer.
    pub button: Option<DoctorButton>,
}

/// What the diagnostic screen draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorView {
    /// Every line, in the core's order.
    pub rows: Vec<DoctorRow>,
    /// How many are problems.
    pub problems: usize,
    /// How many are warnings.
    pub warnings: usize,
}

/// Whether `path` is the home or inside it. The path must be absolute and already normal (a `..`
/// or `.` would let a reply walk out of the home), and the names are compared the way Windows
/// does, without regard to case.
#[must_use]
pub fn inside_home(paths: &CrisolPaths, path: &str) -> bool {
    let path = Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return false;
    }
    let lower = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (home, path) = (lower(paths.home()), lower(path));
    path.len() >= home.len() && path[..home.len()] == home[..]
}

/// Whether the address may be opened from a finding: `https` only (§5.4).
fn is_https(url: &str) -> bool {
    url.starts_with("https://") && crisol_platform::is_web_address(url)
}

/// The label of a list of calls, from what they are: the ones `doctor` makes.
fn calls_words(calls: &[Call]) -> Words {
    match calls {
        [Call::Install {
            service,
            version: Some(version),
            ..
        }] => Words::new("doctor.act.install")
            .with("service", service.clone())
            .with("version", version.clone()),
        [Call::Install {
            service,
            version: None,
            ..
        }] => Words::new("doctor.act.install_recommended").with("service", service.clone()),
        [Call::Uninstall { service, version }, Call::Install { .. }] => {
            Words::new("doctor.act.reinstall")
                .with("service", service.clone())
                .with("version", version.clone())
        }
        [Call::Start { service: None }] => Words::new("doctor.act.start"),
        _ => Words::new("doctor.act.calls"),
    }
}

/// An installation from an archive the person vouches for, not from the catalog.
fn own_archive(call: &Call) -> bool {
    matches!(call, Call::Install { from, sha256, .. } if from.is_some() || sha256.is_some())
}

/// The button an action becomes, or `None` when it must not be offered: a link that is not
/// `https`, a path outside the home, an action with nothing in it.
#[must_use]
pub fn doctor_button(paths: &CrisolPaths, action: &CheckAction) -> Option<DoctorButton> {
    let (label, kind, confirm, text) = match action {
        // An archive of one's own (`install --from`) stays a command of the terminal (F13.5):
        // the core never offers it here, so a button that asks for one is not offered either.
        CheckAction::Calls { calls } if !calls.is_empty() && !calls.iter().any(own_archive) => {
            let removes = calls.iter().any(|c| matches!(c, Call::Uninstall { .. }));
            (calls_words(calls), ButtonKind::Run, removes, None)
        }
        CheckAction::Local { op } => {
            let (key, confirm) = match op {
                LocalOp::Trust => ("doctor.act.trust", true),
                LocalOp::UntrustStale => ("doctor.act.untrust_stale", true),
                LocalOp::AutostartOff => ("doctor.act.autostart_off", false),
                LocalOp::PathAdd => ("doctor.act.path_add", false),
                LocalOp::SystemInstall => ("doctor.act.system_install", true),
            };
            (Words::new(key), ButtonKind::Run, confirm, None)
        }
        CheckAction::Link { url } if is_https(url) => {
            (Words::new("doctor.act.link"), ButtonKind::Run, false, None)
        }
        CheckAction::ShowLog { service } if !service.is_empty() => (
            Words::new("doctor.act.show_log"),
            ButtonKind::ShowLog,
            false,
            Some(service.clone()),
        ),
        CheckAction::Copy { text } if !text.is_empty() => (
            Words::new("doctor.act.copy"),
            ButtonKind::Copy,
            false,
            Some(text.clone()),
        ),
        CheckAction::Reveal { path } if inside_home(paths, path) => (
            Words::new("doctor.act.reveal"),
            ButtonKind::Run,
            false,
            None,
        ),
        _ => return None,
    };
    Some(DoctorButton {
        label,
        kind,
        confirm,
        text,
        action: action.clone(),
    })
}

/// Puts the findings of `doctor` in the shape the screen draws.
#[must_use]
pub fn doctor_view(checks: Vec<Check>, paths: &CrisolPaths) -> DoctorView {
    let count = |status| checks.iter().filter(|c| c.status == status).count();
    let (problems, warnings) = (count(CheckStatus::Problem), count(CheckStatus::Warning));
    let rows = checks
        .into_iter()
        .map(|check| DoctorRow {
            button: check
                .action
                .as_ref()
                .and_then(|action| doctor_button(paths, action)),
            name: check.name,
            status: check.status,
            detail: check.detail,
            fix: check.fix,
        })
        .collect();
    DoctorView {
        rows,
        problems,
        warnings,
    }
}

/// Asks `doctor` and draws it. The daemon answers, or this process when there is none.
pub fn fetch_doctor(context: &Context) -> Result<DoctorView, RpcError> {
    match connect::call(context, &Call::Doctor, &mut |_| {})? {
        Reply::Doctor { checks } => Ok(doctor_view(checks, &context.paths)),
        other => Err(RpcError::new(
            ErrorCode::Internal,
            format!("unexpected answer to `doctor`: {other:?}"),
        )),
    }
}

/// What a button did, in words the page translates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionDone {
    /// What happened.
    pub words: Words,
    /// Windows or the person said no: nothing was changed, and it is not an error.
    pub declined: bool,
}

impl ActionDone {
    fn ok(words: Words) -> Self {
        Self {
            words,
            declined: false,
        }
    }

    fn declined(words: Words) -> Self {
        Self {
            words,
            declined: true,
        }
    }
}

fn io_failure(error: &std::io::Error) -> RpcError {
    RpcError::new(ErrorCode::Io, error.to_string())
}

fn trust_failure(error: TrustError) -> RpcError {
    match error {
        TrustError::Home(home) => crisol_core::engine::home_error(&home),
        TrustError::Ca(ca) => RpcError::new(ErrorCode::InvalidParams, ca.to_string()),
        TrustError::Store(io) => RpcError::new(ErrorCode::Io, format!("trust store: {io}")),
    }
}

/// What `crisol trust` shows before Windows asks (§5.3): the second click of the trust button
/// shows this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustPreview {
    /// `Crisol Local CA 7F3A9C`.
    pub name: String,
    /// Made by this very click: there was none.
    pub created: bool,
    /// Windows trusts it already: nothing will be asked.
    pub already: bool,
    /// What it vouches for, and nothing else.
    pub vouches_for: String,
    /// Until when.
    pub valid_until: String,
    /// Its SHA-256.
    pub sha256: String,
    /// The thumbprint the dialog of Windows shows (SHA-1), to compare with it.
    pub sha1: Option<String>,
}

/// Gets the CA ready (making it if there is none, as `crisol trust` does) and says what it is.
/// Nothing is added to the store: that is the next click.
pub fn trust_preview(context: &Context) -> Result<TrustPreview, RpcError> {
    let store = (context.trust_store)().map_err(|e| io_failure(&e))?;
    let plan = prepare_trust(
        store.as_ref(),
        &context.paths,
        context.dirs.as_ref(),
        &suffix_of(&context.paths),
        SystemTime::now(),
    )
    .map_err(trust_failure)?;
    let info = plan.ca.info();
    Ok(TrustPreview {
        name: info.common_name.clone(),
        created: plan.created,
        already: plan.already,
        vouches_for: format!(
            "{} and their subdomains, and {}",
            info.permitted_dns.join(", "),
            info.permitted_ips.join(", ")
        ),
        valid_until: info.valid_until(),
        sha256: info.fingerprint.clone(),
        sha1: plan.thumbprint,
    })
}

fn run_trust(context: &Context) -> Result<ActionDone, RpcError> {
    let store = (context.trust_store)().map_err(|e| io_failure(&e))?;
    let plan = prepare_trust(
        store.as_ref(),
        &context.paths,
        context.dirs.as_ref(),
        &suffix_of(&context.paths),
        SystemTime::now(),
    )
    .map_err(trust_failure)?;
    let outcome = trust_ca(
        store.as_ref(),
        &plan.ca,
        &context.paths,
        context.dirs.as_ref(),
    )
    .map_err(trust_failure)?;
    Ok(match outcome {
        AddOutcome::Added => ActionDone::ok(Words::new("doctor.done.trust_added")),
        AddOutcome::AlreadyThere => ActionDone::ok(Words::new("doctor.done.trust_already")),
        AddOutcome::Declined => ActionDone::declined(Words::new("doctor.done.trust_declined")),
    })
}

fn run_untrust_stale(context: &Context) -> Result<ActionDone, RpcError> {
    let store = (context.trust_store)().map_err(|e| io_failure(&e))?;
    let report = untrust_local(
        store.as_ref(),
        &context.paths,
        context.dirs.as_ref(),
        true,
        SystemTime::now(),
    )
    .map_err(trust_failure)?;
    Ok(if !report.removed.is_empty() {
        ActionDone::ok(
            Words::new("doctor.done.untrust_done").with("names", report.removed.join(", ")),
        )
    } else if !report.declined.is_empty() {
        ActionDone::declined(Words::new("doctor.done.untrust_declined"))
    } else {
        ActionDone::ok(Words::new("doctor.done.untrust_none"))
    })
}

fn run_system_install(context: &Context) -> Result<ActionDone, RpcError> {
    match context.setup.install(&suffix_of(&context.paths)) {
        Ok(report) => Ok(ActionDone::ok(
            Words::new("doctor.done.system_installed").with("steps", report.steps.join("; ")),
        )),
        // The person said no at the permission prompt: nothing was changed.
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(
            ActionDone::declined(Words::new("doctor.done.system_declined")),
        ),
        Err(error) => Err(error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<crisol_core::programs::ProgramError>())
            .map_or_else(
                || io_failure(&error),
                |program| RpcError::new(ErrorCode::NotAvailable, program.to_string()),
            )),
    }
}

fn run_local(context: &Context, op: LocalOp) -> Result<ActionDone, RpcError> {
    match op {
        LocalOp::Trust => run_trust(context),
        LocalOp::UntrustStale => run_untrust_stale(context),
        LocalOp::AutostartOff => match autostart::disable(context.autostart.as_ref()) {
            Ok(true) => Ok(ActionDone::ok(Words::new("doctor.done.autostart_off"))),
            Ok(false) => Ok(ActionDone::ok(Words::new("doctor.done.autostart_was_off"))),
            Err(error) => Err(io_failure(&error)),
        },
        LocalOp::PathAdd => match shimsetup::path_add(context.user_path.as_ref(), &context.paths) {
            Ok(PathChange::Added) => Ok(ActionDone::ok(Words::new("doctor.done.path_added"))),
            Ok(_) => Ok(ActionDone::ok(Words::new("doctor.done.path_already"))),
            Err(error) => Err(io_failure(&error)),
        },
        LocalOp::SystemInstall => run_system_install(context),
    }
}

/// Carries out the action of a button. It is checked again here, as if it had never been drawn:
/// what [`doctor_button`] would not offer is refused, and so is a copy or a log, which the page
/// does itself. `on_progress` hears how far an installation is.
pub fn run_doctor_action(
    context: &Context,
    action: &CheckAction,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<ActionDone, RpcError> {
    let offered = doctor_button(&context.paths, action)
        .filter(|button| button.kind == ButtonKind::Run)
        .ok_or_else(|| {
            RpcError::new(
                ErrorCode::InvalidParams,
                "that action is not one the screen offers",
            )
        })?;
    match &offered.action {
        CheckAction::Calls { calls } => {
            for call in calls {
                let mut gate = ProgressGate::new();
                connect::call_with_events(context, call, &mut |_| {}, &mut |event| {
                    if let Some(progress) = gate.pass(&event) {
                        on_progress(progress);
                    }
                })?;
            }
            Ok(ActionDone::ok(Words::new("doctor.done.calls")))
        }
        CheckAction::Local { op } => run_local(context, *op),
        CheckAction::Link { url } => {
            context.desktop.open_url(url).map_err(|e| io_failure(&e))?;
            Ok(ActionDone::ok(Words::new("doctor.done.link")))
        }
        CheckAction::Reveal { path } => {
            context
                .desktop
                .reveal(Path::new(path))
                .map_err(|e| io_failure(&e))?;
            Ok(ActionDone::ok(Words::new("doctor.done.reveal")))
        }
        CheckAction::ShowLog { .. } | CheckAction::Copy { .. } => Err(RpcError::new(
            ErrorCode::InvalidParams,
            "the page does that one itself",
        )),
    }
}

#[cfg(test)]
mod tests;
