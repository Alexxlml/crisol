//! The real dependencies of the engine on Windows, and the checks of `doctor` that need the
//! operating system (F8.2, F8.3).
//!
//! [`default_deps`] is what both the daemon and the command line (when it works without the
//! daemon) are built with, so that a command gives the same answer either way.
//! [`WindowsSystemChecks`] only reads: the user `PATH`, whether the privileged helper is
//! installed and trustworthy, the NRPT rule, the managed block of `hosts`, the local CA and the
//! trust store. It changes nothing.

use std::fmt::Write as _;
use std::io;
use std::sync::Arc;
use std::time::SystemTime;

use crisol_core::ca::load_ca;
use crisol_core::catalog::{Catalog, Target};
use crisol_core::dns::{status as nrpt_status, NrptStatus};
use crisol_core::engine::{Deps, SystemChecks};
use crisol_core::fetch::HttpFetcher;
use crisol_core::hosts::{block_names, foreign_names, HostsFile};
use crisol_core::paths::CrisolPaths;
use crisol_core::setup::suffix_of;
use crisol_core::shimsetup::{describe_shadows, path_status};
use crisol_core::trust::status as trust_status;
use crisol_core::web::TcpConfigProbe;
use crisol_protocol::{Check, CheckStatus};

use crate::{
    FileHosts, HelperLocation, JobObjectSupervisor, PowerShellNrpt, RegistryAutostart,
    RegistryUserPath, WindowsDbAdmin, WindowsFileOwners, WindowsPortProbe, WindowsPrivateDirs,
    WindowsRootCertificates, WindowsTrustStore,
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
        roots: Arc::new(WindowsRootCertificates),
        config_probe: Arc::new(TcpConfigProbe),
        db_admin: Arc::new(WindowsDbAdmin),
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

impl SystemChecks for WindowsSystemChecks {
    fn checks(&self, paths: &CrisolPaths) -> Vec<Check> {
        let tld = suffix_of(paths);
        let daemon_running = crate::pipe_name_for_user(paths.home())
            .is_ok_and(|name| crate::connect_to_daemon(&name).is_ok());
        let mut checks = vec![
            path_check(paths),
            helper_check(),
            nrpt_check(&tld, daemon_running),
            hosts_check(&tld),
            trust_check(paths),
            crate::diagnose::vc_runtime_check(),
            crate::diagnose::ipv6_check(),
        ];
        checks.extend(crisol_core::autostart::check(
            &RegistryAutostart::new(),
            &WindowsPrivateDirs::new(),
            daemon_running,
        ));
        checks
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

    fn hosts_content(&self) -> Option<io::Result<String>> {
        Some(FileHosts::system().and_then(|file| file.read()))
    }

    fn explain_exit(&self, code: i32) -> Option<String> {
        crate::diagnose::explain_exit(code)
    }

    fn slow_start_hint(&self, paths: &CrisolPaths) -> Option<String> {
        Some(crate::diagnose::slow_start_hint(&paths.data_dir()))
    }
}

fn path_check(paths: &CrisolPaths) -> Check {
    let shims = paths.shims_dir();
    let status = match path_status(&RegistryUserPath::new(), paths, &|p| p.is_file()) {
        Ok(status) => status,
        Err(error) => return check("PATH", CheckStatus::Problem, error.to_string(), None),
    };
    let shadows = describe_shadows(&status.shadows);
    // What wins over the shims says more than whether they are in the PATH: the machine's Node
    // is ahead of them even when they are (ADR-0002, F10).
    match (status.in_path, shadows.is_empty()) {
        (true, true) => check(
            "PATH",
            CheckStatus::Ok,
            format!("{} is in your user PATH", shims.display()),
            None,
        ),
        (true, false) => check(
            "PATH",
            CheckStatus::Warning,
            format!(
                "{} is in your user PATH, but other programs come before it: {}",
                shims.display(),
                shadows.join("; ")
            ),
            Some(crisol_core::shimsetup::ENV_POWERSHELL),
        ),
        (false, _) => {
            let mut detail = format!(
                "{} is not in your user PATH, so `php` and `node` in a terminal are not Crisol's",
                shims.display()
            );
            if !shadows.is_empty() {
                let _ = write!(
                    detail,
                    "; and once it is, other programs still come before it: {}",
                    shadows.join("; ")
                );
            }
            check(
                "PATH",
                CheckStatus::Warning,
                detail,
                Some("crisol path add"),
            )
        }
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

fn nrpt_check(tld: &str, daemon_running: bool) -> Check {
    let name = "DNS rule";
    match nrpt_status(&PowerShellNrpt, tld) {
        // The rule alone says nothing about whether the answers arrive: ask Windows.
        Ok(NrptStatus::Ours(rule)) if daemon_running => match resolves_locally(tld) {
            Ok(()) => check(
                name,
                CheckStatus::Ok,
                format!(
                    "{} goes to {}, and Windows gets this computer's address for its names",
                    rule.namespace, rule.name_server
                ),
                None,
            ),
            Err(why) => check(
                name,
                CheckStatus::Problem,
                format!(
                    "{} goes to {}, but {why}; a VPN or a DNS client that ignores the rule is the usual cause",
                    rule.namespace, rule.name_server
                ),
                Some("ipconfig /flushdns, then crisol doctor again"),
            ),
        },
        // Without the daemon nobody answers: programs wait and then give up.
        Ok(NrptStatus::Ours(rule)) => check(
            name,
            CheckStatus::Warning,
            format!(
                "{} goes to {}, which does not answer while the daemon is stopped, so the names do not resolve",
                rule.namespace, rule.name_server
            ),
            Some("crisol start"),
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

/// Resolves a name under `.tld` that no site has, through the resolver every program uses, and
/// says what went wrong if it does not come back as this computer.
fn resolves_locally(tld: &str) -> Result<(), String> {
    use std::net::ToSocketAddrs as _;
    let probe = format!("crisol-doctor.{tld}");
    let addresses: Vec<std::net::IpAddr> = (probe.as_str(), 80)
        .to_socket_addrs()
        .map_err(|e| format!("`{probe}` does not resolve ({e})"))?
        .map(|a| a.ip())
        .collect();
    if !addresses.is_empty() && addresses.iter().all(std::net::IpAddr::is_loopback) {
        Ok(())
    } else {
        let list: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        Err(format!(
            "`{probe}` resolves to {}, not to this computer",
            list.join(", ")
        ))
    }
}

fn hosts_check(tld: &str) -> Check {
    let name = "hosts block";
    let text = match FileHosts::system().and_then(|file| file.read()) {
        Ok(text) => text,
        Err(error) => {
            return check(
                name,
                CheckStatus::Warning,
                format!("cannot read the hosts file: {error}"),
                None,
            )
        }
    };
    let (names, foreign) =
        match block_names(&text).and_then(|n| Ok((n, foreign_names(&text, tld)?))) {
            Ok(found) => found,
            Err(error) => return check(name, CheckStatus::Problem, error.to_string(), None),
        };
    let ours = if names.is_empty() {
        "Crisol has no names in the hosts file".to_owned()
    } else {
        format!(
            "{} name(s) in Crisol's block: {}",
            names.len(),
            names.join(", ")
        )
    };
    // Whether one of them is a site of Crisol's is the engine's to say (`site names`).
    let detail = if foreign.is_empty() {
        ours
    } else {
        format!(
            "{ours}; {} other name(s) under .{tld} are defined outside it by other tools",
            foreign.len()
        )
    };
    check(name, CheckStatus::Ok, detail, None)
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
        Ok(state) if !state.trusted => check(
            name,
            CheckStatus::Warning,
            format!(
                "{} is not trusted by Windows: browsers warn about secured sites, and PHP streams \
                 (`file_get_contents`) refuse them although cURL in PHP accepts them",
                ca.info().common_name
            ),
            Some("crisol trust"),
        ),
        // A CA that was replaced is still trusted until it is taken out (ADR-0013).
        Ok(state) if !state.stale.is_empty() => check(
            name,
            CheckStatus::Warning,
            format!(
                "{} is trusted by Windows, and so are older Crisol CAs that nothing uses: {}",
                ca.info().common_name,
                state
                    .stale
                    .iter()
                    .map(|c| c.common_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some("crisol untrust --stale"),
        ),
        Ok(_) => check(
            name,
            CheckStatus::Ok,
            format!("{} is trusted by Windows", ca.info().common_name),
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
                "local CA",
                "Visual C++ runtime",
                "IPv6 loopback"
            ]
        );
        for check in &checks {
            assert!(!check.detail.contains("  "), "{check:?}");
        }
        // Nothing was made in the home by looking.
        assert!(!paths.certs_dir().join("ca.pem").exists());
    }
}
