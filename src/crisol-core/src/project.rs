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

use std::fmt;
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

/// Who owns a file, as far as deciding whether to obey it is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOwner {
    /// The owner is the current user, `SYSTEM` or an administrator.
    pub trusted: bool,
    /// The owner as the system names it (a SID on Windows), for messages.
    pub name: String,
}

/// Tells who owns a file (ADR-0010). The implementation is per operating system
/// (`crisol-platform`); the core only decides what to do with the answer.
pub trait FileOwners: Send + Sync {
    /// The owner of `path`. An error means "could not find out", which the search treats like
    /// an untrusted owner.
    fn owner_of(&self, path: &Path) -> io::Result<FileOwner>;
}

/// Trusts every file. For platforms where ownership is not checked yet, and for tests of
/// code that does not care. Prefer a real implementation anywhere a stranger's file could
/// be found.
#[derive(Debug, Default, Clone, Copy)]
pub struct TrustAllOwners;

impl FileOwners for TrustAllOwners {
    fn owner_of(&self, _path: &Path) -> io::Result<FileOwner> {
        Ok(FileOwner {
            trusted: true,
            name: "anyone".to_owned(),
        })
    }
}

/// A `.crisol.toml` that was found but not obeyed because of who owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredProject {
    /// The file.
    pub file: PathBuf,
    /// Its owner, or why it could not be determined.
    pub owner: String,
}

impl fmt::Display for IgnoredProject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ignoring `{path}`: it belongs to {owner}, not to you, SYSTEM or an administrator. \
             If the file is yours, take it over (`takeown /f \"{path}\"`) or create it again",
            path = self.file.display(),
            owner = self.owner,
        )
    }
}

/// What a search for a project file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSearch {
    /// The nearest trusted `.crisol.toml`, if any.
    pub file: Option<PathBuf>,
    /// Files passed over on the way because of their owner, nearest first. Show them as
    /// warnings.
    pub ignored: Vec<IgnoredProject>,
}

/// Finds the nearest trusted `.crisol.toml` at or above `start` (ADR-0010).
///
/// `start` may be a directory or a file (its directory is used). A file whose owner is not
/// trusted according to `owners` is **skipped, and reported in `ignored`**: the search goes
/// on upwards as if it did not exist. The search stops, after checking that directory, at
/// the first one that contains `.git` (a folder, or a file in worktrees and submodules), at
/// `ceiling` if given (like git's `GIT_CEILING_DIRECTORIES`), or at the filesystem root. A
/// directory that happens to be named `.crisol.toml` is ignored. The path is normalized
/// lexically; symlinks are not resolved.
#[must_use]
pub fn find_project_file(
    start: &Path,
    ceiling: Option<&Path>,
    owners: &dyn FileOwners,
) -> ProjectSearch {
    let mut search = find_files_upward(PROJECT_FILE, start, ceiling, owners);
    let file = (!search.files.is_empty()).then(|| search.files.remove(0));
    if let Some(found) = &file {
        // Only the strangers that stood in the way matter; one further up is not news.
        let depth = found.components().count();
        search
            .ignored
            .retain(|i| i.file.components().count() > depth);
    }
    ProjectSearch {
        file,
        ignored: search.ignored,
    }
}

/// Every trusted file called `file_name` at or above `start`, nearest first, under the same
/// rules as [`find_project_file`]: untrusted owners are skipped and reported, and the
/// search stops at the first `.git`, at `ceiling` or at the root. This is how the version
/// files that pick a tool (`.php-version`, `.nvmrc`, `composer.json`, `package.json`) are
/// found, with the same protection as `.crisol.toml`.
#[must_use]
pub fn find_files_upward(
    file_name: &str,
    start: &Path,
    ceiling: Option<&Path>,
    owners: &dyn FileOwners,
) -> FilesSearch {
    let mut found = FilesSearch {
        files: Vec::new(),
        ignored: Vec::new(),
    };
    let start = normalize(start);
    let ceiling = ceiling.map(normalize);
    let first = if start.is_file() {
        start.parent()
    } else {
        Some(start.as_path())
    };

    for dir in first.into_iter().flat_map(Path::ancestors) {
        let candidate = dir.join(file_name);
        if candidate.is_file() {
            match owners.owner_of(&candidate) {
                Ok(owner) if owner.trusted => found.files.push(candidate),
                Ok(owner) => found.ignored.push(IgnoredProject {
                    file: candidate,
                    owner: owner.name,
                }),
                Err(error) => found.ignored.push(IgnoredProject {
                    file: candidate,
                    owner: format!("an unknown owner ({error})"),
                }),
            }
        }
        if ceiling.as_deref().is_some_and(|c| same_dir(c, dir)) || dir.join(".git").exists() {
            break;
        }
    }
    found
}

/// What [`find_files_upward`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesSearch {
    /// Trusted files, nearest first.
    pub files: Vec<PathBuf>,
    /// Files passed over because of their owner, nearest first. Show them as warnings.
    pub ignored: Vec<IgnoredProject>,
}

/// The outcome of [`load_project`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLookup {
    /// The project that applies, if there is one.
    pub project: Option<LoadedProject>,
    /// Files passed over because of their owner. Show them as warnings.
    pub ignored: Vec<IgnoredProject>,
}

/// Finds and loads the nearest trusted `.crisol.toml` at or above `start`.
///
/// `project` is `None` when there is none. See [`find_project_file`] for the search rules.
pub fn load_project(
    start: &Path,
    ceiling: Option<&Path>,
    owners: &dyn FileOwners,
) -> Result<ProjectLookup, ConfigError> {
    let ProjectSearch { file, ignored } = find_project_file(start, ceiling, owners);
    let Some(file) = file else {
        return Ok(ProjectLookup {
            project: None,
            ignored,
        });
    };
    let text = fs::read_to_string(&file).map_err(|source: io::Error| ConfigError::Read {
        path: file.clone(),
        source,
    })?;
    let (config, warnings) = ProjectConfig::parse(&text)?;
    let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(ProjectLookup {
        project: Some(LoadedProject {
            config,
            file,
            root,
            warnings,
        }),
        ignored,
    })
}

/// Whether two normalised directories are the same place. Windows paths ignore case
/// (`D:\Proyectos` is `d:\proyectos`); elsewhere they do not.
fn same_dir(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Makes `path` absolute and removes `.` and `..` components lexically.
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
    use std::collections::BTreeSet;

    /// The nearest trusted file, as the tests below care about it.
    fn find(start: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
        find_project_file(start, ceiling, &TrustAllOwners).file
    }

    fn load(start: &Path, ceiling: Option<&Path>) -> Result<Option<LoadedProject>, ConfigError> {
        load_project(start, ceiling, &TrustAllOwners).map(|lookup| lookup.project)
    }

    /// Every file is trusted except the ones listed.
    struct Strangers(BTreeSet<PathBuf>);

    impl Strangers {
        fn of(files: &[&Path]) -> Self {
            Self(files.iter().map(|f| f.to_path_buf()).collect())
        }
    }

    impl FileOwners for Strangers {
        fn owner_of(&self, path: &Path) -> io::Result<FileOwner> {
            let stranger = self.0.contains(path);
            Ok(FileOwner {
                trusted: !stranger,
                name: if stranger { "S-1-5-21-9-9-9-1" } else { "me" }.to_owned(),
            })
        }
    }

    /// Owners cannot be determined at all.
    struct Unreadable;

    impl FileOwners for Unreadable {
        fn owner_of(&self, _path: &Path) -> io::Result<FileOwner> {
            Err(io::Error::other("no way to tell"))
        }
    }
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
        assert_eq!(find(&b, Some(&shouty)), None);
    }

    #[test]
    fn a_file_of_a_stranger_is_skipped_with_a_warning_and_the_search_goes_on() {
        let (_tmp, a, b, c) = tree();
        let far = put(&a, "php = \"8.1\"\n");
        let near = put(&b, "php = \"8.4\"\n");
        let owners = Strangers::of(&[&near]);

        let search = find_project_file(&c, None, &owners);
        assert_eq!(search.file, Some(far), "the trusted file above wins");
        assert_eq!(
            search.ignored,
            [IgnoredProject {
                file: near.clone(),
                owner: "S-1-5-21-9-9-9-1".to_owned()
            }]
        );
        let message = search.ignored[0].to_string();
        assert!(message.contains(&near.display().to_string()), "{message}");
        assert!(message.contains("S-1-5-21-9-9-9-1"), "{message}");
        assert!(
            message.contains("takeown"),
            "tells how to fix it: {message}"
        );
    }

    #[test]
    fn a_stranger_further_up_than_the_obeyed_file_is_not_reported() {
        let (_tmp, a, b, c) = tree();
        let far = put(
            &a,
            "php = \"8.1\"
",
        );
        let near = put(
            &b,
            "php = \"8.4\"
",
        );
        let search = find_project_file(&c, None, &Strangers::of(&[&far]));
        assert_eq!(search.file, Some(near));
        assert_eq!(search.ignored, [], "the stranger is behind the file in use");
    }

    #[test]
    fn when_every_file_is_a_stranger_s_nothing_applies() {
        let (_tmp, a, b, c) = tree();
        let outer = put(&a, "php = \"8.1\"\n");
        let inner = put(&b, "php = \"8.4\"\n");
        let lookup = load_project(&c, None, &Strangers::of(&[&outer, &inner])).unwrap();
        assert_eq!(lookup.project, None);
        assert_eq!(
            lookup.ignored.iter().map(|i| &i.file).collect::<Vec<_>>(),
            [&inner, &outer],
            "nearest first"
        );
    }

    #[test]
    fn an_owner_that_cannot_be_determined_is_not_trusted() {
        let (_tmp, a, _b, c) = tree();
        let file = put(&a, "php = \"8.1\"\n");
        let lookup = load_project(&c, None, &Unreadable).unwrap();
        assert_eq!(lookup.project, None);
        assert_eq!(lookup.ignored.len(), 1);
        assert_eq!(lookup.ignored[0].file, file);
        assert!(lookup.ignored[0].owner.contains("no way to tell"));
    }

    #[test]
    fn the_search_stops_at_the_first_directory_with_a_git_folder() {
        let (_tmp, a, b, c) = tree();
        put(&a, "php = \"8.1\"\n"); // outside the repository
        fs::create_dir(b.join(".git")).unwrap();
        assert_eq!(
            find(&c, None),
            None,
            "it must not climb past the repository"
        );

        // The file of the directory that holds `.git` still counts.
        let inside = put(&b, "php = \"8.4\"\n");
        assert_eq!(find(&c, None), Some(inside));
    }

    #[test]
    fn a_git_file_stops_the_search_too() {
        // Worktrees and submodules have a `.git` *file* instead of a folder.
        let (_tmp, a, b, c) = tree();
        put(&a, "php = \"8.1\"\n");
        fs::write(b.join(".git"), "gitdir: ../elsewhere\n").unwrap();
        assert_eq!(find(&c, None), None);
    }

    #[test]
    fn a_nested_project_does_not_inherit_the_repository_around_it() {
        let (_tmp, a, b, c) = tree();
        fs::create_dir(a.join(".git")).unwrap();
        let outer = put(&a, "php = \"8.1\"\n");
        fs::create_dir(c.join(".git")).unwrap(); // a repository inside the repository
        assert_eq!(find(&c, None), None);
        assert_eq!(
            find(&b, None),
            Some(outer),
            "but its parent still sees the outer file"
        );
    }

    #[test]
    fn finds_the_file_in_the_start_directory() {
        let (_tmp, a, _b, _c) = tree();
        let file = put(&a, "php = \"8.3\"\n");
        assert_eq!(find(&a, Some(&a)), Some(file));
    }

    #[test]
    fn finds_the_file_in_an_ancestor_from_a_deep_directory() {
        let (_tmp, a, _b, c) = tree();
        let file = put(&a, "php = \"8.3\"\n");
        assert_eq!(find(&c, Some(&a)), Some(file.clone()));

        let loaded = load(&c, Some(&a)).unwrap().unwrap();
        assert_eq!(loaded.file, file);
        assert_eq!(loaded.root, a);
        assert_eq!(loaded.config.php.as_deref(), Some("8.3"));
    }

    #[test]
    fn the_nearest_file_wins_and_levels_are_not_merged() {
        let (_tmp, a, b, c) = tree();
        put(&a, "php = \"8.1\"\nnode = \"20\"\n");
        put(&b, "php = \"8.4\"\n");
        let loaded = load(&c, Some(&a)).unwrap().unwrap();
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
        let loaded = load(&c, Some(&a)).unwrap().unwrap();
        assert_eq!(loaded.root, b);
        assert_eq!(loaded.config, ProjectConfig::default());
    }

    #[test]
    fn a_file_in_a_sibling_directory_is_not_found() {
        let (tmp, a, _b, c) = tree();
        let sibling = a.join("other");
        fs::create_dir(&sibling).unwrap();
        put(&sibling, "php = \"8.3\"\n");
        assert_eq!(find(&c, Some(tmp.path())), None);
    }

    #[test]
    fn nothing_found_gives_none() {
        let (tmp, _a, _b, c) = tree();
        assert_eq!(find(&c, Some(tmp.path())), None);
        assert!(load(&c, Some(tmp.path())).unwrap().is_none());
    }

    #[test]
    fn the_ceiling_is_checked_but_nothing_above_it() {
        let (_tmp, a, b, c) = tree();
        let above = put(&a, "php = \"8.3\"\n");

        // Ceiling below the file: not reached.
        assert_eq!(find(&c, Some(&b)), None);
        // Ceiling at the file's own directory: it is still checked.
        assert_eq!(find(&c, Some(&a)), Some(above));
    }

    #[test]
    fn starting_from_a_file_searches_from_its_directory() {
        let (_tmp, a, _b, c) = tree();
        let file = put(&a, "php = \"8.3\"\n");
        let source = c.join("index.php");
        fs::write(&source, "<?php").unwrap();
        assert_eq!(find(&source, Some(&a)), Some(file));
    }

    #[test]
    fn a_directory_named_like_the_file_is_ignored() {
        let (_tmp, a, b, c) = tree();
        let real = put(&a, "php = \"8.3\"\n");
        fs::create_dir(b.join(PROJECT_FILE)).unwrap();
        assert_eq!(find(&c, Some(&a)), Some(real));
    }

    #[test]
    fn dot_dot_components_are_resolved_before_searching() {
        let (_tmp, a, b, c) = tree();
        put(&b, "php = \"8.4\"\n");
        put(&a, "php = \"8.1\"\n");
        // a/b/c/../../  is `a`, not `b`.
        let tricky = c.join("..").join("..");
        let loaded = load(&tricky, Some(&a)).unwrap().unwrap();
        assert_eq!(loaded.root, a);
        assert_eq!(loaded.config.php.as_deref(), Some("8.1"));
    }

    #[test]
    fn a_broken_file_is_an_error_and_does_not_fall_back_to_a_parent() {
        let (_tmp, a, b, c) = tree();
        put(&a, "php = \"8.1\"\n");
        put(&b, "php = = broken");
        let err = load(&c, Some(&a)).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)), "{err}");

        put(&b, "php = \"../x\"\n");
        let err = load(&c, Some(&a)).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
    }

    #[test]
    fn warnings_travel_with_the_loaded_project() {
        let (_tmp, a, _b, _c) = tree();
        put(&a, "php = \"8.3\"\ncolour = 1\n");
        let loaded = load(&a, Some(&a)).unwrap().unwrap();
        assert_eq!(
            loaded.warnings,
            vec![ConfigWarning::UnknownField("colour".to_owned())]
        );
    }
}
