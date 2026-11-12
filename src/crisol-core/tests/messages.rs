//! The dictionaries of the core and the code that uses them (ADR-0030, tests 1 and 2): no
//! language lacks a sentence or a piece of data, and no `msg!` names a key or a datum that is
//! not there.

// Test code: a failed `unwrap` or `expect` is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

type Words = BTreeMap<String, String>;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every language: its code and its words.
fn dictionaries() -> BTreeMap<String, Words> {
    let mut all = BTreeMap::new();
    for entry in std::fs::read_dir(manifest().join("i18n")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            let text = std::fs::read_to_string(&path).unwrap();
            let words: Words = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} is not an object of words: {e}", path.display()));
            let code = path.file_stem().unwrap().to_str().unwrap().to_owned();
            all.insert(code, words);
        }
    }
    all
}

fn placeholders(template: &str) -> BTreeSet<String> {
    crisol_core::i18n::placeholders(template)
        .into_iter()
        .collect()
}

#[test]
fn every_language_has_exactly_the_keys_of_english() {
    let all = dictionaries();
    let english = &all["en"];
    assert!(!english.is_empty());
    for (code, words) in &all {
        let missing: Vec<_> = english.keys().filter(|k| !words.contains_key(*k)).collect();
        let extra: Vec<_> = words.keys().filter(|k| !english.contains_key(*k)).collect();
        assert!(missing.is_empty(), "{code} lacks: {missing:?}");
        assert!(
            extra.is_empty(),
            "{code} has keys English has not: {extra:?}"
        );
    }
}

#[test]
fn every_sentence_has_the_same_data_in_every_language_and_none_is_empty() {
    let all = dictionaries();
    let english = &all["en"];
    let count = BTreeSet::from(["count".to_owned()]);
    for (code, words) in &all {
        for (key, template) in words {
            assert!(!template.trim().is_empty(), "{key} is empty in {code}");
            let (en, other) = (placeholders(&english[key]), placeholders(template));
            // A singular may leave `count` out of its sentence ("one site").
            if key.ends_with("_one") {
                assert_eq!(
                    en.difference(&count).collect::<Vec<_>>(),
                    other.difference(&count).collect::<Vec<_>>(),
                    "{key} in {code}"
                );
            } else {
                assert_eq!(en, other, "{key} in {code}");
            }
        }
    }
}

#[test]
fn plurals_come_in_pairs() {
    for (code, words) in dictionaries() {
        for key in words.keys() {
            if let Some(base) = key.strip_suffix("_one") {
                assert!(
                    words.contains_key(&format!("{base}_other")),
                    "{key} in {code}"
                );
            }
            if let Some(base) = key.strip_suffix("_other") {
                assert!(
                    words.contains_key(&format!("{base}_one")),
                    "{key} in {code}"
                );
            }
        }
    }
}

/// One use of `msg!` in the source.
#[derive(Debug)]
struct Use {
    file: PathBuf,
    key: String,
    names: BTreeSet<String>,
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The text of `source` with its comment lines blanked, so a `msg!` in documentation is not a use.
fn without_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The arguments of a call whose `(` is at `open`: the text of each, split at the commas that
/// are not inside a string or a bracket.
fn arguments(source: &str, open: usize) -> Vec<String> {
    let bytes = source.as_bytes();
    let (mut depth, mut args, mut start, mut i) = (0_usize, Vec::new(), open + 1, open);
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while bytes[i] != b'"' {
                    if bytes[i] == 0x5C {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    args.push(source[start..i].to_owned());
                    return args;
                }
            }
            b',' if depth == 1 => {
                args.push(source[start..i].to_owned());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    panic!("a msg! that is never closed");
}

fn uses() -> Vec<Use> {
    let root = manifest().parent().unwrap();
    let mut files = Vec::new();
    rust_files(root, &mut files);
    let own = manifest().join("src").join("i18n.rs");
    // This file names `msg!` in its own text.
    let this = manifest().join("tests").join("messages.rs");
    let mut found = Vec::new();
    for file in files {
        // The module of the messages explains `msg!` and tests it with keys of its own.
        if file == own || file == this {
            continue;
        }
        let source = without_comments(&std::fs::read_to_string(&file).unwrap());
        for (at, _) in source.match_indices("msg!(") {
            let args = arguments(&source, at + "msg!".len());
            assert!(
                args[0].trim().starts_with('"'),
                "{}: msg! must start with a literal key",
                file.display()
            );
            let key = args[0].trim().trim_matches('"').to_owned();
            let names = args[1..]
                .iter()
                .filter_map(|arg| {
                    let (name, _) = arg.split_once('=')?;
                    let name = name.trim();
                    (!name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                    .then(|| name.to_owned())
                })
                .collect();
            found.push(Use {
                file: file.clone(),
                key,
                names,
            });
        }
    }
    found
}

#[test]
fn the_scan_finds_the_uses_there_are() {
    // Guard of the guard: a scan that finds nothing would pass every other test.
    assert!(uses().len() >= 5, "{:?}", uses());
}

#[test]
fn every_key_in_use_exists_and_gets_exactly_its_data() {
    let all = dictionaries();
    let english = &all["en"];
    for used in uses() {
        let singular = english.get(&used.key);
        let (one, other) = (
            english.get(&format!("{}_one", used.key)),
            english.get(&format!("{}_other", used.key)),
        );
        let expected: BTreeSet<String> = match (singular, one, other) {
            (Some(template), None, None) => placeholders(template),
            // A plural is picked by `count`, which it must be given.
            (None, Some(one), Some(other)) => placeholders(one)
                .into_iter()
                .chain(placeholders(other))
                .chain(["count".to_owned()])
                .collect(),
            _ => panic!(
                "{}: `{}` is not a key of i18n/en.json",
                used.file.display(),
                used.key
            ),
        };
        assert_eq!(
            used.names,
            expected,
            "{}: the data of `{}` are not its placeholders",
            used.file.display(),
            used.key
        );
    }
}
