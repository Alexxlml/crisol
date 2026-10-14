//! Per-project configuration: `.crisol.toml`.
//!
//! A project pins the tool versions it needs:
//!
//! ```toml
//! php  = "8.3"
//! node = "22"
//! ```
//!
//! Rules:
//! - The file is found by walking **up** from a directory; the **nearest** one wins and
//!   nothing is merged across levels. An empty file is still "found" and ends the search.
//! - A found file that cannot be parsed is an **error**, not a reason to keep looking in a
//!   parent directory: silently using another version would be worse than failing.
//! - Unknown fields are warnings. There are no secrets here (ADR-0006).
//! - Project pins sit below the `CRISOL_PHP`/`CRISOL_NODE` overrides and above the other
//!   detection sources and the global defaults (see `docs/arquitectura.md`); that
//!   resolution lives elsewhere (F4.2).

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::config::{deserialize_reporting_unknown, invalid, ConfigError, ConfigWarning};
use crate::version::VersionReq;

/// Name of the per-project file.
pub const PROJECT_FILE: &str = ".crisol.toml";

/// Tool versions pinned by a project. `None` means "not pinned here".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    /// PHP version, e.g. `"8.3"` (matches the highest installed 8.3.x).
    pub php: Option<String>,
    /// Node version, e.g. `"22"`.
    pub node: Option<String>,
}

/// A `.crisol.toml` found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProject {
    /// The parsed file.
    pub config: ProjectConfig,
    /// Path of the `.crisol.toml` that was used.
    pub file: PathBuf,
    /// Directory that contains it: the project root.
    pub root: PathBuf,
    /// Non-fatal findings.
    pub warnings: Vec<ConfigWarning>,
}

impl ProjectConfig {
    /// Parses the text of a `.crisol.toml`.
    pub fn parse(text: &str) -> Result<(Self, Vec<ConfigWarning>), ConfigError> {
        let (config, warnings): (Self, _) = deserialize_reporting_unknown(text)?;
        config.validate()?;
        Ok((config, warnings))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (field, value) in [("php", &self.php), ("node", &self.node)] {
            if let Some(version) = value {
                VersionReq::parse(version).map_err(|_| invalid(field, "not a valid version"))?;
            }
        }
        Ok(())
    }
}

/// Finds the nearest `.crisol.toml` at or above `start`.
///
/// `start` may be a directory or a file (its directory is used). The search goes up to the
/// filesystem root, or stops after checking `ceiling` if given (like git's
/// `GIT_CEILING_DIRECTORIES`). A directory that happens to be named `.crisol.toml` is
/// ignored. The path is normalized lexically; symlinks are not resolved.
#[must_use]
pub fn find_project_file(start: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
    let start = normalize(start);
    let ceiling = ceiling.map(normalize);
    let first = if start.is_file() {
        start.parent()?
    } else {
        start.as_path()
    };

    for dir in first.ancestors() {
        let candidate = dir.join(PROJECT_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        if ceiling.as_deref().is_some_and(|c| same_dir(c, dir)) {
            break;
        }
    }
    None
}

/// Finds and loads the nearest `.crisol.toml` at or above `start`.
///
/// Returns `Ok(None)` when there is none. See [`find_project_file`] for the search rules.
pub fn load_project(
    start: &Path,
    ceiling: Option<&Path>,
) -> Result<Option<LoadedProject>, ConfigError> {
    let Some(file) = find_project_file(start, ceiling) else {
        return Ok(None);
    };
    let text = fs::read_to_string(&file).map_err(|source: io::Error| ConfigError::Read {
        path: file.clone(),
        source,
    })?;
    let (config, warnings) = ProjectConfig::parse(&text)?;
    let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(Some(LoadedProject {
        config,
        file,
        root,
        warnings,
    }))
}

/// Makes `path` absolute and removes `.` and `..` components lexically.
/// Whether two normalised directories are the same place. Windows paths ignore case
/// (`D:\Proyectos` is `d:\proyectos`); elsewhere they do not.
fn same_dir(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn normalize(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop the root or the drive prefix.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Builds `<tmp>/a/b/c` and returns (tmp, the three directories).
    fn tree() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = a.join("b");
        let c = b.join("c");
        fs::create_dir_all(&c).unwrap();
        (tmp, a, b, c)
    }

    fn put(dir: &Path, text: &str) -> PathBuf {
        let file = dir.join(PROJECT_FILE);
        fs::write(&file, text).unwrap();
        file
    }

    #[test]
    fn parses_pins_and_an_empty_file() {
        let (config, warnings) = ProjectConfig::parse("php = \"8.3\"\nnode = \"22\"\n").unwrap();
        assert_eq!(config.php.as_deref(), Some("8.3"));
        assert_eq!(config.node.as_deref(), Some("22"));
        assert_eq!(warnings, Vec::new());

        let (config, warnings) = ProjectConfig::parse("").unwrap();
        assert_eq!(config, ProjectConfig::default());
        assert_eq!(warnings, Vec::new());

        let (config, _) = ProjectConfig::parse("# only a comment\n").unwrap();
        assert_eq!(config, ProjectConfig::default());
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let (config, _) = ProjectConfig::parse("\u{feff}php = \"8.4\"\n").unwrap();
        assert_eq!(config.php.as_deref(), Some("8.4"));
    }

    #[test]
    fn unknown_fields_warn() {
        let (config, warnings) = ProjectConfig::parse("php = \"8.3\"\nruby = \"3\"\n").unwrap();
        assert_eq!(config.php.as_deref(), Some("8.3"));
        assert_eq!(
            warnings,
            vec![ConfigWarning::UnknownField("ruby".to_owned())]
        );
    }

    #[test]
    fn bad_versions_and_types_are_errors() {
        for text in ["php = \"../8\"\n", "node = \"\"\n", "php = \"8 3\"\n"] {
            let err = ProjectConfig::parse(text).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { .. }), "{text}: {err}");
        }
        assert!(matches!(
            ProjectConfig::parse("php = 8\n"),
            Err(ConfigError::Parse(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn the_ceiling_ignores_letter_case_on_windows() {
        let (_tmp, a, b, _c) = tree();
        put(&a, "php = \"8.1\"\n");
        // Same folder as `b`, spelt in upper case: the search must still stop there.
        let shouty = PathBuf::from(b.to_string_lossy().to_uppercase());
        assert_eq!(find_project_file(&b, Some(&shouty)), None);
    }

    #[test]
    fn finds_the_file_in_the_start_directory() {
        let (_tmp, a, _b, _c) = tree();
        let file = put(&a, "php = \"8.3\"\n");
        assert_eq!(find_project_file(&a, Some(&a)), Some(file));
    }

    #[test]
    fn finds_the_file_in_an_ancestor_from_a_deep_directory() {
        let (_tmp, a, _b, c) = tree();
        let file = put(&a, "php = \"8.3\"\n");
        assert_eq!(find_project_file(&c, Some(&a)), Some(file.clone()));

        let loaded = load_project(&c, Some(&a)).unwrap().unwrap();
        assert_eq!(loaded.file, file);
        assert_eq!(loaded.root, a);
        assert_eq!(loaded.config.php.as_deref(), Some("8.3"));
    }

    #[test]
    fn the_nearest_file_wins_and_levels_are_not_merged() {
        let (_tmp, a, b, c) = tree();
        put(&a, "php = \"8.1\"\nnode = \"20\"\n");
        put(&b, "php = \"8.4\"\n");
        let loaded = load_project(&c, Some(&a)).unwrap().unwrap();
        assert_eq!(loaded.root, b);
        assert_eq!(loaded.config.php.as_deref(), Some("8.4"));
        assert_eq!(
            loaded.config.node, None,
            "node from the outer file must not leak in"
        );
    }

    #[test]
    fn an_empty_inner_file_still_ends_the_search() {
        let (_tmp, a, b, c) = tree();
        put(&a, "php = \"8.1\"\n");
        put(&b, "");
        let loaded = load_project(&c, Some(&a)).unwrap().unwrap();
        assert_eq!(loaded.root, b);
        assert_eq!(loaded.config, ProjectConfig::default());
    }

    #[test]
    fn a_file_in_a_sibling_directory_is_not_found() {
        let (tmp, a, _b, c) = tree();
        let sibling = a.join("other");
        fs::create_dir(&sibling).unwrap();
        put(&sibling, "php = \"8.3\"\n");
        assert_eq!(find_project_file(&c, Some(tmp.path())), None);
    }

    #[test]
    fn nothing_found_gives_none() {
        let (tmp, _a, _b, c) = tree();
        assert_eq!(find_project_file(&c, Some(tmp.path())), None);
        assert!(load_project(&c, Some(tmp.path())).unwrap().is_none());
    }

    #[test]
    fn the_ceiling_is_checked_but_nothing_above_it() {
        let (_tmp, a, b, c) = tree();
        let above = put(&a, "php = \"8.3\"\n");

        // Ceiling below the file: not reached.
        assert_eq!(find_project_file(&c, Some(&b)), None);
        // Ceiling at the file's own directory: it is still checked.
        assert_eq!(find_project_file(&c, Some(&a)), Some(above));
    }

    #[test]
    fn starting_from_a_file_searches_from_its_directory() {
        let (_tmp, a, _b, c) = tree();
        let file = put(&a, "php = \"8.3\"\n");
        let source = c.join("index.php");
        fs::write(&source, "<?php").unwrap();
        assert_eq!(find_project_file(&source, Some(&a)), Some(file));
    }

    #[test]
    fn a_directory_named_like_the_file_is_ignored() {
        let (_tmp, a, b, c) = tree();
        let real = put(&a, "php = \"8.3\"\n");
        fs::create_dir(b.join(PROJECT_FILE)).unwrap();
        assert_eq!(find_project_file(&c, Some(&a)), Some(real));
    }

    #[test]
    fn dot_dot_components_are_resolved_before_searching() {
        let (_tmp, a, b, c) = tree();
        put(&b, "php = \"8.4\"\n");
        put(&a, "php = \"8.1\"\n");
        // a/b/c/../../  is `a`, not `b`.
        let tricky = c.join("..").join("..");
        let loaded = load_project(&tricky, Some(&a)).unwrap().unwrap();
        assert_eq!(loaded.root, a);
        assert_eq!(loaded.config.php.as_deref(), Some("8.1"));
    }

    #[test]
    fn a_broken_file_is_an_error_and_does_not_fall_back_to_a_parent() {
        let (_tmp, a, b, c) = tree();
        put(&a, "php = \"8.1\"\n");
        put(&b, "php = = broken");
        let err = load_project(&c, Some(&a)).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)), "{err}");

        put(&b, "php = \"../x\"\n");
        let err = load_project(&c, Some(&a)).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
    }

    #[test]
    fn warnings_travel_with_the_loaded_project() {
        let (_tmp, a, _b, _c) = tree();
        put(&a, "php = \"8.3\"\ncolour = 1\n");
        let loaded = load_project(&a, Some(&a)).unwrap().unwrap();
        assert_eq!(
            loaded.warnings,
            vec![ConfigWarning::UnknownField("colour".to_owned())]
        );
    }
}
