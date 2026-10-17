//! The real dependencies of the engine on Windows, and the checks of `doctor` that need the
//! operating system (F8.2, F8.3).
//!
//! [`default_deps`] is what both the daemon and the command line (when it works without the
//! daemon) are built with, so that a command gives the same answer either way.
//! [`WindowsSystemChecks`] only reads: the user `PATH`, whether the privileged helper is
//! installed and trustworthy, the NRPT rule, the managed block of `hosts`, the local CA and the
//! trust store. It changes nothing.

use std::io;
use std::sync::Arc;
use std::time::SystemTime;

use crisol_core::ca::load_ca;
use crisol_core::catalog::{Catalog, Target};
use crisol_core::config::Config;
use crisol_core::dns::{status as nrpt_status, NrptStatus};
use crisol_core::engine::{Deps, SystemChecks};
use crisol_core::fetch::HttpFetcher;
use crisol_core::hosts::{block_names, HostsFile};
use crisol_core::paths::CrisolPaths;
use crisol_core::trust::status as trust_status;
use crisol_core::userpath::shims_in_path;
use crisol_protocol::{Check, CheckStatus};

use crate::{
    FileHosts, HelperLocation, JobObjectSupervisor, PowerShellNrpt, RegistryUserPath,
    WindowsFileOwners, WindowsPortProbe, WindowsPrivateDirs, WindowsTrustStore,
};

/// The dependencies of the engine with the Windows implementations.
pub fn default_deps() -> io::Result<Deps> {
    let catalog = Catalog::embedded().map_err(|e| io::Error::other(e.to_string()))?;
    let target = Target::current()
        .ok_or_else(|| io::Error::other("this platform is not supported by Crisol"))?;
    Ok(Deps {
        supervisor: Arc::new(JobObjectSupervisor::new()),
        probe: Arc::new(WindowsPortProbe::new()),
        dirs: Arc::new(WindowsPrivateDirs::new()),
        fetcher: Arc::new(HttpFetcher::new()),
        owners: Arc::new(WindowsFileOwners),
        env: Arc::new(|name| std::env::var(name).ok()),
        system: Arc::new(WindowsSystemChecks),
        catalog,
        target,
    })
}

/// The checks of `doctor` that look at Windows.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsSystemChecks;

fn check(name: &str, status: CheckStatus, detail: impl Into<String>, fix: Option<&str>) -> Check {
    Check {
        name: name.to_owned(),
        status,
        detail: detail.into(),
        fix: fix.map(str::to_owned),
    }
}

/// The suffix of the local sites, `test` when the configuration cannot be read.
fn suffix_of(paths: &CrisolPaths) -> String {
    Config::load_from(paths).map_or_else(|_| "test".to_owned(), |loaded| loaded.config.sites.tld)
}

impl SystemChecks for WindowsSystemChecks {
    fn checks(&self, paths: &CrisolPaths) -> Vec<Check> {
        let tld = suffix_of(paths);
        vec![
            path_check(paths),
            helper_check(),
            nrpt_check(&tld),
            hosts_check(),
            trust_check(paths),
        ]
    }

    fn shutdown_warnings(&self, paths: &CrisolPaths) -> Vec<String> {
        let tld = suffix_of(paths);
        match nrpt_status(&PowerShellNrpt, &tld) {
            Ok(NrptStatus::Ours(_)) => vec![format!(
                "Windows is set to ask Crisol's DNS about .{tld}: those names do not resolve while the daemon is stopped, \
                 and programs may wait a moment before giving up. `crisol start` brings them back"
            )],
            _ => Vec::new(),
        }
    }
}

fn path_check(paths: &CrisolPaths) -> Check {
    match shims_in_path(&RegistryUserPath::new(), &paths.shims_dir()) {
        Ok(true) => check(
            "PATH",
            CheckStatus::Ok,
            format!("{} is in your user PATH", paths.shims_dir().display()),
            None,
        ),
        Ok(false) => check(
            "PATH",
            CheckStatus::Warning,
            format!(
                "{} is not in your user PATH, so `php` and `node` in a terminal are not Crisol's",
                paths.shims_dir().display()
            ),
            None,
        ),
        Err(error) => check("PATH", CheckStatus::Problem, error.to_string(), None),
    }
}

fn helper_check() -> Check {
    let name = "privileged helper";
    match HelperLocation::installed().and_then(|location| location.verify().map(|()| location)) {
        Ok(location) => check(
            name,
            CheckStatus::Ok,
            format!(
                "{} is installed and only administrators can change it",
                location.path().display()
            ),
            None,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => check(
            name,
            CheckStatus::Warning,
            "not installed: the names of the local sites cannot be set up automatically",
            Some("crisol system install"),
        ),
        Err(error) => check(
            name,
            CheckStatus::Problem,
            error.to_string(),
            Some("crisol system install"),
        ),
    }
}

fn nrpt_check(tld: &str) -> Check {
    let name = "DNS rule";
    match nrpt_status(&PowerShellNrpt, tld) {
        Ok(NrptStatus::Ours(rule)) => check(
            name,
            CheckStatus::Ok,
            format!("{} goes to {}", rule.namespace, rule.name_server),
            None,
        ),
        Ok(NrptStatus::Absent) => check(
            name,
            CheckStatus::Warning,
            format!("no rule sends .{tld} to Crisol's DNS; the names rely on the hosts file or are not set up"),
            None,
        ),
        Ok(NrptStatus::Foreign(rule)) => check(
            name,
            CheckStatus::Warning,
            format!(
                "another tool has a rule for {} (server {}, comment `{}`); Crisol leaves it alone",
                rule.namespace, rule.name_server, rule.comment
            ),
            None,
        ),
        Err(error) => check(name, CheckStatus::Problem, error.to_string(), None),
    }
}

fn hosts_check() -> Check {
    let name = "hosts block";
    let read = FileHosts::system().and_then(|file| file.read());
    match read.map(|text| block_names(&text)) {
        Ok(Ok(names)) if names.is_empty() => check(
            name,
            CheckStatus::Ok,
            "Crisol has no names in the hosts file",
            None,
        ),
        Ok(Ok(names)) => check(
            name,
            CheckStatus::Ok,
            format!(
                "{} name(s) in Crisol's block: {}",
                names.len(),
                names.join(", ")
            ),
            None,
        ),
        Ok(Err(error)) => check(name, CheckStatus::Problem, error.to_string(), None),
        Err(error) => check(
            name,
            CheckStatus::Warning,
            format!("cannot read the hosts file: {error}"),
            None,
        ),
    }
}

fn trust_check(paths: &CrisolPaths) -> Check {
    let name = "local CA";
    let ca = match load_ca(paths, &WindowsPrivateDirs::new(), SystemTime::now()) {
        Ok(Some(ca)) => ca,
        Ok(None) => {
            return check(
                name,
                CheckStatus::Ok,
                "no local CA yet: it is made when a site is secured",
                None,
            )
        }
        Err(error) => return check(name, CheckStatus::Problem, error.to_string(), None),
    };
    let store = match WindowsTrustStore::current_user_root() {
        Ok(store) => store,
        Err(error) => {
            return check(
                name,
                CheckStatus::Warning,
                format!("cannot open the trust store: {error}"),
                None,
            )
        }
    };
    match trust_status(&store, &ca) {
        Ok(state) if state.trusted => check(
            name,
            CheckStatus::Ok,
            format!("{} is trusted by Windows", ca.info().common_name),
            None,
        ),
        Ok(_) => check(
            name,
            CheckStatus::Warning,
            format!(
                "{} is not trusted by Windows: browsers warn about secured sites",
                ca.info().common_name
            ),
            None,
        ),
        Err(error) => check(name, CheckStatus::Problem, error.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_dependencies_can_be_built() {
        let deps = default_deps().unwrap();
        assert_ne!(deps.catalog.packages().len(), 0);
    }

    #[test]
    fn every_system_check_answers_without_changing_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let checks = WindowsSystemChecks.checks(&paths);
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "PATH",
                "privileged helper",
                "DNS rule",
                "hosts block",
                "local CA"
            ]
        );
        for check in &checks {
            assert!(!check.detail.contains("  "), "{check:?}");
        }
        // Nothing was made in the home by looking.
        assert!(!paths.certs_dir().join("ca.pem").exists());
    }
}
