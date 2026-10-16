//! Link-time hardening of the one program that runs as an administrator (ADR-0014):
//!
//! - an embedded manifest that asks Windows for administrator rights (`requireAdministrator`),
//!   so it can never run half-elevated by accident;
//! - `/DEPENDENTLOADFLAG:0x800` (`LOAD_LIBRARY_SEARCH_SYSTEM32`), so even the DLLs it imports
//!   statically are looked for in `System32` only, not next to the executable.
//!
//! Only the binary gets them (`-bins`): the test executables of this crate must keep running
//! without elevation.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if !windows_msvc {
        return;
    }
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTUAC:level='requireAdministrator' uiAccess='false'"
    );
    println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
}
