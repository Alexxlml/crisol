//! What the GUI remembers between runs. It is not Crisol's configuration (`crisol.toml`): it
//! lives in `state/gui.json`, inside the home, so a test home isolates it and a purge deletes
//! it (ADR-0026 §2).

use std::path::{Path, PathBuf};

use crisol_core::paths::CrisolPaths;
use serde::{Deserialize, Serialize};

/// The memory of the GUI.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    /// The one-time notice "the services go on" was already shown on leaving.
    pub exit_notice_shown: bool,
}

impl GuiSettings {
    /// Where the file lives.
    #[must_use]
    pub fn file(paths: &CrisolPaths) -> PathBuf {
        paths.state_dir().join("gui.json")
    }

    /// Reads it. A missing or damaged file is the defaults: this is a convenience, never a
    /// reason not to open.
    #[must_use]
    pub fn load(paths: &CrisolPaths) -> Self {
        Self::load_from(&Self::file(paths))
    }

    fn load_from(file: &Path) -> Self {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes it (next to the final file first, then renamed over it).
    pub fn save(&self, paths: &CrisolPaths) -> std::io::Result<()> {
        let file = Self::file(paths);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary = file.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temporary, &file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &tempfile::TempDir) -> CrisolPaths {
        CrisolPaths::new(dir.path().join("home")).expect("a home")
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(GuiSettings::load(&paths(&dir)), GuiSettings::default());
        assert!(!GuiSettings::default().exit_notice_shown);
    }

    #[test]
    fn what_is_saved_is_read_back() {
        let dir = tempfile::tempdir().expect("dir");
        let paths = paths(&dir);
        let settings = GuiSettings {
            exit_notice_shown: true,
        };
        settings.save(&paths).expect("saved");
        assert_eq!(GuiSettings::load(&paths), settings);
        assert!(!GuiSettings::file(&paths)
            .with_extension("json.tmp")
            .exists());
    }

    #[test]
    fn a_damaged_file_is_the_defaults_and_is_not_deleted() {
        let dir = tempfile::tempdir().expect("dir");
        let paths = paths(&dir);
        let file = GuiSettings::file(&paths);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("dir");
        std::fs::write(&file, "{ not json").expect("written");
        assert_eq!(GuiSettings::load(&paths), GuiSettings::default());
        assert!(file.exists());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = tempfile::tempdir().expect("dir");
        let paths = paths(&dir);
        let file = GuiSettings::file(&paths);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("dir");
        std::fs::write(&file, r#"{"exit_notice_shown":true,"later":1}"#).expect("written");
        assert!(GuiSettings::load(&paths).exit_notice_shown);
    }
}
