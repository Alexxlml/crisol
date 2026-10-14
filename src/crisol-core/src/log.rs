//! Service logs: a size-rotating file and the pump that feeds it.
//!
//! A service's stdout and stderr go through a pipe that Crisol reads and writes into
//! `logs/<service>/runtime.log`. Because Crisol is the only writer, it can rotate the file
//! while the service keeps running:
//!
//! ```text
//! runtime.log      current
//! runtime.log.1    previous (newest rotated file)
//! runtime.log.2    older
//! ```
//!
//! At most [`LogRotation::keep`] rotated files are kept; the oldest is deleted.
//!
//! Two rules protect the services and the data:
//! - [`pump`] **always drains the pipe**, even when the log cannot be written. A service
//!   whose output pipe fills up would block forever, which is worse than losing log lines.
//! - If a rotation fails (say another program holds the file open), the log keeps growing
//!   instead of dropping data, and rotation is retried on the next write.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Default size at which a log file is rotated: 5 MiB.
pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Default number of rotated files kept next to the current one.
pub const DEFAULT_KEEP: u32 = 3;
/// Largest piece read from a pipe and written at once. A log file can exceed its limit by
/// at most this much.
pub const CHUNK_SIZE: usize = 16 * 1024;

/// When to rotate and how much history to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRotation {
    /// Rotate before a write that would take the file past this size. Must be above zero.
    pub max_bytes: u64,
    /// Rotated files to keep (`runtime.log.1` … `runtime.log.<keep>`). Zero keeps no
    /// history: the file is simply emptied when it fills up.
    pub keep: u32,
}

impl Default for LogRotation {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            keep: DEFAULT_KEEP,
        }
    }
}

/// An append-only log file that rotates by size.
#[derive(Debug)]
pub struct RotatingLog {
    path: PathBuf,
    rotation: LogRotation,
    file: File,
    size: u64,
}

impl RotatingLog {
    /// Opens `path` for appending, creating it and its parent directories. Existing content
    /// is kept and counts towards the limit.
    pub fn open(path: impl Into<PathBuf>, rotation: LogRotation) -> io::Result<Self> {
        let path = path.into();
        if rotation.max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "max_bytes must be above zero",
            ));
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = open_append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            rotation,
            file,
            size,
        })
    }

    /// The current log file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `bytes`, rotating first if they would not fit. A single write is never
    /// split, and an empty file is never rotated, so an oversized write lands whole in a
    /// fresh file.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let incoming = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if self.size > 0 && self.size.saturating_add(incoming) > self.rotation.max_bytes {
            // Best effort: on failure keep appending rather than lose the data.
            let _ = self.rotate();
        }
        self.file.write_all(bytes)?;
        self.size += incoming;
        Ok(())
    }

    /// Moves the current file to `.1`, shifting the older ones, and starts a new file.
    fn rotate(&mut self) -> io::Result<()> {
        let keep = self.rotation.keep;
        let moved = if keep == 0 {
            self.truncate()
        } else {
            self.shift(keep)
        };

        // Whatever happened, keep logging into a usable file.
        match open_append(&self.path) {
            Ok(file) => {
                self.size = file.metadata().map_or(self.size, |m| m.len());
                self.file = file;
            }
            Err(e) => return moved.and(Err(e)),
        }
        moved
    }

    fn truncate(&self) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map(|_| ())
    }

    fn shift(&self, keep: u32) -> io::Result<()> {
        // Renaming the open file is fine: `std` opens files with `FILE_SHARE_DELETE` on
        // Windows. The caller swaps in a new handle right after.
        let _ = fs::remove_file(numbered(&self.path, keep));
        for i in (1..keep).rev() {
            let from = numbered(&self.path, i);
            if from.exists() {
                fs::rename(&from, numbered(&self.path, i + 1))?;
            }
        }
        fs::rename(&self.path, numbered(&self.path, 1))
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// `runtime.log` + 2 -> `runtime.log.2`.
#[must_use]
pub fn numbered(path: &Path, index: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// What a [`pump`] run moved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PumpStats {
    /// Bytes read from the pipe.
    pub bytes: u64,
    /// Bytes that could not be written and were discarded.
    pub dropped: u64,
}

/// Reads `reader` until it ends and passes every chunk to `sink`.
///
/// A broken pipe or a read error ends the loop like an end of stream. A failing `sink`
/// never stops the loop: the data is discarded and counted, so the process on the other
/// end of the pipe cannot block on a full buffer.
pub fn pump(mut reader: impl Read, mut sink: impl FnMut(&[u8]) -> io::Result<()>) -> PumpStats {
    let mut stats = PumpStats::default();
    let mut buffer = vec![0_u8; CHUNK_SIZE];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                let n64 = u64::try_from(n).unwrap_or(u64::MAX);
                stats.bytes += n64;
                if sink(&buffer[..n]).is_err() {
                    stats.dropped += n64;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn rotation(max_bytes: u64, keep: u32) -> LogRotation {
        LogRotation { max_bytes, keep }
    }

    fn text(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    fn files(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn defaults_are_five_mebibytes_and_three_files() {
        let d = LogRotation::default();
        assert_eq!((d.max_bytes, d.keep), (5 * 1024 * 1024, 3));
    }

    #[test]
    fn numbered_appends_the_index_to_the_file_name() {
        assert_eq!(
            numbered(Path::new("logs/nginx/runtime.log"), 2),
            Path::new("logs/nginx/runtime.log.2")
        );
    }

    #[test]
    fn open_creates_the_file_and_its_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("logs").join("nginx").join("runtime.log");
        let mut log = RotatingLog::open(&path, rotation(100, 2)).unwrap();
        log.write(b"hello\n").unwrap();
        assert_eq!(text(&path), "hello\n");
        assert_eq!(log.path(), path);
    }

    #[test]
    fn zero_max_bytes_is_rejected() {
        let dir = TempDir::new().unwrap();
        let err = RotatingLog::open(dir.path().join("a.log"), rotation(0, 1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn writes_append_and_existing_content_is_kept() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        fs::write(&path, "old\n").unwrap();
        let mut log = RotatingLog::open(&path, rotation(1000, 2)).unwrap();
        log.write(b"new\n").unwrap();
        assert_eq!(text(&path), "old\nnew\n");
    }

    #[test]
    fn it_rotates_before_a_write_that_would_not_fit() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        let mut log = RotatingLog::open(&path, rotation(10, 2)).unwrap();
        log.write(b"aaaaaa").unwrap(); // 6
        log.write(b"bbbbbb").unwrap(); // would reach 12 > 10: rotate first
        assert_eq!(text(&path), "bbbbbb");
        assert_eq!(text(&numbered(&path, 1)), "aaaaaa");
        assert_eq!(files(&dir), ["runtime.log", "runtime.log.1"]);
    }

    #[test]
    fn a_write_that_exactly_fits_does_not_rotate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        let mut log = RotatingLog::open(&path, rotation(10, 2)).unwrap();
        log.write(b"12345").unwrap();
        log.write(b"67890").unwrap();
        assert_eq!(text(&path), "1234567890");
        assert_eq!(files(&dir), ["runtime.log"]);
    }

    #[test]
    fn only_the_configured_history_is_kept_and_order_is_newest_first() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        let mut log = RotatingLog::open(&path, rotation(4, 2)).unwrap();
        for chunk in ["AAAA", "BBBB", "CCCC", "DDDD", "EEEE"] {
            log.write(chunk.as_bytes()).unwrap();
        }
        assert_eq!(text(&path), "EEEE");
        assert_eq!(text(&numbered(&path, 1)), "DDDD");
        assert_eq!(text(&numbered(&path, 2)), "CCCC");
        assert_eq!(
            files(&dir),
            ["runtime.log", "runtime.log.1", "runtime.log.2"]
        );
    }

    #[test]
    fn keep_zero_empties_the_file_without_history() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        let mut log = RotatingLog::open(&path, rotation(8, 0)).unwrap();
        log.write(b"11111111").unwrap();
        log.write(b"2222").unwrap();
        assert_eq!(text(&path), "2222");
        assert_eq!(files(&dir), ["runtime.log"]);
    }

    #[test]
    fn an_oversized_write_is_never_split_and_an_empty_file_is_not_rotated() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        let mut log = RotatingLog::open(&path, rotation(4, 2)).unwrap();
        log.write(b"0123456789").unwrap();
        assert_eq!(text(&path), "0123456789");
        assert_eq!(files(&dir), ["runtime.log"], "nothing to rotate yet");

        log.write(b"x").unwrap();
        assert_eq!(text(&path), "x");
        assert_eq!(text(&numbered(&path, 1)), "0123456789");
    }

    #[test]
    fn an_already_full_file_is_rotated_on_the_first_write_after_opening() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        fs::write(&path, "0123456789").unwrap();
        let mut log = RotatingLog::open(&path, rotation(10, 1)).unwrap();
        log.write(b"z").unwrap();
        assert_eq!(text(&path), "z");
        assert_eq!(text(&numbered(&path, 1)), "0123456789");
    }

    #[test]
    fn a_log_whose_rotation_fails_keeps_growing_instead_of_losing_data() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        // A directory squatting on the name of the first rotated file makes the rename fail.
        fs::create_dir(numbered(&path, 1)).unwrap();
        fs::write(numbered(&path, 1).join("pin"), "x").unwrap();

        let mut log = RotatingLog::open(&path, rotation(4, 1)).unwrap();
        log.write(b"AAAA").unwrap();
        log.write(b"BBBB").unwrap();
        log.write(b"CCCC").unwrap();
        assert_eq!(text(&path), "AAAABBBBCCCC", "no data may be lost");
    }

    #[test]
    fn pump_copies_everything_in_chunks() {
        let data: Vec<u8> = (0..100_000_u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let mut out = Vec::new();
        let mut largest = 0;
        let stats = pump(Cursor::new(&data), |bytes| {
            largest = largest.max(bytes.len());
            out.extend_from_slice(bytes);
            Ok(())
        });
        assert_eq!(out, data);
        assert_eq!(
            stats,
            PumpStats {
                bytes: 100_000,
                dropped: 0
            }
        );
        assert!(largest <= CHUNK_SIZE);
    }

    #[test]
    fn pump_keeps_draining_when_the_sink_fails() {
        let data = vec![7_u8; 50_000];
        let mut calls = 0;
        let stats = pump(Cursor::new(&data), |_| {
            calls += 1;
            Err(io::Error::other("disk full"))
        });
        assert_eq!(stats.bytes, 50_000, "the whole pipe must be consumed");
        assert_eq!(stats.dropped, 50_000);
        assert!(
            calls >= 4,
            "it must keep trying, not give up after the first error"
        );
    }

    #[test]
    fn pump_treats_a_read_error_as_the_end_of_the_stream() {
        struct Failing(bool);
        impl Read for Failing {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    Err(io::Error::from(io::ErrorKind::BrokenPipe))
                } else {
                    self.0 = true;
                    buf[..3].copy_from_slice(b"abc");
                    Ok(3)
                }
            }
        }
        let mut out = Vec::new();
        let stats = pump(Failing(false), |b| {
            out.extend_from_slice(b);
            Ok(())
        });
        assert_eq!(out, b"abc");
        assert_eq!(stats.bytes, 3);
    }

    #[test]
    fn pump_into_a_rotating_log_loses_nothing_within_the_history() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("runtime.log");
        let mut log = RotatingLog::open(&path, rotation(40_000, 3)).unwrap();
        let line = "0123456789abcdefghij0123456789abcdefghij\n"; // 41 bytes
        let data = line.repeat(1500); // ~61 KB: forces a rotation
        pump(Cursor::new(data.as_bytes()), |b| log.write(b));

        let mut total = text(&path).len() + text(&numbered(&path, 1)).len();
        total += text(&numbered(&path, 2)).len();
        assert_eq!(
            total,
            data.len(),
            "history was large enough to keep everything"
        );
        assert!(text(&path).len() <= 40_000 + CHUNK_SIZE);
    }
}
