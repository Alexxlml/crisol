//! The user's `PATH` and the one entry Crisol puts in it (F4.4, ADR-0002).
//!
//! Crisol adds a single folder, `CRISOL_HOME\shims`, to the **user** `PATH` (never the
//! machine one), and never changes it again: which PHP or Node runs is decided by the shim,
//! not by the `PATH`. This module is the logic of adding and removing that entry, and of
//! telling the user when something else in the `PATH` would win over the shims.
//!
//! The registry (or any other place the value lives) is behind [`UserPath`], so all of this is
//! tested with an in-memory double. The rules that matter:
//!
//! - **Only our entry is touched.** The rest of the value is kept byte for byte: order,
//!   spelling, `%VARIABLES%`, empty pieces and a trailing `;`. Adding and then removing the
//!   entry gives back exactly the value that was there.
//! - **The kind of value is kept** (`REG_SZ` or `REG_EXPAND_SZ`), so `%USERPROFILE%` entries
//!   keep expanding.
//! - **Nothing is written if nothing changes**, and the rest of the system is told about a
//!   change only when there was one.
//! - The entry goes **first**, so that the shims win over other tools in the user `PATH`.
//!   The machine `PATH` comes before the user one in Windows and cannot be beaten from here:
//!   [`shadowing`] reports those cases.

use std::io;
use std::path::{Path, PathBuf};

pub mod fake;

/// A `PATH` value as it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathValue {
    /// The text, entries separated by `;`, exactly as stored.
    pub text: String,
    /// Whether it may contain `%VARIABLES%` that the system expands (`REG_EXPAND_SZ`).
    pub expandable: bool,
}

/// Where the user `PATH` lives. The Windows implementation is the registry
/// (`HKCU\Environment`, value `Path`).
pub trait UserPath: Send + Sync {
    /// The user `PATH`, or `None` if the user has none.
    fn read(&self) -> io::Result<Option<PathValue>>;

    /// Stores `value` as the user `PATH`, or removes the value if it is `None`.
    fn write(&self, value: Option<&PathValue>) -> io::Result<()>;

    /// The machine `PATH`. Read only: Crisol never changes it.
    fn system_path(&self) -> io::Result<Option<String>>;

    /// `text` with `%VARIABLES%` expanded.
    fn expand(&self, text: &str) -> String;

    /// Tells running programs that the environment changed, so new terminals see it.
    fn notify_changed(&self) -> io::Result<()>;
}

/// What an add or a remove did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathChange {
    /// The entry was added.
    Added,
    /// The entry was already there; nothing was written.
    AlreadyPresent,
    /// The entry was removed.
    Removed,
    /// The entry was not there; nothing was written.
    NotPresent,
}

/// Whether two `PATH` entries name the same folder: same spelling apart from case, the kind
/// of slash and a trailing slash.
fn same_folder(a: &str, b: &str) -> bool {
    let normal = |text: &str| {
        text.trim()
            .trim_matches('"')
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let (a, b) = (normal(a), normal(b));
    !a.is_empty() && a == b
}

/// Whether `entry`, as written or expanded, is the folder `dir`.
fn is_entry(store: &dyn UserPath, entry: &str, dir: &str) -> bool {
    same_folder(entry, dir) || (entry.contains('%') && same_folder(&store.expand(entry), dir))
}

/// Puts `shims` at the front of the user `PATH` unless it is already in it.
pub fn add_shims(store: &dyn UserPath, shims: &Path) -> io::Result<PathChange> {
    let dir = shims.to_string_lossy().into_owned();
    if let Some(ch) = dir.chars().find(|c| matches!(c, ';' | '%')) {
        // `;` would split the entry in two and `%` would be expanded: it could not be found
        // again to remove it.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{dir}` contains `{ch}`, which cannot be part of a PATH entry"),
        ));
    }
    let current = store.read()?;
    let (text, expandable) = current.map_or((String::new(), true), |v| (v.text, v.expandable));
    if text.split(';').any(|entry| is_entry(store, entry, &dir)) {
        return Ok(PathChange::AlreadyPresent);
    }
    let text = if text.is_empty() {
        dir
    } else {
        format!("{dir};{text}")
    };
    store.write(Some(&PathValue { text, expandable }))?;
    store.notify_changed()?;
    Ok(PathChange::Added)
}

/// Removes every entry that is `shims` from the user `PATH`, and nothing else. If that leaves
/// the value empty it is removed, which is what it was before Crisol if it did not exist.
pub fn remove_shims(store: &dyn UserPath, shims: &Path) -> io::Result<PathChange> {
    let dir = shims.to_string_lossy().into_owned();
    let Some(value) = store.read()? else {
        return Ok(PathChange::NotPresent);
    };
    let pieces: Vec<&str> = value.text.split(';').collect();
    let kept: Vec<&str> = pieces
        .iter()
        .copied()
        .filter(|entry| !is_entry(store, entry, &dir))
        .collect();
    if kept.len() == pieces.len() {
        return Ok(PathChange::NotPresent);
    }
    let text = kept.join(";");
    if text.is_empty() {
        store.write(None)?;
    } else {
        store.write(Some(&PathValue {
            text,
            expandable: value.expandable,
        }))?;
    }
    store.notify_changed()?;
    Ok(PathChange::Removed)
}

/// Whether `shims` is in the user `PATH`.
pub fn shims_in_path(store: &dyn UserPath, shims: &Path) -> io::Result<bool> {
    let dir = shims.to_string_lossy().into_owned();
    Ok(store
        .read()?
        .is_some_and(|v| v.text.split(';').any(|e| is_entry(store, e, &dir))))
}

/// The programs a shim stands for and the file names that would run in their place.
pub const SHIMMED_PROGRAMS: [(&str, &[&str]); 10] = [
    ("php", &["php.exe", "php.bat", "php.cmd"]),
    ("node", &["node.exe"]),
    ("npm", &["npm.cmd", "npm.exe"]),
    ("npx", &["npx.cmd", "npx.exe"]),
    (
        "composer",
        &["composer.exe", "composer.bat", "composer.cmd"],
    ),
    ("corepack", &["corepack.cmd", "corepack.exe"]),
    ("pnpm", &["pnpm.cmd", "pnpm.exe"]),
    ("pnpx", &["pnpx.cmd", "pnpx.exe"]),
    ("yarn", &["yarn.cmd", "yarn.exe"]),
    ("yarnpkg", &["yarnpkg.cmd", "yarnpkg.exe"]),
];

/// Another installation of a shimmed program that wins over the shims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadow {
    /// The command (`php`).
    pub program: &'static str,
    /// The file that would run instead.
    pub found: PathBuf,
    /// The tool it seems to belong to, if it can be told from the path.
    pub suite: Option<&'static str>,
    /// It is in the machine `PATH`, which Crisol cannot reorder.
    pub in_system_path: bool,
}

/// Which tool a path seems to belong to.
fn suite_of(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    [
        ("herd", "Herd"),
        ("laragon", "Laragon"),
        ("xampp", "XAMPP"),
        ("wamp", "WampServer"),
        ("mamp", "MAMP"),
        ("devaron", "Devaron"),
        ("scoop", "Scoop"),
        ("chocolatey", "Chocolatey"),
        ("nvm", "nvm"),
    ]
    .into_iter()
    .find(|(needle, _)| lower.contains(needle))
    .map(|(_, name)| name)
}

/// The shimmed programs that something else in the `PATH` would run instead of the shims, in
/// the order Windows searches: the machine `PATH`, then the user one. If the shims are not in
/// the user `PATH` yet they are counted as if [`add_shims`] had put them first. `exists`
/// says whether a file is there.
pub fn shadowing(
    store: &dyn UserPath,
    shims: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> io::Result<Vec<Shadow>> {
    let dir = shims.to_string_lossy().into_owned();
    let split = |text: &str| -> Vec<String> {
        text.split(';')
            .map(|e| store.expand(e.trim().trim_matches('"')))
            .filter(|e| !e.is_empty())
            .collect()
    };
    let system = store.system_path()?.map(|t| split(&t)).unwrap_or_default();
    let user = store.read()?.map(|v| split(&v.text)).unwrap_or_default();

    let mut order: Vec<(String, bool)> = system.into_iter().map(|e| (e, true)).collect();
    if !user.iter().any(|e| same_folder(e, &dir)) {
        order.push((dir.clone(), false));
    }
    order.extend(user.into_iter().map(|e| (e, false)));

    let mut found = Vec::new();
    for (program, files) in SHIMMED_PROGRAMS {
        for (entry, in_system_path) in &order {
            if same_folder(entry, &dir) {
                break; // the shims come first from here on
            }
            let hit = files
                .iter()
                .map(|file| Path::new(entry).join(file))
                .find(|candidate| exists(candidate));
            if let Some(path) = hit {
                found.push(Shadow {
                    program,
                    suite: suite_of(&path.to_string_lossy()),
                    found: path,
                    in_system_path: *in_system_path,
                });
                break;
            }
        }
    }
    Ok(found)
}

/// The extensions Windows runs without being typed, when `PATHEXT` cannot be read.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC";

/// The programs of the machine and user `PATH` that a command in `shims/` would hide: `shims/`
/// comes first, so a global npm program called `ssh` or `git` would take their place (ADR-0018
/// §4). Left out are the folders the shims are meant to win over: `shims/` itself, anything in
/// the Crisol home, a Node installation (a folder with `node.exe`) and npm's own global folder,
/// `%APPDATA%\npm`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathPrograms {
    folders: Vec<PathBuf>,
    extensions: Vec<String>,
}

impl PathPrograms {
    /// Reads the machine and user `PATH` and `PATHEXT`.
    pub fn read(
        store: &dyn UserPath,
        shims: &Path,
        home: &Path,
        exists: &dyn Fn(&Path) -> bool,
    ) -> io::Result<Self> {
        let shims = shims.to_string_lossy();
        let home = home.to_string_lossy();
        let npm = store.expand(r"%APPDATA%\npm");
        let mut text = store.system_path()?.unwrap_or_default();
        if let Some(user) = store.read()? {
            text.push(';');
            text.push_str(&user.text);
        }
        let folders = text
            .split(';')
            .map(|e| store.expand(e.trim().trim_matches('"')))
            .filter(|e| !e.is_empty())
            .filter(|e| !same_folder(e, &shims) && !is_inside(e, &home))
            // An `%APPDATA%` that could not be expanded names no folder.
            .filter(|e| npm.contains('%') || !same_folder(e, &npm))
            .map(PathBuf::from)
            .filter(|folder| !exists(&folder.join("node.exe")))
            .collect();
        let pathext = store.expand("%PATHEXT%");
        let pathext = if pathext.contains('%') || pathext.trim().is_empty() {
            DEFAULT_PATHEXT.to_owned()
        } else {
            pathext
        };
        let extensions = pathext
            .split(';')
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        Ok(Self {
            folders,
            extensions,
        })
    }

    /// The file a command called `name` would run, if a folder of the `PATH` has one.
    #[must_use]
    pub fn find(&self, name: &str, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
        self.folders.iter().find_map(|folder| {
            self.extensions
                .iter()
                .map(|extension| folder.join(format!("{name}.{extension}")))
                .find(|file| exists(file))
        })
    }
}

/// Whether the `PATH` entry `entry` is the folder `dir` or inside it.
fn is_inside(entry: &str, dir: &str) -> bool {
    let normal = |text: &str| {
        text.trim()
            .trim_matches('"')
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let (entry, dir) = (normal(entry), normal(dir));
    !dir.is_empty() && (entry == dir || entry.starts_with(&format!("{dir}\\")))
}

#[cfg(test)]
mod tests;
