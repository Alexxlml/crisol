//! Where the daemon of a Crisol home can be reached.
//!
//! Every home has its own daemon, so every home has its own pipe: the name is derived from the
//! path of the home **and the account that runs it**. Two homes never share a daemon (tests run
//! several side by side), the CLI finds the right one from `CRISOL_HOME` alone, and because pipe
//! names are global to the computer, a name that depends on the user cannot be taken ahead of
//! time by another account to stop this user's daemon from starting (`first_pipe_instance`).

use std::fmt::Write as _;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The start of every pipe name of Crisol.
pub const PIPE_PREFIX: &str = r"\\.\pipe\crisol-";

/// The named pipe of the daemon of `home` run by `user` (the SID of the account, as text):
/// `\\.\pipe\crisol-<8 hex digits of the user>-<8 hex digits of the home>`.
///
/// The path is lower-cased and its separators unified first, because Windows treats
/// `C:\Crisol`, `c:/crisol` and `C:\Crisol\` as one folder.
#[must_use]
pub fn pipe_name(home: &Path, user: &str) -> String {
    let text = home
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    format!(
        "{PIPE_PREFIX}{}-{}",
        short_hash(&user.to_ascii_uppercase()),
        short_hash(&text)
    )
}

fn short_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(4)
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const ME: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn the_name_is_stable_and_has_the_pipe_shape() {
        let name = pipe_name(Path::new(r"C:\Crisol"), ME);
        assert!(name.starts_with(r"\\.\pipe\crisol-"), "{name}");
        assert_eq!(name.len(), PIPE_PREFIX.len() + 8 + 1 + 8);
        let (user, home) = name[PIPE_PREFIX.len()..].split_once('-').unwrap();
        assert!(user
            .chars()
            .chain(home.chars())
            .all(|c| c.is_ascii_hexdigit()));
        assert_eq!(name, pipe_name(Path::new(r"C:\Crisol"), ME));
    }

    #[test]
    fn one_folder_has_one_name_however_it_is_written() {
        let one = pipe_name(Path::new(r"C:\Crisol"), ME);
        for same in [r"c:\crisol", r"C:/Crisol", "C:\\Crisol\\", r"C:/CRISOL/"] {
            assert_eq!(pipe_name(&PathBuf::from(same), ME), one, "{same}");
        }
    }

    #[test]
    fn different_homes_have_different_pipes() {
        assert_ne!(
            pipe_name(Path::new(r"C:\Crisol"), ME),
            pipe_name(Path::new(r"D:\Crisol"), ME)
        );
        assert_ne!(
            pipe_name(Path::new(r"C:\Crisol"), ME),
            pipe_name(Path::new(r"C:\Crisol2"), ME)
        );
    }

    #[test]
    fn two_accounts_never_share_a_pipe_even_with_the_same_home() {
        // `C:\Crisol` is the default home of everybody: the name must still be one's own.
        let mine = pipe_name(Path::new(r"C:\Crisol"), ME);
        let theirs = pipe_name(Path::new(r"C:\Crisol"), "S-1-5-21-1-2-3-1002");
        assert_ne!(mine, theirs);
        assert_eq!(
            mine,
            pipe_name(Path::new(r"C:\Crisol"), &ME.to_lowercase()),
            "a SID is the same SID in any case"
        );
    }
}
