//! Vulnerability advisories of the catalog (ADR-0020).
//!
//! ```toml
//! [[advisory]]
//! id       = "PHP2Shell"
//! service  = "php"
//! affects  = ["8.3", "8.4", "8.5"]            # lines; without it, every version of the service
//! fixed_in = ["8.3.36", "8.4.27", "8.5.12"]   # first fixed version of each line; empty = no fix
//! summary  = "…"
//! url      = "https://…"
//! ```
//!
//! An advisory informs; it never blocks an install and it is not `end_of_life`.

use serde::Deserialize;

use super::{check_url, is_service_name, CatalogError};
use crate::version::{Version, VersionReq};

#[derive(Debug, Deserialize)]
pub(super) struct RawAdvisory {
    id: String,
    service: String,
    #[serde(default)]
    affects: Vec<String>,
    #[serde(default)]
    fixed_in: Vec<String>,
    summary: String,
    url: String,
}

/// A known vulnerability in some versions of a tool, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    /// Short name, unique in the catalog (`PHP2Shell`).
    pub id: String,
    /// The tool it is about.
    pub service: String,
    /// One sentence on what is wrong.
    pub summary: String,
    /// Where to read more (always `https`).
    pub url: String,
    /// Lines of the tool that are affected; empty means every version.
    affects: Vec<VersionReq>,
    /// First fixed version of each line; empty means there is no fix.
    fixed_in: Vec<Version>,
}

impl Advisory {
    /// Whether `version` of `service` is affected: it is in an affected line and no fix of
    /// that same line is at or below it.
    #[must_use]
    pub fn affects(&self, service: &str, version: &Version) -> bool {
        if self.service != service {
            return false;
        }
        let in_line = |line: &VersionReq| line.matches(version);
        if self.affects.is_empty() {
            return !self.fixed_in.iter().any(|fix| fix <= version);
        }
        self.affects
            .iter()
            .filter(|line| in_line(line))
            .any(|line| {
                !self
                    .fixed_in
                    .iter()
                    .any(|fix| line.matches(fix) && fix <= version)
            })
    }

    /// The versions that fix it, lowest first. Empty when there is no fix.
    #[must_use]
    pub fn fixed_in(&self) -> &[Version] {
        &self.fixed_in
    }

    /// The fix for the line `version` is in, if there is one.
    #[must_use]
    pub fn fix_for(&self, version: &Version) -> Option<&Version> {
        self.fixed_in.iter().find(|fix| {
            *fix > version
                && (self.affects.is_empty()
                    || self
                        .affects
                        .iter()
                        .any(|line| line.matches(version) && line.matches(fix)))
        })
    }
}

pub(super) fn validate(index: usize, raw: RawAdvisory) -> Result<Advisory, CatalogError> {
    let entry = format!("[[advisory]] #{} ({})", index + 1, raw.id);
    let invalid = |field: &'static str, reason: &str| CatalogError::Invalid {
        entry: entry.clone(),
        field,
        reason: reason.to_owned(),
    };
    if raw.id.is_empty() || raw.id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("id", "must be a short name without spaces"));
    }
    if !is_service_name(&raw.service) {
        return Err(invalid(
            "service",
            "must be lowercase letters, digits, `-` or `_`",
        ));
    }
    if raw.summary.trim().is_empty() || raw.summary.chars().any(char::is_control) {
        return Err(invalid("summary", "must be one line of text"));
    }
    check_url(&raw.url).map_err(|reason| invalid("url", reason))?;
    let affects = raw
        .affects
        .iter()
        .map(|line| VersionReq::parse(line))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("affects", "must be version lines such as `8.4`"))?;
    let mut fixed_in = raw
        .fixed_in
        .iter()
        .map(|version| Version::parse(version))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("fixed_in", "must be versions such as `8.4.27`"))?;
    fixed_in.sort();
    Ok(Advisory {
        id: raw.id,
        service: raw.service,
        summary: raw.summary,
        url: raw.url,
        affects,
        fixed_in,
    })
}
