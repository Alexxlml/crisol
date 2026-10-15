//! The `php-cgi` pool of one PHP version (F5.3).
//!
//! PHP on Windows has no php-fpm: each PHP version in use gets a pool of `php-cgi.exe`
//! workers, every worker listening for `FastCGI` on its own local port, and Nginx spreads the
//! requests over them (`upstream`, see [`crate::nginx`]). This module only builds the
//! [`ServiceSpec`] of one pool; running it, restarting workers and reporting a degraded pool
//! is [`crate::service::Service`] (ADR-0009):
//!
//! - a worker that exits with code 0 after `PHP_FCGI_MAX_REQUESTS` requests was recycled and
//!   is relaunched at once;
//! - any other exit is a crash of that worker, relaunched after a pause and counted;
//! - the pool is `Running` while at least one worker lives.
//!
//! Each worker gets the `php.ini` that was generated for its version (`PHPRC`), its own working
//! directory (the installation) and a shared `logs/php-<version>/runtime.log` for the output of
//! all of them.

use std::path::Path;

use crate::config::PhpConfig;
use crate::paths::{CrisolPaths, PathsError};
use crate::process::ProcessSpec;
use crate::service::{HealthCheck, ServiceSpec};

/// The name of the service (and of its log folder) for a PHP version: `php-8.4.26`.
#[must_use]
pub fn service_name(version: &str) -> String {
    format!("php-{version}")
}

/// What runs the workers.
fn php_cgi(install_dir: &Path) -> std::path::PathBuf {
    install_dir.join(format!("php-cgi{}", std::env::consts::EXE_SUFFIX))
}

/// The pool of `version`: one `php-cgi` per entry of `ports`, installed in `install_dir`.
///
/// `ports` come from [`crate::nginx::PhpPools`], the same ones the Nginx upstream lists, so
/// both sides always agree. All of them must be free to start (checked by the service, which
/// never frees a port by force), and the pool is ready when all of them listen.
pub fn pool_spec(
    paths: &CrisolPaths,
    config: &PhpConfig,
    version: &str,
    install_dir: &Path,
    ports: &[u16],
) -> Result<ServiceSpec, PathsError> {
    let name = service_name(version);
    let log = paths.runtime_log(&name)?;
    let ini_dir = paths.etc_for("php")?.join(version);
    let mut spec = ServiceSpec::new(name).pool();
    for &port in ports {
        spec = spec
            .process(
                ProcessSpec::new(php_cgi(install_dir))
                    .args(["-b", &format!("127.0.0.1:{port}")])
                    .cwd(install_dir)
                    // 0 means "never recycle".
                    .env("PHP_FCGI_MAX_REQUESTS", config.max_requests.to_string())
                    .env("PHPRC", &ini_dir)
                    .log_file(&log),
            )
            .port(port);
    }
    Ok(spec.health(HealthCheck::Ports(ports.to_vec())))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn spec(ports: &[u16]) -> ServiceSpec {
        let paths = CrisolPaths::new(if cfg!(windows) {
            r"C:\Crisol"
        } else {
            "/crisol"
        })
        .unwrap();
        let install = paths.bin_for("php", "8.4.26").unwrap();
        pool_spec(&paths, &PhpConfig::default(), "8.4.26", &install, ports).unwrap()
    }

    #[test]
    fn one_worker_per_port_each_on_its_own_loopback_port() {
        let spec = spec(&[9100, 9101, 9102]);
        assert_eq!(spec.name, "php-8.4.26");
        assert!(spec.pool);
        assert_eq!(spec.ports, [9100, 9101, 9102]);
        assert_eq!(spec.health, HealthCheck::Ports(vec![9100, 9101, 9102]));
        let args: Vec<&OsString> = spec.processes.iter().map(|p| &p.args[1]).collect();
        assert_eq!(args, ["127.0.0.1:9100", "127.0.0.1:9101", "127.0.0.1:9102"]);
        assert!(spec.processes.iter().all(|p| p.args[0] == "-b"));
    }

    #[test]
    fn the_workers_recycle_with_the_configured_count_and_read_the_generated_ini() {
        let spec = spec(&[9100]);
        let worker = &spec.processes[0];
        let env: Vec<(String, String)> = worker
            .env
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().replace('\\', "/"),
                )
            })
            .collect();
        assert!(env.contains(&("PHP_FCGI_MAX_REQUESTS".to_owned(), "500".to_owned())));
        let phprc = env.iter().find(|(k, _)| k == "PHPRC").unwrap();
        assert!(phprc.1.ends_with("etc/php/8.4.26"), "{phprc:?}");
    }

    #[test]
    fn the_program_and_the_working_directory_are_the_installation() {
        let spec = spec(&[9100]);
        let worker = &spec.processes[0];
        let program = worker.program.to_string_lossy().replace('\\', "/");
        assert!(program.ends_with("bin/php/8.4.26/php-cgi.exe") || program.ends_with("php-cgi"));
        assert_eq!(worker.cwd.as_deref(), worker.program.parent());
    }

    #[test]
    fn all_the_workers_share_one_log() {
        let spec = spec(&[9100, 9101]);
        let log = spec.processes[0].log_file.clone().unwrap();
        assert_eq!(spec.processes[1].log_file, Some(log.clone()));
        let text = log.to_string_lossy().replace('\\', "/");
        assert!(text.ends_with("logs/php-8.4.26/runtime.log"), "{text}");
    }

    #[test]
    fn a_pool_without_ports_is_an_empty_service() {
        assert_eq!(spec(&[]).processes, vec![]);
    }
}
