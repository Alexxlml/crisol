//! `crisol setup`: what the user does once after installing Crisol (P.3, ADR-0024).
//!
//! The package manager copies the programs and nothing else. The rest changes the user's
//! Windows, so it is only done because they asked, and the same pattern as
//! [`crate::teardown`] keeps it honest: a **plan** that only reads, an **agreement**, and a
//! **run** that says how every step went. The terminal and a future GUI call the same three.
//!
//! The steps are the shims and their place in the user `PATH`, which `setup` asks for once,
//! and three that change more than the user's own files and are asked one by one: trusting
//! the local CA (Windows asks too), the administrator helper (one prompt) and starting Crisol
//! when the user signs in.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::autostart::{self, Autostart, AutostartChange, AutostartError, AutostartState};
use crate::ca::load_ca;
use crate::i18n::{io_said, Localized, Message};
use crate::msg;
use crate::paths::{home_problems, CrisolPaths, HomeError, PrivateDirs};
use crate::programs::{require_trusted_program, ProgramError};
use crate::setup::{suffix_of, SystemSetup};
use crate::shimsetup::{install_shims, path_add, path_status, shims_current, shims_present};
use crate::startmenu::{same_path, StartMenu};
use crate::teardown::{Outcome, OutcomeKind};
use crate::trust::{prepare_trust, status, trust_ca, AddOutcome, TrustStore};
use crate::userpath::{PathChange, UserPath};

/// What the setup looks at and changes.
pub struct System<'a> {
    /// The Crisol home.
    pub paths: &'a CrisolPaths,
    /// Judges who can write to the programs, the home and the CA.
    pub dirs: &'a dyn PrivateDirs,
    /// The sign-in entry.
    pub autostart: &'a dyn Autostart,
    /// The user `PATH`.
    pub user_path: &'a dyn UserPath,
    /// The certificates the user trusts, or why they could not be opened.
    pub trust: Result<&'a dyn TrustStore, String>,
    /// The helper and what it changes.
    pub setup: &'a dyn SystemSetup,
    /// The `crisol-shim` the shims are copies of, if it is next to this program.
    pub shim_exe: Option<PathBuf>,
    /// The `crisold` the sign-in entry would start, if it is next to this program.
    pub daemon_exe: Option<PathBuf>,
    /// The Start menu folder of the user.
    pub start_menu: &'a dyn StartMenu,
    /// The `crisol-gui` the Start menu entry opens, if it is next to this program (a package
    /// without the window has none, and then there is nothing to add).
    pub gui_exe: Option<PathBuf>,
    /// Whether a program with that name is in the `PATH` already (a shim would hide it).
    pub reserved_name: &'a dyn Fn(&str) -> bool,
}

impl std::fmt::Debug for System<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("System")
            .field("paths", &self.paths)
            .field("shim_exe", &self.shim_exe)
            .field("daemon_exe", &self.daemon_exe)
            .finish_non_exhaustive()
    }
}

/// One thing to set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Copy `crisol-shim` once per command into `shims/`.
    Shims {
        /// The shims folder.
        folder: PathBuf,
        /// They are all there, but copies of another `crisol-shim` (the one before an update).
        refresh: bool,
    },
    /// Put the shims folder first in the user `PATH`.
    Path {
        /// The shims folder.
        folder: PathBuf,
    },
    /// Add the entry that opens the window to the user's Start menu.
    StartMenu {
        /// The shortcut.
        path: PathBuf,
        /// The window it opens.
        target: PathBuf,
        /// There is one already, but it opens another `crisol-gui` (the one before an update).
        refresh: bool,
    },
    /// Make the local CA if there is none and ask Windows to trust it.
    Trust,
    /// Install the helper and the DNS rule for the sites' suffix (one administrator prompt).
    Helper {
        /// The suffix (`test`).
        suffix: String,
    },
    /// Start `crisold` when the user signs in.
    Autostart {
        /// The daemon the entry starts.
        daemon: PathBuf,
    },
}

/// Every [`Step::id`], for the screens that name each step (a test checks the list is whole).
pub const STEP_IDS: [&str; 6] = ["shims", "path", "startmenu", "trust", "helper", "autostart"];

impl Step {
    /// A short name, for JSON. The same as [`Outcome::step`].
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::Shims { .. } => "shims",
            Self::Path { .. } => "path",
            Self::StartMenu { .. } => "startmenu",
            Self::Trust => "trust",
            Self::Helper { .. } => "helper",
            Self::Autostart { .. } => "autostart",
        }
    }

    /// Whether the user is asked about this step alone, because it changes more than their own
    /// files (the certificates Windows trusts, the administrator rights, what runs at sign-in).
    #[must_use]
    pub fn optional(&self) -> bool {
        matches!(
            self,
            Self::Trust | Self::Helper { .. } | Self::Autostart { .. }
        )
    }

    /// What will be done, in a sentence.
    #[must_use]
    pub fn describe(&self) -> Message {
        match self {
            Self::Shims {
                folder,
                refresh: true,
            } => msg!("setup.step.shims_update", folder = folder.display()),
            Self::Shims { folder, .. } => msg!("setup.step.shims", folder = folder.display()),
            Self::Path { folder } => msg!("setup.step.path", folder = folder.display()),
            Self::StartMenu {
                path,
                target,
                refresh: true,
            } => msg!(
                "setup.step.startmenu_update",
                path = path.display(),
                target = target.display()
            ),
            Self::StartMenu { path, target, .. } => msg!(
                "setup.step.startmenu",
                path = path.display(),
                target = target.display()
            ),
            Self::Trust => msg!("setup.step.trust", command = "crisol trust"),
            Self::Helper { suffix } => msg!("setup.step.helper", suffix = suffix),
            Self::Autostart { daemon } => msg!("setup.step.autostart", daemon = daemon.display()),
        }
    }

    /// The question for a step that is asked alone.
    #[must_use]
    pub fn question(&self) -> Message {
        match self {
            Self::Trust => msg!("setup.question.trust"),
            Self::Helper { .. } => msg!("setup.question.helper"),
            Self::Autostart { .. } => msg!("setup.question.autostart"),
            Self::Shims { .. } | Self::Path { .. } | Self::StartMenu { .. } => {
                msg!("setup.question.other")
            }
        }
    }
}

/// In JSON a step is `{"step": <id>, "description": <sentence>, "optional": <bool>}`.
impl Serialize for Step {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut step = serializer.serialize_struct("Step", 3)?;
        step.serialize_field("step", self.id())?;
        step.serialize_field("description", &self.describe())?;
        step.serialize_field("optional", &self.optional())?;
        step.end()
    }
}

/// Everything the setup will do, and what stops it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Plan {
    /// What is left to do, in order.
    pub steps: Vec<Step>,
    /// What is in place already.
    pub done: Vec<Message>,
    /// What stops the setup: nothing is changed while there is any.
    pub problems: Vec<Problem>,
    /// Things worth knowing that do not stop it.
    pub notes: Vec<Message>,
}

/// Why the setup cannot run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// What is wrong, in a sentence.
    pub message: Message,
    /// The exact command that fixes it, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Plan {
    /// Whether there is nothing left to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Whether something stops the setup.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        !self.problems.is_empty()
    }
}

/// Looks at what is in place and lists what [`run`] will do. Changes nothing.
#[must_use]
pub fn plan(system: &System<'_>, now: SystemTime) -> Plan {
    let paths = system.paths;
    let mut plan = Plan::default();

    // Before anything else: the programs are what the shims, the sign-in entry and the helper
    // will run with the user's rights, so a folder others can write to stops everything here
    // rather than at the last step.
    check_programs(system, &mut plan);
    // The home too: the shims go in it and the PATH leads to them. One that is not there yet
    // is made private by the first step.
    plan.problems
        .extend(home_problems(paths, system.dirs).iter().map(home_problem));
    if plan.is_blocked() {
        return plan;
    }

    let folder = paths.shims_dir();
    // `check_programs` has made sure there is a `shim_exe`.
    if system
        .shim_exe
        .as_deref()
        .is_some_and(|exe| shims_current(paths, exe))
    {
        plan.done
            .push(msg!("setup.done.shims", folder = folder.display()));
    } else {
        plan.steps.push(Step::Shims {
            folder: folder.clone(),
            refresh: shims_present(paths).iter().all(|(_, present)| *present),
        });
    }
    match path_status(system.user_path, paths, &|_| false) {
        Ok(status) if status.in_path => plan
            .done
            .push(msg!("setup.done.path", folder = folder.display())),
        Ok(_) => plan.steps.push(Step::Path { folder }),
        Err(error) => plan.problems.push(Problem {
            message: msg!("setup.problem.path_read", error = error),
            fix: None,
        }),
    }

    start_menu_step(system, &mut plan);

    trust_step(system, now, &mut plan);

    match system.setup.installed() {
        Ok(true) => plan.done.push(msg!("setup.done.helper")),
        Ok(false) => plan.steps.push(Step::Helper {
            suffix: suffix_of(paths),
        }),
        Err(error) => plan.notes.push(msg!(
            "setup.note.helper_unknown",
            error => io_said(&error),
            command = "crisol system install"
        )),
    }

    if let Some(daemon) = &system.daemon_exe {
        match autostart::state(system.autostart, daemon) {
            Ok(AutostartState::On) => plan.done.push(msg!("setup.done.autostart")),
            Ok(AutostartState::Off) => plan.steps.push(Step::Autostart {
                daemon: daemon.clone(),
            }),
            Ok(AutostartState::Other(other)) => {
                plan.notes
                    .push(msg!("setup.note.autostart_elsewhere", other = other));
                plan.steps.push(Step::Autostart {
                    daemon: daemon.clone(),
                });
            }
            Err(error) => plan.notes.push(msg!(
                "setup.note.autostart_read",
                error = error,
                command = "crisol autostart status"
            )),
        }
    }
    plan
}

fn start_menu_step(system: &System<'_>, plan: &mut Plan) {
    let Some(target) = system.gui_exe.as_ref().filter(|exe| exe.is_file()) else {
        return;
    };
    let path = match system.start_menu.path() {
        Ok(path) => path,
        Err(error) => {
            plan.notes.push(msg!(
                "setup.note.startmenu_path",
                error = error,
                target = target.display()
            ));
            return;
        }
    };
    match system.start_menu.target() {
        Ok(Some(current)) if same_path(&current, target) => {
            plan.done.push(msg!("setup.done.startmenu"));
        }
        Ok(current) => plan.steps.push(Step::StartMenu {
            path,
            target: target.clone(),
            refresh: current.is_some(),
        }),
        Err(error) => plan.notes.push(msg!(
            "setup.note.startmenu_read",
            error = error,
            command = "crisol setup"
        )),
    }
}

fn check_programs(system: &System<'_>, plan: &mut Plan) {
    let mut seen: Vec<ProgramError> = Vec::new();
    for (name, program) in [
        ("crisol-shim.exe", &system.shim_exe),
        ("crisold.exe", &system.daemon_exe),
    ] {
        let Some(program) = program else {
            plan.problems.push(Problem {
                message: msg!("setup.problem.program_missing", name = name),
                fix: None,
            });
            continue;
        };
        if let Err(error) = require_trusted_program(program, system.dirs) {
            // One folder holds all the programs: say it once.
            let same = seen.iter().any(|before| match (before, &error) {
                (ProgramError::Exposed { path: a, .. }, ProgramError::Exposed { path: b, .. }) => {
                    a == b
                }
                _ => false,
            });
            if !same {
                plan.problems.push(problem_of(&error));
            }
            seen.push(error);
        }
    }
}

fn home_problem(error: &HomeError) -> Problem {
    match error {
        HomeError::Exposed { path, fix } => Problem {
            message: msg!("setup.problem.home_exposed", path = path.display()),
            fix: Some(fix.clone()),
        },
        other => Problem {
            message: other.message(),
            fix: None,
        },
    }
}

fn problem_of(error: &ProgramError) -> Problem {
    match error {
        ProgramError::Exposed { path, fix, .. } => Problem {
            message: msg!("setup.problem.programs_exposed", path = path.display()),
            fix: Some(fix.clone()),
        },
        other => Problem {
            message: other.message(),
            fix: None,
        },
    }
}

fn trust_step(system: &System<'_>, now: SystemTime, plan: &mut Plan) {
    let store = match &system.trust {
        Ok(store) => *store,
        Err(error) => {
            plan.notes.push(msg!(
                "setup.note.trust_open",
                error = error,
                command = "crisol trust"
            ));
            return;
        }
    };
    match load_ca(system.paths, system.dirs, now) {
        Ok(None) => plan.steps.push(Step::Trust),
        Ok(Some(ca)) => match status(store, &ca) {
            Ok(state) if state.trusted => plan.done.push(msg!(
                "setup.done.trust",
                name = ca.info().common_name.as_str()
            )),
            Ok(_) => plan.steps.push(Step::Trust),
            Err(error) => plan.notes.push(msg!(
                "setup.note.trust_read",
                error = error,
                command = "crisol trust"
            )),
        },
        Err(error) => plan.problems.push(Problem {
            message: msg!("setup.problem.ca", error => error.message()),
            fix: Some("crisol doctor".to_owned()),
        }),
    }
}

/// Does what `plan` says, in order. `agree_step` is asked about each step that
/// [`Step::optional`]; a no is a result ([`OutcomeKind::Declined`]), not an error. Every step
/// is tried even if one before failed, since none depends on another. Nothing runs while the
/// plan has problems.
pub fn run(
    system: &System<'_>,
    plan: &Plan,
    agree_step: &mut dyn FnMut(&Step) -> bool,
    now: SystemTime,
) -> Vec<Outcome> {
    if plan.is_blocked() {
        return Vec::new();
    }
    let mut outcomes = Vec::new();
    for step in &plan.steps {
        let (kind, message) = if step.optional() && !agree_step(step) {
            (OutcomeKind::Declined, declined_message(step))
        } else {
            do_step(system, step, now)
        };
        outcomes.push(Outcome {
            step: step.id(),
            kind,
            message,
        });
    }
    outcomes
}

fn declined_message(step: &Step) -> Message {
    match step {
        Step::Trust => msg!("setup.declined.later", command = "crisol trust"),
        Step::Helper { .. } => msg!("setup.declined.later", command = "crisol system install"),
        Step::Autostart { .. } => msg!("setup.declined.later", command = "crisol autostart on"),
        Step::Shims { .. } | Step::Path { .. } | Step::StartMenu { .. } => {
            msg!("setup.declined.plain")
        }
    }
}

fn do_step(system: &System<'_>, step: &Step, now: SystemTime) -> (OutcomeKind, Message) {
    match step {
        Step::Shims { folder, .. } => shims(system, folder),
        Step::Path { folder } => match path_add(system.user_path, system.paths) {
            Ok(PathChange::Added) => (
                OutcomeKind::Done,
                msg!("setup.did.path", folder = folder.display()),
            ),
            Ok(_) => (
                OutcomeKind::Nothing,
                msg!("setup.nothing.path", folder = folder.display()),
            ),
            Err(error) => (
                OutcomeKind::Failed,
                msg!("setup.failed.path", error = error),
            ),
        },
        Step::StartMenu { path, target, .. } => match system.start_menu.create(target) {
            Ok(()) => (
                OutcomeKind::Done,
                msg!("setup.did.startmenu", path = path.display()),
            ),
            Err(error) => (
                OutcomeKind::Failed,
                msg!("setup.failed.startmenu", error = error),
            ),
        },
        Step::Trust => trust(system, now),
        Step::Helper { suffix } => match system.setup.install(suffix) {
            Ok(report) => (OutcomeKind::Done, report.message()),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => (
                OutcomeKind::Declined,
                msg!(
                    "setup.declined.prompt",
                    error = error,
                    command = "crisol system install"
                ),
            ),
            Err(error) => (
                OutcomeKind::Failed,
                msg!("setup.failed.helper", error => io_said(&error)),
            ),
        },
        Step::Autostart { daemon } => {
            match autostart::enable(system.autostart, daemon, system.dirs) {
                Ok(AutostartChange::Enabled) => (OutcomeKind::Done, msg!("setup.did.autostart")),
                Ok(AutostartChange::Replaced) => {
                    (OutcomeKind::Done, msg!("setup.did.autostart_replaced"))
                }
                Ok(AutostartChange::Unchanged) => {
                    (OutcomeKind::Nothing, msg!("setup.nothing.autostart"))
                }
                Err(AutostartError::Program(error)) => (OutcomeKind::Failed, error.message()),
                Err(AutostartError::Io(error)) => (
                    OutcomeKind::Failed,
                    msg!("setup.failed.autostart", error = error),
                ),
            }
        }
    }
}

fn shims(system: &System<'_>, folder: &Path) -> (OutcomeKind, Message) {
    let Some(exe) = &system.shim_exe else {
        return (
            OutcomeKind::Failed,
            msg!("setup.problem.program_missing", name = "crisol-shim.exe"),
        );
    };
    match install_shims(system.paths, exe, system.dirs, system.reserved_name) {
        Ok(done) if !done.failed.is_empty() => (
            OutcomeKind::Failed,
            msg!(
                "setup.failed.shims",
                names = done
                    .failed
                    .iter()
                    .map(|(name, why)| format!("{name} ({why})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        Ok(done) if done.written.is_empty() => (
            OutcomeKind::Nothing,
            msg!("setup.nothing.shims", folder = folder.display()),
        ),
        Ok(done) => (
            OutcomeKind::Done,
            msg!(
                "setup.did.shims",
                count = done.written.len(),
                folder = folder.display()
            ),
        ),
        Err(error) => (OutcomeKind::Failed, error.message()),
    }
}

fn trust(system: &System<'_>, now: SystemTime) -> (OutcomeKind, Message) {
    let store = match &system.trust {
        Ok(store) => *store,
        Err(error) => return (OutcomeKind::Failed, msg!("io.error", error = error)),
    };
    let tld = suffix_of(system.paths);
    let prepared = match prepare_trust(store, system.paths, system.dirs, &tld, now) {
        Ok(prepared) => prepared,
        Err(error) => return (OutcomeKind::Failed, error.message()),
    };
    let name = prepared.ca.info().common_name.clone();
    if prepared.already {
        return (
            OutcomeKind::Nothing,
            msg!("setup.nothing.trust", name = name),
        );
    }
    match trust_ca(store, &prepared.ca, system.paths, system.dirs) {
        Ok(AddOutcome::Added) => (
            OutcomeKind::Done,
            match prepared.thumbprint {
                Some(sha1) => msg!("setup.did.trust_sha1", name = name, sha1 = sha1),
                None => msg!("setup.did.trust", name = name),
            },
        ),
        Ok(AddOutcome::AlreadyThere) => (
            OutcomeKind::Nothing,
            msg!("setup.nothing.trust", name = name),
        ),
        Ok(AddOutcome::Declined) => (
            OutcomeKind::Declined,
            msg!(
                "setup.declined.trust",
                name = name,
                command = "crisol trust"
            ),
        ),
        Err(error) => (OutcomeKind::Failed, error.message()),
    }
}

/// How a setup that needed the user's agreement ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Agreed {
    /// Something stops it ([`Plan::problems`]): nobody was asked and nothing was done.
    Blocked,
    /// Everything is in place already: nobody was asked and nothing was done.
    NothingToDo,
    /// The user did not agree: nothing changed.
    No,
    /// The user agreed and the steps ran.
    Ran(Vec<Outcome>),
}

/// Whether the person must open a new terminal for the commands to be found: only when the
/// shims folder was just put in the user `PATH`. A terminal opened before keeps its own copy.
#[must_use]
pub fn needs_new_terminal(outcomes: &[Outcome]) -> bool {
    outcomes
        .iter()
        .any(|o| o.step == "path" && o.kind == OutcomeKind::Done)
}

/// Gives `agree` the whole `plan` and, only if it says yes, runs it. `agree_step` is asked
/// about the steps that are asked alone, when their turn comes.
pub fn run_if_agreed(
    system: &System<'_>,
    plan: &Plan,
    agree: &mut dyn FnMut(&Plan) -> bool,
    agree_step: &mut dyn FnMut(&Step) -> bool,
    now: SystemTime,
) -> Agreed {
    if plan.is_blocked() {
        Agreed::Blocked
    } else if plan.is_empty() {
        Agreed::NothingToDo
    } else if agree(plan) {
        Agreed::Ran(run(system, plan, agree_step, now))
    } else {
        Agreed::No
    }
}

#[cfg(test)]
mod tests;
