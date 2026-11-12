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
    /// Data by placeholder name: text already formatted (a path, a version, a number), or a
    /// sentence of its own, said in the same language (the error inside another sentence).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, TextArg>,
}

/// One datum of a [`Text`]. In the JSON, a string or a `Text`. A client older than nested texts
/// cannot read the whole `Text` and shows the English sentence of the plain field instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TextArg {
    /// Text that is never translated.
    Plain(String),
    /// A sentence of its own.
    Text(Text),
}

impl From<String> for TextArg {
    fn from(text: String) -> Self {
        Self::Plain(text)
    }
}

impl From<&str> for TextArg {
    fn from(text: &str) -> Self {
        Self::Plain(text.to_owned())
    }
}

impl From<Text> for TextArg {
    fn from(text: Text) -> Self {
        Self::Text(text)
    }
}

/// So a test or a client can compare a datum with the text it should be.
impl PartialEq<&str> for TextArg {
    fn eq(&self, other: &&str) -> bool {
        matches!(self, Self::Plain(text) if text == other)
    }
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
    pub fn with(mut self, name: impl Into<String>, value: impl Into<TextArg>) -> Self {
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
