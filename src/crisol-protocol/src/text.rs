//! Sentences that a client can translate (ADR-0030 §4): a key and its data.
//!
//! The daemon always writes its sentence in English in the field it always had, and adds a
//! [`Text`] next to it in an optional `*_text` field. A client that knows the key says it in
//! its own language; one that does not (an older client, a newer daemon) shows the English.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A sentence as a key of the dictionaries and the data that fills its `{placeholders}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Text {
    /// The key of the sentence (`doctor.ca.stale`).
    pub key: String,
    /// Data by placeholder name. Always text already formatted: a path, a version, a number.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
}

impl Text {
    /// A text with no data.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            args: BTreeMap::new(),
        }
    }

    /// With one more piece of data.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.insert(name.into(), value.into());
        self
    }
}

/// Reads an optional [`Text`], and gives `None` when it is not one this version understands.
pub(crate) fn lenient_text<'de, D>(deserializer: D) -> Result<Option<Text>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Like [`lenient_text`], for a field that is boxed.
pub(crate) fn lenient_boxed_text<'de, D>(deserializer: D) -> Result<Option<Box<Text>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(lenient_text(deserializer)?.map(Box::new))
}

/// Reads a list of optional [`Text`]s; one that is not understood reads as `None`.
pub(crate) fn lenient_texts<'de, D>(deserializer: D) -> Result<Vec<Option<Text>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?;
    Ok(values
        .unwrap_or_default()
        .into_iter()
        .map(|value| serde_json::from_value(value).ok())
        .collect())
}

/// The text of position `index` in `texts`, if `texts` lines up with the `plain` sentences. A
/// list of another length is ignored as a whole (ADR-0030 §4).
#[must_use]
pub fn text_at(texts: &[Option<Text>], plain: usize, index: usize) -> Option<&Text> {
    if texts.len() == plain {
        texts.get(index)?.as_ref()
    } else {
        None
    }
}
