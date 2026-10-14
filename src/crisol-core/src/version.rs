//! Versions of the tools Crisol installs, and requests such as `8.3`.
//!
//! `semver` is not used because it demands three numeric parts, while real tools publish
//! `8.4`, `5.0.14` or `1.2.3.4`. A [`Version`] is dot-separated numbers with an optional
//! pre-release suffix (`8.5.0-rc1`). Trailing zeros do not matter: `8.4` equals `8.4.0`.
//!
//! A [`VersionReq`] is a prefix: `8.3` matches `8.3.0`, `8.3.12`, but not `8.4.0` or `8.30.1`.
//! Pre-releases are only matched when asked for exactly.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Why a version or request could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a valid version: use numbers separated by dots, optionally followed by `-` and a pre-release tag")]
pub struct VersionError(pub String);

/// An installable version.
#[derive(Debug, Clone)]
pub struct Version {
    parts: Vec<u64>,
    pre: Option<String>,
    text: String,
}

impl Version {
    /// Parses `8.4.21`, `8.4` or `8.5.0-rc1`.
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        let (numbers, pre) = match text.split_once('-') {
            Some((numbers, pre)) => (numbers, Some(pre)),
            None => (text, None),
        };
        let parts = parse_numbers(numbers).ok_or_else(|| VersionError(text.to_owned()))?;
        if let Some(pre) = pre {
            let valid = !pre.is_empty()
                && pre.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
                && !pre.starts_with('.')
                && !pre.ends_with('.')
                && !pre.contains("..");
            if !valid {
                return Err(VersionError(text.to_owned()));
            }
        }
        Ok(Self {
            parts,
            pre: pre.map(str::to_owned),
            text: text.to_owned(),
        })
    }

    /// The numeric parts, e.g. `[8, 4, 21]`.
    #[must_use]
    pub fn parts(&self) -> &[u64] {
        &self.parts
    }

    /// Whether this is a pre-release (`8.5.0-rc1`).
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }

    /// The first part (`8` for `8.4.21`): used to split data by major version.
    #[must_use]
    pub fn major(&self) -> u64 {
        self.parts.first().copied().unwrap_or(0)
    }

    /// The numeric parts without trailing zeros, the identity used for equality.
    fn significant(&self) -> &[u64] {
        let end = self
            .parts
            .iter()
            .rposition(|p| *p != 0)
            .map_or(0, |i| i + 1);
        &self.parts[..end]
    }
}

fn parse_numbers(text: &str) -> Option<Vec<u64>> {
    if text.is_empty() {
        return None;
    }
    text.split('.')
        .map(|part| {
            let digits = !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
            // No leading zeros ("08"): they are ambiguous and never part of a release number.
            let canonical = part == "0" || !part.starts_with('0');
            if digits && canonical {
                part.parse().ok()
            } else {
                None
            }
        })
        .collect()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.significant().hash(state);
        self.pre.hash(state);
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let longest = self.parts.len().max(other.parts.len());
        for i in 0..longest {
            let a = self.parts.get(i).copied().unwrap_or(0);
            let b = other.parts.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                different => return different,
            }
        }
        // A release is newer than any of its pre-releases.
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => compare_tags(a, b),
        }
    }
}

/// Orders pre-release tags piece by piece: numbers numerically (`rc2 < rc10`), text by
/// letters.
fn compare_tags(a: &str, b: &str) -> Ordering {
    let split = |s: &str| -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match out.last_mut() {
                Some((is_digit, text)) if *is_digit == digit => text.push(c),
                _ => out.push((digit, c.to_string())),
            }
        }
        out
    };
    let (a, b) = (split(a), split(b));
    for ((a_digit, a_text), (b_digit, b_text)) in a.iter().zip(&b) {
        let order = if *a_digit && *b_digit {
            let (a_num, b_num): (u64, u64) =
                (a_text.parse().unwrap_or(0), b_text.parse().unwrap_or(0));
            a_num.cmp(&b_num)
        } else {
            a_text.cmp(b_text)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// A request for a version: `8`, `8.3`, `8.3.12`, or an exact pre-release `8.5.0-rc1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReq {
    parts: Vec<u64>,
    pre: Option<String>,
}

impl VersionReq {
    /// Parses a request. Same syntax as [`Version::parse`].
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        let version = Version::parse(text)?;
        Ok(Self {
            parts: version.parts,
            pre: version.pre,
        })
    }

    /// Whether `version` satisfies the request.
    #[must_use]
    pub fn matches(&self, version: &Version) -> bool {
        match &self.pre {
            Some(_) => {
                // Exactly that pre-release.
                version.pre == self.pre && version.significant() == trim_zeros(&self.parts)
            }
            None => {
                !version.is_prerelease()
                    && self
                        .parts
                        .iter()
                        .enumerate()
                        .all(|(i, p)| version.parts.get(i).copied().unwrap_or(0) == *p)
            }
        }
    }
}

fn trim_zeros(parts: &[u64]) -> &[u64] {
    let end = parts.iter().rposition(|p| *p != 0).map_or(0, |i| i + 1);
    &parts[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn req(text: &str) -> VersionReq {
        VersionReq::parse(text).unwrap()
    }

    #[test]
    fn parses_real_world_versions() {
        for text in [
            "8.4.21",
            "8.4",
            "8",
            "5.0.14",
            "1.2.3.4",
            "22.11.0",
            "8.5.0-rc1",
            "0.0.1",
        ] {
            assert_eq!(v(text).to_string(), text);
        }
        assert_eq!(v("8.4.21").parts(), [8, 4, 21]);
        assert_eq!(v("8.4.21").major(), 8);
        assert!(v("8.5.0-rc1").is_prerelease() && !v("8.5.0").is_prerelease());
    }

    #[test]
    fn rejects_things_that_are_not_versions() {
        for text in [
            "",
            ".",
            "8.",
            ".8",
            "8..4",
            "a.b",
            "8.x",
            "08.1",
            "8.04",
            "v8.4",
            "8.4.21-",
            "8.4-rc..1",
            "8.4-.rc",
            "8.4 ",
            " 8.4",
            "-rc1",
            "8.4+build",
            "../8",
            "8/4",
            "8.4-rc 1",
        ] {
            assert!(Version::parse(text).is_err(), "`{text}` must be rejected");
        }
        let err = Version::parse("nope").unwrap_err();
        assert!(err.to_string().contains("`nope`"));
    }

    #[test]
    fn numbers_compare_numerically_not_as_text() {
        assert!(v("8.10.0") > v("8.9.9"));
        assert!(v("8.4.21") > v("8.4.3"));
        assert!(v("10.0.0") > v("9.99.99"));
        assert!(v("1.2.3.4") > v("1.2.3"));
        assert!(v("1.2.3") < v("1.2.3.1"));
    }

    #[test]
    fn trailing_zeros_do_not_matter() {
        assert_eq!(v("8.4"), v("8.4.0"));
        assert_eq!(v("8"), v("8.0.0.0"));
        assert_ne!(v("8.4"), v("8.4.1"));

        let set: HashSet<Version> = [v("8.4"), v("8.4.0"), v("8.4.0.0")].into_iter().collect();
        assert_eq!(set.len(), 1, "equal versions must hash alike");
    }

    #[test]
    fn a_release_is_newer_than_its_prereleases() {
        assert!(v("8.5.0") > v("8.5.0-rc1"));
        assert!(v("8.5.0-rc1") < v("8.5.0-rc2"));
        assert!(v("8.5.0-rc2") < v("8.5.0-rc10"), "rc2 < rc10, numerically");
        assert!(v("8.5.0-beta1") < v("8.5.0-rc1"));
        assert!(v("8.5.0-rc1") > v("8.4.99"));
        assert!(v("8.5.0-rc1") < v("8.5.1"));
    }

    #[test]
    fn sorting_picks_the_right_latest() {
        let mut all = [
            v("8.3.12"),
            v("8.4.2"),
            v("8.10.0"),
            v("8.4.10"),
            v("8.5.0-rc1"),
        ];
        all.sort();
        let sorted: Vec<String> = all.iter().map(ToString::to_string).collect();
        assert_eq!(sorted, ["8.3.12", "8.4.2", "8.4.10", "8.5.0-rc1", "8.10.0"]);
    }

    #[test]
    fn a_request_is_a_prefix_on_whole_parts() {
        let r = req("8.3");
        assert!(r.matches(&v("8.3.0")));
        assert!(r.matches(&v("8.3.12")));
        assert!(r.matches(&v("8.3")));
        assert!(!r.matches(&v("8.4.0")));
        assert!(!r.matches(&v("8.30.1")), "8.3 must not match 8.30");
        assert!(!r.matches(&v("8")), "8 is 8.0, not 8.3");

        let r = req("8");
        assert!(r.matches(&v("8.0.1")) && r.matches(&v("8.99.0")));
        assert!(!r.matches(&v("9.0.0")) && !r.matches(&v("80.1")));

        let r = req("8.3.12");
        assert!(r.matches(&v("8.3.12")) && r.matches(&v("8.3.12.0")));
        assert!(!r.matches(&v("8.3.13")));
    }

    #[test]
    fn prereleases_only_match_when_requested_exactly() {
        assert!(!req("8.5").matches(&v("8.5.0-rc1")));
        assert!(!req("8").matches(&v("8.5.0-rc1")));
        assert!(req("8.5.0-rc1").matches(&v("8.5.0-rc1")));
        assert!(req("8.5-rc1").matches(&v("8.5.0-rc1")));
        assert!(!req("8.5.0-rc1").matches(&v("8.5.0-rc2")));
        assert!(!req("8.5.0-rc1").matches(&v("8.5.0")));
    }
}
