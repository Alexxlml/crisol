use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;

use clap::Parser;
use crisol_core::config::Config;
use crisol_core::dns::DEFAULT_DNS_ADDR;
use crisol_core::ipc::pipe_name;
use crisol_daemon::{run, DnsOptions, Options};
use crisol_platform::{default_deps, resolve_paths, WindowsPrivateDirs};

/// Crisol daemon: keeps services alive and serves the IPC.
///
/// You do not start it by hand: `crisol start` does. It uses the home in `CRISOL_HOME` (or
/// `C:\Crisol`) and listens on a named pipe that only your account can open.
#[derive(Parser)]
#[command(name = "crisold", version)]
struct Args {}

fn main() -> ExitCode {
    let _args = Args::parse();
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("crisold: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<(), String> {
    let paths = resolve_paths().map_err(|e| e.to_string())?;
    // A new home is made private; an existing one is never re-permissioned (ADR-0008), and the
    // operations that would run something from an open one refuse by themselves.
    paths
        .ensure_private_layout(&WindowsPrivateDirs::new())
        .map_err(|e| e.to_string())?;
    let deps = default_deps().map_err(|e| e.to_string())?;
    let suffix = Config::load_from(&paths)
        .map_or_else(|_| "test".to_owned(), |loaded| loaded.config.sites.tld);
    let options = Options {
        pipe: pipe_name(paths.home()),
        deps,
        dns: Some(DnsOptions {
            addr: SocketAddr::new(IpAddr::V4(DEFAULT_DNS_ADDR), 53),
            suffix,
        }),
        paths,
    };
    run(options).map_err(|e| e.to_string())
}
