//! crisol-elevate: the only part of Crisol that runs as an administrator (ADR-0014).
//!
//! It does two things and nothing else, chosen by its arguments:
//!
//! ```text
//! crisol-elevate nrpt-add <suffix> <loopback ip>
//! crisol-elevate nrpt-remove <suffix>
//! ```
//!
//! The arguments are validated by `crisol_core::dns::ElevatedOp` (a special-use suffix, a
//! loopback server), and a rule that another tool made is never replaced or removed. It reads
//! no files, opens no sockets and takes no other input. Exit codes: 0 done, 2 refused (bad
//! arguments or a rule that is not ours), 1 the change failed.

use std::process::ExitCode;

use crisol_core::dns::{DnsError, ElevatedOp};
use crisol_platform::PowerShellNrpt;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let op = match ElevatedOp::parse(&args) {
        Ok(op) => op,
        Err(error) => {
            eprintln!("crisol-elevate: {error}");
            return ExitCode::from(2);
        }
    };
    match op.apply(&PowerShellNrpt) {
        Ok(()) => ExitCode::SUCCESS,
        Err(
            error @ (DnsError::Foreign { .. }
            | DnsError::NotSpecialUse(_)
            | DnsError::NotLoopback(_)
            | DnsError::InvalidSuffix(_)),
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
