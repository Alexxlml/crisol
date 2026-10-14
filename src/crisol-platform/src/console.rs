//! Console behaviour for the shims.
//!
//! A shim sits between the terminal and the real program. When the user presses Ctrl+C, Windows
//! delivers the event to **every** process attached to the console: the real program stops, and
//! the shim would stop with it before it could pass on the exit code. So the shim installs a
//! handler that says "handled" and keeps waiting for its child.
//!
//! Passing `None` to `SetConsoleCtrlHandler` would also make the child ignore Ctrl+C, because
//! that setting is inherited, and `php -S` could no longer be stopped. A handler routine is
//! not inherited.

use std::io;

use windows::core::BOOL;
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT};

/// Called by Windows on its own thread for every console event.
unsafe extern "system" fn on_console_event(event: u32) -> BOOL {
    // TRUE = handled here, do not run the default handler (which would end this process).
    BOOL::from(event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT)
}

/// Keeps this process alive through Ctrl+C and Ctrl+Break, so that it can wait for its child
/// and return the child's exit code. Child processes are not affected.
pub fn survive_ctrl_c() -> io::Result<()> {
    // SAFETY: `on_console_event` is a valid handler for the whole life of the process, takes
    // and returns what Windows expects, and touches nothing else.
    unsafe { SetConsoleCtrlHandler(Some(on_console_event), true) }.map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handler_takes_ctrl_c_and_ctrl_break_and_nothing_else() {
        // SAFETY: the handler only inspects its argument.
        let handled = |event| unsafe { on_console_event(event) }.as_bool();
        assert!(handled(CTRL_C_EVENT));
        assert!(handled(CTRL_BREAK_EVENT));
        assert!(!handled(2), "closing the window is not ours to refuse");
    }

    #[test]
    fn installing_the_handler_works() {
        survive_ctrl_c().unwrap();
    }
}
