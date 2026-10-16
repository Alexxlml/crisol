//! Hand test of the one thing the automatic tests never do: put Crisol's local CA in the
//! **real** `Trusted Root Certification Authorities` store of the current user (ADR-0013).
//!
//! ```text
//! set CRISOL_HOME=D:\scratch\crisol-trust
//! cargo run -p crisol-platform --example trust_manual -- status
//! cargo run -p crisol-platform --example trust_manual -- install
//! cargo run -p crisol-platform --example trust_manual -- remove
//! cargo run -p crisol-platform --example trust_manual -- remove-stale
//! ```
//!
//! `CRISOL_HOME` is required, so that this never creates a CA in your real Crisol folder by
//! accident; use an empty scratch folder. `install` makes the CA there if needed, asks you to
//! type `yes`, and then Windows shows its own confirmation dialog. Everything it does is
//! undone by `remove` (the exact certificate) or `remove-stale` (older Crisol CAs). Nothing
//! else in the store is read for any other purpose or changed.

use std::io::Write;
use std::time::SystemTime;

use crisol_core::ca::{ensure_ca, load_ca};
use crisol_core::paths::CrisolPaths;
use crisol_core::trust::{remove_stale, status, trust_ca, untrust_ca, AddOutcome};
use crisol_platform::WindowsTrustStore;

fn main() {
    if let Err(message) = run() {
        eprintln!("trust_manual: {message}");
        std::process::exit(1);
    }
}

fn confirm(question: &str) -> bool {
    print!("{question} Type `yes` to go on: ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && answer.trim() == "yes"
}

fn run() -> Result<(), String> {
    let command = std::env::args().nth(1).unwrap_or_default();
    let home = std::env::var_os("CRISOL_HOME")
        .ok_or("set CRISOL_HOME to an empty scratch folder first")?;
    let paths = CrisolPaths::new(home).map_err(|e| e.to_string())?;
    let store = WindowsTrustStore::current_user_root().map_err(|e| e.to_string())?;
    let now = SystemTime::now();

    match command.as_str() {
        "status" => {
            let Some(ca) = load_ca(&paths, now).map_err(|e| e.to_string())? else {
                println!(
                    "There is no CA in {} yet; `install` makes one.",
                    paths.certs_dir().display()
                );
                return Ok(());
            };
            let state = status(&store, &ca).map_err(|e| e.to_string())?;
            println!("CA {} ({})", ca.info().common_name, ca.info().fingerprint);
            println!("  trusted by the current user: {}", state.trusted);
            for old in state.stale {
                println!("  older Crisol CA still trusted: {}", old.common_name);
            }
        }
        "install" => {
            let ensured = ensure_ca(&paths, "test", now).map_err(|e| e.to_string())?;
            let ca = ensured.ca;
            println!(
                "{} CA {} (valid for {:?}, SHA-256 {})",
                if ensured.created { "Made" } else { "Found" },
                ca.info().common_name,
                ca.info().permitted_dns,
                ca.info().fingerprint
            );
            if !confirm("This adds it to YOUR trusted root store (Windows will ask again).") {
                println!("Nothing was changed.");
                return Ok(());
            }
            match trust_ca(&store, &ca).map_err(|e| e.to_string())? {
                AddOutcome::Added => println!("Added. `status` should now say trusted: true."),
                AddOutcome::AlreadyThere => println!("It was already trusted."),
                AddOutcome::Declined => println!("Windows was told no. Nothing was changed."),
            }
        }
        "remove" => {
            let ca = load_ca(&paths, now)
                .map_err(|e| e.to_string())?
                .ok_or("there is no CA in that CRISOL_HOME")?;
            if !confirm(&format!(
                "This removes {} from your trusted roots.",
                ca.info().common_name
            )) {
                return Ok(());
            }
            let removed = untrust_ca(&store, &ca).map_err(|e| e.to_string())?;
            println!(
                "{}",
                if removed {
                    "Removed."
                } else {
                    "It was not trusted."
                }
            );
        }
        "remove-stale" => {
            let ca = load_ca(&paths, now)
                .map_err(|e| e.to_string())?
                .ok_or("there is no CA in that CRISOL_HOME")?;
            if !confirm("This removes older Crisol CAs (never the current one).") {
                return Ok(());
            }
            let removed = remove_stale(&store, &ca).map_err(|e| e.to_string())?;
            println!("Removed {} older Crisol CA(s).", removed.len());
        }
        _ => return Err("use: status | install | remove | remove-stale".to_owned()),
    }
    Ok(())
}
