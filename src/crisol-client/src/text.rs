//! Saying what the daemon sent in the language of the user (ADR-0030 §4).
//!
//! The daemon writes its sentences in English and adds, next to each, a [`Text`]: a key and its
//! data. Here each sentence is replaced by the one of the user's language and the `*_text`
//! fields are emptied (all but `name_text` of a check, which names the row), so whoever
//! receives the reply (the CLI, the page) reads plain sentences and nothing else changes for it.
//! A key this language lacks is said in English; one that no dictionary has (a daemon newer than
//! this client) leaves the sentence the daemon sent, and so does a nested text with such a key.

use crisol_core::i18n::{translate, Language};
use crisol_protocol::{text_at, Check, DoneInfo, Reply, RpcError, StatusInfo, Text, TextArg};

/// `text` said in `language`, or `plain` when there is no text or no dictionary has its key.
#[must_use]
pub fn say(language: Language, text: Option<&Text>, plain: &str) -> String {
    text.and_then(|text| render(language, text))
        .unwrap_or_else(|| plain.to_owned())
}

/// `text` said in `language`, with each nested text said too. `None` when a dictionary lacks
/// any of the keys: then the whole English sentence is better than half of one.
fn render(language: Language, text: &Text) -> Option<String> {
    let values = text
        .args
        .values()
        .map(|value| match value {
            TextArg::Plain(plain) => Some(plain.clone()),
            TextArg::Text(nested) => render(language, nested),
        })
        .collect::<Option<Vec<String>>>()?;
    let args: Vec<(&str, &str)> = text
        .args
        .keys()
        .zip(&values)
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    translate(language, &text.key, &args)
}

/// The sentences of `warnings`, each said in `language`.
fn say_all(language: Language, warnings: &[String], texts: &[Option<Text>]) -> Vec<String> {
    warnings
        .iter()
        .enumerate()
        .map(|(index, plain)| say(language, text_at(texts, warnings.len(), index), plain))
        .collect()
}

/// Every sentence of `reply` in `language`.
pub fn localize_reply(reply: &mut Reply, language: Language) {
    match reply {
        Reply::Status(info) => localize_status(info, language),
        Reply::Done(info) => localize_done(info, language),
        Reply::Doctor { checks } => {
            for check in checks {
                localize_check(check, language);
            }
        }
        _ => {}
    }
}

fn localize_status(info: &mut StatusInfo, language: Language) {
    info.warnings = say_all(language, &info.warnings, &info.warnings_text);
    info.strip_texts();
}

fn localize_done(info: &mut DoneInfo, language: Language) {
    info.message = say(language, info.message_text.as_ref(), &info.message);
    info.warnings = say_all(language, &info.warnings, &info.warnings_text);
    info.strip_texts();
}

pub(crate) fn localize_check(check: &mut Check, language: Language) {
    // `name` stays: it identifies the row (ADR-0030 §6). Its `name_text` stays too, for whoever
    // shows the name of the row in the user's language.
    check.detail = say(language, check.detail_text.as_ref(), &check.detail);
    if let Some(fix) = &check.fix {
        // The fix is a command more often than a sentence; only a sentence has a text.
        check.fix = Some(say(language, check.fix_text.as_ref(), fix));
    }
    check.detail_text = None;
    check.fix_text = None;
}

/// The sentences of `error` in `language`.
pub fn localize_error(error: &mut RpcError, language: Language) {
    error.message = say(language, error.text.as_deref(), &error.message);
    if let Some(fix) = &error.fix {
        error.fix = Some(say(language, error.fix_text.as_deref(), fix));
    }
    error.strip_texts();
}

#[cfg(test)]
mod tests;
