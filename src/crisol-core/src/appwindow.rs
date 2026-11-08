//! The window of Crisol (`crisol-gui.exe`) as another program sees it (ADR-0026 §6).
//!
//! The window lives in the tray when it is closed, so it is usually running when
//! `crisol uninstall --all` is typed in a terminal. While it runs, its program and its web view
//! profile cannot be deleted: the uninstall asks it to close first, the way the window closes
//! itself after an uninstall made from it.
//!
//! The process is behind [`AppWindow`]; the Windows implementation is in `crisol-platform`.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The argument that asks the window that is running to close (`crisol-gui.exe --quit`). Given
/// to a second instance, it is passed on to the first one; with none running, the program ends
/// without opening anything.
pub const QUIT_ARG: &str = "--quit";

/// The window of Crisol, running in another process.
pub trait AppWindow: Send + Sync {
    /// Whether the window is running in another process of this session.
    fn running(&self) -> io::Result<bool>;
    /// Asks the window to close and waits until it has ended. Returns whether it was running.
    fn close(&self) -> io::Result<bool>;
}

/// No window: a package without `crisol-gui.exe`, or the window itself asking.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAppWindow;

impl AppWindow for NoAppWindow {
    fn running(&self) -> io::Result<bool> {
        Ok(false)
    }

    fn close(&self) -> io::Result<bool> {
        Ok(false)
    }
}

/// A window kept in memory, for tests.
#[derive(Debug, Default)]
pub struct FakeAppWindow {
    running: AtomicBool,
    unreadable: bool,
    stuck: bool,
    closes: AtomicUsize,
}

impl FakeAppWindow {
    /// With the window running.
    #[must_use]
    pub fn open() -> Self {
        Self {
            running: AtomicBool::new(true),
            ..Self::default()
        }
    }

    /// With the window not running.
    #[must_use]
    pub fn closed() -> Self {
        Self::default()
    }

    /// When the processes cannot be looked at.
    #[must_use]
    pub fn unreadable() -> Self {
        Self {
            unreadable: true,
            ..Self::default()
        }
    }

    /// With the window running and not closing when asked.
    #[must_use]
    pub fn stuck() -> Self {
        Self {
            running: AtomicBool::new(true),
            stuck: true,
            ..Self::default()
        }
    }

    /// How many times it was asked to close.
    #[must_use]
    pub fn closes(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }
}

impl AppWindow for FakeAppWindow {
    fn running(&self) -> io::Result<bool> {
        if self.unreadable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot list the processes",
            ));
        }
        Ok(self.running.load(Ordering::SeqCst))
    }

    fn close(&self) -> io::Result<bool> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        if self.stuck {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "it was still running after 15 seconds",
            ));
        }
        Ok(self.running.swap(false, Ordering::SeqCst))
    }
}
