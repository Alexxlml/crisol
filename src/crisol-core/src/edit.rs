//! Safe editing of the user's TOML files (`crisol.toml`, `sites.toml`).
//!
//! Every change to a file the user owns goes through [`TomlEditor`] (ADR-0003):
//!
//! - **Nothing else changes.** Comments, blank lines, spacing, key order and line endings
//!   are preserved byte for byte (`toml_edit`). Replacing a value keeps its inline comment.
//! - **Atomic.** The new content is written to a temporary file in the same directory and
//!   renamed over the target, so a crash never leaves a half-written file.
//! - **A no-op does not touch the file.** If the rendered text equals the original, nothing
//!   is written.
//! - **Never overwrite someone else's edit.** If the file changed on disk after it was
//!   opened (e.g. the user saved it in their editor), saving fails instead of clobbering it.
//! - **Never write what Crisol could not read back.** For `crisol.toml` the result is
//!   validated with [`Config::parse`] before it is written.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

use crate::config::Config;
use crate::paths::CrisolPaths;
use crate::project::{ProjectConfig, PROJECT_FILE};

const BOM: char = '\u{feff}';

/// Validates the full text of a file before it is written.
type Check = fn(&str) -> Result<(), String>;

/// Why an edit or a save failed.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The file exists but could not be read.
    #[error("cannot read `{}`: {source}", path.display())]
    Read {
        /// File that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The file could not be written.
    #[error("cannot write `{}`: {source}", path.display())]
    Write {
        /// File that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The existing text is not valid TOML. Nothing is changed.
    #[error("cannot parse `{}`: {source}", path.display())]
    Parse {
        /// File that failed.
        path: PathBuf,
        /// Parser error, with line and column.
        source: toml_edit::TomlError,
    },
    /// The key path is empty.
    #[error("the key path must not be empty")]
    EmptyPath,
    /// A segment of the key path is a value, not a table.
    #[error("`{0}` is not a table")]
    NotATable(String),
    /// The edited text is not an acceptable configuration, so it was not written.
    #[error("the change would produce an invalid file: {0}")]
    Rejected(String),
    /// The file changed on disk since it was opened.
    #[error("`{}` changed on disk while it was being edited; reload and try again", .0.display())]
    ChangedOnDisk(PathBuf),
}

/// An open TOML file with pending edits.
#[derive(Debug)]
pub struct TomlEditor {
    path: PathBuf,
    doc: DocumentMut,
    /// Raw text read from disk, or `None` if the file did not exist.
    original: Option<String>,
    bom: bool,
    crlf: bool,
    check: Option<Check>,
}

impl TomlEditor {
    /// Opens `path`. A missing file is an empty document and is created on the first save.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, EditError> {
        let path = path.into();
        let original = match fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(EditError::Read { path, source }),
        };
        Self::from_text(path, original)
    }

    /// Opens `crisol.toml`, validating every save against the [`Config`] model.
    pub fn open_config(paths: &CrisolPaths) -> Result<Self, EditError> {
        let mut editor = Self::open(paths.config_file())?;
        editor.check = Some(check_config);
        Ok(editor)
    }

    /// Opens (or prepares to create) the `.crisol.toml` of the project in `dir`, validating
    /// every save against the project model.
    pub fn open_project(dir: &Path) -> Result<Self, EditError> {
        let mut editor = Self::open(dir.join(PROJECT_FILE))?;
        editor.check = Some(check_project);
        Ok(editor)
    }

    /// Opens `sites.toml`.
    pub fn open_sites(paths: &CrisolPaths) -> Result<Self, EditError> {
        Self::open(paths.sites_file())
    }

    fn from_text(path: PathBuf, original: Option<String>) -> Result<Self, EditError> {
        let raw = original.as_deref().unwrap_or("");
        let bom = raw.starts_with(BOM);
        let body = raw.strip_prefix(BOM).unwrap_or(raw);
        let crlf = body.contains("\r\n") && !body.replace("\r\n", "").contains('\n');
        let doc = body
            .parse::<DocumentMut>()
            .map_err(|source| EditError::Parse {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            path,
            doc,
            original,
            bom,
            crlf,
            check: None,
        })
    }

    /// The file this editor will write.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the item at `path` (`["services", "redis", "port"]`).
    #[must_use]
    pub fn get(&self, path: &[&str]) -> Option<&Item> {
        let mut current = self.doc.as_item();
        for key in path {
            current = current.get(*key)?;
        }
        Some(current)
    }

    /// Sets the value at `path`, creating the tables that are missing.
    ///
    /// Replacing an existing value keeps its inline comment and spacing.
    pub fn set(&mut self, path: &[&str], new: impl Into<Value>) -> Result<(), EditError> {
        let (last, parents) = path.split_last().ok_or(EditError::EmptyPath)?;
        let table = self.table_at(parents, true)?.ok_or(EditError::EmptyPath)?;

        let mut new = new.into();
        // Assign into the existing item instead of re-inserting it: that keeps the key's
        // own formatting (`port    = ...`) as well as the value's inline comment.
        if let Some(item) = table.get_mut(last) {
            if let Some(old) = item.as_value() {
                *new.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(new);
        } else {
            table.insert(last, Item::Value(new));
        }
        Ok(())
    }

    /// Removes the key at `path`. Returns whether it existed.
    pub fn remove(&mut self, path: &[&str]) -> Result<bool, EditError> {
        let (last, parents) = path.split_last().ok_or(EditError::EmptyPath)?;
        match self.table_at(parents, false)? {
            Some(table) => Ok(table.remove(last).is_some()),
            None => Ok(false),
        }
    }

    /// The text that [`save`](Self::save) would write.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = self.doc.to_string();
        if self.crlf {
            out = out.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        if self.bom {
            out.insert(0, BOM);
        }
        out
    }

    /// Whether saving would change the file.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.original.as_deref() != Some(self.render().as_str())
            && !(self.original.is_none() && self.doc.as_table().is_empty())
    }

    /// Writes the file atomically. Returns `false` when there was nothing to write.
    pub fn save(&self) -> Result<bool, EditError> {
        if !self.is_dirty() {
            return Ok(false);
        }
        let text = self.render();
        if let Some(check) = self.check {
            check(&text).map_err(EditError::Rejected)?;
        }
        if self.on_disk_now()? != self.original {
            return Err(EditError::ChangedOnDisk(self.path.clone()));
        }

        let write_err = |source| EditError::Write {
            path: self.path.clone(),
            source,
        };
        let dir = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        fs::create_dir_all(dir).map_err(write_err)?;
        let mut temp = NamedTempFile::new_in(dir).map_err(write_err)?;
        temp.write_all(text.as_bytes()).map_err(write_err)?;
        temp.as_file().sync_all().map_err(write_err)?;
        temp.persist(&self.path).map_err(|e| write_err(e.error))?;
        Ok(true)
    }

    fn on_disk_now(&self) -> Result<Option<String>, EditError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(EditError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Walks to the table at `path`. With `create`, missing tables are made (implicit, so
    /// no empty `[header]` is printed); otherwise a missing table gives `None`.
    fn table_at(
        &mut self,
        path: &[&str],
        create: bool,
    ) -> Result<Option<&mut dyn TableLike>, EditError> {
        let mut table: &mut dyn TableLike = self.doc.as_table_mut();
        for (i, key) in path.iter().enumerate() {
            if !table.contains_key(key) {
                if !create {
                    return Ok(None);
                }
                let mut fresh = Table::new();
                fresh.set_implicit(true);
                table.insert(key, Item::Table(fresh));
            }
            table = table
                .get_mut(key)
                .and_then(Item::as_table_like_mut)
                .ok_or_else(|| EditError::NotATable(path[..=i].join(".")))?;
        }
        Ok(Some(table))
    }
}

fn check_config(text: &str) -> Result<(), String> {
    Config::parse(text).map(|_| ()).map_err(|e| e.to_string())
}

fn check_project(text: &str) -> Result<(), String> {
    ProjectConfig::parse(text)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A file full of things a naive rewrite would destroy.
    const COMMENTED: &str = "\
# ----- My Crisol -----
# Keep this banner.

[defaults]
php  =   \"8.4\"      # my favourite
node = \"22\"
# node is only for the frontend

[sites]
tld    = \"test\"
parked = [ \"www\",
           \"D:\\\\Proyectos\" ]   # two folders

# Services I actually use
[services.redis]
enabled = true     # needed for queues
port    = 6379


[php]
workers = 4 # a bit of a guess
# end of file
";

    fn file(dir: &TempDir, text: &str) -> PathBuf {
        let path = dir.path().join("crisol.toml");
        fs::write(&path, text).unwrap();
        path
    }

    fn edited(text: &str, f: impl FnOnce(&mut TomlEditor)) -> String {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, text);
        let mut editor = TomlEditor::open(&path).unwrap();
        f(&mut editor);
        assert!(editor.save().unwrap());
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn changing_one_value_leaves_every_other_byte_identical() {
        let after = edited(COMMENTED, |e| e.set(&["defaults", "php"], "8.3").unwrap());
        assert_eq!(after, COMMENTED.replacen("\"8.4\"", "\"8.3\"", 1));
    }

    #[test]
    fn replacing_a_value_keeps_its_inline_comment_and_spacing() {
        let after = edited(COMMENTED, |e| {
            e.set(&["services", "redis", "port"], 6380_i64).unwrap();
        });
        assert_eq!(after, COMMENTED.replacen("6379", "6380", 1));
        let after = edited(COMMENTED, |e| e.set(&["php", "workers"], 8_i64).unwrap());
        assert_eq!(after, COMMENTED.replacen("workers = 4", "workers = 8", 1));
        let after = edited(COMMENTED, |e| {
            e.set(&["services", "redis", "enabled"], false).unwrap();
        });
        assert_eq!(
            after,
            COMMENTED.replacen("enabled = true", "enabled = false", 1)
        );
    }

    #[test]
    fn a_new_key_in_an_existing_table_changes_nothing_before_it() {
        let after = edited(COMMENTED, |e| {
            e.set(&["defaults", "python"], "3.13").unwrap();
        });
        assert!(after.contains("python = \"3.13\""), "{after}");
        let without_new = after.replacen("python = \"3.13\"\n", "", 1);
        assert_eq!(without_new, COMMENTED);
    }

    #[test]
    fn a_new_table_is_inserted_without_touching_existing_bytes() {
        let after = edited(COMMENTED, |e| {
            e.set(&["services", "mysql", "enabled"], true).unwrap();
        });
        // It lands next to its sibling `[services.redis]`; taking it out restores the file.
        assert!(
            after.contains(
                "[services.mysql]
enabled = true
"
            ),
            "{after}"
        );
        assert_eq!(
            after.replacen(
                "
[services.mysql]
enabled = true
",
                "",
                1
            ),
            COMMENTED
        );
        let loaded = Config::parse(&after).unwrap();
        assert!(loaded.config.services.mysql.enabled);
    }

    #[test]
    fn remove_deletes_only_that_key() {
        let after = edited(COMMENTED, |e| {
            assert!(e.remove(&["defaults", "node"]).unwrap());
        });
        assert_eq!(after, COMMENTED.replacen("node = \"22\"\n", "", 1));
    }

    #[test]
    fn removing_something_absent_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, COMMENTED);
        let mut editor = TomlEditor::open(&path).unwrap();
        assert!(!editor.remove(&["nope", "nothing"]).unwrap());
        assert!(!editor.remove(&["defaults", "nothing"]).unwrap());
        assert!(!editor.is_dirty());
        assert!(!editor.save().unwrap());
    }

    #[test]
    fn setting_the_same_value_does_not_write() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, COMMENTED);
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        let mut editor = TomlEditor::open(&path).unwrap();
        editor.set(&["defaults", "node"], "22").unwrap();
        assert!(!editor.is_dirty());
        assert!(!editor.save().unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), COMMENTED);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn get_reads_values() {
        let dir = TempDir::new().unwrap();
        let editor = TomlEditor::open(file(&dir, COMMENTED)).unwrap();
        let tld = editor.get(&["sites", "tld"]).and_then(Item::as_str);
        assert_eq!(tld, Some("test"));
        assert!(editor.get(&["sites", "missing"]).is_none());
        assert!(editor.get(&["nope", "deeper"]).is_none());
    }

    #[test]
    fn a_missing_file_is_created_on_the_first_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("crisol.toml");
        let mut editor = TomlEditor::open(&path).unwrap();
        assert!(!editor.is_dirty(), "an empty new document is not a change");
        assert!(!editor.save().unwrap());
        assert!(!path.exists());

        editor.set(&["defaults", "php"], "8.4").unwrap();
        assert!(editor.save().unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            Config::parse(&text).unwrap().config.defaults.php.as_deref(),
            Some("8.4")
        );
    }

    #[test]
    fn writing_through_a_value_is_an_error() {
        let dir = TempDir::new().unwrap();
        let mut editor = TomlEditor::open(file(&dir, COMMENTED)).unwrap();
        let err = editor.set(&["defaults", "php", "minor"], "3").unwrap_err();
        assert!(
            matches!(err, EditError::NotATable(ref p) if p == "defaults.php"),
            "{err}"
        );
        assert!(matches!(
            editor.set(&[], "x").unwrap_err(),
            EditError::EmptyPath
        ));
    }

    #[test]
    fn inline_tables_can_be_edited() {
        let text = "services = { redis = { port = 6379 } }  # compact\n";
        let after = edited(text, |e| {
            e.set(&["services", "redis", "port"], 7000_i64).unwrap();
        });
        assert_eq!(after, text.replacen("6379", "7000", 1));
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let crlf = COMMENTED.replace('\n', "\r\n");
        let after = edited(&crlf, |e| e.set(&["defaults", "php"], "8.3").unwrap());
        assert_eq!(after, crlf.replacen("\"8.4\"", "\"8.3\"", 1));

        let after = edited(&crlf, |e| {
            e.set(&["services", "mysql", "enabled"], true).unwrap();
        });
        let block = "\r\n[services.mysql]\r\nenabled = true\r\n";
        assert_eq!(after.replacen(block, "", 1), crlf);
        assert!(
            !after.replace("\r\n", "").contains('\n'),
            "no bare LF may appear"
        );
    }

    #[test]
    fn a_byte_order_mark_is_preserved() {
        let with_bom = format!("{BOM}{COMMENTED}");
        let after = edited(&with_bom, |e| e.set(&["defaults", "php"], "8.3").unwrap());
        assert_eq!(after, with_bom.replacen("\"8.4\"", "\"8.3\"", 1));
        assert_eq!(
            Config::parse(&with_bom)
                .unwrap()
                .config
                .defaults
                .php
                .as_deref(),
            Some("8.4")
        );
    }

    #[test]
    fn invalid_existing_toml_fails_to_open_and_is_left_alone() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "[defaults\nphp = ");
        let err = TomlEditor::open(&path).unwrap_err();
        assert!(matches!(err, EditError::Parse { .. }), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "[defaults\nphp = ");
    }

    #[test]
    fn saving_over_someone_elses_edit_is_refused() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, COMMENTED);
        let mut editor = TomlEditor::open(&path).unwrap();
        editor.set(&["defaults", "php"], "8.3").unwrap();

        let theirs = format!("{COMMENTED}# added in the editor\n");
        fs::write(&path, &theirs).unwrap();
        let err = editor.save().unwrap_err();
        assert!(matches!(err, EditError::ChangedOnDisk(_)), "{err}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            theirs,
            "their edit survives"
        );
    }

    #[test]
    fn a_file_that_appears_after_opening_is_not_overwritten() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crisol.toml");
        let mut editor = TomlEditor::open(&path).unwrap();
        editor.set(&["defaults", "php"], "8.3").unwrap();
        fs::write(&path, "# created meanwhile\n").unwrap();
        assert!(matches!(
            editor.save().unwrap_err(),
            EditError::ChangedOnDisk(_)
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "# created meanwhile\n");
    }

    #[test]
    fn a_failed_write_keeps_the_original_and_leaves_no_temp_files() {
        let dir = TempDir::new().unwrap();
        // The target is a directory, so the final rename must fail.
        let target = dir.path().join("crisol.toml");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside.txt"), "x").unwrap();

        let mut editor = TomlEditor::open(dir.path().join("other.toml")).unwrap();
        editor.set(&["defaults", "php"], "8.3").unwrap();
        // Point the editor at the directory.
        editor.path = target.clone();
        let err = editor.save().unwrap_err();
        assert!(
            matches!(err, EditError::Read { .. } | EditError::Write { .. }),
            "{err}"
        );

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["crisol.toml".to_owned()],
            "no temp file may remain"
        );
        assert!(target.join("inside.txt").is_file());
    }

    #[test]
    fn a_successful_save_leaves_only_the_target() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, COMMENTED);
        let mut editor = TomlEditor::open(&path).unwrap();
        editor.set(&["defaults", "php"], "8.3").unwrap();
        editor.save().unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["crisol.toml".to_owned()]);
    }

    #[test]
    fn open_config_refuses_to_write_an_invalid_configuration() {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path()).unwrap();
        fs::write(paths.config_file(), COMMENTED).unwrap();

        let mut editor = TomlEditor::open_config(&paths).unwrap();
        editor.set(&["sites", "tld"], "Not A Domain").unwrap();
        let err = editor.save().unwrap_err();
        assert!(
            matches!(&err, EditError::Rejected(m) if m.contains("sites.tld")),
            "{err}"
        );
        assert_eq!(fs::read_to_string(paths.config_file()).unwrap(), COMMENTED);

        let mut editor = TomlEditor::open_config(&paths).unwrap();
        editor
            .set(&["services", "redis", "port"], 70000_i64)
            .unwrap();
        assert!(matches!(editor.save().unwrap_err(), EditError::Rejected(_)));
    }

    #[test]
    fn open_config_accepts_a_valid_change() {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path()).unwrap();
        let mut editor = TomlEditor::open_config(&paths).unwrap();
        editor.set(&["sites", "tld"], "localhost").unwrap();
        assert!(editor.save().unwrap());
        let loaded = Config::load_from(&paths).unwrap();
        assert_eq!(loaded.config.sites.tld, "localhost");
    }

    #[test]
    fn the_project_file_is_created_and_validated() {
        let dir = TempDir::new().unwrap();

        let mut editor = TomlEditor::open_project(dir.path()).unwrap();
        editor.set(&["php"], "../etc").unwrap();
        assert!(matches!(editor.save().unwrap_err(), EditError::Rejected(_)));
        assert!(!dir.path().join(PROJECT_FILE).exists());

        editor.set(&["php"], "8.4").unwrap();
        assert!(editor.save().unwrap());
        let loaded = crate::project::load_project(dir.path(), Some(dir.path()))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.config.php.as_deref(), Some("8.4"));

        // Changing the pin later keeps the user's comments.
        let file = dir.path().join(PROJECT_FILE);
        fs::write(
            &file,
            "# pinned for the client\nphp = \"8.4\" # LTS\nnode = \"22\"\n",
        )
        .unwrap();
        let mut editor = TomlEditor::open_project(dir.path()).unwrap();
        editor.set(&["php"], "8.3").unwrap();
        editor.save().unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "# pinned for the client\nphp = \"8.3\" # LTS\nnode = \"22\"\n"
        );
    }

    #[test]
    fn sites_file_uses_its_own_path() {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path()).unwrap();
        let editor = TomlEditor::open_sites(&paths).unwrap();
        assert_eq!(editor.path(), paths.sites_file());
    }
}
