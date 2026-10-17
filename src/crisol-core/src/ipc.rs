//! Where the daemon of a Crisol home can be reached.
//!
//! Every home has its own daemon, so every home has its own pipe: the name is derived from the
//! path of the home. Two homes never share a daemon (tests run several side by side) and the
//! CLI finds the right one from `CRISOL_HOME` alone.

use std::fmt::Write as _;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The start of every pipe name of Crisol.
pub const PIPE_PREFIX: &str = r"\\.\pipe\crisol-";

/// The named pipe of the daemon of `home`: `\\.\pipe\crisol-<16 hex digits>`.
///
/// The path is lower-cased and its separators unified first, because Windows treats
/// `C:\Crisol`, `c:/crisol` and `C:\Crisol\` as one folder.
#[must_use]
pub fn pipe_name(home: &Path) -> String {
    let text = home
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let digest = Sha256::digest(text.as_bytes());
    let short = digest.iter().take(8).fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    });
    format!("{PIPE_PREFIX}{short}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_name_is_stable_and_has_the_pipe_shape() {
        let name = pipe_name(Path::new(r"C:\Crisol"));
        assert!(name.starts_with(r"\\.\pipe\crisol-"), "{name}");
        assert_eq!(name.len(), PIPE_PREFIX.len() + 16);
        assert!(
            name[PIPE_PREFIX.len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "{name}"
        );
        assert_eq!(name, pipe_name(Path::new(r"C:\Crisol")));
    }

    #[test]
    fn one_folder_has_one_name_however_it_is_written() {
        let one = pipe_name(Path::new(r"C:\Crisol"));
        for same in [r"c:\crisol", r"C:/Crisol", "C:\\Crisol\\", r"C:/CRISOL/"] {
            assert_eq!(pipe_name(&PathBuf::from(same)), one, "{same}");
        }
    }

    #[test]
    fn different_homes_have_different_pipes() {
        assert_ne!(
            pipe_name(Path::new(r"C:\Crisol")),
            pipe_name(Path::new(r"D:\Crisol"))
        );
        assert_ne!(
            pipe_name(Path::new(r"C:\Crisol")),
            pipe_name(Path::new(r"C:\Crisol2"))
        );
    }
}
