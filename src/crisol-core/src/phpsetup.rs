//! Keeps the generated PHP files in step with what is installed (review of the manual
//! verification of phase 9, V1).
//!
//! `phpini` and `cacert` know how to produce `etc/php/cacert.pem` and
//! `etc/php/<version>/php.ini`; this is the piece that decides **when**: before a pool starts,
//! after PHP is installed, when `[php]` changes and when the local CA appears. Order matters:
//! the CA bundle first (the `php.ini` only names it if it exists), then one `php.ini` per
//! version.
//!
//! Nothing here fails the caller. A file that cannot be written becomes a warning: PHP then
//! runs with whatever was there before, and the user is told.

use std::time::SystemTime;

use crate::ca::load_ca;
use crate::cacert::{write_php_cacert_with, RootCertificates};
use crate::config::PhpConfig;
use crate::manage::list_installed;
use crate::paths::{CrisolPaths, PrivateDirs};
use crate::phpini::{write_php_ini, PhpIniSettings};

/// What [`refresh_php`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhpRefresh {
    /// Versions whose `php.ini` was created or changed (their workers must restart to see it).
    pub changed: Vec<String>,
    /// Versions whose `php.ini` is in place, changed or not.
    pub ready: Vec<String>,
    /// Things the user may want to act on.
    pub warnings: Vec<String>,
    /// Facts that need no action; for the daemon's log and `doctor`, not for `start`.
    pub notes: Vec<String>,
}

/// The settings of the `php.ini` that come from `[php]` in `crisol.toml`.
#[must_use]
pub fn ini_settings(php: &PhpConfig) -> PhpIniSettings {
    PhpIniSettings {
        timezone: php.timezone.clone(),
        ..PhpIniSettings::default()
    }
}

/// Every installed PHP version.
#[must_use]
pub fn installed_php_versions(paths: &CrisolPaths) -> Vec<String> {
    list_installed(paths)
        .map(|list| {
            list.into_iter()
                .filter(|i| i.service == "php")
                .map(|i| i.version.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Writes `etc/php/cacert.pem` and the `php.ini` of each of `versions`. A file that is already
/// up to date is not touched.
pub fn refresh_php(
    paths: &CrisolPaths,
    php: &PhpConfig,
    versions: &[String],
    roots: &dyn RootCertificates,
    dirs: &dyn PrivateDirs,
) -> PhpRefresh {
    let mut result = PhpRefresh::default();
    if versions.is_empty() {
        return result;
    }

    // The local CA goes in the bundle even if Windows does not trust it (ADR-0013). A CA that
    // does not validate is left out, with its reason.
    let local_ca = match load_ca(paths, dirs, SystemTime::now()) {
        Ok(ca) => ca,
        Err(error) => {
            result
                .warnings
                .push(format!("the local CA is not in PHP's CA bundle: {error}"));
            None
        }
    };
    match write_php_cacert_with(paths, roots, local_ca.as_ref(), dirs) {
        Ok(Some(written)) => {
            result.warnings.extend(written.warnings);
            result.notes.extend(written.notes);
        }
        Ok(None) => result.warnings.push(
            "the CA bundle is not installed (`crisol install cacert`): PHP's cURL will not trust any HTTPS site"
                .to_owned(),
        ),
        Err(error) => result
            .warnings
            .push(format!("could not write the CA bundle for PHP: {error}")),
    }

    let settings = ini_settings(php);
    for version in versions {
        match write_php_ini(paths, version, &settings, dirs) {
            Ok(written) => {
                // The missing bundle is already said once, above.
                result.warnings.extend(
                    written
                        .warnings
                        .into_iter()
                        .filter(|w| !w.contains("no CA bundle")),
                );
                if written.changed {
                    result.changed.push(version.clone());
                }
                result.ready.push(version.clone());
            }
            Err(error) => result.warnings.push(format!(
                "could not write the php.ini of PHP {version}: {error}"
            )),
        }
    }
    result
}

#[cfg(test)]
mod tests;
