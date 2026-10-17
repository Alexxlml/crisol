//! Programs installed with `npm install --global` (F10.3).
//!
//! Each Node version has its own global folder (`data/node/<version>/global`, see
//! [`CrisolPaths::node_global`]), so `npm install -g typescript` under Node 22 is not seen by
//! Node 24. npm leaves a `tsc.cmd` in that folder, and for the person to type `tsc` in a
//! terminal something has to answer to that name in the `PATH`. The one folder Crisol puts in the
//! `PATH` is `shims/`, so the answer is a **shim per global program**: a copy of `crisol-shim`
//! called `tsc.exe` (decision in ADR-0018). Run as `tsc`, the shim finds the Node version for the
//! directory, looks for `tsc.cmd` in that version's global folder and runs it, with the Node of
//! that version first in its `PATH`.
//!
//! The copies are made by the `npm` shim itself when an `npm install|uninstall|update --global`
//! ends well, and by `crisol shims install|sync`. The names are written down in
//! `shims/.crisol-globals`, which is how a file in `shims/` is known to be ours: a file with the
//! same name that is not in the list is never overwritten or deleted.
//!
//! A program that is installed for one Node version only says so when it is run under another
//! (and which one has it), instead of failing as if it did not exist.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::manage::list_installed;
use crate::paths::CrisolPaths;
use crate::resolve::Command;
use crate::shims::{replace_running, shim_file_name, write_atomically, SHIM_COMMANDS};

/// The file in `shims/` that lists the commands that stand for global programs.
pub const MANIFEST: &str = ".crisol-globals";

/// The longest command name that gets a shim.
const MAX_NAME: usize = 64;

/// What [`sync_global_shims`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalSync {
    /// Commands that got a shim now.
    pub added: Vec<String>,
    /// Commands whose shim was deleted because no installed Node version has the program.
    pub removed: Vec<String>,
    /// Commands left alone, with why.
    pub skipped: Vec<(String, String)>,
}

/// Whether a command name can have a shim: plain characters, not a hidden file, not one of the
/// names Crisol already uses, and not a device name.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.ends_with('.')
        && !SHIM_COMMANDS.contains(&name.to_ascii_lowercase().as_str())
        && !name.eq_ignore_ascii_case("crisol-shim")
        && !crate::paths::is_reserved_device_name(name)
}

/// The programs in a global folder: the `<name>.cmd` files npm leaves there.
#[must_use]
pub fn global_commands(prefix: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(prefix) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let (stem, extension) = file.rsplit_once('.')?;
            extension
                .eq_ignore_ascii_case("cmd")
                .then(|| stem.to_owned())
        })
        .collect();
    names.sort();
    names
}

fn manifest_path(paths: &CrisolPaths) -> PathBuf {
    paths.shims_dir().join(MANIFEST)
}

fn read_manifest(paths: &CrisolPaths) -> BTreeSet<String> {
    fs::read_to_string(manifest_path(paths))
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|l| valid_name(l))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn write_manifest(paths: &CrisolPaths, names: &BTreeSet<String>) -> io::Result<()> {
    let path = manifest_path(paths);
    if names.is_empty() {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let mut text = String::new();
    for name in names {
        text.push_str(name);
        text.push('\n');
    }
    write_atomically(&paths.shims_dir(), &path, text.as_bytes())
}

/// The commands that have a shim for a global program.
#[must_use]
pub fn shimmed_globals(paths: &CrisolPaths) -> Vec<String> {
    read_manifest(paths).into_iter().collect()
}

/// Whether `name` is one of the commands in the list: only those answer as global programs.
#[must_use]
pub fn is_global_shim(paths: &CrisolPaths, name: &str) -> bool {
    valid_name(name) && read_manifest(paths).contains(name)
}

/// The installed Node versions.
fn node_versions(paths: &CrisolPaths) -> Vec<String> {
    list_installed(paths)
        .map(|list| {
            list.into_iter()
                .filter(|i| i.service == "node")
                .map(|i| i.version.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The `.cmd` that runs the global program `name` under Node `version`, if it is installed
/// there.
#[must_use]
pub fn find_global(paths: &CrisolPaths, name: &str, version: &str) -> Option<PathBuf> {
    if !valid_name(name) {
        return None;
    }
    let file = paths.node_global(version).ok()?.join(format!("{name}.cmd"));
    file.is_file().then_some(file)
}

/// The installed Node versions that have the global program `name`.
#[must_use]
pub fn installed_under(paths: &CrisolPaths, name: &str) -> Vec<String> {
    node_versions(paths)
        .into_iter()
        .filter(|v| find_global(paths, name, v).is_some())
        .collect()
}

/// Makes the shims of the global programs match what the installed Node versions have: one for
/// every program of any of them, none for a program that is gone. `shim` is the program that is
/// copied (`crisol-shim`). `reserved` says which names belong to the system (`ping`): they get
/// no command, because the shims folder comes first in the `PATH` and would hide it.
pub fn sync_global_shims(
    paths: &CrisolPaths,
    shim: &Path,
    reserved: &dyn Fn(&str) -> bool,
) -> io::Result<GlobalSync> {
    let program = fs::read(shim)?;
    let dir = paths.shims_dir();
    let mut wanted = BTreeSet::new();
    let mut skipped = Vec::new();
    for version in node_versions(paths) {
        let Ok(prefix) = paths.node_global(&version) else {
            continue;
        };
        for name in global_commands(&prefix) {
            if valid_name(&name) && reserved(&name) {
                skipped.push((
                    name,
                    "Windows has a program with that name, and a command of the shims would hide it"
                        .to_owned(),
                ));
            } else if valid_name(&name) {
                wanted.insert(name);
            } else if !SHIM_COMMANDS.contains(&name.to_ascii_lowercase().as_str()) {
                skipped.push((name, "the name cannot be a command of the shims".to_owned()));
            }
        }
    }
    let mut known = read_manifest(paths);
    let mut result = GlobalSync::default();
    if wanted.is_empty() && known.is_empty() {
        result.skipped = skipped;
        return Ok(result);
    }
    fs::create_dir_all(&dir)?;

    for name in &wanted {
        let target = dir.join(shim_file_name(name));
        let current = fs::read(&target).ok();
        let ours = known.contains(name);
        if current.is_some() && !ours && current.as_deref() != Some(program.as_slice()) {
            skipped.push((
                name.clone(),
                format!(
                    "`{}` is already in the shims folder and is not Crisol's",
                    target.display()
                ),
            ));
            continue;
        }
        if current.as_deref() != Some(program.as_slice()) {
            write_atomically(&dir, &target, &program)
                .or_else(|e| replace_running(&dir, &target, &program).map_err(|_| e))?;
        }
        if known.insert(name.clone()) {
            result.added.push(name.clone());
        }
    }
    for name in known.clone().difference(&wanted) {
        match fs::remove_file(dir.join(shim_file_name(name))) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                // Probably running: it stays in the list and goes the next time.
                skipped.push((name.clone(), format!("could not delete it yet: {e}")));
                continue;
            }
        }
        known.remove(name);
        result.removed.push(name.clone());
    }
    write_manifest(paths, &known)?;
    result.skipped = skipped;
    Ok(result)
}

/// Deletes the shims of the global programs and the list. Returns the names.
pub fn remove_global_shims(paths: &CrisolPaths) -> io::Result<Vec<String>> {
    let dir = paths.shims_dir();
    let mut removed = Vec::new();
    let mut kept = BTreeSet::new();
    for name in read_manifest(paths) {
        match fs::remove_file(dir.join(shim_file_name(&name))) {
            Ok(()) => removed.push(name),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                kept.insert(name);
            }
        }
    }
    write_manifest(paths, &kept)?;
    Ok(removed)
}

/// What to say when the global program `name` is not installed for the Node version that applies
/// here.
#[must_use]
pub fn not_installed_message(paths: &CrisolPaths, name: &str, version: &str) -> String {
    let elsewhere: Vec<String> = installed_under(paths, name)
        .into_iter()
        .filter(|v| v != version)
        .collect();
    if elsewhere.is_empty() {
        format!(
            "`{name}` is not installed for Node {version}: it was a global program that is gone now. \
             Install it with `npm install --global <package>` (its shim goes away with `crisol shims sync`)"
        )
    } else {
        format!(
            "`{name}` is not installed for Node {version}, only for Node {}. Every Node version has its own \
             global programs: install it here with `npm install --global <package>`, or use that version \
             (`crisol use node {}`)",
            elsewhere.join(", "),
            elsewhere[0]
        )
    }
}

/// Whether `name` is one of the commands Crisol itself provides (`php`, `npm`, `pnpm`…), as
/// opposed to a global program.
#[must_use]
pub fn is_core_command(name: &str) -> bool {
    Command::from_name(name).is_some()
}

#[cfg(test)]
mod tests;
