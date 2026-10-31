//! Brings the shared brand files next to the page before Tauri embeds it: the page imports
//! `tokens.css` and `symbol.svg`, whose one copy lives in `assets/brand/` (ADR-0027).

use std::path::Path;

fn main() {
    let brand = Path::new("../../assets/brand");
    for file in ["tokens.css", "symbol.svg"] {
        let from = brand.join(file);
        println!("cargo:rerun-if-changed={}", from.display());
        std::fs::copy(&from, Path::new("ui").join(file))
            .unwrap_or_else(|e| panic!("cannot copy {}: {e}", from.display()));
    }
    tauri_build::build();
}
