//! The passwords Crisol generates for the services (ADR-0006, ADR-0016).
//!
//! `127.0.0.1` keeps the network out, not the other accounts of the same computer, and a database
//! superuser can run programs as whoever runs the server (PostgreSQL `COPY … TO PROGRAM`, MySQL
//! `INTO OUTFILE`, Redis `CONFIG SET dir` + `SAVE`). So every service that has a password gets
//! one when its data is made: random, from the system's generator, kept in `secrets/` and shown
//! by `crisol db credentials`. The folder is made with [`PrivateDirs`] and checked like `certs/`
//! (ADR-0008): if other accounts can write in it, nothing is read from it or put in it.
//!
//! A secret never goes into `crisol.toml`, a configuration the user might share, a command line
//! another program can read, or a log. [`Secret`] cannot be printed by accident: its `Debug` and
//! `Display` say `<secret>`, and the text is reached only through [`Secret::expose`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::{CrisolPaths, PrivateDirs};

/// Characters of a generated password. Letters and digits only: the password goes into SQL
/// text, configuration files, URLs and `.env` files, and none of them needs quoting.
const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Length of a generated password: about 190 bits.
pub const PASSWORD_LENGTH: usize = 32;

/// A password. It prints as `<secret>`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a text as a secret.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The text. Call it only where it has to be written (a private file, a pipe, the
    /// environment of a child) and nowhere it could be logged.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<secret>")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<secret>")
    }
}

/// Why the secrets could not be used.
#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    /// Other accounts can write in `secrets/`: they could read or change every password.
    #[error("`{}` can be modified by other accounts of this computer, so the passwords in it cannot be trusted", .0.display())]
    Exposed(PathBuf),
    /// The system could not give random bytes.
    #[error("the system could not generate random bytes: {0}")]
    Random(String),
    /// A file or folder failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// What failed.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
}

fn io_error(path: &Path) -> impl Fn(io::Error) -> SecretsError + '_ {
    move |source| SecretsError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A new random password of [`PASSWORD_LENGTH`] letters and digits, from the operating system's
/// generator. Every character is equally likely (bytes that would bias the choice are skipped).
pub fn generate_password() -> Result<Secret, SecretsError> {
    let mut text = String::with_capacity(PASSWORD_LENGTH);
    let mut buffer = [0_u8; 64];
    while text.len() < PASSWORD_LENGTH {
        getrandom::fill(&mut buffer).map_err(|e| SecretsError::Random(e.to_string()))?;
        for byte in buffer {
            // 248 is the largest multiple of 62 that fits in a byte: above it the remainder
            // would favour the first characters.
            if byte < 248 && text.len() < PASSWORD_LENGTH {
                text.push(char::from(ALPHABET[usize::from(byte) % ALPHABET.len()]));
            }
        }
    }
    Ok(Secret(text))
}

/// Makes `secrets/` (private, through `dirs`) and checks that nobody else can write in it.
/// Returns its path.
pub fn ensure_dir(paths: &CrisolPaths, dirs: &dyn PrivateDirs) -> Result<PathBuf, SecretsError> {
    let dir = paths.secrets_dir();
    dirs.create_private_dir(&dir).map_err(io_error(&dir))?;
    if dirs.is_writable_by_others(&dir).map_err(io_error(&dir))?
        || dirs
            .owned_by_another_account(&dir)
            .map_err(io_error(&dir))?
    {
        return Err(SecretsError::Exposed(dir));
    }
    Ok(dir)
}

/// Writes `text` to `file`, replacing it, by writing a neighbour first and renaming it: a reader
/// never sees half a secret. The file is in `secrets/`, whose permissions it inherits.
pub fn write_file(file: &Path, text: &str) -> Result<(), SecretsError> {
    let mut name = file.as_os_str().to_owned();
    name.push(".new");
    let tmp = PathBuf::from(name);
    fs::write(&tmp, text).map_err(io_error(&tmp))?;
    fs::rename(&tmp, file).map_err(io_error(file))
}

/// Reads a secret kept with [`write_file`]; `None` if there is none.
pub fn read_secret(file: &Path) -> Result<Option<Secret>, SecretsError> {
    match fs::read_to_string(file) {
        Ok(text) => {
            let text = text.trim();
            Ok((!text.is_empty()).then(|| Secret(text.to_owned())))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(file)(e)),
    }
}

/// Deletes a file Crisol made for a moment (a password file for `initdb`, an `--init-file`). A
/// file that is not there is fine.
pub fn remove_file(file: &Path) {
    let _ = fs::remove_file(file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::fake::FakePrivateDirs;

    #[test]
    fn a_password_is_long_random_and_made_of_letters_and_digits() {
        let one = generate_password().unwrap();
        let two = generate_password().unwrap();
        assert_ne!(one, two);
        assert_eq!(one.expose().len(), PASSWORD_LENGTH);
        assert!(one.expose().chars().all(|c| c.is_ascii_alphanumeric()));
        // Over many passwords every character of the alphabet shows up.
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..200 {
            seen.extend(generate_password().unwrap().expose().chars());
        }
        assert_eq!(seen.len(), 62, "{seen:?}");
    }

    #[test]
    fn a_secret_cannot_be_printed_by_accident() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            password: Secret,
        }
        let secret = Secret::new("hunter2hunter2");
        assert_eq!(format!("{secret} {secret:?}"), "<secret> <secret>");
        assert!(!format!("{:?}", Holder { password: secret }).contains("hunter2"));
    }

    #[test]
    fn the_folder_is_made_private_and_an_open_or_foreign_one_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let dirs = FakePrivateDirs::new();
        let dir = ensure_dir(&paths, &dirs).unwrap();
        assert_eq!(dirs.created(), std::slice::from_ref(&dir));
        // Open to others: refused, and nothing is changed.
        dirs.expose(&dir);
        assert!(matches!(
            ensure_dir(&paths, &dirs),
            Err(SecretsError::Exposed(_))
        ));
        dirs.close(&dir);
        dirs.make_foreign(&dir);
        assert!(matches!(
            ensure_dir(&paths, &dirs),
            Err(SecretsError::Exposed(_))
        ));
    }

    #[test]
    fn a_secret_is_written_whole_read_back_and_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("mysql-8.4");
        assert_eq!(read_secret(&file).unwrap(), None);
        write_file(&file, "first\n").unwrap();
        assert_eq!(read_secret(&file).unwrap().unwrap().expose(), "first");
        write_file(&file, "second").unwrap();
        assert_eq!(read_secret(&file).unwrap().unwrap().expose(), "second");
        assert!(
            !tmp.path().join("mysql-8.4.new").exists(),
            "no neighbour is left"
        );
        write_file(&file, "  \n").unwrap();
        assert_eq!(
            read_secret(&file).unwrap(),
            None,
            "an empty file is no secret"
        );
    }
}
