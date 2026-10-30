//! The DLLs a Windows program imports, read from its file without running it (F11, decision of
//! the owner on the Visual C++ runtime): PHP, MySQL and PostgreSQL import the runtime, Nginx,
//! Node, Mailpit and Redis do not, so whether a service needs it is read from the program
//! itself instead of being kept in a list.
//!
//! Only the headers and the import directory are read (a `mysqld.exe` is tens of megabytes);
//! anything that does not look like a PE image is an `InvalidData` error.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// More import entries than this is not a program worth believing.
const MAX_IMPORTS: usize = 512;

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buf)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// A section: where its bytes are in memory (`va`, `size`) and in the file (`raw`).
struct Section {
    va: u32,
    size: u32,
    raw: u32,
}

fn file_offset(sections: &[Section], rva: u32) -> Option<u64> {
    sections
        .iter()
        .find(|s| rva >= s.va && rva < s.va.saturating_add(s.size))
        .map(|s| u64::from(rva - s.va) + u64::from(s.raw))
}

/// What Crisol reads from the headers of a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The DLLs it imports, in lowercase, in the order of its import table.
    pub imports: Vec<String>,
    /// The version of the linker that made it (`14.44`): the Visual C++ runtime it runs with
    /// must be at least as new (review of phase 11, finding 5).
    pub linker: (u8, u8),
}

/// The imports of `program` and the version of the linker that made it.
pub fn read_image(program: &Path) -> io::Result<Image> {
    let mut file = File::open(program)?;
    let mut dos = [0_u8; 64];
    read_at(&mut file, 0, &mut dos)?;
    if &dos[..2] != b"MZ" {
        return Err(invalid("not a Windows program (no MZ header)"));
    }
    let pe = u64::from(u32_at(&dos, 0x3C));
    let mut header = [0_u8; 24];
    read_at(&mut file, pe, &mut header)?;
    if &header[..4] != b"PE\0\0" {
        return Err(invalid("not a Windows program (no PE header)"));
    }
    let sections_count = usize::from(u16_at(&header, 6));
    let optional_size = usize::from(u16_at(&header, 20));
    if optional_size < 4 || sections_count > 96 {
        return Err(invalid("a damaged PE header"));
    }
    let mut optional = vec![0_u8; optional_size];
    read_at(&mut file, pe + 24, &mut optional)?;
    // After the magic number: the major and minor version of the linker, one byte each.
    let linker = (optional[2], optional[3]);
    let image = |imports| Image { imports, linker };
    // The data directories start at 96 in a 32-bit image and at 112 in a 64-bit one; the
    // import directory is the second (8 bytes each).
    let directories = match u16_at(&optional, 0) {
        0x10B => 96,
        0x20B => 112,
        _ => return Err(invalid("an unknown kind of PE image")),
    };
    if optional_size < directories + 16 {
        return Ok(image(Vec::new()));
    }
    let import_rva = u32_at(&optional, directories + 8);
    if import_rva == 0 {
        return Ok(image(Vec::new()));
    }
    let mut table = vec![0_u8; sections_count * 40];
    read_at(&mut file, pe + 24 + optional_size as u64, &mut table)?;
    let sections: Vec<Section> = table
        .as_chunks::<40>()
        .0
        .iter()
        .map(|s| Section {
            va: u32_at(s, 12),
            size: u32_at(s, 8).max(u32_at(s, 16)),
            raw: u32_at(s, 20),
        })
        .collect();
    let mut at = file_offset(&sections, import_rva)
        .ok_or_else(|| invalid("imports outside the sections"))?;
    let mut names = Vec::new();
    loop {
        let mut entry = [0_u8; 20];
        read_at(&mut file, at, &mut entry)?;
        let name_rva = u32_at(&entry, 12);
        if name_rva == 0 {
            break;
        }
        if names.len() == MAX_IMPORTS {
            return Err(invalid("too many imports"));
        }
        let offset = file_offset(&sections, name_rva)
            .ok_or_else(|| invalid("a name outside the sections"))?;
        let mut name = [0_u8; 256];
        file.seek(SeekFrom::Start(offset))?;
        let read = file.read(&mut name)?;
        let end = name[..read].iter().position(|b| *b == 0).unwrap_or(read);
        names.push(String::from_utf8_lossy(&name[..end]).to_ascii_lowercase());
        at += 20;
    }
    Ok(image(names))
}

/// Whether a program that imports `dlls` needs the Visual C++ runtime: its `vcruntime` or its
/// C++ library. The `api-ms-win-crt-*` set is the Universal CRT, which Windows itself brings.
#[must_use]
pub fn needs_vc_runtime(dlls: &[String]) -> bool {
    dlls.iter()
        .any(|dll| dll.starts_with("vcruntime140") || dll.starts_with("msvcp140"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_imports_of_a_real_program_are_read() {
        // This test program itself: Crisol links the C runtime statically (src/.cargo), so it
        // imports kernel32 and not the Visual C++ runtime.
        let me = std::env::current_exe().unwrap();
        let image = read_image(&me).unwrap();
        let dlls = &image.imports;
        assert!(dlls.contains(&"kernel32.dll".to_owned()), "{dlls:?}");
        assert!(!needs_vc_runtime(dlls), "{dlls:?}");
        // The linker version, which Rust's MSVC toolchain also writes (14.x).
        assert_eq!(image.linker.0, 14, "{:?}", image.linker);
        // A program of Windows, too.
        let system = crate::system_paths::system_directory().unwrap();
        let cmd = read_image(&system.join("cmd.exe")).unwrap().imports;
        assert_ne!(cmd, Vec::<String>::new());
    }

    #[test]
    fn what_is_not_a_program_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("empty.exe", &b""[..]),
            ("text.exe", &b"hello, this is not a program at all, but long enough to read a header of sixty-four bytes"[..]),
            ("mz-only.exe", &b"MZ\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\xff\xff\x00\x00"[..]),
        ] {
            let path = tmp.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(read_image(&path).is_err(), "{name}");
        }
        assert!(read_image(&tmp.path().join("missing.exe")).is_err());
    }

    #[test]
    fn the_visual_cpp_runtime_is_told_from_the_universal_crt() {
        let names = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(needs_vc_runtime(&names(&[
            "kernel32.dll",
            "vcruntime140.dll"
        ])));
        assert!(needs_vc_runtime(&names(&["msvcp140.dll"])));
        assert!(needs_vc_runtime(&names(&["vcruntime140_1.dll"])));
        assert!(!needs_vc_runtime(&names(&[
            "kernel32.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "ucrtbase.dll"
        ])));
    }
}
