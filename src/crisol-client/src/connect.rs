//! Reaching the daemon: connect, start it when the call should, say hello, or fall back to
//! this process.

use std::time::{Duration, Instant};

use crisol_core::daemon::Daemon;
use crisol_core::engine::Engine;
use crisol_core::programs::{program_error, require_trusted_program};
use crisol_protocol::{
    Call, Check, CheckStatus, ClientError, ErrorCode, Reply, RpcError, PROTOCOL_VERSION,
};

use crate::context::Context;
use crate::link::{Link, Local};

/// A connection that said hello.
#[allow(missing_debug_implementations)] // holds a `dyn Link`
pub struct Opened {
    /// Where to send the calls.
    pub link: Box<dyn Link>,
    /// The version of the daemon that answered (or of this process, when there is no daemon).
    pub daemon_version: Option<String>,
}

/// The `hello` that opens every connection: the same protocol on both sides, or nothing is sent.
#[must_use]
pub fn hello(context: &Context) -> Call {
    Call::Hello {
        client: format!("{} {}", context.client, context.version),
        protocol: PROTOCOL_VERSION,
    }
}

/// Connects to the daemon, starting it if the call is one that should, and says hello. With no
/// daemon, and a call that does not need one, the answer comes from this process. `notice`
/// receives what is worth telling the user while it waits (`Starting the daemon...`).
pub fn open(
    context: &Context,
    call: &Call,
    notice: &mut dyn FnMut(&str),
) -> Result<Opened, RpcError> {
    let mut link = match (context.connect)() {
        Ok(link) => Some(link),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(RpcError::new(ErrorCode::Io, error.to_string())),
    };
    if link.is_none() && matches!(call, Call::Start { .. } | Call::Restart { .. }) {
        link = Some(start_daemon(context, notice)?);
    }
    let mut link = match link {
        Some(link) => link,
        None => local(context, call)?,
    };
    match link.call(&hello(context), &mut |_| {}) {
        Ok(reply) => Ok(Opened {
            link,
            daemon_version: match reply {
                Reply::Hello(info) => Some(info.daemon),
                _ => None,
            },
        }),
        Err(error) => Err(client_error(error)),
    }
}

/// Makes `call` the way a button does: connects (starting the daemon when the call should),
/// says hello and sends it. The error is the one to show, with its `fix` when it has one.
pub fn call(
    context: &Context,
    call: &Call,
    notice: &mut dyn FnMut(&str),
) -> Result<Reply, RpcError> {
    let mut opened = open(context, call, notice)?;
    opened.link.call(call, &mut |_| {}).map_err(client_error)
}

/// This process answering for a daemon that is not there.
fn local(context: &Context, call: &Call) -> Result<Box<dyn Link>, RpcError> {
    let deps = (context.deps)().map_err(|e| RpcError::new(ErrorCode::Io, e.to_string()))?;
    let engine = Engine::new(context.paths.clone(), deps);
    // The first command of a new user may be this one: the home must be born private. The
    // commands that only look do not make it.
    if call.writes() {
        engine.prepare_home()?;
    }
    Ok(Box::new(Local::new(Daemon::new(engine))))
}

/// Starts the daemon and waits for its pipe.
pub fn start_daemon(
    context: &Context,
    notice: &mut dyn FnMut(&str),
) -> Result<Box<dyn Link>, RpcError> {
    // The daemon runs with the user's rights: not if someone else could have replaced it.
    if let Some(exe) = &context.daemon_exe {
        require_trusted_program(exe, context.dirs.as_ref()).map_err(|e| program_error(&e))?;
    }
    (context.spawn_daemon)().map_err(|e| {
        RpcError::new(
            ErrorCode::Io,
            format!("could not start the daemon (crisold): {e}"),
        )
    })?;
    notice("Starting the daemon...");
    let end = Instant::now() + context.daemon_wait;
    loop {
        match (context.connect)() {
            Ok(link) => return Ok(link),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if Instant::now() >= end {
                    return Err(RpcError::new(
                        ErrorCode::NotAvailable,
                        format!(
                            "the daemon did not open its pipe in {} seconds; its log is {}",
                            context.daemon_wait.as_secs(),
                            context.paths.runtime_log("crisold").map_or_else(
                                |_| "logs/crisold/runtime.log".to_owned(),
                                |p| p.display().to_string()
                            )
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(RpcError::new(ErrorCode::Io, error.to_string())),
        }
    }
}

/// Asks the daemon to stop and waits until its pipe is gone, so its files are free.
pub fn stop_daemon(context: &Context, notice: &mut dyn FnMut(&str)) -> Result<(), String> {
    let mut opened = open(context, &Call::Shutdown, notice).map_err(|e| e.message)?;
    opened
        .link
        .call(&Call::Shutdown, &mut |_| {})
        .map_err(|e| client_error(e).message)?;
    drop(opened);
    let end = Instant::now() + context.daemon_wait;
    while (context.connect)().is_ok() {
        if Instant::now() >= end {
            return Err("the daemon said it would stop, but it is still running".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// The `doctor` line for a daemon of another version than this program, if it is: after an
/// update the old one keeps running until it is stopped (Windows does not let the program be
/// replaced while it runs, so the update itself needs it stopped).
#[must_use]
pub fn daemon_version_check(daemon: &str, mine: &str) -> Option<Check> {
    (daemon != mine).then(|| Check {
        name: "daemon version".to_owned(),
        status: CheckStatus::Warning,
        detail: format!(
            "the daemon that is running is {daemon} and this program is {mine}: it was started before an update"
        ),
        fix: Some("crisol shutdown, then any command starts the new one".to_owned()),
    })
}

/// What went wrong with a call, as the error the daemon would have sent.
#[must_use]
pub fn client_error(error: ClientError) -> RpcError {
    match error {
        ClientError::Rpc(error) => error,
        other => RpcError::new(ErrorCode::Io, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_of_another_version_is_a_warning_with_the_way_out_and_the_same_version_says_nothing()
    {
        assert!(daemon_version_check("0.1.0", "0.1.0").is_none());
        let check = daemon_version_check("0.1.0", "0.2.0").unwrap();
        assert_eq!(check.status, CheckStatus::Warning);
        assert!(check.detail.contains("0.1.0") && check.detail.contains("0.2.0"));
        assert!(check.fix.unwrap().starts_with("crisol shutdown"));
    }
}
