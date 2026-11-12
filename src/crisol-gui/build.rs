//! Brings the shared brand files next to the page before Tauri embeds it: the page imports
//! `tokens.css` and `symbol.svg`, whose one copy lives in `assets/brand/` (ADR-0027).

use std::fmt::Write as _;
use std::path::Path;

/// Lists the languages of the page (ADR-0030 §2): every `ui/i18n/<code>.json` is one, so adding
/// a language is adding a file. English comes first.
fn list_languages() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join("i18n");
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
    codes.sort_by_key(|code| (code != "en", code.clone()));
    assert!(
        codes.first().is_some_and(|c| c == "en"),
        "ui/i18n/en.json is missing"
    );
    let mut out = String::from(
        "const DICTIONARIES: &[(&str, &str)] = &[
",
    );
    for code in &codes {
        let file = dir.join(format!("{code}.json"));
        println!("cargo:rerun-if-changed={}", file.display());
        let _ = writeln!(
            out,
            "    ({code:?}, include_str!({:?})),",
            file.display().to_string()
        );
    }
    out.push_str(
        "];
",
    );
    let target = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("ui_languages.rs");
    std::fs::write(target, out).expect("write ui_languages.rs");
}

fn main() {
    let brand = Path::new("../../assets/brand");
    for file in ["tokens.css", "symbol.svg"] {
        let from = brand.join(file);
        println!("cargo:rerun-if-changed={}", from.display());
        std::fs::copy(&from, Path::new("ui").join(file))
            .unwrap_or_else(|e| panic!("cannot copy {}: {e}", from.display()));
    }
    list_languages();
    tauri_build::build();
}
