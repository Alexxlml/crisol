//! Hand test of what the automatic tests never do: create (and remove) the **real** NRPT rule
//! for `.test` through the elevated helper, and answer queries from the local responder
//! (ADR-0014). Needs an interactive session: Windows shows its UAC prompt for each change.
//!
//! ```text
//! cargo build -p crisol-elevate
//! cargo run -p crisol-platform --example dns_manual -- status
//! cargo run -p crisol-platform --example dns_manual -- serve      (leave it running)
//! cargo run -p crisol-platform --example dns_manual -- enable     (UAC prompt)
//! Resolve-DnsName shop.test        (in another terminal; also try a browser and curl)
//! cargo run -p crisol-platform --example dns_manual -- disable    (UAC prompt)
//! cargo run -p crisol-platform --example dns_manual -- hosts-set   (UAC prompt; the fallback)
//! cargo run -p crisol-platform --example dns_manual -- hosts-clear (UAC prompt)
//! ```
//!
//! `enable` and `disable` only ever add or remove the rule marked `Crisol` for the suffix; a
//! rule made by anything else is reported and left alone. `disable` leaves the table as it was.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crisol_core::dns::{disable, enable, status, DnsServer, Zone, DEFAULT_DNS_ADDR};
use crisol_core::hosts::HostsManager;
use crisol_platform::{ElevatedHosts, ElevatedNrpt};

const SUFFIX: &str = "test";

fn main() {
    if let Err(message) = run() {
        eprintln!("dns_manual: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let command = std::env::args().nth(1).unwrap_or_default();
    let helper = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .ancestors()
        .nth(2)
        .map(|dir| dir.join("crisol-elevate.exe"))
        .ok_or("no target directory")?;
    let table = ElevatedNrpt::new(helper.clone());
    let hosts = ElevatedHosts::new(helper);
    let server = IpAddr::V4(DEFAULT_DNS_ADDR);
    match command.as_str() {
        "status" => println!("{:#?}", status(&table, SUFFIX).map_err(|e| e.to_string())?),
        "enable" => println!(
            "{:?}",
            enable(&table, SUFFIX, server).map_err(|e| e.to_string())?
        ),
        "disable" => println!(
            "removed: {}",
            disable(&table, SUFFIX).map_err(|e| e.to_string())?
        ),
        "hosts-set" => {
            let conflicts = hosts
                .set(&["manual-check.test".to_owned()])
                .map_err(|e| e.to_string())?;
            println!(
                "block names: {:?}, conflicts: {conflicts:?}",
                hosts.names().map_err(|e| e.to_string())?
            );
        }
        "hosts-clear" => {
            hosts.clear().map_err(|e| e.to_string())?;
            println!(
                "block names: {:?}",
                hosts.names().map_err(|e| e.to_string())?
            );
        }
        "serve" => {
            let zone = Zone::new(SUFFIX).map_err(|e| e.to_string())?;
            let dns =
                DnsServer::bind(SocketAddr::new(server, 53), zone).map_err(|e| e.to_string())?;
            println!("answering for .{SUFFIX} on {server}:53; press Enter to stop");
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let handle = std::thread::spawn(move || dns.run(&flag));
            let _ = std::io::stdin().read_line(&mut String::new());
            stop.store(true, Ordering::Relaxed);
            println!("answered {} queries", handle.join().unwrap_or(0));
        }
        _ => {
            return Err(
                "use: status | serve | enable | disable | hosts-set | hosts-clear".to_owned(),
            )
        }
    }
    Ok(())
}
