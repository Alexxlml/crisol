//! First run and uninstall (F13.8, ADR-0026 §1.5): the screens of `crisol setup` and
//! `crisol uninstall --all`.
//!
//! The core already does both in three parts: a **plan** that only reads, an **agreement**, and
//! a **run** that says how every step went (`onboarding` and `teardown`). The command line
//! prints the plan and waits for `yes`; a window shows it and waits for buttons. This module is
//! what they share: it builds the `System` both need from the [`Context`], turns a plan into
//! what a screen draws, and runs it. The run happens in this process, not in the daemon, so the
//! dialogs of Windows and the permission prompt come up in front of the person who asked.
//!
//! Nothing the page says is trusted: a run plans again and acts on that plan. The page only
//! sends which of the steps that are asked alone it agreed to, and whether the person said they
//! have a backup.

use std::time::SystemTime;

use crisol_core::i18n::{Language, Message};
use crisol_core::msg;
use crisol_core::onboarding::{self, Agreed as SetupAgreed};
use crisol_core::teardown::{self, Agreed as TeardownAgreed, Outcome, OutcomeKind, PurgeNotice};
use crisol_protocol::{ErrorCode, RpcError};
use serde::Serialize;

use crate::connect;
use crate::context::Context;

// ---- first run ---------------------------------------------------------------------------

/// One step of the setup, as the screen draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupStep {
    /// A short name (`trust`), which the page sends back for the steps it agreed to.
    pub id: &'static str,
    /// What will be done, in the core's words.
    pub description: String,
    /// It is asked alone, because it changes more than the person's own files: the page draws a
    /// switch for it. The others are done when the person presses the button.
    pub optional: bool,
    /// For an optional step, whether its switch starts on. What makes the `.test` sites work
    /// (trusting the CA and the helper) starts on; what changes what runs at sign-in does not.
    pub on_by_default: bool,
}

/// Something that stops the setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupProblem {
    /// What is wrong.
    pub message: String,
    /// The command that fixes it, when there is one.
    pub fix: Option<String>,
}

/// What the first-run screen draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupView {
    /// What is left to do, in order.
    pub steps: Vec<SetupStep>,
    /// What is in place already.
    pub done: Vec<String>,
    /// What stops the setup. While there is any, nothing is offered.
    pub problems: Vec<SetupProblem>,
    /// Things worth knowing that do not stop it.
    pub notes: Vec<String>,
    /// Crisol is not set up yet: a step it needs (the commands, the PATH) is left and nothing
    /// stops it. The other screens offer the setup while this holds. An optional step the
    /// person left off (starting at sign-in, which begins off) is a choice, not something
    /// missing, so it does not keep the offer up.
    pub incomplete: bool,
}

impl SetupView {
    /// Whether there is something to set up and nothing stops it: the screen offers its button.
    #[must_use]
    pub fn can_run(&self) -> bool {
        !self.steps.is_empty() && self.problems.is_empty()
    }
}

/// Whether the switch of an optional step starts on.
fn on_by_default(step: &onboarding::Step) -> bool {
    matches!(
        step,
        onboarding::Step::Trust | onboarding::Step::Helper { .. }
    )
}

/// The plan of the setup, as the screen draws it.
#[must_use]
pub fn setup_view(plan: &onboarding::Plan, language: Language) -> SetupView {
    SetupView {
        steps: plan
            .steps
            .iter()
            .map(|step| SetupStep {
                id: step.id(),
                description: step.describe().render(language),
                optional: step.optional(),
                on_by_default: step.optional() && on_by_default(step),
            })
            .collect(),
        done: said(&plan.done, language),
        problems: plan
            .problems
            .iter()
            .map(|p| SetupProblem {
                message: p.message.render(language),
                fix: p.fix.clone(),
            })
            .collect(),
        notes: said(&plan.notes, language),
        incomplete: plan.problems.is_empty() && plan.steps.iter().any(|step| !step.optional()),
    }
}

/// `messages` said in `language`.
fn said(messages: &[Message], language: Language) -> Vec<String> {
    messages.iter().map(|m| m.render(language)).collect()
}

/// One line of what a run did, as the screen draws it: the step, how it went and what happened,
/// said in the language of the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutcomeLine {
    /// The step (`trust`).
    pub step: &'static str,
    /// How it went.
    #[serde(rename = "result")]
    pub kind: OutcomeKind,
    /// What happened.
    pub message: String,
}

fn outcome_lines(outcomes: &[Outcome], language: Language) -> Vec<OutcomeLine> {
    outcomes
        .iter()
        .map(|o| OutcomeLine {
            step: o.step,
            kind: o.kind,
            message: o.message.render(language),
        })
        .collect()
}

/// What a setup run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupDone {
    /// One line per step that was tried or left, in order.
    pub outcomes: Vec<OutcomeLine>,
    /// Something failed (a person's "no" is not a failure).
    pub failed: bool,
}

/// Builds the `System` the setup works on from the wiring of Windows and hands it to `work`.
pub fn with_setup_system<R>(
    context: &Context,
    work: impl FnOnce(&onboarding::System<'_>) -> R,
) -> R {
    let store = (context.trust_store)();
    let system = onboarding::System {
        paths: &context.paths,
        dirs: context.dirs.as_ref(),
        autostart: context.autostart.as_ref(),
        user_path: context.user_path.as_ref(),
        trust: match &store {
            Ok(store) => Ok(store.as_ref()),
            Err(error) => Err(error.to_string()),
        },
        setup: context.setup.as_ref(),
        shim_exe: context.shim_exe.clone(),
        daemon_exe: context.daemon_exe.clone(),
        start_menu: context.start_menu.as_ref(),
        gui_exe: context.gui_exe.clone(),
        reserved_name: context.reserved_name.as_ref(),
    };
    work(&system)
}

/// What is left to set up. Reads and changes nothing.
#[must_use]
pub fn fetch_setup(context: &Context) -> SetupView {
    with_setup_system(context, |system| {
        setup_view(
            &onboarding::plan(system, SystemTime::now()),
            context.language,
        )
    })
}

/// Plans again and does the plan. `accepted` are the ids of the optional steps the person
/// agreed to; the others are left as they were and reported as such. The steps that are not
/// optional are done: pressing the button is the agreement to them. What is said comes in
/// `language`.
pub fn run_setup_on(
    system: &onboarding::System<'_>,
    accepted: &[String],
    now: SystemTime,
    language: Language,
) -> Result<SetupDone, RpcError> {
    let plan = onboarding::plan(system, now);
    let agreed = onboarding::run_if_agreed(
        system,
        &plan,
        &mut |_| true,
        &mut |step| accepted.iter().any(|id| id == step.id()),
        now,
    );
    match agreed {
        SetupAgreed::Blocked => Err(RpcError::new(
            ErrorCode::Conflict,
            plan.problems
                .first()
                .map_or_else(|| msg!("setup.cannot_run"), |p| p.message.clone())
                .render(language),
        )),
        SetupAgreed::NothingToDo | SetupAgreed::No => Ok(SetupDone {
            outcomes: Vec::new(),
            failed: false,
        }),
        SetupAgreed::Ran(outcomes) => Ok(SetupDone {
            failed: outcomes
                .iter()
                .any(|o| o.kind == teardown::OutcomeKind::Failed),
            outcomes: outcome_lines(&outcomes, language),
        }),
    }
}

/// Sets up what is left. See [`run_setup_on`].
pub fn run_setup(context: &Context, accepted: &[String]) -> Result<SetupDone, RpcError> {
    with_setup_system(context, |system| {
        run_setup_on(system, accepted, SystemTime::now(), context.language)
    })
}

// ---- uninstall ---------------------------------------------------------------------------

/// One step of the removal, as the screen draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UninstallStep {
    /// A short name (`ca`).
    pub step: &'static str,
    /// What will be done.
    pub description: String,
}

/// What the uninstall screen draws: the plan of `uninstall --all` said in the person's language,
/// and whether the home goes with it, which is what the backup question is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UninstallView {
    /// What will be undone, in order.
    pub steps: Vec<UninstallStep>,
    /// What the person has to do by hand, as instructions.
    pub by_hand: Vec<String>,
    /// What could not be looked at.
    pub problems: Vec<String>,
    /// What stays, and how to get rid of it.
    pub kept: Vec<String>,
    /// With `purge`: what is deleted.
    pub purge: Option<PurgeNotice>,
    /// The home is deleted with its data and projects: the page asks whether there is a backup
    /// before it offers the button.
    pub deletes_home: bool,
}

/// The plan of the removal, as the screen draws it.
#[must_use]
pub fn uninstall_view(plan: &teardown::Plan, language: Language) -> UninstallView {
    UninstallView {
        deletes_home: plan.deletes_home(),
        steps: plan
            .steps
            .iter()
            .map(|step| UninstallStep {
                step: step.id(),
                description: step.describe().render(language),
            })
            .collect(),
        by_hand: said(&plan.by_hand, language),
        problems: said(&plan.problems, language),
        kept: said(&plan.kept, language),
        purge: plan.purge.clone(),
    }
}

/// What an uninstall did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UninstallDone {
    /// One line per step, in order.
    pub outcomes: Vec<OutcomeLine>,
    /// Every step was done or had nothing left to do: Crisol is out of Windows.
    pub done: bool,
    /// The app should close: its programs are deleted a few seconds after it ends.
    pub close_app: bool,
}

/// Builds the `System` the removal works on and hands it to `work`.
pub fn with_teardown_system<R>(
    context: &Context,
    work: impl FnOnce(&teardown::System<'_>) -> R,
) -> R {
    let store = (context.trust_store)();
    let system = teardown::System {
        paths: &context.paths,
        autostart: context.autostart.as_ref(),
        user_path: context.user_path.as_ref(),
        trust: match &store {
            Ok(store) => Ok(store.as_ref()),
            Err(error) => Err(error.to_string()),
        },
        setup: context.setup.as_ref(),
        daemon_running: (context.connect)().is_ok(),
        programs: context.programs.clone(),
        remove_programs: context.remove_programs.as_ref(),
        winget: context.winget,
        start_menu: context.start_menu.as_ref(),
        gui_data: context.gui_data.clone(),
        app_window: context.app_window.as_ref(),
    };
    work(&system)
}

/// What `uninstall --all` would do, with or without `--purge`. Reads and changes nothing.
#[must_use]
pub fn fetch_uninstall(context: &Context, purge: bool) -> UninstallView {
    with_teardown_system(context, |system| {
        uninstall_view(&teardown::plan(system, purge), context.language)
    })
}

/// Plans again and does the plan. When it deletes the home, `backups_confirmed` must be true:
/// the person has said they have what they need elsewhere. `stop_daemon` stops the daemon and
/// waits until its files are free. What is said comes in `language`.
pub fn run_uninstall_on(
    system: &teardown::System<'_>,
    purge: bool,
    backups_confirmed: bool,
    stop_daemon: &mut dyn FnMut() -> Result<(), String>,
    language: Language,
) -> Result<UninstallDone, RpcError> {
    let plan = teardown::plan(system, purge);
    if plan.deletes_home() && !backups_confirmed {
        return Err(RpcError::new(
            ErrorCode::InvalidParams,
            msg!("teardown.backup_first").render(language),
        ));
    }
    match teardown::run_if_agreed(system, &plan, &mut |_| true, stop_daemon) {
        TeardownAgreed::NothingToDo | TeardownAgreed::No => Ok(UninstallDone {
            outcomes: Vec::new(),
            done: plan.problems.is_empty(),
            close_app: false,
        }),
        TeardownAgreed::Ran(outcomes) => {
            let done = teardown::all_done(&outcomes) && plan.problems.is_empty();
            Ok(UninstallDone {
                outcomes: outcome_lines(&outcomes, language),
                done,
                // The app is one of the programs Crisol leaves running, and its files are
                // deleted once it has ended; a failed run is left open to read what failed.
                close_app: done,
            })
        }
    }
}

/// Uninstalls. See [`run_uninstall_on`]. `notice` is told the progress of stopping the daemon.
pub fn run_uninstall(
    context: &Context,
    purge: bool,
    backups_confirmed: bool,
    notice: &mut dyn FnMut(&str),
) -> Result<UninstallDone, RpcError> {
    with_teardown_system(context, |system| {
        run_uninstall_on(
            system,
            purge,
            backups_confirmed,
            &mut || connect::stop_daemon(context, notice),
            context.language,
        )
    })
}

#[cfg(test)]
mod tests;
