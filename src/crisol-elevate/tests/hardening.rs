//! The built helper carries the hardening that `build.rs` asks the linker for. The program is
//! only read as bytes here: it is never run (it would ask for administrator rights).

#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::fs;

fn built_helper() -> Vec<u8> {
    fs::read(env!("CARGO_BIN_EXE_crisol-elevate")).unwrap()
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[test]
fn it_asks_windows_for_administrator_rights() {
    let text = String::from_utf8_lossy(&built_helper()).into_owned();
    assert!(text.contains("requireAdministrator"));
    assert!(!text.contains("asInvoker"));
}

#[test]
fn its_static_imports_are_looked_for_in_system32_only() {
    let bytes = built_helper();
    let pe = u32_at(&bytes, 0x3c) as usize;
    let optional = pe + 24;
    assert_eq!(u16_at(&bytes, optional), 0x20b, "a 64-bit image");
    // Data directory 10 is the load configuration.
    let entry = optional + 112 + 10 * 8;
    let (rva, size) = (u32_at(&bytes, entry), u32_at(&bytes, entry + 4));
    assert!(
        rva != 0 && size >= 0x50,
        "the image has a load configuration"
    );

    let sections = u16_at(&bytes, pe + 6) as usize;
    let table = optional + u16_at(&bytes, pe + 20) as usize;
    let offset = (0..sections)
        .map(|i| table + i * 40)
        .find_map(|s| {
            let (virtual_size, address, raw_size, raw) = (
                u32_at(&bytes, s + 8),
                u32_at(&bytes, s + 12),
                u32_at(&bytes, s + 16),
                u32_at(&bytes, s + 20),
            );
            (address <= rva && rva < address + virtual_size.max(raw_size))
                .then(|| (rva - address + raw) as usize)
        })
        .expect("the load configuration is inside a section");
    // IMAGE_LOAD_CONFIG_DIRECTORY64.DependentLoadFlags.
    let flags = u16_at(&bytes, offset + 0x4E);
    assert_eq!(flags, 0x800, "LOAD_LIBRARY_SEARCH_SYSTEM32");
}
