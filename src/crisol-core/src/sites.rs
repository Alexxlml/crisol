//! The sites Crisol serves (F5.1).
//!
//! A *site* is a folder served at `<name>.<tld>`. There are two ways to have one:
//!
//! - **Parked**: every subfolder of a parked directory (`[sites] parked` in `crisol.toml`, by
//!   default `CRISOL_HOME\www`) is a site named after the folder.
//! - **Linked**: a folder anywhere, with a name of its own, listed in `sites.toml`:
//!
//! ```toml
//! [linked.shop]
//! path    = "D:/code/shop"
//! docroot = "public"      # optional: where the web server's root is, relative to `path`
//! ```
//!
//! `sites.toml` is the user's file: it is only changed through [`link`] and [`unlink`], which
//! go through [`TomlEditor`] (comments and order stay). The parked directories stay in
//! `crisol.toml`, where [`Config`] already reads them.
//!
//! The document root is detected from what the folder contains (see [`detect_docroot`]) unless
//! the linked site names one. Nothing here touches the web server; F5.2 turns a [`Site`] into
//! configuration.
//!
//! Problems with a single site (an invalid name, a folder that is gone, two sites with the same
//! name) never stop the others: the site is left out and a warning says why.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::config::{deserialize_reporting_unknown, invalid, Config, ConfigError, ConfigWarning};
use crate::edit::{EditError, TomlEditor};
use crate::paths::CrisolPaths;

/// How a site came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    /// A subfolder of a parked directory.
    Parked,
    /// Listed in `sites.toml`.
    Linked,
}

/// How the document root of a site was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocRoot {
    /// The linked site said so (`docroot = …`).
    Explicit,
    /// `artisan` next to `public/index.php`: Laravel.
    Laravel,
    /// `bin/console` next to `public/index.php`: Symfony.
    Symfony,
    /// `wp-config.php` or `wp-load.php` in the folder: `WordPress` serves from the top.
    WordPress,
    /// `public/index.php` or `public/index.html`.
    PublicFolder,
    /// `web/index.php` (older Symfony, Yii, Bedrock).
    WebFolder,
    /// Nothing special: the folder itself.
    Root,
}

/// A folder served at `<name>.<tld>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The host name without the suffix (`shop`).
    pub name: String,
    /// The full host name (`shop.test`).
    pub domain: String,
    /// The project folder.
    pub root: PathBuf,
    /// The folder the web server serves.
    pub docroot: PathBuf,
    /// Why `docroot` is where it is.
    pub docroot_rule: DocRoot,
    /// Parked or linked.
    pub kind: SiteKind,
    /// Served over HTTPS too (`secure` in `sites.toml`, ADR-0013).
    pub secure: bool,
}

/// The sites that exist, and what to tell the user about the ones that do not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SiteList {
    /// Every site, by name.
    pub sites: Vec<Site>,
    /// Folders and entries left out, with why.
    pub warnings: Vec<String>,
}

// ---- sites.toml -----------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct RawSites {
    #[serde(default)]
    linked: BTreeMap<String, RawLinked>,
    #[serde(default)]
    secure: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawLinked {
    path: PathBuf,
    #[serde(default)]
    docroot: Option<String>,
}

/// A linked site as written in `sites.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedSite {
    /// The project folder (absolute).
    pub path: PathBuf,
    /// Document root relative to `path`, if the user set one.
    pub docroot: Option<String>,
}

/// The contents of `sites.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SitesFile {
    /// Linked sites by name.
    pub linked: BTreeMap<String, LinkedSite>,
    /// Names of the sites (parked or linked) served over HTTPS.
    pub secure: BTreeSet<String>,
    /// Unknown fields.
    pub warnings: Vec<ConfigWarning>,
}

impl SitesFile {
    /// Parses and validates the text of a `sites.toml`.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let (raw, warnings): (RawSites, _) = deserialize_reporting_unknown(text)?;
        let mut linked = BTreeMap::new();
        for (name, entry) in raw.linked {
            let field = format!("linked.{name}");
            validate_name(&name).map_err(|why| invalid(&field, &why))?;
            if !entry.path.is_absolute() {
                return Err(invalid(
                    &format!("{field}.path"),
                    "must be an absolute path",
                ));
            }
            if let Some(docroot) = &entry.docroot {
                relative_inside(docroot).ok_or_else(|| {
                    invalid(
                        &format!("{field}.docroot"),
                        "must be a relative path that stays inside the site",
                    )
                })?;
            }
            linked.insert(
                name,
                LinkedSite {
                    path: entry.path,
                    docroot: entry.docroot,
                },
            );
        }
        let mut secure = BTreeSet::new();
        for name in raw.secure {
            validate_name(&name).map_err(|why| invalid(&format!("secure.{name}"), &why))?;
            secure.insert(name);
        }
        Ok(Self {
            linked,
            secure,
            warnings,
        })
    }

    /// Reads `sites.toml`; a missing file is an empty list.
    pub fn load(paths: &CrisolPaths) -> Result<Self, ConfigError> {
        let file = paths.sites_file();
        match fs::read_to_string(&file) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read { path: file, source }),
        }
    }
}

/// Makes `path` a site called `name` (or changes the folder of the one that has that name).
/// `docroot` is relative to `path`; leave it out to have it detected.
///
/// The folder must exist and be absolute, and the name a valid host name. Only that entry of
/// `sites.toml` changes. Returns whether the file changed.
pub fn link(
    paths: &CrisolPaths,
    name: &str,
    path: &Path,
    docroot: Option<&str>,
) -> Result<bool, SitesError> {
    validate_name(name).map_err(|why| SitesError::Invalid {
        what: format!("the name `{name}`"),
        why,
    })?;
    if !path.is_absolute() {
        return Err(SitesError::Invalid {
            what: format!("`{}`", path.display()),
            why: "must be an absolute path".to_owned(),
        });
    }
    if !path.is_dir() {
        return Err(SitesError::NotAFolder(path.to_path_buf()));
    }
    if let Some(docroot) = docroot {
        if relative_inside(docroot).is_none() {
            return Err(SitesError::Invalid {
                what: format!("the document root `{docroot}`"),
                why: "must be a relative path that stays inside the site".to_owned(),
            });
        }
    }
    let mut editor = TomlEditor::open_sites(paths)?;
    editor.set(
        &["linked", name, "path"],
        path.to_string_lossy().into_owned(),
    )?;
    match docroot {
        Some(docroot) => editor.set(&["linked", name, "docroot"], docroot)?,
        None => {
            editor.remove(&["linked", name, "docroot"])?;
        }
    }
    Ok(editor.save()?)
}

/// Removes the linked site `name`. Returns whether it existed. The folder is not touched.
pub fn unlink(paths: &CrisolPaths, name: &str) -> Result<bool, SitesError> {
    let mut editor = TomlEditor::open_sites(paths)?;
    let existed = editor.remove(&["linked", name])?;
    editor.save()?;
    Ok(existed)
}

/// Marks the site `name` (parked or linked) as served over HTTPS (`on`) or not. Only the
/// `secure` list of `sites.toml` changes; the names in it keep their order. Returns whether the
/// file changed. The name must be a valid site name; whether the site exists is for the caller
/// to check against [`discover`].
pub fn set_secure(paths: &CrisolPaths, name: &str, on: bool) -> Result<bool, SitesError> {
    validate_name(name).map_err(|why| SitesError::Invalid {
        what: format!("the name `{name}`"),
        why,
    })?;
    let mut editor = TomlEditor::open_sites(paths)?;
    let mut names: Vec<String> = editor
        .get(&["secure"])
        .and_then(|item| item.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let present = names.iter().any(|n| n == name);
    match (on, present) {
        (true, false) => names.push(name.to_owned()),
        (false, true) => names.retain(|n| n != name),
        _ => return Ok(false),
    }
    if names.is_empty() {
        editor.remove(&["secure"])?;
    } else {
        editor.set(&["secure"], names.into_iter().collect::<toml_edit::Array>())?;
    }
    Ok(editor.save()?)
}

/// Why a site could not be linked or unlinked.
#[derive(Debug, thiserror::Error)]
pub enum SitesError {
    /// A value is not acceptable.
    #[error("{what} {why}")]
    Invalid {
        /// What was wrong.
        what: String,
        /// Why.
        why: String,
    },
    /// The folder does not exist.
    #[error("`{}` is not a folder", .0.display())]
    NotAFolder(PathBuf),
    /// `sites.toml` could not be edited or written.
    #[error(transparent)]
    Edit(#[from] EditError),
}

// ---- finding the sites ----------------------------------------------------------------

/// Every site: the subfolders of the parked directories plus the linked ones.
///
/// A linked site wins over a parked folder of the same name; between two parked directories the
/// first wins. Either way the one left out is mentioned.
#[must_use]
pub fn discover(paths: &CrisolPaths, config: &Config, linked: &SitesFile) -> SiteList {
    let tld = &config.sites.tld;
    let mut list = SiteList::default();
    let mut taken: BTreeSet<String> = BTreeSet::new();

    let mut candidates: Vec<(String, PathBuf, SiteKind, Option<&LinkedSite>)> = Vec::new();
    for (name, site) in &linked.linked {
        candidates.push((
            name.clone(),
            site.path.clone(),
            SiteKind::Linked,
            Some(site),
        ));
    }
    for dir in config.parked_dirs(paths) {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue; // a parked directory that is not there has no sites
        };
        let mut found: Vec<(String, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name.starts_with('.') || !path.is_dir() {
                continue;
            }
            let name = file_name.to_lowercase();
            match validate_name(&name) {
                Ok(()) => found.push((name, path)),
                Err(why) => list.warnings.push(format!(
                    "the folder `{}` is not served: its name {why}",
                    path.display()
                )),
            }
        }
        found.sort();
        candidates.extend(
            found
                .into_iter()
                .map(|(name, path)| (name, path, SiteKind::Parked, None)),
        );
    }

    for (name, root, kind, explicit) in candidates {
        if !taken.insert(name.clone()) {
            list.warnings.push(format!(
                "`{}` is not served as {name}.{tld}: that name already belongs to another site",
                root.display()
            ));
            continue;
        }
        if !root.is_dir() {
            list.warnings.push(format!(
                "the site `{name}` is not served: `{}` is not a folder",
                root.display()
            ));
            continue;
        }
        let (docroot, docroot_rule) = choose_docroot(&root, explicit, &name, &mut list.warnings);
        list.sites.push(Site {
            domain: format!("{name}.{tld}"),
            secure: linked.secure.contains(&name),
            name,
            root,
            docroot,
            docroot_rule,
            kind,
        });
    }
    list.sites.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

fn choose_docroot(
    root: &Path,
    explicit: Option<&LinkedSite>,
    name: &str,
    warnings: &mut Vec<String>,
) -> (PathBuf, DocRoot) {
    if let Some(relative) = explicit.and_then(|s| s.docroot.as_deref()) {
        if let Some(inside) = relative_inside(relative) {
            let dir = root.join(inside);
            if dir.is_dir() {
                return (dir, DocRoot::Explicit);
            }
            warnings.push(format!(
                "the document root `{relative}` of `{name}` does not exist; it is detected instead"
            ));
        }
    }
    detect_docroot(root)
}

/// The folder a web server should serve for the project in `root`, and why.
///
/// In order: `WordPress` (`wp-config.php` or `wp-load.php`, served from the top), then
/// `public/index.php` (Laravel with `artisan`, Symfony with `bin/console`, else any
/// `public/`), `public/index.html`, `web/index.php`, and otherwise the folder itself.
#[must_use]
pub fn detect_docroot(root: &Path) -> (PathBuf, DocRoot) {
    let has = |relative: &str| root.join(relative).is_file();
    if has("wp-config.php") || has("wp-load.php") {
        return (root.to_path_buf(), DocRoot::WordPress);
    }
    if has("public/index.php") {
        let rule = if has("artisan") {
            DocRoot::Laravel
        } else if has("bin/console") {
            DocRoot::Symfony
        } else {
            DocRoot::PublicFolder
        };
        return (root.join("public"), rule);
    }
    if has("public/index.html") || has("public/index.htm") {
        return (root.join("public"), DocRoot::PublicFolder);
    }
    if has("web/index.php") {
        return (root.join("web"), DocRoot::WebFolder);
    }
    (root.to_path_buf(), DocRoot::Root)
}

// ---- names and paths ------------------------------------------------------------------

/// A host name label: lowercase letters, digits and inner hyphens, 1 to 63 characters.
fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err("must be 1 to 63 lowercase letters, digits or inner hyphens (no spaces, dots or underscores)".to_owned())
    }
}

/// A relative path of plain components, as it can be joined under a folder without leaving it.
fn relative_inside(path: &str) -> Option<PathBuf> {
    let candidate = Path::new(path);
    let plain = candidate
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    (plain && candidate.components().next().is_some()).then(|| candidate.to_path_buf())
}

#[cfg(test)]
mod tests;
