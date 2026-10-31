//! The language of the GUI's own words (ADR-0026 §4): English or Spanish, by what Windows
//! prefers. What comes from the core is shown as it is, in English.

use serde::Serialize;

/// A language the GUI is translated into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// The default, and the language of the core's messages.
    #[default]
    English,
    /// Spanish, in any of its regions.
    Spanish,
}

impl Language {
    /// The language code (`en`, `es`) the dictionaries are named by.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Spanish => "es",
        }
    }

    /// The first of the user's preferred languages (BCP 47 tags such as `es-MX` or `en-US`,
    /// most wanted first) that the GUI speaks. English when none is.
    #[must_use]
    pub fn from_preferences<S: AsRef<str>>(tags: impl IntoIterator<Item = S>) -> Self {
        tags.into_iter()
            .find_map(|tag| {
                let primary = tag.as_ref().split(['-', '_']).next()?.to_ascii_lowercase();
                match primary.as_str() {
                    "es" => Some(Self::Spanish),
                    "en" => Some(Self::English),
                    _ => None,
                }
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spanish_in_any_region_is_spanish() {
        assert_eq!(Language::from_preferences(["es-MX"]), Language::Spanish);
        assert_eq!(Language::from_preferences(["es"]), Language::Spanish);
        assert_eq!(Language::from_preferences(["ES-es"]), Language::Spanish);
        assert_eq!(Language::from_preferences(["es_AR"]), Language::Spanish);
    }

    #[test]
    fn the_first_language_the_gui_speaks_wins() {
        assert_eq!(
            Language::from_preferences(["fr-FR", "es-MX", "en-US"]),
            Language::Spanish
        );
        assert_eq!(
            Language::from_preferences(["en-GB", "es-MX"]),
            Language::English
        );
    }

    #[test]
    fn what_is_not_spoken_is_english() {
        assert_eq!(Language::from_preferences(["de-DE"]), Language::English);
        assert_eq!(
            Language::from_preferences(Vec::<String>::new()),
            Language::English
        );
        assert_eq!(Language::from_preferences([""]), Language::English);
        // A language that only starts with the letters is not Spanish.
        assert_eq!(Language::from_preferences(["est-EE"]), Language::English);
    }

    #[test]
    fn the_codes_name_the_dictionaries() {
        assert_eq!(Language::English.code(), "en");
        assert_eq!(Language::Spanish.code(), "es");
        assert_eq!(
            serde_json::to_value(Language::Spanish).expect("json"),
            "spanish"
        );
    }
}
