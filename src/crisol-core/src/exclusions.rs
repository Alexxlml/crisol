//! The antivirus exclusions that point inside the Crisol home (F11.2, ADR-0022).
//!
//! Crisol never makes an exclusion: `doctor` only tells the user the command for one when the
//! databases start slowly (ADR-0021). But when Crisol is uninstalled, the ones inside its home
//! go too, so that nothing of Crisol is left in the system. This module is the part with no
//! operating system in it:
//!
//! - which exclusions are Crisol's: [`inside`], by path components and ignoring case; an
//!   exclusion of another folder, or a process named without a path (`mysqld.exe`), is never
//!   touched;
//! - the store, behind [`ScanExclusions`] (the Windows one is Defender, in `crisol-platform`,
//!   and only works as an administrator);
//! - [`remove_inside`], which the elevated helper runs;
//! - how the home travels to the helper: [`encode_home`] and [`decode_home`]. The arguments of
//!   the elevation prompt cannot carry spaces, and a home under `C:\Users\First Last\` has one.

use std::io;
use std::path::{Path, PathBuf};

/// What an exclusion leaves out of the scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionKind {
    /// A file or folder.
    Path,
    /// The files a process opens.
    Process,
}

/// One exclusion, as the store writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclusion {
    /// What it leaves out.
    pub kind: ExclusionKind,
    /// The path, or the process, exactly as the store has it.
    pub value: String,
}

/// The exclusions of the antivirus.
pub trait ScanExclusions {
    /// Every exclusion.
    fn list(&self) -> io::Result<Vec<Exclusion>>;
    /// Removes this one.
    fn remove(&self, exclusion: &Exclusion) -> io::Result<()>;
}

/// What [`remove_inside`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusionsOutcome {
    /// None was inside the home.
    NoneFound,
    /// These were removed.
    Removed(Vec<Exclusion>),
    /// The store could not be read or changed; this is why.
    Unavailable(String),
}

/// The text of a path for comparing: backslashes, no trailing one, lowercase.
fn comparable(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Whether `value` is `home` or something inside it.
#[must_use]
pub fn inside(home: &Path, value: &str) -> bool {
    let home = comparable(&home.to_string_lossy());
    let value = comparable(value.trim());
    if home.is_empty() {
        return false;
    }
    value == home
        || value
            .strip_prefix(&home)
            .is_some_and(|rest| rest.starts_with('\\'))
}

/// Removes the exclusions inside `home`, and no other. Stops at the first that cannot be
/// removed.
pub fn remove_inside(store: &dyn ScanExclusions, home: &Path) -> ExclusionsOutcome {
    let all = match store.list() {
        Ok(all) => all,
        Err(error) => return ExclusionsOutcome::Unavailable(error.to_string()),
    };
    let ours: Vec<Exclusion> = all.into_iter().filter(|e| inside(home, &e.value)).collect();
    if ours.is_empty() {
        return ExclusionsOutcome::NoneFound;
    }
    for exclusion in &ours {
        if let Err(error) = store.remove(exclusion) {
            return ExclusionsOutcome::Unavailable(format!("{}: {error}", exclusion.value));
        }
    }
    ExclusionsOutcome::Removed(ours)
}

/// Reads the exclusions from the JSON the Windows implementation prints:
/// `{"Path": [...], "Process": [...]}`. A value that says the list is hidden (the process is not
/// an administrator) is an error, not an exclusion.
pub fn parse_list_json(json: &str) -> io::Result<Vec<Exclusion>> {
    let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
    let value: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| invalid(e.to_string()))?;
    let mut found = Vec::new();
    for (key, kind) in [
        ("Path", ExclusionKind::Path),
        ("Process", ExclusionKind::Process),
    ] {
        // One value alone may come as a string instead of a list.
        let values = match value.get(key) {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::String(text)) => vec![text.clone()],
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
            Some(other) => return Err(invalid(format!("unexpected `{key}`: {other}"))),
        };
        for text in values {
            if text.starts_with("N/A") {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Defender only shows its exclusions to an administrator",
                ));
            }
            found.push(Exclusion { kind, value: text });
        }
    }
    Ok(found)
}

/// The home as one plain word: its UTF-8 bytes in hex.
#[must_use]
pub fn encode_home(home: &Path) -> String {
    use std::fmt::Write as _;
    home.to_string_lossy()
        .bytes()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// The home back from [`encode_home`]. Only an absolute path on a drive (`C:\…`) that is not
/// the drive itself, without control characters: anything else is refused.
pub fn decode_home(word: &str) -> Result<PathBuf, String> {
    let refuse = || format!("`{word}` is not a home");
    if !word.len().is_multiple_of(2) || !word.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(refuse());
    }
    let bytes = (0..word.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&word[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| refuse())?;
    let text = String::from_utf8(bytes).map_err(|_| refuse())?;
    let mut chars = text.chars();
    let drive = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest: String = chars.collect();
    let rooted = rest.starts_with(":\\");
    let below_root = rest.len() > 2 && !comparable(&rest[2..]).is_empty();
    if !drive || !rooted || !below_root || text.chars().any(char::is_control) {
        return Err(format!("`{text}` is not an absolute folder below a drive"));
    }
    Ok(PathBuf::from(text))
}

/// The double for tests: a list in memory.
#[derive(Debug, Default)]
pub struct FakeExclusions {
    state: std::sync::Mutex<(Vec<Exclusion>, Option<String>)>,
}

impl FakeExclusions {
    /// With these paths and processes.
    #[must_use]
    pub fn with(paths: &[&str], processes: &[&str]) -> Self {
        let mut all: Vec<Exclusion> = paths
            .iter()
            .map(|p| Exclusion {
                kind: ExclusionKind::Path,
                value: (*p).to_owned(),
            })
            .collect();
        all.extend(processes.iter().map(|p| Exclusion {
            kind: ExclusionKind::Process,
            value: (*p).to_owned(),
        }));
        Self {
            state: std::sync::Mutex::new((all, None)),
        }
    }

    /// Every call fails with this message.
    pub fn fail_with(&self, message: &str) {
        self.lock().1 = Some(message.to_owned());
    }

    /// The exclusions left.
    #[must_use]
    pub fn values(&self) -> Vec<String> {
        self.lock().0.iter().map(|e| e.value.clone()).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (Vec<Exclusion>, Option<String>)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ScanExclusions for FakeExclusions {
    fn list(&self) -> io::Result<Vec<Exclusion>> {
        let state = self.lock();
        match &state.1 {
            Some(message) => Err(io::Error::other(message.clone())),
            None => Ok(state.0.clone()),
        }
    }

    fn remove(&self, exclusion: &Exclusion) -> io::Result<()> {
        let mut state = self.lock();
        if let Some(message) = &state.1 {
            return Err(io::Error::other(message.clone()));
        }
        state.0.retain(|e| e != exclusion);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_home_and_what_is_inside_it_count() {
        let home = Path::new(r"C:\Crisol");
        for yes in [
            r"C:\Crisol",
            r"c:\crisol\",
            r"C:\Crisol\data",
            r"C:/Crisol/data/mysql",
            r"C:\CRISOL\bin\mysql\8.4.3\bin\mysqld.exe",
        ] {
            assert!(inside(home, yes), "{yes}");
        }
        for no in [
            r"C:\Crisol2",
            r"C:\CrisolData",
            r"C:\",
            r"D:\Crisol\data",
            "mysqld.exe",
            r"C:\Users\me\.config\herd",
            "",
        ] {
            assert!(!inside(home, no), "{no}");
        }
        assert!(!inside(Path::new(""), r"C:\anything"));
    }

    #[test]
    fn removing_takes_out_the_ones_inside_and_leaves_the_rest() {
        let store = FakeExclusions::with(
            &[r"C:\Crisol\data", r"C:\Users\me\Herd", r"D:\Projects"],
            &[r"C:\Crisol\bin\mysql\8.4.3\bin\mysqld.exe", "node.exe"],
        );
        let outcome = remove_inside(&store, Path::new(r"C:\Crisol"));
        let ExclusionsOutcome::Removed(removed) = outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].kind, ExclusionKind::Path);
        assert_eq!(removed[1].kind, ExclusionKind::Process);
        assert_eq!(
            store.values(),
            [r"C:\Users\me\Herd", r"D:\Projects", "node.exe"]
        );
        assert_eq!(
            remove_inside(&store, Path::new(r"C:\Crisol")),
            ExclusionsOutcome::NoneFound
        );
    }

    #[test]
    fn a_store_that_fails_is_unavailable_and_nothing_is_claimed() {
        let store = FakeExclusions::with(&[r"C:\Crisol\data"], &[]);
        store.fail_with("no Defender here");
        assert_eq!(
            remove_inside(&store, Path::new(r"C:\Crisol")),
            ExclusionsOutcome::Unavailable("no Defender here".to_owned())
        );
    }

    #[test]
    fn the_list_is_read_whether_it_has_none_one_or_many() {
        assert_eq!(parse_list_json(r#"{"Path":[],"Process":[]}"#).unwrap(), []);
        assert_eq!(parse_list_json(r#"{"Path":null}"#).unwrap(), []);
        let one = parse_list_json(r#"{"Path":"C:\\Crisol\\data","Process":[]}"#).unwrap();
        assert_eq!(
            one,
            [Exclusion {
                kind: ExclusionKind::Path,
                value: r"C:\Crisol\data".to_owned()
            }]
        );
        let many = parse_list_json(
            r#"{"Path":["C:\\Crisol\\data","D:\\Herd"],"Process":["C:\\Crisol\\bin\\mysqld.exe"]}"#,
        )
        .unwrap();
        assert_eq!(many.len(), 3);
        assert_eq!(many[2].kind, ExclusionKind::Process);
    }

    #[test]
    fn a_hidden_list_or_something_else_is_an_error() {
        let hidden =
            parse_list_json(r#"{"Path":["N/A: Must be an administrator to view exclusions"]}"#)
                .unwrap_err();
        assert_eq!(hidden.kind(), io::ErrorKind::PermissionDenied);
        assert!(parse_list_json("not json").is_err());
        assert!(parse_list_json(r#"{"Path":3}"#).is_err());
    }

    #[test]
    fn the_home_travels_as_one_word_and_comes_back_the_same() {
        for home in [
            r"C:\Crisol",
            r"C:\Users\Marco Alexis\AppData\Local\Crisol",
            r"D:\Prüfung\Crisol's home",
        ] {
            let word = encode_home(Path::new(home));
            assert!(word.bytes().all(|b| b.is_ascii_hexdigit()), "{word}");
            assert_eq!(decode_home(&word).unwrap(), PathBuf::from(home));
        }
    }

    #[test]
    fn a_home_that_is_not_a_folder_below_a_drive_is_refused() {
        for bad in [
            r"C:\",
            "C:",
            r"\\server\share\Crisol",
            r"Crisol\data",
            "C:\\Crisol\n",
            r"1:\Crisol",
        ] {
            let word = encode_home(Path::new(bad));
            assert!(decode_home(&word).is_err(), "{bad:?}");
        }
        for word in ["xyz", "abc", "ff", ""] {
            assert!(decode_home(word).is_err(), "{word}");
        }
    }
}
