//! `crisol use`: choosing the version of PHP or Node (F4.5).
//!
//! - [`use_global`] sets the default for everywhere: `[defaults]` in `crisol.toml`.
//! - [`use_local`] pins a project: it writes `.crisol.toml` in the project folder.
//!
//! Both are explicit commands, so writing the user's files is what they asked for
//! (ADR-0003); both go through [`TomlEditor`], which changes only that one value and keeps the
//! comments, the order and the line endings of the rest. What is written is the request as given
//! (`8.3`, not `8.3.35`): the pin keeps following the newest installed patch of that line.
//!
//! The version has to be **installed**, so that a pin never leaves the shims with nothing to
//! run. A project can still carry a pin for something that is not installed yet (somebody else
//! wrote the file); that is the shim's error to report, with what is installed.

use std::path::{Path, PathBuf};

use crate::edit::{EditError, TomlEditor};
use crate::manage::{resolve_installed, InstalledVersion, ManageError};
use crate::paths::CrisolPaths;
use crate::project::{load_project, FileOwners, PROJECT_FILE};
use crate::resolve::Tool;
use crate::version::{VersionError, VersionReq};

/// Why a version could not be chosen.
#[derive(Debug, thiserror::Error)]
pub enum UseError {
    /// The request is not a version.
    #[error(transparent)]
    BadVersion(#[from] VersionError),
    /// No installed version matches.
    #[error("{tool} {request} is not installed (installed: {})", list(installed))]
    NotInstalled {
        /// The tool.
        tool: &'static str,
        /// What was asked for.
        request: String,
        /// The versions that are installed.
        installed: Vec<String>,
    },
    /// The project folder does not exist.
    #[error("`{}` is not a folder", .0.display())]
    NotAFolder(PathBuf),
    /// The installed versions could not be listed.
    #[error(transparent)]
    Manage(#[from] ManageError),
    /// The file could not be edited or written.
    #[error(transparent)]
    Edit(#[from] EditError),
}

fn list(installed: &[String]) -> String {
    if installed.is_empty() {
        "none".to_owned()
    } else {
        installed.join(", ")
    }
}

/// What a `use` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Used {
    /// The file that holds the choice.
    pub file: PathBuf,
    /// The tool.
    pub tool: Tool,
    /// What was written, as the user gave it.
    pub request: String,
    /// What was there before, if anything.
    pub previous: Option<String>,
    /// The installed version it selects right now.
    pub selects: InstalledVersion,
    /// The file was created or changed; `false` if it already said exactly this.
    pub changed: bool,
    /// Things the user should know (a variable of this terminal that overrides the choice).
    pub warnings: Vec<String>,
}

/// Makes `request` the default version of `tool` for every project that does not pin one.
pub fn use_global(
    paths: &CrisolPaths,
    tool: Tool,
    request: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Used, UseError> {
    let selects = installed_match(paths, tool, request)?;
    let mut editor = TomlEditor::open_config(paths)?;
    let key = ["defaults", tool.service()];
    finish(&mut editor, &key, tool, request, selects, env)
}

/// Pins `tool` to `request` for the project in `dir`, in `dir/.crisol.toml`. The pin of the
/// other tool, and anything else in the file, stays.
///
/// Levels are not merged (F1.4), so a **new** `.crisol.toml` in a subfolder hides what a trusted
/// one above pins for the other tool. When that happens the result carries a warning.
/// `owners` and `ceiling` are the ones of the search for the file above (ADR-0010).
pub fn use_local(
    paths: &CrisolPaths,
    dir: &Path,
    tool: Tool,
    request: &str,
    env: &dyn Fn(&str) -> Option<String>,
    owners: &dyn FileOwners,
    ceiling: Option<&Path>,
) -> Result<Used, UseError> {
    if !dir.is_dir() {
        return Err(UseError::NotAFolder(dir.to_path_buf()));
    }
    let selects = installed_match(paths, tool, request)?;
    let hidden = (!dir.join(PROJECT_FILE).exists())
        .then(|| hidden_pin(dir, tool, owners, ceiling))
        .flatten();
    let mut editor = TomlEditor::open_project(dir)?;
    let key = [tool.service()];
    let mut used = finish(&mut editor, &key, tool, request, selects, env)?;
    used.warnings.extend(hidden);
    Ok(used)
}

/// A warning if a trusted `.crisol.toml` above `dir` pins the tool **other** than `tool`: a new
/// file in `dir` would hide that pin.
fn hidden_pin(
    dir: &Path,
    tool: Tool,
    owners: &dyn FileOwners,
    ceiling: Option<&Path>,
) -> Option<String> {
    let above = load_project(dir.parent()?, ceiling, owners).ok()?.project?;
    let (other, pin) = match tool {
        Tool::Php => ("node", above.config.node?),
        Tool::Node => ("php", above.config.php?),
    };
    Some(format!(
        "{} pins {other} = \"{pin}\", but the new {PROJECT_FILE} in this folder hides it \
         (levels are not merged): {other} here no longer follows that pin. Add \
         `{other} = \"{pin}\"` to this folder's file to keep it",
        above.file.display()
    ))
}

fn installed_match(
    paths: &CrisolPaths,
    tool: Tool,
    request: &str,
) -> Result<InstalledVersion, UseError> {
    VersionReq::parse(request)?;
    if let Some(found) = resolve_installed(paths, tool.service(), request)? {
        return Ok(found);
    }
    let installed = crate::manage::list_installed(paths)?
        .into_iter()
        .filter(|i| i.service == tool.service())
        .map(|i| i.version.to_string())
        .collect();
    Err(UseError::NotInstalled {
        tool: tool.service(),
        request: request.to_owned(),
        installed,
    })
}

fn finish(
    editor: &mut TomlEditor,
    key: &[&str],
    tool: Tool,
    request: &str,
    selects: InstalledVersion,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Used, UseError> {
    let previous = editor
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::to_owned);
    editor.set(key, request)?;
    let changed = editor.save()?;

    let mut warnings = Vec::new();
    if let Some(value) = env(tool.env_var()).filter(|v| !v.trim().is_empty()) {
        warnings.push(format!(
            "{} is set to `{}` in this terminal and takes precedence over this choice",
            tool.env_var(),
            value.trim()
        ));
    }
    Ok(Used {
        file: editor.path().to_path_buf(),
        tool,
        request: request.to_owned(),
        previous,
        selects,
        changed,
        warnings,
    })
}

#[cfg(test)]
mod tests;
