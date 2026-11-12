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
use crisol_core::dns::{status as nrpt_status, NrptStatus};
use crisol_core::engine::{Deps, SystemChecks};
use crisol_core::fetch::HttpFetcher;
use crisol_core::hosts::{block_names, foreign_names, HostsFile};
use crisol_core::i18n::{finding, Fix, Localized, Message};
use crisol_core::msg;
use crisol_core::paths::CrisolPaths;
use crisol_core::setup::suffix_of;
use crisol_core::shimsetup::{describe_shadows, path_status};
use crisol_core::trust::{crisol_cas, status as trust_status};
use crisol_core::web::TcpConfigProbe;
use crisol_protocol::{Check, CheckAction, CheckStatus, LocalOp};

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

fn check(name: &Message, status: CheckStatus, detail: &Message, fix: Option<Fix>) -> Check {
    finding(status, name, detail, fix, None)
}

/// A check with a button: `fix` is the command the button is equivalent to, so the two cannot
/// say different things (ADR-0021 section 5).
fn check_do(name: &Message, status: CheckStatus, detail: &Message, action: CheckAction) -> Check {
    finding(
        status,
        name,
        detail,
        action.command().map(Fix::Command),
        Some(action),
    )
}

fn local(op: LocalOp) -> CheckAction {
    CheckAction::Local { op }
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

    fn slow_start_hint(&self, paths: &CrisolPaths) -> Option<Message> {
        Some(crate::diagnose::slow_start_hint(&paths.data_dir()))
    }

    fn slow_start_command(&self, paths: &CrisolPaths) -> Option<String> {
        Some(crate::diagnose::slow_start_command(&paths.data_dir()))
    }

    fn missing_runtime(&self, install_dir: &std::path::Path) -> Option<(String, String)> {
        crate::diagnose::missing_runtime(install_dir)
    }
}

fn path_check(paths: &CrisolPaths) -> Check {
    let name = msg!("doctor.name.path");
    let shims = paths.shims_dir();
    let status = match path_status(&RegistryUserPath::new(), paths, &|p| p.is_file()) {
        Ok(status) => status,
        Err(error) => {
            return check(
                &name,
                CheckStatus::Problem,
                &msg!("io.error", error = error),
                None,
            )
        }
    };
    let shadows = describe_shadows(&status.shadows);
    // What wins over the shims says more than whether they are in the PATH: the machine's Node
    // is ahead of them even when they are (ADR-0002, F10).
    let shadow_list = shadows
        .split_first()
        .map(|(first, rest)| Message::joined(first.clone(), rest.to_vec()));
    match (status.in_path, shadow_list) {
        (true, None) => check(
            &name,
            CheckStatus::Ok,
            &msg!("doctor.path.ok", folder = shims.display()),
            None,
        ),
        (true, Some(list)) => check(
            &name,
            CheckStatus::Warning,
            &msg!("doctor.path.shadowed", folder = shims.display(), list => list),
            Some(Fix::Command(
                crisol_core::shimsetup::ENV_POWERSHELL.to_owned(),
            )),
        ),
        (false, list) => {
            let detail = match list {
                None => msg!("doctor.path.missing", folder = shims.display()),
                Some(list) => msg!(
                    "doctor.path.missing_shadowed",
                    folder = shims.display(),
                    list => list
                ),
            };
            check_do(
                &name,
                CheckStatus::Warning,
                &detail,
                local(LocalOp::PathAdd),
            )
        }
    }
}

fn helper_check() -> Check {
    let name = msg!("doctor.name.helper");
    match HelperLocation::installed().and_then(|location| location.verify().map(|()| location)) {
        Ok(location) => check(
            &name,
            CheckStatus::Ok,
            &msg!("doctor.helper.ok", path = location.path().display()),
            None,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => check_do(
            &name,
            CheckStatus::Warning,
            &msg!("doctor.helper.missing"),
            local(LocalOp::SystemInstall),
        ),
        Err(error) => check_do(
            &name,
            CheckStatus::Problem,
            &msg!("io.error", error = error),
            local(LocalOp::SystemInstall),
        ),
    }
}

fn nrpt_check(tld: &str, daemon_running: bool) -> Check {
    let name = msg!("doctor.name.dns_rule");
    match nrpt_status(&PowerShellNrpt, tld) {
        // The rule alone says nothing about whether the answers arrive: ask Windows.
        Ok(NrptStatus::Ours(rule)) if daemon_running => match resolves_locally(tld) {
            Ok(()) => check(
                &name,
                CheckStatus::Ok,
                &msg!(
                    "doctor.dns.ok",
                    namespace = rule.namespace,
                    server = rule.name_server
                ),
                None,
            ),
            Err(why) => check(
                &name,
                CheckStatus::Problem,
                &msg!(
                    "doctor.dns.broken",
                    namespace = rule.namespace,
                    server = rule.name_server,
                    why => why
                ),
                Some(Fix::Say(msg!(
                    "doctor.dns.fix",
                    flush = "ipconfig /flushdns",
                    doctor = "crisol doctor"
                ))),
            )
            .with_action(CheckAction::Copy {
                text: "ipconfig /flushdns".to_owned(),
            }),
        },
        // Without the daemon nobody answers: programs wait and then give up.
        Ok(NrptStatus::Ours(rule)) => check_do(
            &name,
            CheckStatus::Warning,
            &msg!(
                "doctor.dns.stopped",
                namespace = rule.namespace,
                server = rule.name_server
            ),
            CheckAction::start(),
        ),
        Ok(NrptStatus::Absent) => check(
            &name,
            CheckStatus::Warning,
            &msg!("doctor.dns.absent", tld = tld),
            None,
        ),
        Ok(NrptStatus::Foreign(rule)) => check(
            &name,
            CheckStatus::Warning,
            &msg!(
                "doctor.dns.foreign",
                namespace = rule.namespace,
                server = rule.name_server,
                comment = rule.comment
            ),
            None,
        ),
        Err(error) => check(
            &name,
            CheckStatus::Problem,
            &msg!("io.error", error = error),
            None,
        ),
    }
}

/// Resolves a name under `.tld` that no site has, through the resolver every program uses, and
/// says what went wrong if it does not come back as this computer.
fn resolves_locally(tld: &str) -> Result<(), Message> {
    use std::net::ToSocketAddrs as _;
    let probe = format!("crisol-doctor.{tld}");
    let addresses: Vec<std::net::IpAddr> = (probe.as_str(), 80)
        .to_socket_addrs()
        .map_err(|e| msg!("doctor.dns.no_resolve", probe = &probe, error = e))?
        .map(|a| a.ip())
        .collect();
    if !addresses.is_empty() && addresses.iter().all(std::net::IpAddr::is_loopback) {
        Ok(())
    } else {
        let list: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        Err(msg!(
            "doctor.dns.wrong_address",
            probe = probe,
            list = list.join(", ")
        ))
    }
}

fn hosts_check(tld: &str) -> Check {
    let name = msg!("doctor.name.hosts_block");
    let text = match FileHosts::system().and_then(|file| file.read()) {
        Ok(text) => text,
        Err(error) => {
            return check(
                &name,
                CheckStatus::Warning,
                &msg!("doctor.hosts.cannot_read", error = error),
                None,
            )
        }
    };
    let (names, foreign) =
        match block_names(&text).and_then(|n| Ok((n, foreign_names(&text, tld)?))) {
            Ok(found) => found,
            Err(error) => {
                return check(
                    &name,
                    CheckStatus::Problem,
                    &msg!("io.error", error = error),
                    None,
                )
            }
        };
    let ours = if names.is_empty() {
        msg!("doctor.hosts.none")
    } else {
        msg!(
            "doctor.hosts.names",
            count = names.len(),
            list = names.join(", ")
        )
    };
    // Whether one of them is a site of Crisol's is the engine's to say (`site names`).
    let detail = if foreign.is_empty() {
        ours
    } else {
        msg!(
            "doctor.hosts.with_foreign",
            ours => ours,
            count = foreign.len(),
            tld = tld
        )
    };
    check(&name, CheckStatus::Ok, &detail, None)
}

fn trust_check(paths: &CrisolPaths) -> Check {
    let name = msg!("doctor.name.local_ca");
    let ca = match load_ca(paths, &WindowsPrivateDirs::new(), SystemTime::now()) {
        Ok(Some(ca)) => ca,
        // With no CA in `certs/`, Windows may still trust one of an earlier home (deleted
        // files, a new home on a computer that had Crisol): nothing signs with it any more,
        // and `crisol untrust --stale` takes every Crisol CA out when there is no current one.
        Ok(None) => {
            let left = WindowsTrustStore::current_user_root()
                .and_then(|store| crisol_cas(&store))
                .unwrap_or_default();
            return if left.is_empty() {
                check(&name, CheckStatus::Ok, &msg!("doctor.ca.none_yet"), None)
            } else {
                check_do(
                    &name,
                    CheckStatus::Warning,
                    &msg!(
                        "doctor.ca.only_old",
                        list = left
                            .iter()
                            .map(|c| c.common_name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    local(LocalOp::UntrustStale),
                )
            };
        }
        Err(error) => {
            return check(&name, CheckStatus::Problem, &error.message(), None);
        }
    };
    let store = match WindowsTrustStore::current_user_root() {
        Ok(store) => store,
        Err(error) => {
            return check(
                &name,
                CheckStatus::Warning,
                &msg!("doctor.ca.cannot_open", error = error),
                None,
            )
        }
    };
    let common_name = ca.info().common_name.clone();
    match trust_status(&store, &ca) {
        Ok(state) if !state.trusted => check_do(
            &name,
            CheckStatus::Warning,
            &msg!("doctor.ca.not_trusted", name = common_name),
            local(LocalOp::Trust),
        ),
        // A CA that was replaced is still trusted until it is taken out (ADR-0013).
        Ok(state) if !state.stale.is_empty() => check_do(
            &name,
            CheckStatus::Warning,
            &msg!(
                "doctor.ca.trusted_with_old",
                name = common_name,
                list = state
                    .stale
                    .iter()
                    .map(|c| c.common_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            local(LocalOp::UntrustStale),
        ),
        Ok(_) => check(
            &name,
            CheckStatus::Ok,
            &msg!("doctor.ca.trusted", name = common_name),
            None,
        ),
        Err(error) => check(
            &name,
            CheckStatus::Problem,
            &msg!("io.error", error = error),
            None,
        ),
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
            assert!(
                check.action_agrees_with_fix(),
                "the button and the fix differ: {check:?}"
            );
            assert!(!check.detail.contains("  "), "{check:?}");
            // Nothing is left to the English of the code: every finding has its key and data.
            assert!(
                check.name_text.is_some() && check.detail_text.is_some(),
                "{check:?}"
            );
            for language in crisol_core::i18n::Language::all() {
                let said = crisol_core::i18n::Message::from(check.detail_text.as_ref().unwrap())
                    .render(language);
                assert!(!said.contains('{'), "{}: {said}", language.code());
            }
        }
        // Nothing was made in the home by looking.
        assert!(!paths.certs_dir().join("ca.pem").exists());
    }
}
