//! Windows implementation of [`DbAdmin`]: runs the client program of the database (`mysql.exe`,
//! `psql.exe`) with its window hidden, gives it the SQL through its standard input (the new
//! password is never in a command line) and reports why it failed. Redis and MongoDB are asked
//! over their own protocols by [`WireAdmin`].

use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crisol_core::dbservice::{client_call, ClientCall, DbAdmin, DbPlan, WireAdmin};
use crisol_core::secrets::Secret;

/// `CREATE_NO_WINDOW`: a console program started by a process with no console must not open one.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long a client may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Changes passwords with the programs of the installed database.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsDbAdmin;

impl DbAdmin for WindowsDbAdmin {
    fn change_password(
        &self,
        plan: &DbPlan,
        old: Option<&Secret>,
        new: &Secret,
    ) -> Result<(), String> {
        match client_call(plan, old, new) {
            Some(call) => run(&call),
            None => WireAdmin.change_password(plan, old, new),
        }
    }
}

fn run(call: &ClientCall) -> Result<(), String> {
    let mut command = Command::new(&call.program);
    command
        .args(&call.args)
        .envs(call.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", call.program.display()))?;
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(call.stdin.as_bytes())
            .map_err(|e| format!("cannot give the SQL to {}: {e}", call.program.display()))?;
    }
    let end = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if Instant::now() >= end => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not finish in {} seconds",
                    call.program.display(),
                    TIMEOUT.as_secs()
                ));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut message = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut message);
    }
    let first = message.lines().next().unwrap_or("").trim();
    Err(if first.is_empty() {
        format!("{} ended with {status}", call.program.display())
    } else {
        first.to_owned()
    })
}
