//! crisol-elevate: the only part of Crisol that runs as an administrator (ADR-0014).
//!
//! It does four things and nothing else, chosen by its arguments:
//!
//! ```text
//! crisol-elevate nrpt-add <suffix> <loopback ip>
//! crisol-elevate nrpt-remove <suffix>
//! crisol-elevate hosts-set <name>...
//! crisol-elevate hosts-clear
//! ```
//!
//! The arguments are validated by `crisol_core::dns::ElevatedOp` (a special-use suffix, a
//! loopback server), and a rule that another tool made is never replaced or removed. It reads
//! no files, opens no sockets and takes no other input. Exit codes: 0 done, 2 refused (bad
//! arguments or a rule that is not ours), 1 the change failed.

use std::process::ExitCode;

use crisol_core::dns::{DnsError, ElevatedOp};
use crisol_core::hosts::DirectHosts;
use crisol_core::setup::SetupOp;
use crisol_platform::{
    flush_dns, install_self, restrict_dll_search, uninstall_self, FileHosts, PowerShellNrpt,
};

/// The exit code of a setup operation: 0, or the number that says what failed.
fn finish(
    outcome: &Result<crisol_core::setup::SetupReport, crisol_core::setup::SetupFailure>,
) -> ExitCode {
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("crisol-elevate: {}", failure.message());
            ExitCode::from(failure.code())
        }
    }
}

fn main() -> ExitCode {
    // From here on, DLLs are loaded from System32 only (the static imports are limited at link
    // time, see build.rs).
    if let Err(error) = restrict_dll_search() {
        eprintln!("crisol-elevate: could not restrict where DLLs are loaded from: {error}");
        return ExitCode::FAILURE;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The one-time installation and its removal come first; everything else is an everyday
    // operation on the NRPT or the hosts file.
    if let Some(parsed) = SetupOp::parse(&args) {
        return match parsed {
            Err(error) => {
                eprintln!("crisol-elevate: {error}");
                ExitCode::from(2)
            }
            Ok(SetupOp::InstallSelf { suffix }) => finish(&install_self(&suffix)),
            Ok(SetupOp::UninstallSelf { suffix }) => finish(&uninstall_self(&suffix)),
        };
    }
    let op = match ElevatedOp::parse(&args) {
        Ok(op) => op,
        Err(error) => {
            eprintln!("crisol-elevate: {error}");
            return ExitCode::from(2);
        }
    };
    let system_hosts = match FileHosts::system() {
        Ok(file) => file,
        Err(error) => {
            eprintln!("crisol-elevate: {error}");
            return ExitCode::FAILURE;
        }
    };
    let hosts = DirectHosts::new(system_hosts);
    match op.apply(&PowerShellNrpt, &hosts) {
        Ok(()) => {
            if matches!(op, ElevatedOp::HostsSet { .. } | ElevatedOp::HostsClear) {
                let _ = flush_dns(); // best effort: the change is made either way
            }
            ExitCode::SUCCESS
        }
        Err(
            error @ (DnsError::Foreign { .. }
            | DnsError::NotSpecialUse(_)
            | DnsError::NotLoopback(_)
            | DnsError::InvalidSuffix(_)
            | DnsError::InvalidAddress(_)
            | DnsError::InvalidArguments(_)
            | DnsError::Hosts(_)),
        ) => {
            eprintln!("crisol-elevate: {error}");
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("crisol-elevate: {error}");
            ExitCode::FAILURE
        }
    }
}
