//! The fallback for resolving `.test`: a managed block in the `hosts` file (F7.3, ADR-0014).
//!
//! ```text
//! # BEGIN CRISOL
//! # Managed by Crisol: whatever is between these markers is rewritten.
//! 127.0.0.1 shop.test
//! ::1 shop.test
//! # END CRISOL
//! ```
//!
//! Everything here is plain text editing, free of any operating system, so it is tested against
//! strings and copies, never the real file. The rules:
//!
//! - **Only the block is ours.** Every byte outside the markers is kept exactly as it was, with
//!   its own line endings; the block uses the file's line endings.
//! - **Reversible.** [`remove_block`] undoes [`apply_block`]: the file comes back byte for byte,
//!   except that a file which did not end with a newline keeps the one that was added.
//! - **Nothing doubtful is touched.** A file with a `BEGIN` and no `END`, two blocks or markers
//!   in the wrong order is refused, not repaired.
//! - **Names are validated** like the DNS suffix (a special-use TLD, plain host characters), and
//!   a name another tool already defines outside the block is reported as a [`Conflict`]
//!   (ADR-0007), not removed.
//! - Writing the real file needs an administrator; the platform does it atomically and keeps
//!   a copy of the original (`crisol-platform`), through the elevated helper.

use std::fmt::Write as _;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, PoisonError};

use crate::ca::is_special_use;

/// First line of the block.
pub const BEGIN: &str = "# BEGIN CRISOL";
/// Last line of the block.
pub const END: &str = "# END CRISOL";
const NOTE: &str = "# Managed by Crisol: whatever is between these markers is rewritten.";

/// Why the file or the names are refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostsError {
    /// A `BEGIN` marker without its `END`, an `END` without `BEGIN`, markers in the wrong
    /// order, or more than one block.
    #[error(
        "the hosts file has a damaged Crisol block (line {line}: {problem}); fix or remove it by \
         hand, Crisol will not guess"
    )]
    DamagedBlock {
        /// 1-based line where the problem was found.
        line: usize,
        /// What is wrong.
        problem: &'static str,
    },
    /// The name is not a plain host name under a special-use suffix.
    #[error("`{0}` cannot go in the hosts file: only plain names under test, localhost, example, invalid, local, internal or home.arpa")]
    InvalidName(String),
}

/// A name that something else already defines in the file, outside the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// 1-based line number.
    pub line: usize,
    /// The name Crisol wanted.
    pub name: String,
    /// The line, trimmed.
    pub text: String,
}

/// The new content and what the caller should warn about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The whole file after the change.
    pub content: String,
    /// Names defined outside the block.
    pub conflicts: Vec<Conflict>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && is_special_use(name)
}

/// Validates, lower-cases, sorts and de-duplicates the names.
pub fn clean_names<S: AsRef<str>>(names: &[S]) -> Result<Vec<String>, HostsError> {
    let mut clean = Vec::new();
    for name in names {
        let name = name.as_ref().to_ascii_lowercase();
        if !valid_name(&name) {
            return Err(HostsError::InvalidName(name));
        }
        clean.push(name);
    }
    clean.sort();
    clean.dedup();
    Ok(clean)
}

fn line_end(content: &str) -> &'static str {
    if content.is_empty() || content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Where the block is: the line indexes of its markers.
fn find_block(lines: &[&str]) -> Result<Option<(usize, usize)>, HostsError> {
    let mut begin = None;
    let mut found = None;
    for (i, line) in lines.iter().enumerate() {
        match line.trim() {
            BEGIN => {
                if begin.is_some() || found.is_some() {
                    return Err(HostsError::DamagedBlock {
                        line: i + 1,
                        problem: "a second block starts",
                    });
                }
                begin = Some(i);
            }
            END => match begin.take() {
                Some(b) => found = Some((b, i)),
                None => {
                    return Err(HostsError::DamagedBlock {
                        line: i + 1,
                        problem: "an end marker with no start",
                    })
                }
            },
            _ => {}
        }
    }
    match begin {
        Some(b) => Err(HostsError::DamagedBlock {
            line: b + 1,
            problem: "the block never ends",
        }),
        None => Ok(found),
    }
}

/// The names in the block, sorted. No block is no names.
pub fn block_names(content: &str) -> Result<Vec<String>, HostsError> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let Some((begin, end)) = find_block(&lines)? else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = lines[begin + 1..end]
        .iter()
        .filter_map(|line| entry(line))
        .flat_map(|(_, names)| names)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// `(address, names)` of a hosts line, or `None` for blanks, comments and anything that is not
/// an address followed by names.
fn entry(line: &str) -> Option<(String, Vec<String>)> {
    let text = line.split('#').next().unwrap_or("");
    let mut words = text.split_whitespace();
    let address = words.next()?;
    if address.parse::<Ipv4Addr>().is_err() && address.parse::<Ipv6Addr>().is_err() {
        return None;
    }
    let names: Vec<String> = words.map(str::to_ascii_lowercase).collect();
    if names.is_empty() {
        None
    } else {
        Some((address.to_owned(), names))
    }
}

/// The names Crisol wants that are defined outside its block.
fn conflicts(lines: &[&str], block: Option<(usize, usize)>, names: &[String]) -> Vec<Conflict> {
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if block.is_some_and(|(b, e)| (b..=e).contains(&i)) {
            continue;
        }
        let Some((_, defined)) = entry(line) else {
            continue;
        };
        for name in names.iter().filter(|n| defined.contains(n)) {
            found.push(Conflict {
                line: i + 1,
                name: name.clone(),
                text: line.trim().to_owned(),
            });
        }
    }
    found
}

fn render(names: &[String], eol: &str) -> String {
    let mut out = format!("{BEGIN}{eol}{NOTE}{eol}");
    for name in names {
        let _ = write!(out, "127.0.0.1 {name}{eol}::1 {name}{eol}");
    }
    let _ = write!(out, "{END}{eol}");
    out
}

/// Puts the block with exactly these names in the file: replaces the one that is there or adds
/// it at the end. No names removes the block ([`remove_block`]).
pub fn apply_block<S: AsRef<str>>(content: &str, names: &[S]) -> Result<Update, HostsError> {
    let names = clean_names(names)?;
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let block = find_block(&lines)?;
    let conflicts = conflicts(&lines, block, &names);
    if names.is_empty() {
        return Ok(Update {
            content: remove_block(content)?,
            conflicts,
        });
    }
    let eol = line_end(content);
    let rendered = render(&names, eol);
    let content = if let Some((begin, end)) = block {
        let mut out: String = lines[..begin].concat();
        out.push_str(&rendered);
        out.push_str(&lines[end + 1..].concat());
        out
    } else {
        let mut out = content.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&rendered);
        out
    };
    Ok(Update { content, conflicts })
}

/// Takes the block out and nothing else.
pub fn remove_block(content: &str) -> Result<String, HostsError> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    Ok(match find_block(&lines)? {
        None => content.to_owned(),
        Some((begin, end)) => {
            let mut out: String = lines[..begin].concat();
            out.push_str(&lines[end + 1..].concat());
            out
        }
    })
}

// ---- the file --------------------------------------------------------------------------

/// The `hosts` file as text. The real one lives in `crisol-platform`; tests use
/// [`MemoryHosts`] or a copy in a temporary folder.
pub trait HostsFile: Send + Sync {
    /// The whole file. A missing file is an empty one.
    fn read(&self) -> io::Result<String>;
    /// Replaces the whole file.
    fn write(&self, content: &str) -> io::Result<()>;
}

/// What Crisol asks of the machine's `hosts`: which names it has put there, and to put others.
pub trait HostsManager: Send + Sync {
    /// The names in Crisol's block.
    fn names(&self) -> Result<Vec<String>, HostsManagerError>;
    /// Makes the block hold exactly these names. Returns the names someone else defines.
    fn set(&self, names: &[String]) -> Result<Vec<Conflict>, HostsManagerError>;
    /// Takes the block out.
    fn clear(&self) -> Result<(), HostsManagerError>;
}

/// Why a [`HostsManager`] call failed.
#[derive(Debug, thiserror::Error)]
pub enum HostsManagerError {
    /// The content or the names are refused.
    #[error(transparent)]
    Hosts(#[from] HostsError),
    /// The file could not be read or written.
    #[error("hosts file: {0}")]
    Io(#[from] io::Error),
}

/// A [`HostsManager`] that edits a [`HostsFile`] directly (the helper uses it over the real
/// file; tests over copies). It writes only when the content would change.
#[derive(Debug)]
pub struct DirectHosts<F> {
    file: F,
}

impl<F: HostsFile> DirectHosts<F> {
    /// Over this file.
    pub fn new(file: F) -> Self {
        Self { file }
    }
}

impl<F: HostsFile> HostsManager for DirectHosts<F> {
    fn names(&self) -> Result<Vec<String>, HostsManagerError> {
        Ok(block_names(&self.file.read()?)?)
    }

    fn set(&self, names: &[String]) -> Result<Vec<Conflict>, HostsManagerError> {
        let current = self.file.read()?;
        let update = apply_block(&current, names)?;
        if update.content != current {
            self.file.write(&update.content)?;
        }
        Ok(update.conflicts)
    }

    fn clear(&self) -> Result<(), HostsManagerError> {
        let current = self.file.read()?;
        let cleared = remove_block(&current)?;
        if cleared != current {
            self.file.write(&cleared)?;
        }
        Ok(())
    }
}

/// A `hosts` file that lives in memory. For tests.
#[derive(Debug, Default)]
pub struct MemoryHosts {
    content: Mutex<String>,
    writes: Mutex<usize>,
}

impl MemoryHosts {
    /// With this content.
    #[must_use]
    pub fn new(content: &str) -> Self {
        Self {
            content: Mutex::new(content.to_owned()),
            writes: Mutex::new(0),
        }
    }

    /// The content now.
    #[must_use]
    pub fn content(&self) -> String {
        self.content
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many times it was written.
    #[must_use]
    pub fn writes(&self) -> usize {
        *self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl HostsFile for &MemoryHosts {
    fn read(&self) -> io::Result<String> {
        Ok(self.content())
    }

    fn write(&self, content: &str) -> io::Result<()> {
        content.clone_into(&mut self.content.lock().unwrap_or_else(PoisonError::into_inner));
        *self.writes.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
