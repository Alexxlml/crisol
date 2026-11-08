//! The words of the GUI's own text, in the two languages of ADR-0026 §4.
//!
//! One dictionary per language (`ui/i18n/<code>.json`), shared by the page and by the native
//! parts (the tray menu and the message box), so a word is translated once. What the core says
//! is not in them: it is shown as it comes.

use std::collections::BTreeMap;

use crisol_client::Language;
use serde::Serialize;

const ENGLISH: &str = include_str!("../ui/i18n/en.json");
const SPANISH: &str = include_str!("../ui/i18n/es.json");

/// The words of one language.
#[derive(Debug, Clone, Serialize)]
pub struct Dictionary {
    /// The language they are in.
    pub language: Language,
    words: BTreeMap<String, String>,
}

impl Dictionary {
    /// The dictionary of `language`.
    #[must_use]
    pub fn of(language: Language) -> Self {
        let text = match language {
            Language::English => ENGLISH,
            Language::Spanish => SPANISH,
        };
        Self {
            language,
            // The files are embedded and checked by the tests: a bad one fails the build's tests.
            words: serde_json::from_str(text).unwrap_or_default(),
        }
    }

    /// The word for `key`. A key that is not there shows as itself, which is ugly enough to be
    /// seen and never empty.
    #[must_use]
    pub fn get(&self, key: &str) -> String {
        self.words
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }

    /// The word for `key` with each `{name}` replaced by its value.
    #[must_use]
    pub fn with(&self, key: &str, values: &[(&str, &str)]) -> String {
        let mut text = self.get(key);
        for (name, value) in values {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    fn keys(language: Language) -> BTreeSet<String> {
        Dictionary::of(language).words.into_keys().collect()
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        text.split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name.to_owned()))
            .collect()
    }

    #[test]
    fn both_files_are_valid_and_say_the_same_things() {
        assert!(serde_json::from_str::<BTreeMap<String, String>>(ENGLISH).is_ok());
        assert!(serde_json::from_str::<BTreeMap<String, String>>(SPANISH).is_ok());
        let (english, spanish) = (keys(Language::English), keys(Language::Spanish));
        assert!(!english.is_empty());
        assert_eq!(
            english.difference(&spanish).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "keys only in English"
        );
        assert_eq!(
            spanish.difference(&english).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "keys only in Spanish"
        );
    }

    #[test]
    fn no_word_is_empty_and_the_placeholders_match() {
        let (english, spanish) = (
            Dictionary::of(Language::English),
            Dictionary::of(Language::Spanish),
        );
        for (key, word) in &english.words {
            assert!(!word.trim().is_empty(), "{key} is empty in English");
            let other = &spanish.words[key];
            assert!(!other.trim().is_empty(), "{key} is empty in Spanish");
            assert_eq!(placeholders(word), placeholders(other), "{key}");
        }
    }

    /// Every key the page and the Rust code ask for exists, so no screen shows a bare key.
    #[test]
    fn every_key_in_use_is_in_the_dictionary() {
        let known = keys(Language::English);
        let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
        let mut used = BTreeSet::new();
        for file in ["index.html", "app.js"] {
            let text = std::fs::read_to_string(ui.join(file)).expect("page file");
            // `data-i18n="key"`, `data-i18n-label="key"` and `t("key"`.
            for marker in ["data-i18n=\"", "data-i18n-label=\"", "t(\"", "t('"] {
                for part in text.split(marker).skip(1) {
                    if let Some(key) = part.split(['"', '\'']).next() {
                        // A prefix the page completes at run time (`section.` + name).
                        let is_key = key.chars().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || "_.".contains(c)
                        });
                        if is_key && !key.is_empty() && !key.ends_with('.') {
                            used.insert(key.to_owned());
                        }
                    }
                }
            }
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(source).expect("src") {
            let text = std::fs::read_to_string(entry.expect("entry").path()).expect("source");
            for part in text.split("words.get(\"").skip(1) {
                used.insert(part.split('"').next().unwrap_or_default().to_owned());
            }
            for part in text.split("words.with(\"").skip(1) {
                used.insert(part.split('"').next().unwrap_or_default().to_owned());
            }
        }
        assert!(used.len() > 10, "the scan found {} keys", used.len());
        // `section.*` and `nav.*` keys are built from the section name in the page.
        for section in [
            "services",
            "sites",
            "versions",
            "logs",
            "doctor",
            "credentials",
            "setup",
            "settings",
        ] {
            used.insert(format!("nav.{section}"));
            used.insert(format!("section.{section}.title"));
            used.insert(format!("section.{section}.hint"));
        }
        // The buttons are named from what the app offers (`row.actions`, `site.actions`).
        for action in [
            "start",
            "stop",
            "restart",
            "enable",
            "disable",
            "start_all",
            "stop_all",
            "secure",
            "unsecure",
            "unlink",
            "install",
            "uninstall",
            "use",
        ] {
            used.insert(format!("action.{action}"));
        }
        // The steps of the setup and uninstall screens, named by their `id` (`"step." + id`).
        for id in crisol_client::SETUP_STEP_IDS
            .iter()
            .chain(&crisol_client::UNINSTALL_STEP_IDS)
        {
            used.insert(format!("step.{id}"));
        }
        for kind in ["parked", "linked"] {
            used.insert(format!("sites.kind.{kind}"));
        }
        // Chosen at run time from what the daemon says.
        for state in ["running", "starting", "stopping", "stopped", "failed"] {
            used.insert(format!("state.{state}"));
        }
        for overall in ["running", "starting", "stopped", "failed", "no_daemon"] {
            used.insert(format!("overall.{overall}"));
        }
        let missing: Vec<_> = used.difference(&known).collect();
        assert!(missing.is_empty(), "keys with no word: {missing:?}");
    }

    #[test]
    fn the_states_of_adr_0027_are_named_as_the_adr_says() {
        let (en, es) = (
            Dictionary::of(Language::English),
            Dictionary::of(Language::Spanish),
        );
        for (key, english, spanish) in [
            ("state.running", "Running", "Corriendo"),
            ("state.starting", "Starting", "Arrancando"),
            ("state.stopping", "Stopping", "Deteniendo"),
            ("state.stopped", "Stopped", "Parado"),
            ("state.failed", "Failed", "Falló"),
            ("overall.failed", "Something failed", "Algo falló"),
            ("overall.no_daemon", "Daemon not running", "Sin daemon"),
        ] {
            assert_eq!(en.get(key), english);
            assert_eq!(es.get(key), spanish);
        }
    }

    #[test]
    fn a_missing_key_shows_as_itself_and_placeholders_are_filled() {
        let en = Dictionary::of(Language::English);
        assert_eq!(en.get("no.such.key"), "no.such.key");
        assert_eq!(
            en.with("conn.connected", &[("version", "0.1.0")]),
            "crisold 0.1.0"
        );
    }
}
