//! The terminal of Crisol, opened from a window (ADR-0026 §3.4, F13.4).
//!
//! It is what `crisol shell` does (ADR-0019), in a window of its own: the GUI has no console to
//! hand over. Which terminal, where it opens and what it carries is decided here; the GUI only
//! draws the list and sends the click.

use std::ffi::OsString;
use std::path::PathBuf;

use crisol_core::config::Config;
use crisol_core::i18n::{Language, Localized, Message};
use crisol_core::msg;
use crisol_core::paths::PrivateDirs;
use crisol_core::shell::{self, ShellError, Terminal};
use crisol_core::CrisolPaths;
use crisol_platform::Desktop;
use crisol_protocol::{Call, ErrorCode, Reply, RpcError};
use serde::Serialize;

use crate::connect;
use crate::context::Context;

/// One terminal of the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalOption {
    /// How `crisol.toml` writes it (`pwsh`, `powershell`, `cmd`, `bash`).
    pub name: &'static str,
    /// What the list shows. These are product names, so they are not translated.
    pub label: &'static str,
    /// Whether this computer has it.
    pub found: bool,
}

/// The terminals of this computer and the one the user chose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalOptions {
    /// Every terminal Crisol can open, the ones this computer has marked.
    pub options: Vec<TerminalOption>,
    /// The one in `[shell] terminal`, if the user chose one.
    pub chosen: Option<&'static str>,
    /// What `[shell] terminal` says when it is no terminal Crisol knows (a typo): the list shows
    /// it so that choosing "automatic" takes it out.
    pub unknown: Option<String>,
    /// The one a click on "open terminal" will open: the chosen one if it is there, else the
    /// first of the automatic order. `None` when this computer has none.
    pub effective: Option<&'static str>,
    /// Why `effective` is not the chosen one, as the core says it.
    pub warning: Option<String>,
}

/// What a terminal is called in a list.
/// An error answer said in `language`.
fn failure(language: Language, code: ErrorCode, message: &Message) -> RpcError {
    RpcError::new(code, message.render(language))
}

fn label_of(terminal: Terminal) -> &'static str {
    match terminal {
        Terminal::Pwsh => "PowerShell 7",
        Terminal::WindowsPowerShell => "Windows PowerShell 5.1",
        Terminal::Cmd => "Command Prompt (cmd)",
        Terminal::Bash => "Git Bash",
    }
}

/// The list of terminals for the settings: what this computer has, and what the user chose.
///
/// Windows Terminal is not a choice of its own: it hosts these shells, and a console window the
/// system opens goes to it when the user made it the default terminal of Windows.
#[must_use]
pub fn terminal_options(
    desktop: &dyn Desktop,
    config: &Config,
    language: Language,
) -> TerminalOptions {
    let find = |t: Terminal| desktop.find_terminal(t, None);
    let chosen = config.preferred_terminal();
    // A name Crisol does not know shows as "automatic", with the warning that says so.
    let preference = config.terminal_preference();
    let (effective, warning) = match shell::choose_terminal(None, preference, None, &find) {
        Ok((terminal, _, note)) => (
            Some(terminal.name()),
            note.filter(|_| preference != shell::Preference::Automatic)
                .map(|note| note.render(language)),
        ),
        Err(error) => (None, Some(error.message().render(language))),
    };
    TerminalOptions {
        options: Terminal::ALL
            .into_iter()
            .map(|terminal| TerminalOption {
                name: terminal.name(),
                label: label_of(terminal),
                found: find(terminal).is_some(),
            })
            .collect(),
        chosen: chosen.map(Terminal::name),
        unknown: match preference {
            shell::Preference::Unknown(name) => Some(name.to_owned()),
            shell::Preference::Automatic | shell::Preference::Terminal(_) => None,
        },
        effective,
        warning,
    }
}

/// [`terminal_options`] for the configuration of this context's home.
pub fn current_terminal_options(context: &Context) -> Result<TerminalOptions, RpcError> {
    let config = Config::load_from(&context.paths)
        .map_err(|e| failure(context.language, ErrorCode::InvalidParams, &e.message()))?
        .config;
    Ok(terminal_options(
        &*context.desktop,
        &config,
        context.language,
    ))
}

/// Saves the terminal the user chose (`None`: automatic) in `[shell] terminal` of
/// `crisol.toml`, keeping its comments and order. It is the only thing here that writes, and
/// only because a person picked it. `name` is what [`TerminalOption::name`] says.
pub fn choose_terminal(
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    name: Option<&str>,
    language: Language,
) -> Result<(), RpcError> {
    let terminal = match name {
        Some(name) => Some(Terminal::from_name(name).ok_or_else(|| {
            failure(
                language,
                ErrorCode::InvalidParams,
                &msg!("terminal.not_a_terminal", name = name),
            )
        })?),
        None => None,
    };
    shell::save_terminal_preference(paths, dirs, terminal)
        .map(drop)
        .map_err(|e| failure(language, ErrorCode::Io, &e.message()))
}

/// Where the terminal opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "where", content = "site", rename_all = "snake_case")]
pub enum Place {
    /// In the folder of a project: the site's name.
    Site(String),
    /// Where the projects are (the general button).
    Projects,
}

/// The folder of the projects: the first parked folder, else `www` in the home. The one that
/// does not exist is skipped, and the home is the last resort, so a terminal always opens.
#[must_use]
pub fn projects_folder(paths: &CrisolPaths, config: &Config) -> PathBuf {
    config
        .parked_dirs(paths)
        .into_iter()
        .chain([paths.home().join("www")])
        .find(|dir| dir.is_dir())
        .unwrap_or_else(|| paths.home().to_path_buf())
}

/// The folder a terminal opens in. `sites` is `(name, root)` of every site.
///
/// A site that is not there (it was unlinked meanwhile) is an error and not a silent fallback:
/// the click was about that project.
pub fn start_folder(
    place: &Place,
    sites: &[(String, PathBuf)],
    paths: &CrisolPaths,
    config: &Config,
    language: Language,
) -> Result<PathBuf, RpcError> {
    match place {
        Place::Projects => Ok(projects_folder(paths, config)),
        Place::Site(name) => {
            let root = sites
                .iter()
                .find(|(site, _)| site == name)
                .map(|(_, root)| root)
                .ok_or_else(|| {
                    failure(
                        language,
                        ErrorCode::NotFound,
                        &msg!("terminal.no_site", name = name, command = "crisol sites"),
                    )
                })?;
            if root.is_dir() {
                Ok(root.clone())
            } else {
                Err(failure(
                    language,
                    ErrorCode::NotFound,
                    &msg!("terminal.folder_gone", name = name, path = root.display()),
                ))
            }
        }
    }
}

/// The environment the terminal is built from: the one of this process, without the mark of a
/// `crisol shell` (this is a new window, not a terminal inside another).
#[must_use]
pub fn clean_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(name, _)| !name.to_string_lossy().eq_ignore_ascii_case(shell::INSIDE))
        .collect()
}

/// What opening a terminal did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opened {
    /// The terminal that opened.
    pub terminal: &'static str,
    /// The folder it opened in.
    pub folder: PathBuf,
    /// Why it is not the one the user chose, when that is so.
    pub note: Option<String>,
}

fn shell_error(error: &ShellError, language: Language) -> RpcError {
    let code = match error {
        ShellError::AlreadyInside => ErrorCode::Conflict,
        ShellError::NoShims | ShellError::NotFound(_) | ShellError::NoTerminal => {
            ErrorCode::NotAvailable
        }
        ShellError::Unusable(_) => ErrorCode::InvalidParams,
        ShellError::Io(_) => ErrorCode::Io,
    };
    let mut rpc = failure(language, code, &error.message());
    if matches!(error, ShellError::NoShims) {
        rpc.fix = Some("crisol shims install".to_owned());
    }
    rpc
}

/// The sites as `(name, root)`, from the daemon (or from the files when there is none).
fn site_roots(context: &Context) -> Result<Vec<(String, PathBuf)>, RpcError> {
    match connect::call(context, &Call::Sites, &mut |_| {})? {
        Reply::Sites { sites, .. } => Ok(sites
            .into_iter()
            .map(|site| (site.name, PathBuf::from(site.root)))
            .collect()),
        other => Err(failure(
            context.language,
            ErrorCode::Internal,
            &msg!(
                "client.unexpected_answer",
                call = "sites",
                answer = format!("{other:?}")
            ),
        )),
    }
}

/// Opens a terminal of Crisol in a new window: the shims in front, the variables of other
/// suites out, the clients of the databases at hand and the user's profile respected, with the
/// terminal the user chose (or the automatic one) and the folder of `place`.
///
/// `env` is the environment to build it from ([`clean_env`] of this process's).
pub fn open_terminal(
    context: &Context,
    env: &[(OsString, OsString)],
    place: &Place,
) -> Result<Opened, RpcError> {
    let sites = match place {
        Place::Site(_) => site_roots(context)?,
        Place::Projects => Vec::new(),
    };
    launch_terminal(
        &*context.desktop,
        &context.paths,
        env,
        place,
        &sites,
        context.language,
    )
}

/// [`open_terminal`] once the sites are known: `sites` is `(name, root)` of each.
pub fn launch_terminal(
    desktop: &dyn Desktop,
    paths: &CrisolPaths,
    env: &[(OsString, OsString)],
    place: &Place,
    sites: &[(String, PathBuf)],
    language: Language,
) -> Result<Opened, RpcError> {
    let config = Config::load_from(paths)
        .map_err(|e| failure(language, ErrorCode::InvalidParams, &e.message()))?
        .config;
    let folder = start_folder(place, sites, paths, &config, language)?;
    let find = |t: Terminal| desktop.find_terminal(t, None);
    let preference = config.terminal_preference();
    let (terminal, program, note) = shell::choose_terminal(None, preference, None, &find)
        .map_err(|e| shell_error(&e, language))?;
    let front = shell::front_folders(paths, &config);
    let launch = shell::plan_shell(&shell::ShellInput {
        paths,
        terminal,
        program: &program,
        clean: false,
        front: &front,
        env,
        cwd: Some(&folder),
    })
    .map_err(|e| shell_error(&e, language))?;
    desktop.spawn_terminal(&launch).map_err(|e| {
        failure(
            language,
            ErrorCode::Io,
            &msg!(
                "terminal.cannot_start",
                program = program.display(),
                error = e
            ),
        )
    })?;
    Ok(Opened {
        terminal: terminal.name(),
        folder,
        // With no choice the automatic one is no news; with one (or a name it does not know),
        // why it is not that one is.
        note: note
            .filter(|_| preference != shell::Preference::Automatic)
            .map(|note| note.render(language)),
    })
}

#[cfg(test)]
mod tests;
