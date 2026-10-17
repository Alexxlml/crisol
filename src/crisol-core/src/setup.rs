//! Installing and removing the privileged helper (F8.5, F8.6, ADR-0014).
//!
//! `crisol-elevate.exe` lives in `%ProgramFiles%\Crisol\`, which only administrators can write,
//! and a one-time installer that runs with administrator rights puts it there. This module is
//! the part with no operating system in it:
//!
//! - the **manifest** the installer writes next to the program ([`Manifest`]): its version and
//!   its SHA-256. Because the program asks Windows for administrator rights, *running* it to ask
//!   its version would show a prompt; so the caller reads the manifest instead, from a folder
//!   only administrators can have written, and compares it with the hash of the program
//!   ([`check`]). A program that is not the one that was installed, or is of another version of
//!   Crisol, is refused with "install it again" (review of phase 7, W1);
//! - the two operations the elevated process is given ([`SetupOp`]) and the trait the rest of
//!   Crisol uses to ask for them ([`SystemSetup`]).

use std::fmt::Write as _;
use std::io;

use sha2::{Digest, Sha256};

use crate::dns::{rule_for, DnsError, DEFAULT_DNS_ADDR};

/// The file name of the manifest, next to the helper.
pub const MANIFEST_FILE: &str = "crisol-elevate.manifest";

/// What the installer records about the helper it installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The version of Crisol it was built with.
    pub version: String,
    /// SHA-256 of the program, 64 lowercase hex digits.
    pub sha256: String,
}

impl Manifest {
    /// The manifest of a program with these bytes.
    #[must_use]
    pub fn of(version: &str, program: &[u8]) -> Self {
        Self {
            version: version.to_owned(),
            sha256: sha256_hex(program),
        }
    }

    /// The text to write.
    #[must_use]
    pub fn to_text(&self) -> String {
        format!(
            "# Written by the installer of Crisol; do not edit.\nversion = {}\nsha256 = {}\n",
            self.version, self.sha256
        )
    }

    /// Reads a manifest. Anything unexpected is an error: a manifest that cannot be understood
    /// vouches for nothing.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (mut version, mut sha256) = (None, None);
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("`{line}` is not `key = value`"))?;
            match key.trim() {
                "version" => version = Some(value.trim().to_owned()),
                "sha256" => sha256 = Some(value.trim().to_ascii_lowercase()),
                other => return Err(format!("unknown key `{other}`")),
            }
        }
        let version = version.ok_or("no version")?;
        let sha256 = sha256.ok_or("no sha256")?;
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the sha256 is not 64 hex digits".to_owned());
        }
        Ok(Self { version, sha256 })
    }
}

/// SHA-256 as 64 lowercase hex digits.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Why the installed helper is not to be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelperProblem {
    /// There is no manifest: the helper was copied by hand, or by an older Crisol.
    #[error("the privileged helper has no manifest, so there is no telling which program it is; install it again with `crisol system install`")]
    NoManifest,
    /// The manifest cannot be understood.
    #[error("the manifest of the privileged helper is damaged ({0}); install it again with `crisol system install`")]
    DamagedManifest(String),
    /// The program is not the one that was installed.
    #[error("the privileged helper is not the program that was installed (its hash is {actual}, the manifest says {recorded}); install it again with `crisol system install`")]
    Changed {
        /// What the manifest says.
        recorded: String,
        /// What the file hashes to.
        actual: String,
    },
    /// It was installed by another version of Crisol.
    #[error("the privileged helper is from Crisol {installed} and this is Crisol {expected}; install it again with `crisol system install`")]
    OtherVersion {
        /// The version in the manifest.
        installed: String,
        /// The version of this Crisol.
        expected: String,
    },
}

/// Whether the helper `program` with the manifest `text` is the one this Crisol (version
/// `expected_version`) should run. Nothing is executed.
pub fn check(
    manifest_text: Option<&str>,
    program: &[u8],
    expected_version: &str,
) -> Result<(), HelperProblem> {
    let text = manifest_text.ok_or(HelperProblem::NoManifest)?;
    let manifest = Manifest::parse(text).map_err(HelperProblem::DamagedManifest)?;
    let actual = sha256_hex(program);
    if manifest.sha256 != actual {
        return Err(HelperProblem::Changed {
            recorded: manifest.sha256,
            actual,
        });
    }
    if manifest.version != expected_version {
        return Err(HelperProblem::OtherVersion {
            installed: manifest.version,
            expected: expected_version.to_owned(),
        });
    }
    Ok(())
}

/// What the elevated process is asked to do besides the everyday operations: the one-time
/// installation and its removal. The suffix is the one of the local sites (`test`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupOp {
    /// `install-self <suffix>`: copy this program into `%ProgramFiles%\Crisol\`, write its
    /// manifest, and make the NRPT rule for the suffix, all in this one elevated run.
    InstallSelf {
        /// The suffix, without dots.
        suffix: String,
    },
    /// `uninstall-self <suffix>`: take the NRPT rule and the block of `hosts` out, then the
    /// folder. Only accepted from the installed copy.
    UninstallSelf {
        /// The suffix, without dots.
        suffix: String,
    },
}

impl SetupOp {
    /// Parses the arguments (without the program name). `None` when they are not a setup
    /// operation at all (so the caller tries the everyday ones).
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Option<Result<Self, DnsError>> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let (make, suffix): (fn(String) -> Self, &str) = match args.as_slice() {
            ["install-self", suffix] => (|suffix| Self::InstallSelf { suffix }, suffix),
            ["uninstall-self", suffix] => (|suffix| Self::UninstallSelf { suffix }, suffix),
            ["install-self" | "uninstall-self", ..] => {
                return Some(Err(DnsError::InvalidArguments(args.join(" "))))
            }
            _ => return None,
        };
        // The rule it would make is the validation: a special-use suffix, a loopback server.
        Some(
            rule_for(suffix, std::net::IpAddr::V4(DEFAULT_DNS_ADDR))
                .map(|rule| make(rule.namespace.trim_start_matches('.').to_owned())),
        )
    }

    /// The arguments that [`parse`](Self::parse) turns back into this operation.
    #[must_use]
    pub fn to_args(&self) -> Vec<String> {
        match self {
            Self::InstallSelf { suffix } => vec!["install-self".to_owned(), suffix.clone()],
            Self::UninstallSelf { suffix } => vec!["uninstall-self".to_owned(), suffix.clone()],
        }
    }
}

/// Why the elevated setup step did not finish. Its window is hidden and it is not to write
/// anywhere the unelevated side could point it at, so this is all it can say: its exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupFailure {
    /// Something not listed here.
    Failed,
    /// The arguments were refused.
    Refused,
    /// The folder in `Program Files` can be written by accounts that are not administrators.
    Folder,
    /// The program could not be copied.
    Copy,
    /// The copy did not check out against its manifest.
    Verify,
    /// The DNS rule or the hosts block could not be made or taken out.
    Rule,
    /// Another tool has a DNS rule for the suffix, and it is not touched.
    ForeignRule,
    /// Removal was asked of a program that is not the installed copy.
    NotInstalledCopy,
    /// The program being installed, or the folder it is in, can be written by other accounts.
    SourceOpen,
}

impl SetupFailure {
    /// The exit code of the elevated process.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Failed => 1,
            Self::Refused => 2,
            Self::Folder => 10,
            Self::Copy => 11,
            Self::Verify => 12,
            Self::Rule => 13,
            Self::ForeignRule => 14,
            Self::NotInstalledCopy => 15,
            Self::SourceOpen => 16,
        }
    }

    /// The failure an exit code means, if it means one.
    #[must_use]
    pub fn from_code(code: u32) -> Option<Self> {
        [
            Self::Failed,
            Self::Refused,
            Self::Folder,
            Self::Copy,
            Self::Verify,
            Self::Rule,
            Self::ForeignRule,
            Self::NotInstalledCopy,
            Self::SourceOpen,
        ]
        .into_iter()
        .find(|f| u32::from(f.code()) == code)
    }

    /// A sentence for the user.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Failed => "the elevated step failed; nothing more is known (try again, or run `crisol doctor`)",
            Self::Refused => "the elevated step refused its arguments",
            Self::Folder => "the Crisol folder in Program Files can be written by accounts that are not administrators, so the helper was not installed in it",
            Self::Copy => "the helper could not be copied into Program Files",
            Self::Verify => "the helper was copied but did not check out against its manifest; install it again",
            Self::Rule => "the DNS rule or Crisol's block of the hosts file could not be changed; the helper was left installed so you can try again",
            Self::ForeignRule => "another tool has a DNS rule for that suffix; Crisol leaves it alone, so none was made. Remove it yourself or use another suffix",
            Self::NotInstalledCopy => "removal only runs from the installed helper",
            Self::SourceOpen => "the helper being installed, or the folder it is in, can be changed by other accounts, so it was not given administrator rights; close the folder and try again",
        }
    }
}

/// What a setup operation did, in sentences.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SetupReport {
    /// One line per thing done.
    pub steps: Vec<String>,
}

/// The suffix of the local sites in this home (`test`): what `crisol.toml` says, or the default
/// when it cannot be read (the commands that need the suffix do not fail for that).
#[must_use]
pub fn suffix_of(paths: &crate::paths::CrisolPaths) -> String {
    crate::config::Config::load_from(paths)
        .map_or_else(|_| "test".to_owned(), |loaded| loaded.config.sites.tld)
}

/// Asks for the one-time installation of the helper and for its removal. The Windows
/// implementation shows the UAC prompt once for each.
pub trait SystemSetup: Send + Sync {
    /// Installs the helper and makes the NRPT rule for `suffix`.
    fn install(&self, suffix: &str) -> io::Result<SetupReport>;
    /// Removes the NRPT rule, the block of `hosts` and the helper.
    fn uninstall(&self, suffix: &str) -> io::Result<SetupReport>;
}

/// Setup that records what it was asked and does nothing, for tests.
#[derive(Debug, Default)]
pub struct FakeSetup {
    calls: std::sync::Mutex<Vec<String>>,
}

impl FakeSetup {
    /// A setup that has done nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What was asked, in order (`install test`, `uninstall test`).
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl SystemSetup for FakeSetup {
    fn install(&self, suffix: &str) -> io::Result<SetupReport> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("install {suffix}"));
        Ok(SetupReport {
            steps: vec![format!("installed the helper and the rule for .{suffix}")],
        })
    }

    fn uninstall(&self, suffix: &str) -> io::Result<SetupReport> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("uninstall {suffix}"));
        Ok(SetupReport {
            steps: vec![format!("removed the helper and the rule for .{suffix}")],
        })
    }
}

#[cfg(test)]
mod tests;
