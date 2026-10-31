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
use crate::paths::{CrisolPaths, PrivateDirs};
use crate::programs::{require_trusted_program, ProgramError};
use crate::setup::{suffix_of, SystemSetup};
use crate::shimsetup::{install_shims, path_add, path_status, shims_present};
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
    },
    /// Put the shims folder first in the user `PATH`.
    Path {
        /// The shims folder.
        folder: PathBuf,
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

impl Step {
    /// A short name, for JSON. The same as [`Outcome::step`].
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::Shims { .. } => "shims",
            Self::Path { .. } => "path",
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
    pub fn describe(&self) -> String {
        match self {
            Self::Shims { folder } => format!(
                "Put the commands php, node, npm, composer and the rest in {}, so each runs the version your project asks for",
                folder.display()
            ),
            Self::Path { folder } => format!(
                "Put {} first in your user PATH (only yours; open a new terminal afterwards)",
                folder.display()
            ),
            Self::Trust => "Make Crisol's local certificate authority, which vouches only for .test, .localhost and this computer, and ask Windows to trust it so browsers open your https sites without a warning (Windows asks you to confirm; `crisol trust` shows its fingerprints first)".to_owned(),
            Self::Helper { suffix } => format!(
                "With administrator rights (one prompt): install the helper in Program Files and make the DNS rule that sends .{suffix} to this computer"
            ),
            Self::Autostart { daemon } => format!(
                "Start Crisol when you sign in ({})",
                daemon.display()
            ),
        }
    }

    /// The question for a step that is asked alone.
    #[must_use]
    pub fn question(&self) -> String {
        match self {
            Self::Trust => "Trust Crisol's local certificate authority?",
            Self::Helper { .. } => {
                "Install the helper and the DNS rule (asks for administrator rights)?"
            }
            Self::Autostart { .. } => "Start Crisol when you sign in?",
            Self::Shims { .. } | Self::Path { .. } => "Do it?",
        }
        .to_owned()
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
    pub done: Vec<String>,
    /// What stops the setup: nothing is changed while there is any.
    pub problems: Vec<Problem>,
    /// Things worth knowing that do not stop it.
    pub notes: Vec<String>,
}

/// Why the setup cannot run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// What is wrong, in a sentence.
    pub message: String,
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
    if plan.is_blocked() {
        return plan;
    }

    let folder = paths.shims_dir();
    if shims_present(paths).iter().all(|(_, present)| *present) {
        plan.done.push(format!(
            "The commands are in {} (`crisol shims sync` refreshes them after an update)",
            folder.display()
        ));
    } else {
        plan.steps.push(Step::Shims {
            folder: folder.clone(),
        });
    }
    match path_status(system.user_path, paths, &|_| false) {
        Ok(status) if status.in_path => plan
            .done
            .push(format!("{} is in your user PATH", folder.display())),
        Ok(_) => plan.steps.push(Step::Path { folder }),
        Err(error) => plan.problems.push(Problem {
            message: format!("cannot read your user PATH: {error}"),
            fix: None,
        }),
    }

    trust_step(system, now, &mut plan);

    match system.setup.installed() {
        Ok(true) => plan
            .done
            .push("The helper and the DNS rule are installed".to_owned()),
        Ok(false) => plan.steps.push(Step::Helper {
            suffix: suffix_of(paths),
        }),
        Err(error) => plan.notes.push(format!(
            "cannot tell whether the helper is installed ({error}); `crisol system install` installs it"
        )),
    }

    if let Some(daemon) = &system.daemon_exe {
        match autostart::state(system.autostart, daemon) {
            Ok(AutostartState::On) => plan.done.push("Crisol starts when you sign in".to_owned()),
            Ok(AutostartState::Off) => plan.steps.push(Step::Autostart {
                daemon: daemon.clone(),
            }),
            Ok(AutostartState::Other(other)) => {
                plan.notes.push(format!(
                    "Crisol starts when you sign in, but with another crisold ({other}); this replaces it"
                ));
                plan.steps.push(Step::Autostart {
                    daemon: daemon.clone(),
                });
            }
            Err(error) => plan.notes.push(format!(
                "cannot read the sign-in entry ({error}); `crisol autostart status` says more"
            )),
        }
    }
    plan
}

fn check_programs(system: &System<'_>, plan: &mut Plan) {
    let mut seen: Vec<ProgramError> = Vec::new();
    for (name, program) in [
        ("crisol-shim.exe", &system.shim_exe),
        ("crisold.exe", &system.daemon_exe),
    ] {
        let Some(program) = program else {
            plan.problems.push(Problem {
                message: format!("{name} was not found next to this program"),
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

fn problem_of(error: &ProgramError) -> Problem {
    match error {
        ProgramError::Exposed { path, fix, .. } => Problem {
            message: format!(
                "`{}` can be modified by other accounts of this computer, and Crisol would run the programs in it with your rights (or an administrator's), so it will not set anything up",
                path.display()
            ),
            fix: Some(fix.clone()),
        },
        other => Problem {
            message: other.to_string(),
            fix: None,
        },
    }
}

fn trust_step(system: &System<'_>, now: SystemTime, plan: &mut Plan) {
    let store = match &system.trust {
        Ok(store) => *store,
        Err(error) => {
            plan.notes.push(format!(
                "cannot open the certificates you trust ({error}); `crisol trust` tries again"
            ));
            return;
        }
    };
    match load_ca(system.paths, system.dirs, now) {
        Ok(None) => plan.steps.push(Step::Trust),
        Ok(Some(ca)) => match status(store, &ca) {
            Ok(state) if state.trusted => plan
                .done
                .push(format!("Windows trusts {}", ca.info().common_name)),
            Ok(_) => plan.steps.push(Step::Trust),
            Err(error) => plan.notes.push(format!(
                "cannot read the certificates you trust ({error}); `crisol trust` tries again"
            )),
        },
        Err(error) => plan.problems.push(Problem {
            message: format!("the local CA cannot be used: {error}"),
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

fn declined_message(step: &Step) -> String {
    match step {
        Step::Trust => "Left as it was; `crisol trust` does it later".to_owned(),
        Step::Helper { .. } => "Left as it was; `crisol system install` does it later".to_owned(),
        Step::Autostart { .. } => "Left as it was; `crisol autostart on` does it later".to_owned(),
        Step::Shims { .. } | Step::Path { .. } => "Left as it was".to_owned(),
    }
}

fn do_step(system: &System<'_>, step: &Step, now: SystemTime) -> (OutcomeKind, String) {
    match step {
        Step::Shims { folder } => shims(system, folder),
        Step::Path { folder } => match path_add(system.user_path, system.paths) {
            Ok(PathChange::Added) => (
                OutcomeKind::Done,
                format!(
                    "{} is first in your user PATH; open a new terminal to use it",
                    folder.display()
                ),
            ),
            Ok(_) => (
                OutcomeKind::Nothing,
                format!("{} was in your user PATH already", folder.display()),
            ),
            Err(error) => (
                OutcomeKind::Failed,
                format!("could not change your user PATH: {error}"),
            ),
        },
        Step::Trust => trust(system, now),
        Step::Helper { suffix } => match system.setup.install(suffix) {
            Ok(report) => (OutcomeKind::Done, report.steps.join("; ")),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => (
                OutcomeKind::Declined,
                format!("The administrator prompt was declined ({error}); `crisol system install` tries again"),
            ),
            Err(error) => (
                OutcomeKind::Failed,
                format!("the helper could not be installed: {error}"),
            ),
        },
        Step::Autostart { daemon } => {
            match autostart::enable(system.autostart, daemon, system.dirs) {
                Ok(AutostartChange::Enabled) => (
                    OutcomeKind::Done,
                    "Crisol will start when you sign in".to_owned(),
                ),
                Ok(AutostartChange::Replaced) => (
                    OutcomeKind::Done,
                    "Crisol will start when you sign in (the entry of another crisold was replaced)"
                        .to_owned(),
                ),
                Ok(AutostartChange::Unchanged) => (
                    OutcomeKind::Nothing,
                    "Crisol already started when you sign in".to_owned(),
                ),
                Err(AutostartError::Program(error)) => (OutcomeKind::Failed, error.to_string()),
                Err(AutostartError::Io(error)) => (
                    OutcomeKind::Failed,
                    format!("could not make the sign-in entry: {error}"),
                ),
            }
        }
    }
}

fn shims(system: &System<'_>, folder: &Path) -> (OutcomeKind, String) {
    let Some(exe) = &system.shim_exe else {
        return (
            OutcomeKind::Failed,
            "crisol-shim.exe was not found next to this program".to_owned(),
        );
    };
    match install_shims(system.paths, exe, system.dirs, system.reserved_name) {
        Ok(done) if !done.failed.is_empty() => (
            OutcomeKind::Failed,
            format!(
                "could not write {}",
                done.failed
                    .iter()
                    .map(|(name, why)| format!("{name} ({why})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        Ok(done) if done.written.is_empty() => (
            OutcomeKind::Nothing,
            format!("The commands in {} were up to date", folder.display()),
        ),
        Ok(done) => (
            OutcomeKind::Done,
            format!(
                "{} command(s) written in {}",
                done.written.len(),
                folder.display()
            ),
        ),
        Err(error) => (OutcomeKind::Failed, error.to_string()),
    }
}

fn trust(system: &System<'_>, now: SystemTime) -> (OutcomeKind, String) {
    let store = match &system.trust {
        Ok(store) => *store,
        Err(error) => return (OutcomeKind::Failed, error.clone()),
    };
    let tld = suffix_of(system.paths);
    let prepared = match prepare_trust(store, system.paths, system.dirs, &tld, now) {
        Ok(prepared) => prepared,
        Err(error) => return (OutcomeKind::Failed, error.to_string()),
    };
    let name = prepared.ca.info().common_name.clone();
    if prepared.already {
        return (
            OutcomeKind::Nothing,
            format!("Windows trusted {name} already"),
        );
    }
    match trust_ca(store, &prepared.ca, system.paths, system.dirs) {
        Ok(AddOutcome::Added) => (
            OutcomeKind::Done,
            format!(
                "Windows trusts {name} now{}; a browser that was open may need a restart",
                prepared
                    .thumbprint
                    .map(|sha1| format!(" (SHA-1 {sha1})"))
                    .unwrap_or_default()
            ),
        ),
        Ok(AddOutcome::AlreadyThere) => (
            OutcomeKind::Nothing,
            format!("Windows trusted {name} already"),
        ),
        Ok(AddOutcome::Declined) => (
            OutcomeKind::Declined,
            format!("Windows was told no; {name} is not trusted. `crisol trust` tries again"),
        ),
        Err(error) => (OutcomeKind::Failed, error.to_string()),
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
