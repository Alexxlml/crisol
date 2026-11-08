//! What the GUI remembers between runs. It is not Crisol's configuration (`crisol.toml`): it
//! lives in `state/gui.json`, inside the home, so a test home isolates it and a purge deletes
//! it (ADR-0026 §2).

use std::path::{Path, PathBuf};

use crisol_core::paths::{CrisolPaths, PrivateDirs};
use serde::{Deserialize, Serialize};

/// The memory of the GUI.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    /// The one-time notice "the services go on" was already shown on leaving.
    pub exit_notice_shown: bool,
    /// The setup screen already came up by itself once. After that it is a banner the person
    /// can follow or ignore, not a window that opens at every start (F13.8).
    pub setup_offered: bool,
}

impl GuiSettings {
    /// Where the file lives.
    #[must_use]
    pub fn file(paths: &CrisolPaths) -> PathBuf {
        paths.state_dir().join("gui.json")
    }

    /// Whether the setup screen opens by itself now: there is something to set up and nothing
    /// stops it, and it did not open before.
    #[must_use]
    pub fn offers_setup(&self, view: &crate::SetupView) -> bool {
        view.can_run() && !self.setup_offered
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

    /// Writes it (next to the final file first, then renamed over it). The window opens before
    /// anything is set up, so this can be the first thing written in the home: a home that is
    /// not there yet is made private first.
    pub fn save(&self, paths: &CrisolPaths, dirs: &dyn PrivateDirs) -> std::io::Result<()> {
        paths.create_private_home(dirs)?;
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
    use crisol_core::paths::fake::FakePrivateDirs;

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
            setup_offered: true,
        };
        let dirs = FakePrivateDirs::new();
        settings.save(&paths, &dirs).expect("saved");
        assert_eq!(GuiSettings::load(&paths), settings);
        // The home was not there: it was made private, not inherited from the folder above.
        assert_eq!(dirs.created(), [paths.home().to_path_buf()]);
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

    #[test]
    fn the_setup_opens_by_itself_only_once_and_only_when_it_can_run() {
        use crate::{SetupProblem, SetupStep, SetupView};
        let step = SetupStep {
            id: "path",
            description: "put it in the PATH".to_owned(),
            optional: false,
            on_by_default: false,
        };
        let pending = SetupView {
            steps: vec![step],
            done: Vec::new(),
            problems: Vec::new(),
            notes: Vec::new(),
            incomplete: true,
        };
        let blocked = SetupView {
            problems: vec![SetupProblem {
                message: "no".to_owned(),
                fix: None,
            }],
            ..pending.clone()
        };
        let finished = SetupView {
            steps: Vec::new(),
            ..pending.clone()
        };
        let fresh = GuiSettings::default();
        assert!(fresh.offers_setup(&pending));
        assert!(!fresh.offers_setup(&blocked));
        assert!(!fresh.offers_setup(&finished));
        let told = GuiSettings {
            setup_offered: true,
            ..GuiSettings::default()
        };
        assert!(!told.offers_setup(&pending));
    }
}
