//! Changing the password of a running data service without losing its data (`crisol db
//! reset-password`, V3 of the manual verification of phase 9).
//!
//! Each service has its own way, and the server has to be asked while it runs, with the old
//! password:
//!
//! | Service | How |
//! |---|---|
//! | MySQL, MariaDB | `ALTER USER 'root'@…` through the `mysql` client; the SQL goes in through its input, never on a command line |
//! | PostgreSQL | `ALTER ROLE postgres PASSWORD …` through `psql`, the old password in `PGPASSWORD` |
//! | Redis | `CONFIG SET requirepass` over a plain connection (the configuration it is started with is written again from the new secret) |
//! | MongoDB | `updateUser` over the wire protocol ([`crate::mongowire`]) |
//! | Garnet, Meilisearch | not possible while running: the password is read when the process starts |
//! | Mailpit, Memcached | they have no password |
//!
//! The client programs are run by whoever implements [`DbAdmin`] (the platform layer: a console
//! program started by a process without a console needs its window hidden). What to run is
//! decided here ([`client_call`]), so it can be tested without any server.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use super::{specs, DbKind, DbPlan};
use crate::secrets::Secret;

/// Changes the administrator's password on a server that is running.
pub trait DbAdmin: Send + Sync {
    /// Sets `new` as the password of the administrator account of the server `plan` describes,
    /// logging in with `old` (`None` if the server was made without one). Nothing changes if it
    /// fails.
    fn change_password(
        &self,
        plan: &DbPlan,
        old: Option<&Secret>,
        new: &Secret,
    ) -> Result<(), String>;
}

/// An admin that can do nothing: for a daemon built without one.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAdmin;

impl DbAdmin for NoAdmin {
    fn change_password(&self, _: &DbPlan, _: Option<&Secret>, _: &Secret) -> Result<(), String> {
        Err("this build cannot change passwords".to_owned())
    }
}

/// The services that need no program: Redis and MongoDB, over their own protocols. For the SQL
/// databases it says what is missing.
#[derive(Debug, Default, Clone, Copy)]
pub struct WireAdmin;

impl DbAdmin for WireAdmin {
    fn change_password(
        &self,
        plan: &DbPlan,
        old: Option<&Secret>,
        new: &Secret,
    ) -> Result<(), String> {
        match plan.kind {
            DbKind::Redis => redis_set_password(plan.port, old.map(Secret::expose), new.expose()),
            DbKind::Mongodb => match old {
                Some(old) => {
                    crate::mongowire::change_password(plan.port, old.expose(), new.expose())
                        .map_err(|e| e.to_string())
                }
                None => Err("this MongoDB was made without a login".to_owned()),
            },
            other => Err(format!(
                "{} needs its client program to change the password",
                other.service()
            )),
        }
    }
}

/// A client program to run: its arguments, its environment and what it is given as input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCall {
    /// The program.
    pub program: PathBuf,
    /// Its arguments (no password in them).
    pub args: Vec<String>,
    /// Environment variables to set (the old password for `psql`).
    pub env: Vec<(String, String)>,
    /// What to write to its standard input: the SQL, with the new password in it.
    pub stdin: String,
}

/// What to run to change the password of MySQL, MariaDB or PostgreSQL; `None` for the others.
#[must_use]
pub fn client_call(plan: &DbPlan, old: Option<&Secret>, new: &Secret) -> Option<ClientCall> {
    let new = new.expose();
    match plan.kind {
        DbKind::Mysql | DbKind::Mariadb => {
            let sql = if plan.kind == DbKind::Mysql {
                format!("ALTER USER 'root'@'localhost' IDENTIFIED BY '{new}';\n")
            } else {
                // The accounts MariaDB makes on Windows; one that does not exist is skipped.
                format!(
                    "ALTER USER IF EXISTS 'root'@'localhost' IDENTIFIED BY '{new}';\n\
                     ALTER USER IF EXISTS 'root'@'127.0.0.1' IDENTIFIED BY '{new}';\n\
                     ALTER USER IF EXISTS 'root'@'::1' IDENTIFIED BY '{new}';\n"
                )
            };
            Some(ClientCall {
                program: plan.version.path.join("bin").join("mysql.exe"),
                // The options file written at the start has the old password.
                args: vec![
                    format!(
                        "--defaults-extra-file={}",
                        specs::client_options(plan).display()
                    ),
                    "--batch".to_owned(),
                ],
                env: Vec::new(),
                stdin: sql,
            })
        }
        DbKind::Postgres => Some(ClientCall {
            program: plan.version.path.join("bin").join("psql.exe"),
            args: [
                "-h",
                "127.0.0.1",
                "-p",
                &plan.port.to_string(),
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-w",
                "-X",
                "-v",
                "ON_ERROR_STOP=1",
                "-f",
                "-",
            ]
            .map(str::to_owned)
            .to_vec(),
            env: vec![
                (
                    "PGPASSWORD".to_owned(),
                    old.map_or_else(String::new, |o| o.expose().to_owned()),
                ),
                // Not the user's own pgpass.conf: a file that does not exist.
                (
                    "PGPASSFILE".to_owned(),
                    plan.secret_path(".no-pgpass").display().to_string(),
                ),
            ],
            stdin: format!("ALTER ROLE postgres PASSWORD '{new}';\n"),
        }),
        _ => None,
    }
}

/// Sets `requirepass` on a running Redis (or anything that speaks its protocol and allows it).
pub fn redis_set_password(port: u16, old: Option<&str>, new: &str) -> Result<(), String> {
    const WAIT: Duration = Duration::from_secs(10);
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&address, WAIT)
        .map_err(|e| format!("cannot connect to 127.0.0.1:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(WAIT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(WAIT))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut writer = stream;
    let mut ask = |args: &[&str]| -> Result<(), String> {
        let mut request = format!("*{}\r\n", args.len());
        for arg in args {
            let _ = write!(request, "${}\r\n{arg}\r\n", arg.len());
        }
        writer
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if line.starts_with("+OK") {
            Ok(())
        } else {
            Err(line.trim().to_owned())
        }
    };
    if let Some(old) = old {
        ask(&["AUTH", old]).map_err(|e| format!("could not log in: {e}"))?;
    }
    ask(&["CONFIG", "SET", "requirepass", new])
}
