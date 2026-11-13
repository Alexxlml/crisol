//! `crisol uninstall --all`: taking Crisol out of the system (F11.2, ADR-0022).
//!
//! What Crisol changes outside its home is undone here, after it is listed: the sign-in entry,
//! the shims folder in the user `PATH`, every Crisol CA in the trust store, and, with one
//! elevation prompt, the NRPT rule, the block of `hosts`, the antivirus exclusions inside the
//! home and the helper itself. A window of Crisol running in another process is asked to close
//! first, since its program and its profile cannot be deleted while it runs; then the daemon is
//! stopped, by whoever calls [`run`] (the command line, over IPC).
//!
//! Rules:
//!
//! - **First the list, then the changes.** [`plan`] only reads; [`run`] does what the plan says
//!   and nothing else. [`run_if_agreed`] puts the user in between: whoever asks (a terminal, a
//!   GUI) gets the whole [`Plan`], and nothing changes without a yes. The plan serializes to
//!   JSON, the same for `--json` and for a GUI.
//! - **A "no" is an answer.** Windows asks about each CA and about the elevation; a refusal is
//!   [`OutcomeKind::Declined`], and the rest goes on. Running it again finishes what was left.
//! - **A safe uninstall keeps what is the user's** (decision of the owner, ADR-0022): the data
//!   of the databases and their passwords, the settings and the projects ([`kept_entries`]).
//!   Everything else Crisol keeps in its home goes ([`Step::Clean`]), and so do `crisol.exe`
//!   and the programs next to it ([`Step::Programs`]). `--purge` deletes the whole home instead.
//!   Only the entries Crisol makes are ever deleted ([`own_entries`]); a home with anything
//!   else in it is not touched at all, so a `CRISOL_HOME` pointed at the wrong folder deletes
//!   nothing of the user's. Nothing is deleted when something before it was declined or failed,
//!   or when the plan could not look at something ([`Plan::problems`]): running it again must
//!   still be possible, with the same programs.
//! - **What needs an administrator and has no helper to do it** is told as commands for the
//!   user ([`Plan::by_hand`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::appwindow::AppWindow;
use crate::autostart::Autostart;
use crate::dbservice::DbKind;
use crate::dns::RULE_COMMENT;
use crate::hosts::{BEGIN, END};
use crate::i18n::{io_said, Message};
use crate::msg;
use crate::paths::CrisolPaths;
use crate::programs::WINGET_ID;
use crate::setup::{remove_exclusion_command, suffix_of, SystemSetup};
use crate::shimsetup::{path_remove, path_status};
use crate::startmenu::{StartMenu, SHORTCUT_NAME};
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
    /// Crisol's programs (`crisol.exe` and the ones next to it) that are to go.
    pub programs: Vec<PathBuf>,
    /// Deletes them once this program has ended (it is one of them).
    pub remove_programs: &'a dyn Fn(&[PathBuf]) -> io::Result<()>,
    /// The programs were installed by winget, which owns them: they are not deleted here
    /// (`programs` is empty then) and the last line says how winget removes them.
    pub winget: bool,
    /// The Start menu folder of the user.
    pub start_menu: &'a dyn StartMenu,
    /// The folder where the window keeps its web view profile
    /// (`%LOCALAPPDATA%\dev.crisol.gui`), if it is there. It is Crisol's even when winget
    /// owns the programs, and it goes with them once the window has ended.
    pub gui_data: Option<PathBuf>,
    /// The window of Crisol in another process: it holds its program and its profile while it
    /// runs, so it is asked to close first.
    pub app_window: &'a dyn AppWindow,
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
    /// Ask the window of Crisol, open in another process, to close.
    CloseWindow,
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
    /// Delete the Start menu entry that opens the window.
    StartMenu {
        /// The shortcut.
        path: PathBuf,
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
    /// Delete what Crisol keeps in its home except what is the user's ([`kept_entries`]).
    Clean {
        /// The home.
        home: PathBuf,
    },
    /// Delete what Crisol keeps in its home, and the home.
    Purge {
        /// The home.
        home: PathBuf,
    },
    /// Delete what the window keeps for its web view, once this command has ended.
    GuiData {
        /// The folder.
        folder: PathBuf,
    },
    /// Delete Crisol's programs, once this command has ended.
    Programs {
        /// The files.
        files: Vec<PathBuf>,
    },
}

/// Every [`Step::id`], for the screens that name each step (a test checks the list is whole).
pub const STEP_IDS: [&str; 11] = [
    "window",
    "daemon",
    "autostart",
    "path",
    "startmenu",
    "ca",
    "helper",
    "clean",
    "purge",
    "gui_data",
    "programs",
];

impl Step {
    /// A short name, for JSON.
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::CloseWindow => "window",
            Self::StopDaemon => "daemon",
            Self::Autostart { .. } => "autostart",
            Self::Path { .. } => "path",
            Self::StartMenu { .. } => "startmenu",
            Self::Ca { .. } => "ca",
            Self::Helper { .. } => "helper",
            Self::Clean { .. } => "clean",
            Self::Purge { .. } => "purge",
            Self::GuiData { .. } => "gui_data",
            Self::Programs { .. } => "programs",
        }
    }

    /// What will be done, in a sentence.
    #[must_use]
    pub fn describe(&self) -> Message {
        match self {
            Self::CloseWindow => msg!("teardown.step.close_window"),
            Self::StopDaemon => msg!("teardown.step.stop_daemon"),
            Self::Autostart { command } => msg!("teardown.step.autostart", command = command),
            Self::Path { folder } => msg!("teardown.step.path", folder = folder.display()),
            Self::StartMenu { path } => msg!("teardown.step.startmenu", path = path.display()),
            Self::GuiData { folder } => msg!("teardown.step.gui_data", folder = folder.display()),
            Self::Ca { name, .. } => msg!("teardown.step.ca", name = name),
            Self::Helper { suffix, home } => msg!(
                "teardown.step.helper",
                suffix = suffix,
                home = home.display()
            ),
            Self::Clean { home } => msg!("teardown.step.clean", home = home.display()),
            Self::Purge { home } => msg!("teardown.step.purge", home = home.display()),
            Self::Programs { files } => msg!("teardown.step.programs", files = path_list(files)),
        }
    }
}

/// `paths` one after the other, as a list of names for a sentence.
fn path_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// In JSON a step is `{"step": <id>, "description": <sentence>}`.
impl Serialize for Step {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut step = serializer.serialize_struct("Step", 2)?;
        step.serialize_field("step", self.id())?;
        step.serialize_field("description", &self.describe())?;
        step.end()
    }
}

/// What `--purge` would delete, said before the user agrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PurgeNotice {
    /// The home.
    #[serde(serialize_with = "path_text")]
    pub home: PathBuf,
    /// The services with data in `data/` (`mysql`, `postgresql`…).
    pub databases: Vec<String>,
    /// The entries in `www/`, where projects may be kept.
    pub projects: Vec<String>,
    /// Entries of the home that are not Crisol's. With any, the home is not deleted.
    #[serde(serialize_with = "paths_text")]
    pub foreign: Vec<PathBuf>,
}

/// A path as the user reads it (a path that is not valid Unicode would fail as a `PathBuf`).
fn path_text<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&path.display())
}

fn paths_text<S: Serializer>(paths: &[PathBuf], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(paths.iter().map(|p| p.display().to_string()))
}

/// Everything the removal will do, and what it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Plan {
    /// What will be undone, in order.
    pub steps: Vec<Step>,
    /// What the user has to do by hand, as instructions.
    pub by_hand: Vec<Message>,
    /// What could not be looked at.
    pub problems: Vec<Message>,
    /// What stays, and how to get rid of it.
    pub kept: Vec<Message>,
    /// With `--purge`: what it deletes.
    pub purge: Option<PurgeNotice>,
}

impl Plan {
    /// Whether there is nothing to undo.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Whether the home is deleted, data and projects included: the user should be asked
    /// whether they have a backup.
    #[must_use]
    pub fn deletes_home(&self) -> bool {
        self.steps.iter().any(|s| matches!(s, Step::Purge { .. }))
    }
}

/// How one step went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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

/// How one step went, in a sentence. In JSON: `{"step", "result", "message"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// [`Step::id`].
    pub step: &'static str,
    /// How it went.
    #[serde(rename = "result")]
    pub kind: OutcomeKind,
    /// What happened.
    pub message: Message,
}

/// Whether every step was done or had nothing to do.
#[must_use]
pub fn all_done(outcomes: &[Outcome]) -> bool {
    outcomes
        .iter()
        .all(|o| matches!(o.kind, OutcomeKind::Done | OutcomeKind::Nothing))
}

/// What a safe uninstall leaves in the home: the data of the databases and their passwords,
/// the settings (`crisol.toml`, `sites.toml`, and the user's overrides in `etc/<service>/`)
/// and the projects in `www/`.
#[must_use]
pub fn kept_entries(paths: &CrisolPaths) -> Vec<PathBuf> {
    vec![
        paths.config_file(),
        paths.sites_file(),
        paths.data_dir(),
        paths.secrets_dir(),
        paths.www_dir(),
    ]
}

/// The name of the user's own configuration inside `etc/<service>/`, kept too.
const OVERRIDES: &str = "overrides";

/// What a safe uninstall deletes: every entry of the home that Crisol makes and is not kept,
/// and the global programs of Node inside `data/`, which npm can install again (owner,
/// review of phase 11). `etc/` is listed but only partly deleted (its `overrides` stay).
fn cleanable_entries(paths: &CrisolPaths) -> Vec<PathBuf> {
    let kept = kept_entries(paths);
    let mut entries: Vec<PathBuf> = own_entries(paths)
        .into_iter()
        .filter(|e| !kept.contains(e))
        .collect();
    entries.push(paths.node_data());
    entries
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

/// Whether `dir` is an empty folder.
fn is_empty_dir(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none())
}

/// Whether a safe uninstall would delete anything: an entry it deletes, or something in `etc/`
/// that is not an `overrides` folder.
fn anything_to_clean(paths: &CrisolPaths) -> bool {
    let etc = paths.etc_dir();
    cleanable_entries(paths).iter().any(|entry| {
        if *entry != etc {
            return entry.exists();
        }
        fs::read_dir(entry).is_ok_and(|services| {
            services.flatten().any(|service| {
                !service.path().is_dir()
                    || fs::read_dir(service.path())
                        .is_ok_and(|inner| inner.flatten().any(|e| e.file_name() != OVERRIDES))
                    || is_empty_dir(&service.path())
            })
        }) || is_empty_dir(entry)
    })
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
    // Only the services: `data/node` holds Node's global programs, not a database.
    let databases = names_in(&paths.data_dir())
        .into_iter()
        .filter(|name| DbKind::from_service(name).is_some())
        .collect();
    Ok(PurgeNotice {
        home: paths.home().to_path_buf(),
        databases,
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
    // Asking a window that is not there to close does nothing: when the processes cannot be
    // looked at, it is asked all the same.
    if system.app_window.running().unwrap_or(true) {
        plan.steps.push(Step::CloseWindow);
    }
    if system.daemon_running {
        plan.steps.push(Step::StopDaemon);
    }
    match system.autostart.current() {
        Ok(Some(command)) => plan.steps.push(Step::Autostart { command }),
        Ok(None) => {}
        Err(error) => plan
            .problems
            .push(msg!("teardown.problem.autostart_read", error = error)),
    }
    match path_status(system.user_path, paths, &|_| false) {
        Ok(status) if status.in_path => plan.steps.push(Step::Path {
            folder: paths.shims_dir(),
        }),
        Ok(_) => {}
        Err(error) => plan
            .problems
            .push(msg!("teardown.problem.path_read", error = error)),
    }
    start_menu_step(system, &mut plan);
    match &system.trust {
        Ok(store) => match crisol_cas(*store) {
            Ok(cas) => plan.steps.extend(cas.into_iter().map(|ca| Step::Ca {
                name: ca.common_name,
                der: ca.der,
            })),
            Err(error) => plan.problems.push(msg!(
                "teardown.problem.trust_read",
                error = error,
                by_hand => msg!("teardown.by_hand_cas", tool = "certmgr.msc", name = "Crisol Local CA …")
            )),
        },
        Err(error) => plan.problems.push(msg!(
            "teardown.problem.trust_open",
            error = error,
            by_hand => msg!("teardown.by_hand_cas", tool = "certmgr.msc", name = "Crisol Local CA …")
        )),
    }
    let suffix = suffix_of(paths);
    match system.setup.installed() {
        Ok(true) => plan.steps.push(Step::Helper {
            suffix,
            home: home.clone(),
        }),
        Ok(false) => without_helper(system, &suffix, &home, &mut plan),
        Err(error) => plan.problems.push(msg!(
            "teardown.problem.helper_unknown",
            error => io_said(&error),
            command = "crisol system uninstall"
        )),
    }
    home_steps(system, purge, &mut plan);
    if let Some(folder) = system.gui_data.as_ref().filter(|f| f.is_dir()) {
        plan.steps.push(Step::GuiData {
            folder: folder.clone(),
        });
    }
    programs_notice(system, &mut plan);
    plan
}

/// What happens to the home: nothing deleted when it holds what is not Crisol's, everything with
/// `purge`, else only what Crisol keeps there.
fn home_steps(system: &System<'_>, purge: bool, plan: &mut Plan) {
    let paths = system.paths;
    let home = paths.home().to_path_buf();
    match purge_notice(paths) {
        Ok(notice) if !notice.foreign.is_empty() => {
            plan.problems.push(msg!(
                "teardown.problem.foreign",
                home = home.display(),
                list = path_list(&notice.foreign)
            ));
            if purge {
                plan.purge = Some(notice);
            }
        }
        Ok(notice) if purge => {
            plan.steps.push(Step::Purge { home: home.clone() });
            plan.purge = Some(notice);
        }
        Ok(notice) => {
            if anything_to_clean(paths) {
                plan.steps.push(Step::Clean { home: home.clone() });
            }
            plan.kept.push(if notice.databases.is_empty() {
                msg!(
                    "teardown.kept.home",
                    home = home.display(),
                    command = "crisol uninstall --all --purge"
                )
            } else {
                msg!(
                    "teardown.kept.home_databases",
                    home = home.display(),
                    databases = notice.databases.join(", "),
                    command = "crisol uninstall --all --purge"
                )
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => plan.problems.push(msg!(
            "teardown.problem.home_read",
            home = home.display(),
            error = error
        )),
    }
}

/// The Start menu entry that opens the window, if there is one. It is only a shortcut: one that
/// cannot be read is deleted all the same, and not knowing where the folder is does not hold
/// back the programs or the home (it is not one of the [`Plan::problems`]).
fn start_menu_step(system: &System<'_>, plan: &mut Plan) {
    let path = match system.start_menu.path() {
        Ok(path) => path,
        Err(error) => {
            plan.by_hand.push(msg!(
                "teardown.by_hand.startmenu",
                error = error,
                name = SHORTCUT_NAME
            ));
            return;
        }
    };
    match system.start_menu.target() {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => plan.steps.push(Step::StartMenu { path }),
    }
}

/// What happens to Crisol's programs: deleted, left to winget, or left to the user.
fn programs_notice(system: &System<'_>, plan: &mut Plan) {
    if system.winget {
        plan.kept.push(msg!(
            "teardown.kept.winget",
            command = format!("winget uninstall {WINGET_ID}")
        ));
    } else if system.programs.is_empty() {
        plan.kept.push(msg!("teardown.kept.programs"));
    } else {
        plan.steps.push(Step::Programs {
            files: system.programs.clone(),
        });
    }
}

/// With no helper there is no prompt to ask for: what needs an administrator is told as
/// commands for the user.
fn without_helper(system: &System<'_>, suffix: &str, home: &Path, plan: &mut Plan) {
    match system.setup.leftovers(suffix) {
        Ok(leftovers) => {
            if leftovers.nrpt_rule {
                plan.by_hand.push(msg!(
                    "teardown.by_hand.dns_rule",
                    suffix = suffix,
                    command = format!(
                        "Get-DnsClientNrptRule | Where-Object Comment -eq '{RULE_COMMENT}' | Remove-DnsClientNrptRule -Force"
                    )
                ));
            }
            if let Some(hosts) = leftovers.hosts_block {
                plan.by_hand.push(msg!(
                    "teardown.by_hand.hosts_block",
                    hosts = hosts.display(),
                    begin = BEGIN,
                    end = END
                ));
            }
        }
        Err(error) => plan
            .problems
            .push(msg!("teardown.problem.leftovers", error = error)),
    }
    plan.by_hand.push(msg!(
        "teardown.by_hand.exclusions",
        home = home.display(),
        command = remove_exclusion_command(home),
        list = "(Get-MpPreference).ExclusionPath"
    ));
}

/// Why the home or the programs were not deleted.
fn why_kept(outcomes: &[Outcome]) -> Message {
    if outcomes
        .iter()
        .any(|o| matches!(o.kind, OutcomeKind::Declined | OutcomeKind::Failed))
    {
        msg!("teardown.why.in_place")
    } else {
        msg!("teardown.why.unchecked")
    }
}

/// Does what `plan` says, in order. `stop_daemon` stops the daemon (the caller has the link to
/// it). Each step is tried even if one before failed, except deleting the home and the
/// programs: those wait until every step before them is done, and until nothing was left
/// unchecked in the plan ([`Plan::problems`]).
pub fn run(
    system: &System<'_>,
    plan: &Plan,
    stop_daemon: &mut dyn FnMut() -> Result<(), String>,
) -> Vec<Outcome> {
    // Something the plan could not look at may still be in place (a CA, the helper), and
    // taking it out may need the files and the programs: nothing is deleted then.
    let unchecked = !plan.problems.is_empty();
    let mut outcomes: Vec<Outcome> = Vec::new();
    for step in &plan.steps {
        let (kind, message) = match step {
            Step::CloseWindow => close_window(system),
            Step::StopDaemon => match stop_daemon() {
                Ok(()) => (OutcomeKind::Done, msg!("teardown.did.stop_daemon")),
                Err(error) => (
                    OutcomeKind::Failed,
                    msg!("teardown.failed.stop_daemon", error = error),
                ),
            },
            Step::Autostart { .. } => match system.autostart.remove() {
                Ok(true) => (OutcomeKind::Done, msg!("teardown.did.autostart")),
                Ok(false) => (OutcomeKind::Nothing, msg!("teardown.nothing.autostart")),
                Err(error) => (
                    OutcomeKind::Failed,
                    msg!("teardown.failed.autostart", error = error),
                ),
            },
            Step::Path { folder } => match path_remove(system.user_path, system.paths) {
                Ok(PathChange::Removed) => (
                    OutcomeKind::Done,
                    msg!("teardown.did.path", folder = folder.display()),
                ),
                Ok(_) => (
                    OutcomeKind::Nothing,
                    msg!("teardown.nothing.path", folder = folder.display()),
                ),
                Err(error) => (
                    OutcomeKind::Failed,
                    msg!("setup.failed.path", error = error),
                ),
            },
            Step::StartMenu { .. } => remove_start_menu(system),
            Step::Ca { name, der } => remove_ca(system, name, der),
            Step::Helper { suffix, home } => match system.setup.uninstall(suffix, home) {
                Ok(report) => (OutcomeKind::Done, report.message()),
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => (
                    OutcomeKind::Declined,
                    msg!("teardown.declined.prompt", error = error),
                ),
                Err(error) => (
                    OutcomeKind::Failed,
                    msg!("teardown.failed.helper", error => io_said(&error)),
                ),
            },
            // Deleting comes last, and only when everything above is gone: a CA still trusted
            // or a helper still installed needs the files and the programs to be taken out.
            Step::Clean { home } | Step::Purge { home } if unchecked || !all_done(&outcomes) => (
                OutcomeKind::Skipped,
                msg!(
                    "teardown.skipped.home",
                    home = home.display(),
                    why => why_kept(&outcomes)
                ),
            ),
            Step::Clean { .. } => clean(system.paths),
            Step::Purge { .. } => purge(system.paths),
            Step::GuiData { .. } if unchecked || !all_done(&outcomes) => (
                OutcomeKind::Skipped,
                msg!("teardown.skipped.gui_data", why => why_kept(&outcomes)),
            ),
            Step::GuiData { folder } => delete_gui_data(system, folder),
            Step::Programs { .. } if unchecked || !all_done(&outcomes) => (
                OutcomeKind::Skipped,
                msg!("teardown.skipped.programs", why => why_kept(&outcomes)),
            ),
            Step::Programs { files } => delete_programs(system, files),
        };
        outcomes.push(Outcome {
            step: step.id(),
            kind,
            message,
        });
    }
    outcomes
}

/// How an uninstall that needed the user's agreement ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Agreed {
    /// The plan was empty: nobody was asked and nothing was done.
    NothingToDo,
    /// The user did not agree: nothing changed.
    No,
    /// The user agreed and the steps ran.
    Ran(Vec<Outcome>),
}

/// Gives `agree` the whole `plan` (the steps, what is done by hand, what could not be looked
/// at, what stays and what `--purge` deletes) and, only if it says yes, runs it. A terminal
/// prints the plan and waits for `yes`; a GUI shows it and waits for a button. An empty plan
/// asks nothing.
pub fn run_if_agreed(
    system: &System<'_>,
    plan: &Plan,
    agree: &mut dyn FnMut(&Plan) -> bool,
    stop_daemon: &mut dyn FnMut() -> Result<(), String>,
) -> Agreed {
    if plan.is_empty() {
        Agreed::NothingToDo
    } else if agree(plan) {
        Agreed::Ran(run(system, plan, stop_daemon))
    } else {
        Agreed::No
    }
}

fn close_window(system: &System<'_>) -> (OutcomeKind, Message) {
    match system.app_window.close() {
        Ok(true) => (OutcomeKind::Done, msg!("teardown.did.close_window")),
        Ok(false) => (OutcomeKind::Nothing, msg!("teardown.nothing.close_window")),
        Err(error) => (
            OutcomeKind::Failed,
            msg!("teardown.failed.close_window", error = error),
        ),
    }
}

fn remove_start_menu(system: &System<'_>) -> (OutcomeKind, Message) {
    match system.start_menu.remove() {
        Ok(true) => (OutcomeKind::Done, msg!("teardown.did.startmenu")),
        Ok(false) => (OutcomeKind::Nothing, msg!("teardown.nothing.startmenu")),
        Err(error) => (
            OutcomeKind::Failed,
            msg!("teardown.failed.startmenu", error = error),
        ),
    }
}

fn delete_gui_data(system: &System<'_>, folder: &Path) -> (OutcomeKind, Message) {
    // The window is probably the one running this: its web view holds the folder until it ends.
    match (system.remove_programs)(&[folder.to_path_buf()]) {
        Ok(()) => (
            OutcomeKind::Done,
            msg!("teardown.did.gui_data", folder = folder.display()),
        ),
        Err(error) => (
            OutcomeKind::Failed,
            msg!(
                "teardown.failed.gui_data",
                folder = folder.display(),
                error = error
            ),
        ),
    }
}

fn delete_programs(system: &System<'_>, files: &[PathBuf]) -> (OutcomeKind, Message) {
    match (system.remove_programs)(files) {
        // Scheduled, not seen: they go once this command has ended.
        Ok(()) => (
            OutcomeKind::Done,
            msg!("teardown.did.programs", files = path_list(files)),
        ),
        Err(error) => (
            OutcomeKind::Failed,
            msg!("teardown.failed.programs", error = error),
        ),
    }
}

fn remove_ca(system: &System<'_>, name: &str, der: &[u8]) -> (OutcomeKind, Message) {
    let store = match &system.trust {
        Ok(store) => *store,
        Err(error) => return (OutcomeKind::Failed, msg!("io.error", error = error)),
    };
    match store.remove(der) {
        Ok(RemoveOutcome::Removed) => (OutcomeKind::Done, msg!("teardown.did.ca", name = name)),
        Ok(RemoveOutcome::Absent) => (
            OutcomeKind::Nothing,
            msg!("teardown.nothing.ca", name = name),
        ),
        Ok(RemoveOutcome::Declined) => (
            OutcomeKind::Declined,
            msg!("teardown.declined.ca", name = name),
        ),
        Err(error) => (
            OutcomeKind::Failed,
            msg!("teardown.failed.ca", name = name, error = error),
        ),
    }
}

/// Deletes a file or a folder, trying again a few times while it is held open (a service that
/// has just stopped).
fn remove_entry(entry: &Path) -> io::Result<()> {
    let mut result = Ok(());
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(400));
        }
        result = if entry.is_dir() {
            fs::remove_dir_all(entry)
        } else if entry.exists() {
            fs::remove_file(entry)
        } else {
            Ok(())
        };
        if result.is_ok() {
            break;
        }
    }
    result
}

/// Deletes what is in `etc/` except the `overrides` of each service, and the folders left
/// empty.
fn clean_etc(etc: &Path) -> io::Result<()> {
    let Ok(services) = fs::read_dir(etc) else {
        return Ok(());
    };
    for service in services.flatten() {
        let path = service.path();
        if !path.is_dir() {
            remove_entry(&path)?;
            continue;
        }
        for inner in fs::read_dir(&path)?.flatten() {
            if inner.file_name() != OVERRIDES {
                remove_entry(&inner.path())?;
            }
        }
        if is_empty_dir(&path) {
            fs::remove_dir(&path)?;
        }
    }
    if is_empty_dir(etc) {
        fs::remove_dir(etc)?;
    }
    Ok(())
}

/// The safe uninstall of the home: deletes what Crisol keeps there except what is the user's.
fn clean(paths: &CrisolPaths) -> (OutcomeKind, Message) {
    let etc = paths.etc_dir();
    let left: Vec<String> = cleanable_entries(paths)
        .iter()
        .filter_map(|entry| {
            let result = if *entry == etc {
                clean_etc(entry)
            } else {
                remove_entry(entry)
            };
            result
                .err()
                .map(|error| format!("{} ({error})", entry.display()))
        })
        .collect();
    if left.is_empty() {
        (
            OutcomeKind::Done,
            msg!("teardown.did.clean", home = paths.home().display()),
        )
    } else {
        (
            OutcomeKind::Failed,
            msg!("teardown.failed.delete", list = left.join("; ")),
        )
    }
}

/// Deletes what Crisol keeps in the home, then the home if it is empty.
fn purge(paths: &CrisolPaths) -> (OutcomeKind, Message) {
    let mut left = Vec::new();
    for entry in own_entries(paths) {
        if let Err(error) = remove_entry(&entry) {
            left.push(format!("{} ({error})", entry.display()));
        }
    }
    let home = paths.home();
    if left.is_empty() {
        match fs::remove_dir(home) {
            Ok(()) => (
                OutcomeKind::Done,
                msg!("teardown.did.purge", home = home.display()),
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (
                OutcomeKind::Done,
                msg!("teardown.did.purge", home = home.display()),
            ),
            Err(error) => (
                OutcomeKind::Failed,
                msg!(
                    "teardown.failed.purge_folder",
                    home = home.display(),
                    error = error
                ),
            ),
        }
    } else {
        (
            OutcomeKind::Failed,
            msg!("teardown.failed.delete", list = left.join("; ")),
        )
    }
}

#[cfg(test)]
mod tests;
