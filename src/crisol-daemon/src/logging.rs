//! The daemon's own log: `logs/crisold/runtime.log`, written with `tracing`.
//!
//! Each line is `2026-10-16 18:03:22 -06:00  INFO message`: the local time and its offset, the
//! level and the text. The file is the same rotating log as the services' (`RotatingLog`, 512 KiB
//! and three older files), shared by every thread through one lock, so the lines of the actor,
//! the pipe and the main thread end up in order in one file.
//!
//! There is no global subscriber: each daemon builds a [`Dispatch`] of its own and its threads
//! run inside it. That is what lets several daemons live in one process (the tests do), each
//! with its own home and its own log, and keeps a library crate from taking the process-wide
//! slot that belongs to the binary.
//!
//! A daemon that cannot open its log still runs; it just has nothing to say it with.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use crisol_core::log::{LogRotation, RotatingLog};
use crisol_core::paths::CrisolPaths;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::OffsetDateTime;
use tracing::Dispatch;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::MakeWriter;

/// The readable part of a timestamp: `2026-10-16 18:03:22`.
const STAMP: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");

/// The offset from UTC, so a log that is read elsewhere can still be placed: `-06:00`.
const OFFSET: &[FormatItem<'static>] =
    format_description!("[offset_hour sign:mandatory]:[offset_minute]");

/// The local time, or UTC if the system will not say what the local offset is.
#[derive(Debug, Clone, Copy)]
struct LocalClock;

impl FormatTime for LocalClock {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let (stamp, offset) = now
            .format(STAMP)
            .and_then(|stamp| now.format(OFFSET).map(|offset| (stamp, offset)))
            .map_err(|_| std::fmt::Error)?;
        write!(w, "{stamp} {offset}")
    }
}

/// The rotating file behind a lock; `None` when it could not be opened.
#[derive(Debug, Clone)]
struct SharedLog(Option<Arc<Mutex<RotatingLog>>>);

impl io::Write for SharedLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(log) = &self.0 {
            let mut log = log.lock().unwrap_or_else(PoisonError::into_inner);
            // A log that cannot be written is not worth failing a service for.
            let _ = log.write(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SharedLog {
    type Writer = SharedLog;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A subscriber that writes to the daemon's log in `paths`. Run the threads of the daemon
/// inside it with [`tracing::dispatcher::with_default`].
#[must_use]
pub fn dispatch(paths: &CrisolPaths) -> Dispatch {
    let file = paths.runtime_log("crisold").ok().and_then(|path| {
        RotatingLog::open(
            path,
            LogRotation {
                max_bytes: 512 * 1024,
                keep: 3,
            },
        )
        .ok()
    });
    let writer = SharedLog(file.map(|file| Arc::new(Mutex::new(file))));
    Dispatch::new(
        tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .with_target(false)
            .with_timer(LocalClock)
            .with_max_level(tracing::Level::INFO)
            .finish(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_has_a_readable_time_a_level_and_the_text() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let dispatch = dispatch(&paths);
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!("starting in {}", "somewhere");
            tracing::warn!("something odd");
            tracing::debug!("not written");
        });
        let text = std::fs::read_to_string(paths.runtime_log("crisold").unwrap()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        // 2026-10-16 18:03:22 -06:00  INFO starting in somewhere
        let first = lines[0];
        let bytes = first.as_bytes();
        assert!(
            bytes[4] == b'-' && bytes[7] == b'-' && bytes[10] == b' ' && bytes[13] == b':',
            "{first}"
        );
        assert!(first.ends_with("INFO starting in somewhere"), "{first}");
        assert!(lines[1].ends_with("WARN something odd"), "{}", lines[1]);
    }

    #[test]
    fn threads_share_one_file_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let dispatch = dispatch(&paths);
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let dispatch = dispatch.clone();
                std::thread::spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || {
                        for n in 0..25 {
                            tracing::info!("thread {i} line {n}");
                        }
                    });
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let text = std::fs::read_to_string(paths.runtime_log("crisold").unwrap()).unwrap();
        assert_eq!(text.lines().count(), 100);
        assert!(text.lines().all(|l| l.contains("INFO thread ")), "{text}");
    }
}
