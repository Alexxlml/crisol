//! `crisol uninstall --all`: taking Crisol out of the system (F11.2, ADR-0022).
//!
//! What Crisol changes outside its home is undone here, after it is listed: the sign-in entry,
//! the shims folder in the user `PATH`, every Crisol CA in the trust store, and, with one
//! elevation prompt, the NRPT rule, the block of `hosts`, the antivirus exclusions inside the
//! home and the helper itself. The daemon is stopped first, by whoever calls [`run`] (the
//! command line, over IPC).
//!
//! Rules:
//!
//! - **First the list, then the changes.** [`plan`] only reads; [`run`] does what the plan says
//!   and nothing else.
//! - **A "no" is an answer.** Windows asks about each CA and about the elevation; a refusal is
//!   [`OutcomeKind::Declined`], and the rest goes on. Running it again finishes what was left.
//! - **The home is kept** unless `--purge` is asked for. Then only the entries Crisol makes in
//!   it are deleted ([`own_entries`]); a home with anything else in it is not touched at all,
//!   so a `CRISOL_HOME` pointed at the wrong folder deletes nothing of the user's. Nor is it
//!   deleted when something before it was declined or failed.
//! - **What needs an administrator and has no helper to do it** is told as commands for the
//!   user ([`Plan::by_hand`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::autostart::Autostart;
use crate::dns::RULE_COMMENT;
use crate::hosts::{BEGIN, END};
use crate::paths::CrisolPaths;
use crate::setup::{remove_exclusion_command, suffix_of, SystemSetup};
use crate::shimsetup::{path_remove, path_status};
use crate::trust::{crisol_cas, RemoveOutcome, TrustStore};
use crate::userpath::{PathChange, UserPath};

/// What the removal looks at and changes.
pub struct System<'a> {
    /// The Crisol home.
    pub paths: &'a CrisolPaths,
    /// The sign-in entry.
    pub autostart: &'a dyn Autostart,
    /// The user `PATH`.
    pub user_path: &'a dyn UserPath,
    /// The certificates the user trusts, or why they could not be opened.
    pub trust: Result<&'a dyn TrustStore, String>,
    /// The helper and what it changed.
    pub setup: &'a dyn SystemSetup,
    /// Whether the daemon is running now.
    pub daemon_running: bool,
}

impl std::fmt::Debug for System<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("System")
            .field("paths", &self.paths)
            .field("daemon_running", &self.daemon_running)
            .finish_non_exhaustive()
    }
}

/// One thing to undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Stop the daemon and the services it runs.
    StopDaemon,
    /// Remove the sign-in entry, which runs `command`.
    Autostart {
        /// What the entry runs.
        command: String,
    },
    /// Take the shims folder out of the user `PATH`.
    Path {
        /// The shims folder.
        folder: PathBuf,
    },
    /// Take a Crisol CA out of the trust store.
    Ca {
        /// Its common name.
        name: String,
        /// The certificate (DER).
        der: Vec<u8>,
    },
    /// Ask the helper, with one elevation prompt, to undo what it did and remove itself.
    Helper {
        /// The suffix of the NRPT rule.
        suffix: String,
        /// The home, whose antivirus exclusions go too.
        home: PathBuf,
    },
    /// Delete what Crisol keeps in its home, and the home.
    Purge {
        /// The home.
        home: PathBuf,
    },
}

impl Step {
    /// A short name, for JSON.
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::StopDaemon => "daemon",
            Self::Autostart { .. } => "autostart",
            Self::Path { .. } => "path",
            Self::Ca { .. } => "ca",
            Self::Helper { .. } => "helper",
            Self::Purge { .. } => "purge",
        }
    }

    /// What will be done, in a sentence.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::StopDaemon => "Stop the daemon and the services it runs".to_owned(),
            Self::Autostart { command } => {
                format!("Remove the entry that starts Crisol when you sign in ({command})")
            }
            Self::Path { folder } => {
                format!("Take {} out of your user PATH", folder.display())
            }
            Self::Ca { name, .. } => format!(
                "Take {name} out of the certificates Windows trusts (Windows asks you to confirm)"
            ),
            Self::Helper { suffix, home } => format!(
                "With administrator rights (one prompt): remove the DNS rule for .{suffix}, Crisol's block of the hosts file, the Defender exclusions inside {} if there are any, and the helper in Program Files",
                home.display()
            ),
            Self::Purge { home } => format!("Delete {} and everything in it", home.display()),
        }
    }
}

/// What `--purge` would delete, said before the user agrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeNotice {
    /// The home.
    pub home: PathBuf,
    /// The services with data in `data/` (`mysql`, `postgresql`…).
    pub databases: Vec<String>,
    /// The entries in `www/`, where projects may be kept.
    pub projects: Vec<String>,
    /// Entries of the home that are not Crisol's. With any, the home is not deleted.
    pub foreign: Vec<PathBuf>,
}

/// Everything the removal will do, and what it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// What will be undone, in order.
    pub steps: Vec<Step>,
    /// What the user has to do by hand, as instructions.
    pub by_hand: Vec<String>,
    /// What could not be looked at.
    pub problems: Vec<String>,
    /// What stays, and how to get rid of it.
    pub kept: Vec<String>,
    /// With `--purge`: what it deletes.
    pub purge: Option<PurgeNotice>,
}

impl Plan {
    /// Whether there is nothing to undo.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// How one step went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// Done.
    Done,
    /// There was nothing to do any more.
    Nothing,
    /// The user said no.
    Declined,
    /// It failed.
    Failed,
    /// Not tried, because of what happened before.
    Skipped,
}

impl OutcomeKind {
    /// A short name, for JSON.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Nothing => "nothing",
            Self::Declined => "declined",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// How one step went, in a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// [`Step::id`].
    pub step: &'static str,
    /// How it went.
    pub kind: OutcomeKind,
    /// What happened.
    pub message: String,
}

/// Whether every step was done or had nothing to do.
#[must_use]
pub fn all_done(outcomes: &[Outcome]) -> bool {
    outcomes
        .iter()
        .all(|o| matches!(o.kind, OutcomeKind::Done | OutcomeKind::Nothing))
}

/// The entries Crisol makes in its home: what `--purge` deletes.
#[must_use]
pub fn own_entries(paths: &CrisolPaths) -> Vec<PathBuf> {
    vec![
        paths.config_file(),
        paths.sites_file(),
        paths.state_dir(),
        paths.bin_dir(),
        paths.etc_dir(),
        paths.data_dir(),
        paths.logs_dir(),
        paths.certs_dir(),
        paths.secrets_dir(),
        paths.cache_dir(),
        paths.shims_dir(),
        paths.www_dir(),
    ]
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn purge_notice(paths: &CrisolPaths) -> io::Result<PurgeNotice> {
    let own = own_entries(paths);
    let mut foreign: Vec<PathBuf> = fs::read_dir(paths.home())?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            !own.iter()
                .any(|o| o.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
        })
        .collect();
    foreign.sort();
    Ok(PurgeNotice {
        home: paths.home().to_path_buf(),
        databases: names_in(&paths.data_dir()),
        projects: names_in(&paths.www_dir()),
        foreign,
    })
}

/// Looks at what Crisol changed and lists what [`run`] will undo. Changes nothing.
#[must_use]
pub fn plan(system: &System<'_>, purge: bool) -> Plan {
    let paths = system.paths;
    let home = paths.home().to_path_buf();
    let mut plan = Plan::default();
    if system.daemon_running {
        plan.steps.push(Step::StopDaemon);
    }
    match system.autostart.current() {
        Ok(Some(command)) => plan.steps.push(Step::Autostart { command }),
        Ok(None) => {}
        Err(error) => plan
            .problems
            .push(format!("cannot read the sign-in entry: {error}")),
    }
    match path_status(system.user_path, paths, &|_| false) {
        Ok(status) if status.in_path => plan.steps.push(Step::Path {
            folder: paths.shims_dir(),
        }),
        Ok(_) => {}
        Err(error) => plan
            .problems
            .push(format!("cannot read your user PATH: {error}")),
    }
    let by_hand_cas = "open `certmgr.msc`, go to Trusted Root Certification Authorities > Certificates, and delete the ones named `Crisol Local CA …`";
    match &system.trust {
        Ok(store) => match crisol_cas(*store) {
            Ok(cas) => plan.steps.extend(cas.into_iter().map(|ca| Step::Ca {
                name: ca.common_name,
                der: ca.der,
            })),
            Err(error) => plan.problems.push(format!(
                "cannot read the certificates you trust ({error}); to do it by hand, {by_hand_cas}"
            )),
        },
        Err(error) => plan.problems.push(format!(
            "cannot open the certificates you trust ({error}); to do it by hand, {by_hand_cas}"
        )),
    }
    let suffix = suffix_of(paths);
    match system.setup.installed() {
        Ok(true) => plan.steps.push(Step::Helper {
            suffix,
            home: home.clone(),
        }),
        Ok(false) => without_helper(system, &suffix, &home, &mut plan),
        Err(error) => plan.problems.push(format!(
            "cannot tell whether the helper is installed ({error}); `crisol system uninstall` removes it"
        )),
    }
    if purge {
        match purge_notice(paths) {
            Ok(notice) => {
                if notice.foreign.is_empty() {
                    plan.steps.push(Step::Purge { home: home.clone() });
                } else {
                    plan.problems.push(format!(
                        "{} holds things that are not Crisol's, so it will not be deleted: {}. Move them out and run this again, or delete the folder yourself",
                        home.display(),
                        notice
                            .foreign
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                plan.purge = Some(notice);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => plan
                .problems
                .push(format!("cannot read {}: {error}", home.display())),
        }
    } else {
        plan.kept.push(format!(
            "{}: the settings, the installed versions, the certificates and the data of the databases. `crisol uninstall --all --purge` deletes it",
            home.display()
        ));
    }
    plan.kept.push(
        "crisol.exe and the programs next to it: delete them yourself when this is done".to_owned(),
    );
    plan
}

/// With no helper there is no prompt to ask for: what needs an administrator is told as
/// commands for the user.
fn without_helper(system: &System<'_>, suffix: &str, home: &Path, plan: &mut Plan) {
    match system.setup.leftovers(suffix) {
        Ok(leftovers) => {
            if leftovers.nrpt_rule {
                plan.by_hand.push(format!(
                    "The DNS rule for .{suffix} is still there. In PowerShell as an administrator: Get-DnsClientNrptRule | Where-Object Comment -eq '{RULE_COMMENT}' | Remove-DnsClientNrptRule -Force"
                ));
            }
            if let Some(hosts) = leftovers.hosts_block {
                plan.by_hand.push(format!(
                    "Crisol's block is still in the hosts file. Open {} as an administrator and delete the lines from `{BEGIN}` to `{END}`",
                    hosts.display()
                ));
            }
        }
        Err(error) => plan.problems.push(format!(
            "cannot tell whether the DNS rule or the hosts block of Crisol are still there: {error}"
        )),
    }
    plan.by_hand.push(format!(
        "Defender's exclusions cannot be seen without administrator rights, and the helper that could is not installed. If you excluded a folder inside {}, in PowerShell as an administrator: {} (`(Get-MpPreference).ExclusionPath` lists them)",
        home.display(),
        remove_exclusion_command(home)
    ));
}

/// Does what `plan` says, in order. `stop_daemon` stops the daemon (the caller has the link to
/// it). Each step is tried even if one before failed, except deleting the home.
pub fn run(
    system: &System<'_>,
    plan: &Plan,
    stop_daemon: &mut dyn FnMut() -> Result<(), String>,
) -> Vec<Outcome> {
    let mut outcomes: Vec<Outcome> = Vec::new();
    for step in &plan.steps {
        let (kind, message) = match step {
            Step::StopDaemon => match stop_daemon() {
                Ok(()) => (
                    OutcomeKind::Done,
                    "Stopped the daemon and its services".to_owned(),
                ),
                Err(error) => (
                    OutcomeKind::Failed,
                    format!("could not stop the daemon: {error}"),
                ),
            },
            Step::Autostart { .. } => match system.autostart.remove() {
                Ok(true) => (OutcomeKind::Done, "Removed the sign-in entry".to_owned()),
                Ok(false) => (
                    OutcomeKind::Nothing,
                    "There was no sign-in entry any more".to_owned(),
                ),
                Err(error) => (
                    OutcomeKind::Failed,
                    format!("could not remove the sign-in entry: {error}"),
                ),
            },
            Step::Path { folder } => match path_remove(system.user_path, system.paths) {
                Ok(PathChange::Removed) => (
                    OutcomeKind::Done,
                    format!(
                        "Took {} out of your user PATH; terminals that are open keep the old one until they are closed",
                        folder.display()
                    ),
                ),
                Ok(_) => (
                    OutcomeKind::Nothing,
                    format!("{} was not in your user PATH any more", folder.display()),
                ),
                Err(error) => (
                    OutcomeKind::Failed,
                    format!("could not change your user PATH: {error}"),
                ),
            },
            Step::Ca { name, der } => remove_ca(system, name, der),
            Step::Helper { suffix, home } => match system.setup.uninstall(suffix, home) {
                Ok(report) => (OutcomeKind::Done, report.steps.join("; ")),
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => (
                    OutcomeKind::Declined,
                    format!(
                        "The administrator prompt was declined ({error}): the DNS rule, the hosts block, the Defender exclusions and the helper are still there"
                    ),
                ),
                Err(error) => (
                    OutcomeKind::Failed,
                    format!("the helper could not undo its changes: {error}"),
                ),
            },
            Step::Purge { home } => {
                if all_done(&outcomes) {
                    purge(system.paths)
                } else {
                    (
                        OutcomeKind::Skipped,
                        format!(
                            "{} was not deleted, because something above is still in place; run this again when it is solved",
                            home.display()
                        ),
                    )
                }
            }
        };
        outcomes.push(Outcome {
            step: step.id(),
            kind,
            message,
        });
    }
    outcomes
}

fn remove_ca(system: &System<'_>, name: &str, der: &[u8]) -> (OutcomeKind, String) {
    let store = match &system.trust {
        Ok(store) => *store,
        Err(error) => return (OutcomeKind::Failed, error.clone()),
    };
    match store.remove(der) {
        Ok(RemoveOutcome::Removed) => (
            OutcomeKind::Done,
            format!("Windows no longer trusts {name}"),
        ),
        Ok(RemoveOutcome::Absent) => (
            OutcomeKind::Nothing,
            format!("{name} was not trusted any more"),
        ),
        Ok(RemoveOutcome::Declined) => (
            OutcomeKind::Declined,
            format!("Windows was told no; {name} is still trusted"),
        ),
        Err(error) => (
            OutcomeKind::Failed,
            format!("could not take {name} out: {error}"),
        ),
    }
}

/// Deletes what Crisol keeps in the home, then the home if it is empty. A file still held open
/// (a service that has just stopped) is tried again a few times.
fn purge(paths: &CrisolPaths) -> (OutcomeKind, String) {
    let mut left = Vec::new();
    for entry in own_entries(paths) {
        let mut result = Ok(());
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(400));
            }
            result = if entry.is_dir() {
                fs::remove_dir_all(&entry)
            } else if entry.exists() {
                fs::remove_file(&entry)
            } else {
                Ok(())
            };
            if result.is_ok() {
                break;
            }
        }
        if let Err(error) = result {
            left.push(format!("{} ({error})", entry.display()));
        }
    }
    let home = paths.home();
    if left.is_empty() {
        match fs::remove_dir(home) {
            Ok(()) => (OutcomeKind::Done, format!("Deleted {}", home.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                (OutcomeKind::Done, format!("Deleted {}", home.display()))
            }
            Err(error) => (
                OutcomeKind::Failed,
                format!(
                    "emptied {}, but the folder itself could not be deleted: {error}",
                    home.display()
                ),
            ),
        }
    } else {
        (
            OutcomeKind::Failed,
            format!(
                "could not delete: {}. Close what is using them and delete them yourself",
                left.join("; ")
            ),
        )
    }
}

#[cfg(test)]
mod tests;
