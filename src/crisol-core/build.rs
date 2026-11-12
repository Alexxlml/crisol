//! Lists the languages of the messages of the core (ADR-0030 §2): every `i18n/<code>.json` is
//! one. Adding a language is adding a file; this script finds it.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("i18n");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut codes: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "json" {
                return None;
            }
            Some(path.file_stem()?.to_str()?.to_owned())
        })
        .collect();
    // English first: it is the language everything falls back to.
    codes.sort_by_key(|code| (code != "en", code.clone()));
    assert!(
        codes.first().is_some_and(|c| c == "en"),
        "i18n/en.json is missing"
    );

    let mut out = String::from("/// Every language of the messages, as (code, embedded JSON).\n");
    out.push_str("pub(crate) const DICTIONARIES: &[(&str, &str)] = &[\n");
    for code in &codes {
        let file = dir.join(format!("{code}.json"));
        println!("cargo:rerun-if-changed={}", file.display());
        let _ = writeln!(
            out,
            "    ({code:?}, include_str!({:?})),",
            file.display().to_string()
        );
    }
    out.push_str("];\n");
    let target = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("languages.rs");
    std::fs::write(target, out).expect("write languages.rs");
}
