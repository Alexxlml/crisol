//! What the core says, as a key with data (ADR-0030).
//!
//! The core does not write finished sentences for the user: it writes a [`Message`], a key of
//! the dictionaries in `i18n/<code>.json` plus the data that fills the `{placeholders}` of its
//! sentence. Whoever shows it chooses the language: the English dictionary is built into the
//! core, so `to_string()` of a message is its English sentence, as it always was.
//!
//! What is never translated goes in as data, not in the sentence: commands, paths, versions,
//! names of services and sites, addresses and codes.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use crisol_protocol::{Check, CheckAction, CheckStatus, ErrorCode, RpcError, Text, TextArg};
use serde::{Serialize, Serializer};

use crate::msg;

include!(concat!(env!("OUT_DIR"), "/languages.rs"));

/// The environment variable that forces a language (`en`, `es`, …), for the one who needs the
/// output in English to paste it in an issue.
pub const LANGUAGE_VARIABLE: &str = "CRISOL_LANG";

/// A language the messages are translated into: the code of one of the `i18n/*.json` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Language {
    code: &'static str,
}

impl Default for Language {
    fn default() -> Self {
        Self::ENGLISH
    }
}

impl Language {
    /// English: the default, and the language every other one falls back to.
    pub const ENGLISH: Self = Self { code: "en" };
    /// Spanish, in any of its regions.
    pub const SPANISH: Self = Self { code: "es" };

    /// Every language there is a dictionary for, English first.
    pub fn all() -> impl Iterator<Item = Self> {
        DICTIONARIES.iter().map(|(code, _)| Self { code })
    }

    /// The language code (`en`, `es`) the dictionaries are named by.
    #[must_use]
    pub fn code(self) -> &'static str {
        self.code
    }

    /// The language a code or a BCP 47 tag names (`es`, `ES`, `es-MX`, `es_AR`), if there is a
    /// dictionary for it.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        let primary = tag.trim().split(['-', '_']).next()?.to_ascii_lowercase();
        Self::all().find(|language| language.code == primary)
    }

    /// The first of the user's preferred languages (BCP 47 tags such as `es-MX` or `en-US`,
    /// most wanted first) that has a dictionary. English when none has.
    #[must_use]
    pub fn from_preferences<S: AsRef<str>>(tags: impl IntoIterator<Item = S>) -> Self {
        choose(None, tags)
    }

    /// The words of this language.
    fn words(self) -> &'static BTreeMap<String, String> {
        static PARSED: OnceLock<Vec<(&'static str, BTreeMap<String, String>)>> = OnceLock::new();
        let parsed = PARSED.get_or_init(|| {
            DICTIONARIES
                .iter()
                .map(|(code, text)| {
                    // The files are embedded and checked by the tests: a bad one fails them.
                    (*code, serde_json::from_str(text).unwrap_or_default())
                })
                .collect()
        });
        parsed
            .iter()
            .find(|(code, _)| *code == self.code)
            .map(|(_, words)| words)
            .expect("a Language is made from the list of dictionaries")
    }
}

impl Serialize for Language {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code)
    }
}

/// Which language to use: `CRISOL_LANG` if it names one there is a dictionary for, else the
/// first of the preferred languages of Windows that has one, else English. A pure function: the
/// caller reads the environment and Windows.
#[must_use]
pub fn choose<S: AsRef<str>>(
    forced: Option<&str>,
    preferred: impl IntoIterator<Item = S>,
) -> Language {
    forced
        .and_then(Language::from_tag)
        .or_else(|| {
            preferred
                .into_iter()
                .find_map(|tag| Language::from_tag(tag.as_ref()))
        })
        .unwrap_or_default()
}

/// A sentence of the core: the key of the dictionary and its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    key: Cow<'static, str>,
    args: Vec<(Cow<'static, str>, Arg)>,
}

/// One piece of data of a [`Message`]: text already formatted, or another sentence that is said
/// in the same language (the error inside "could not change your user PATH: {error}").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    /// Text that is never translated: a path, a version, a number, a command.
    Plain(String),
    /// A sentence of its own.
    Message(Message),
}

impl From<Message> for Arg {
    fn from(message: Message) -> Self {
        Self::Message(message)
    }
}

impl From<&Message> for Arg {
    fn from(message: &Message) -> Self {
        Self::Message(message.clone())
    }
}

/// An error that came as an answer: its key and data when it has them, else its sentence.
impl From<&RpcError> for Arg {
    fn from(error: &RpcError) -> Self {
        error.text.as_deref().map_or_else(
            || Self::Plain(error.message.clone()),
            |text| Self::Message(Message::from(text)),
        )
    }
}

impl Message {
    /// A message with no data. Use [`msg!`](crate::msg) instead, which the tests check.
    #[doc(hidden)]
    #[must_use]
    pub fn new(key: &'static str) -> Self {
        Self {
            key: Cow::Borrowed(key),
            args: Vec::new(),
        }
    }

    /// With one piece of data (already formatted text: a path as `display()`, a number).
    #[doc(hidden)]
    #[must_use]
    pub fn arg(mut self, name: &'static str, value: impl fmt::Display) -> Self {
        self.args
            .push((Cow::Borrowed(name), Arg::Plain(value.to_string())));
        self
    }

    /// With one piece of data that is a sentence of its own (`msg!("x", error => inner)`).
    #[doc(hidden)]
    #[must_use]
    pub fn nest(mut self, name: &'static str, value: impl Into<Arg>) -> Self {
        self.args.push((Cow::Borrowed(name), value.into()));
        self
    }

    /// Whether the English sentence has `text` in it. For checking what a message says without
    /// building its sentence first.
    #[must_use]
    pub fn contains(&self, text: &str) -> bool {
        self.to_string().contains(text)
    }

    /// Whether the English sentence begins with `text`.
    #[must_use]
    pub fn starts_with(&self, text: &str) -> bool {
        self.to_string().starts_with(text)
    }

    /// The key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The data, by placeholder name.
    #[must_use]
    pub fn args(&self) -> &[(Cow<'static, str>, Arg)] {
        &self.args
    }

    /// The sentence in `language`: the English one when that language lacks the key, and each
    /// nested sentence in the same language. A key that no dictionary has (a bug the tests
    /// catch) shows as itself.
    #[must_use]
    pub fn render(&self, language: Language) -> String {
        let values: Vec<String> = self
            .args
            .iter()
            .map(|(_, arg)| match arg {
                Arg::Plain(text) => text.clone(),
                Arg::Message(message) => message.render(language),
            })
            .collect();
        let args: Vec<(&str, &str)> = self
            .args
            .iter()
            .zip(&values)
            .map(|((name, _), value)| (name.as_ref(), value.as_str()))
            .collect();
        translate(language, &self.key, &args).unwrap_or_else(|| self.key.to_string())
    }

    /// As the protocol carries it.
    #[must_use]
    pub fn to_text(&self) -> Text {
        Text {
            key: self.key.to_string(),
            args: self
                .args
                .iter()
                .map(|(name, arg)| {
                    let value = match arg {
                        Arg::Plain(text) => TextArg::Plain(text.clone()),
                        Arg::Message(message) => TextArg::Text(message.to_text()),
                    };
                    (name.to_string(), value)
                })
                .collect(),
        }
    }
}

/// The English sentence: what an error printed before this existed.
impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(Language::ENGLISH))
    }
}

/// In JSON a message is its English sentence, so `--json` does not change (ADR-0030 §5).
impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl From<&Message> for Text {
    fn from(message: &Message) -> Self {
        message.to_text()
    }
}

/// A text that came in the protocol, as a message.
impl From<&Text> for Message {
    fn from(text: &Text) -> Self {
        Self {
            key: Cow::Owned(text.key.clone()),
            args: text
                .args
                .iter()
                .map(|(name, value)| {
                    let arg = match value {
                        TextArg::Plain(text) => Arg::Plain(text.clone()),
                        TextArg::Text(text) => Arg::Message(Self::from(text)),
                    };
                    (Cow::Owned(name.clone()), arg)
                })
                .collect(),
        }
    }
}

/// An error answer that says `message` in English in `message` and as a key and data in `text`.
#[must_use]
pub fn rpc_error(code: ErrorCode, message: &Message) -> RpcError {
    RpcError::new(code, message.to_string()).with_text(message.to_text())
}

/// What can say what it means as a [`Message`] (an error, a step of a plan).
pub trait Localized {
    /// The message, with its data.
    fn message(&self) -> Message;
}

/// What a finding of `doctor` says to do about it.
#[derive(Debug, Clone)]
pub enum Fix {
    /// An exact command, or any text that is not translated.
    Command(String),
    /// A sentence of its own.
    Say(Message),
}

/// A finding of `doctor` made of messages: the English sentences (what `--json` always had) next
/// to their keys and data (ADR-0030 §4). `name` is also the identifier of the row, in English.
#[must_use]
pub fn finding(
    status: CheckStatus,
    name: &Message,
    detail: &Message,
    fix: Option<Fix>,
    action: Option<CheckAction>,
) -> Check {
    let (fix, fix_text) = match fix {
        None => (None, None),
        Some(Fix::Command(command)) => (Some(command), None),
        Some(Fix::Say(message)) => (Some(message.to_string()), Some(message.to_text())),
    };
    Check {
        name: name.to_string(),
        status,
        detail: detail.to_string(),
        fix,
        action,
        name_text: Some(name.to_text()),
        detail_text: Some(detail.to_text()),
        fix_text,
    }
}

/// `Display` for a type that is [`Localized`]: its message in English, what an error printed
/// before messages had keys.
#[macro_export]
macro_rules! display_as_message {
    ($type:ty) => {
        impl ::std::fmt::Display for $type {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&$crate::i18n::Localized::message(self), f)
            }
        }
    };
}

impl Message {
    /// `first` followed by each of `rest`, one after the other with "; " in between (the lines
    /// of a report). The separator is in the dictionary (`i18n.join`), not in the code.
    #[must_use]
    pub fn joined(first: Self, rest: Vec<Self>) -> Self {
        let join = |head: Self, tail: Self| msg!("i18n.join", first => head, rest => tail);
        let mut rest = rest.into_iter().rev();
        let Some(last) = rest.next() else {
            return first;
        };
        join(first, rest.fold(last, |tail, head| join(head, tail)))
    }
}

/// `items` one after the other with "and" in between (`a and b and c`), the word in the
/// dictionary (`i18n.and`). Each item is shown as it is.
#[must_use]
pub fn and_list(items: &[String]) -> Message {
    let mut items = items.iter().rev();
    let Some(last) = items.next() else {
        return msg!("i18n.text", text = "");
    };
    items.fold(
        msg!("i18n.text", text = last),
        |tail, head| msg!("i18n.and", first = head, rest => tail),
    )
}

/// Makes a [`Message`]: `msg!("doctor.ca.stale", names = list)`. A datum given with `=` is text
/// (anything with `Display`); one given with `=>` is a sentence of its own, said in the same
/// language: `msg!("setup.path_failed", error => error.message())` (a [`Message`], or an
/// [`RpcError`] that came as an answer). The key must be in `i18n/en.json` and the names must be
/// exactly its placeholders; a test reads every use in the source and fails if not.
#[macro_export]
macro_rules! msg {
    (@args $message:expr;) => {
        $message
    };
    (@args $message:expr; $name:ident = $value:expr $(, $($rest:tt)*)?) => {
        $crate::msg!(@args $message.arg(stringify!($name), $value); $($($rest)*)?)
    };
    (@args $message:expr; $name:ident => $value:expr $(, $($rest:tt)*)?) => {
        $crate::msg!(@args $message.nest(stringify!($name), $value); $($($rest)*)?)
    };
    ($key:literal $(,)?) => {
        $crate::i18n::Message::new($key)
    };
    ($key:literal, $($rest:tt)+) => {
        $crate::msg!(@args $crate::i18n::Message::new($key); $($rest)+)
    };
}

/// The sentence for `key` in `language`, with `args` in its placeholders. `None` when neither
/// that language nor English has the key; a key in English only is said in English (ADR-0030 §4).
///
/// A plural has two keys, `<key>_one` and `<key>_other`, and is picked by the datum `count`.
#[must_use]
pub fn translate(language: Language, key: &str, args: &[(&str, &str)]) -> Option<String> {
    let count = args
        .iter()
        .find(|(name, _)| *name == "count")
        .map(|(_, value)| *value);
    [language, Language::ENGLISH]
        .into_iter()
        .find_map(|language| {
            let template = pick(language.words(), key, count)?;
            Some(fill(template, args))
        })
}

/// The template for `key` in one dictionary: the plural form when `count` is given and the
/// dictionary has the plural keys, else the key itself.
fn pick<'a>(
    words: &'a BTreeMap<String, String>,
    key: &str,
    count: Option<&str>,
) -> Option<&'a str> {
    if let Some(count) = count {
        let form = if count == "1" { "one" } else { "other" };
        if let Some(template) = words.get(&format!("{key}_{form}")) {
            return Some(template);
        }
    }
    words.get(key).map(String::as_str)
}

/// `template` with each `{name}` replaced by its value, in one pass: a value that looks like a
/// placeholder is not replaced again. A `{name}` with no datum stays as it is.
fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.split_once('}') {
            Some((name, tail)) if args.iter().any(|(n, _)| *n == name) => {
                let value = args
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map_or("", |(_, v)| *v);
                out.push_str(value);
                rest = tail;
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `{placeholders}` of a sentence, in order of appearance.
#[must_use]
pub fn placeholders(template: &str) -> Vec<String> {
    template
        .split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name.to_owned()))
        .collect()
}

/// Checks that `message` can be said in every language there is: no key shown instead of a
/// sentence and no `{datum}` left unfilled (ADR-0030, test 3).
#[cfg(test)]
pub(crate) fn assert_says_everywhere(message: &Message) {
    for language in Language::all() {
        let text = message.render(language);
        assert!(
            !text.is_empty() && text != message.key() && !text.contains('{'),
            "{} says `{text}` for `{}`",
            language.code(),
            message.key()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    // The languages ---------------------------------------------------------------------

    #[test]
    fn english_comes_first_and_spanish_is_there() {
        let all: Vec<_> = Language::all().collect();
        assert_eq!(all[0], Language::ENGLISH);
        assert!(all.contains(&Language::SPANISH));
    }

    #[test]
    fn a_tag_names_its_language_in_any_region_and_case() {
        for tag in ["es", "ES", "es-MX", "es_AR", "ES-es", " es "] {
            assert_eq!(Language::from_tag(tag), Some(Language::SPANISH), "{tag}");
        }
        assert_eq!(Language::from_tag("en-GB"), Some(Language::ENGLISH));
        assert_eq!(Language::from_tag("de-DE"), None);
        assert_eq!(Language::from_tag(""), None);
        // A language that only starts with the letters is not Spanish.
        assert_eq!(Language::from_tag("est-EE"), None);
    }

    #[test]
    fn the_variable_forces_the_language_when_it_names_one() {
        let windows = ["es-MX"];
        assert_eq!(choose(Some("en"), windows), Language::ENGLISH);
        assert_eq!(choose(Some("EN"), windows), Language::ENGLISH);
        assert_eq!(choose(Some("es"), ["en-US"]), Language::SPANISH);
    }

    #[test]
    fn an_unknown_or_empty_variable_is_ignored() {
        assert_eq!(choose(Some("xx"), ["es-MX"]), Language::SPANISH);
        assert_eq!(choose(Some(""), ["es-MX"]), Language::SPANISH);
        assert_eq!(choose(Some("  "), ["es-MX"]), Language::SPANISH);
        assert_eq!(choose(None, ["es-MX"]), Language::SPANISH);
    }

    #[test]
    fn the_first_preferred_language_with_a_dictionary_wins() {
        assert_eq!(choose(None, ["fr-FR", "es-MX", "en-US"]), Language::SPANISH);
        assert_eq!(choose(None, ["en-GB", "es-MX"]), Language::ENGLISH);
        assert_eq!(
            Language::from_preferences(["de-DE", "es"]),
            Language::SPANISH
        );
    }

    #[test]
    fn with_nothing_to_go_by_it_is_english() {
        assert_eq!(choose(None, ["de-DE"]), Language::ENGLISH);
        assert_eq!(choose(None, Vec::<String>::new()), Language::ENGLISH);
        assert_eq!(choose(None, [""]), Language::ENGLISH);
        assert_eq!(Language::default(), Language::ENGLISH);
    }

    #[test]
    fn a_language_is_its_code_in_json() {
        assert_eq!(Language::SPANISH.code(), "es");
        assert_eq!(serde_json::to_value(Language::SPANISH).expect("json"), "es");
    }

    // The sentences ---------------------------------------------------------------------

    #[test]
    fn placeholders_are_filled_in_one_pass() {
        assert_eq!(
            fill("{a} then {b}", &[("a", "{b}"), ("b", "2")]),
            "{b} then 2"
        );
        // A brace that is not a placeholder, and a placeholder with no datum, stay.
        assert_eq!(fill("{ {x} {", &[("y", "1")]), "{ {x} {");
        assert_eq!(fill("no data", &[]), "no data");
    }

    #[test]
    fn placeholders_are_listed_in_order() {
        assert_eq!(placeholders("{b} and {a} and {b}"), ["b", "a", "b"]);
        assert_eq!(placeholders("none").len(), 0);
    }

    #[test]
    fn a_plural_is_picked_by_count() {
        let words = map(&[("x_one", "{count} file"), ("x_other", "{count} files")]);
        assert_eq!(pick(&words, "x", Some("1")), Some("{count} file"));
        assert_eq!(pick(&words, "x", Some("0")), Some("{count} files"));
        assert_eq!(pick(&words, "x", Some("2")), Some("{count} files"));
        // Without `count` there is no plural to pick.
        assert_eq!(pick(&words, "x", None), None);
        // A key that is not plural is used whatever `count` is.
        let plain = map(&[("y", "{count} things")]);
        assert_eq!(pick(&plain, "y", Some("3")), Some("{count} things"));
    }

    #[test]
    fn a_message_is_its_english_sentence_and_says_it_in_another_language() {
        let message = msg!("i18n.sample.running", services = "nginx, php");
        assert_eq!(message.to_string(), "running: nginx, php");
        assert_eq!(message.render(Language::SPANISH), "corriendo: nginx, php");
        assert_eq!(
            serde_json::to_value(&message).expect("json"),
            "running: nginx, php"
        );
        let text = message.to_text();
        assert_eq!(text.key, "i18n.sample.running");
        assert_eq!(text.args["services"], "nginx, php");
    }

    #[test]
    fn a_plural_message_follows_its_count() {
        let one = msg!("i18n.sample.sites", count = 1);
        let many = msg!("i18n.sample.sites", count = 3);
        assert_eq!(one.to_string(), "1 site");
        assert_eq!(many.to_string(), "3 sites");
        assert_eq!(many.render(Language::SPANISH), "3 sitios");
        assert_eq!(one.render(Language::SPANISH), "1 sitio");
    }

    #[test]
    fn a_nested_message_is_said_in_the_same_language() {
        let inner = msg!("i18n.sample.sites", count = 2);
        let outer = msg!("i18n.sample.running", services => &inner);
        assert_eq!(outer.to_string(), "running: 2 sites");
        assert_eq!(outer.render(Language::SPANISH), "corriendo: 2 sitios");
        // English in JSON, as every message.
        assert_eq!(
            serde_json::to_value(&outer).expect("json"),
            "running: 2 sites"
        );
    }

    #[test]
    fn a_nested_message_travels_as_a_nested_text_and_comes_back() {
        let outer = msg!(
            "i18n.sample.running",
            services => msg!("i18n.sample.sites", count = 1)
        );
        let text = outer.to_text();
        assert_eq!(
            serde_json::to_value(&text).expect("json"),
            serde_json::json!({
                "key": "i18n.sample.running",
                "args": {"services": {"key": "i18n.sample.sites", "args": {"count": "1"}}}
            })
        );
        assert_eq!(Message::from(&text), outer.clone());
        assert_eq!(
            Message::from(&text).render(Language::SPANISH),
            "corriendo: 1 sitio"
        );
    }

    #[test]
    fn an_answer_nests_its_text_or_else_its_sentence() {
        let with_text = rpc_error(ErrorCode::Io, &msg!("i18n.sample.sites", count = 3));
        let outer = msg!("i18n.sample.running", services => &with_text);
        assert_eq!(outer.render(Language::SPANISH), "corriendo: 3 sitios");

        // An error with no key yet is said as it came, in English.
        let plain = RpcError::new(ErrorCode::Io, "disk full");
        let outer = msg!("i18n.sample.running", services => &plain);
        assert_eq!(outer.render(Language::SPANISH), "corriendo: disk full");
    }

    #[test]
    fn a_key_nobody_has_shows_as_itself() {
        assert_eq!(msg!("no.such.key").to_string(), "no.such.key");
        assert_eq!(translate(Language::SPANISH, "no.such.key", &[]), None);
    }
}
