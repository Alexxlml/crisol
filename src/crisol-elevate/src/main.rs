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
use crisol_platform::{flush_dns, restrict_dll_search, FileHosts, PowerShellNrpt};

fn main() -> ExitCode {
    // From here on, DLLs are loaded from System32 only (the static imports are limited at link
    // time, see build.rs).
    if let Err(error) = restrict_dll_search() {
        eprintln!("crisol-elevate: could not restrict where DLLs are loaded from: {error}");
        return ExitCode::FAILURE;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
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
