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
use windows::Win32::System::Console::{
    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, SetConsoleCtrlHandler,
    SetConsoleMode, CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE,
};

/// One of the two output streams of a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl Stream {
    fn handle(self) -> STD_HANDLE {
        match self {
            Stream::Stdout => STD_OUTPUT_HANDLE,
            Stream::Stderr => STD_ERROR_HANDLE,
        }
    }
}

/// Turns on the terminal's own handling of colour and cursor codes (ANSI escape sequences) for
/// `stream`, which the classic console leaves off. Says whether such codes can be written to it.
///
/// A stream that is not a classic console (a pseudo-terminal such as Git Bash's, which is
/// a pipe) handles the codes by itself, so that counts as yes; the caller has already checked
/// that the stream is a terminal at all. Only a console that refuses the mode is a no.
#[must_use]
pub fn enable_ansi(stream: Stream) -> bool {
    // SAFETY: plain queries of the standard handles of this process; the mode is a local.
    unsafe {
        let Ok(handle) = GetStdHandle(stream.handle()) else {
            return false;
        };
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &raw mut mode).is_err() {
            return true;
        }
        if mode.0 & ENABLE_VIRTUAL_TERMINAL_PROCESSING.0 != 0 {
            return true;
        }
        SetConsoleMode(
            handle,
            CONSOLE_MODE(mode.0 | ENABLE_VIRTUAL_TERMINAL_PROCESSING.0),
        )
        .is_ok()
    }
}

/// The width, in characters, of the console window `stream` is attached to; `None` when it is
/// not a classic console (redirected, or a pseudo-terminal).
#[must_use]
pub fn console_width(stream: Stream) -> Option<usize> {
    // SAFETY: a query of a standard handle into a local buffer.
    unsafe {
        let handle = GetStdHandle(stream.handle()).ok()?;
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        GetConsoleScreenBufferInfo(handle, &raw mut info).ok()?;
        let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
        usize::try_from(width).ok().filter(|w| *w > 0)
    }
}

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
    fn a_redirected_stream_has_no_console_width() {
        // Under `cargo test` the streams are pipes or files, never a console window; either
        // way the answer must not be a nonsense number.
        if let Some(width) = console_width(Stream::Stdout) {
            assert!(width > 0);
        }
        let _ = enable_ansi(Stream::Stderr);
    }

    #[test]
    fn installing_the_handler_works() {
        survive_ctrl_c().unwrap();
    }
}
