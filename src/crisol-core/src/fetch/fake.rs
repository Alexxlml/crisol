//! An in-memory [`Fetch`] for tests: no network.

use std::collections::HashMap;
use std::io::{self, Cursor, Read};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{Fetch, FetchError, Response};

/// What a URL answers with.
#[derive(Debug, Clone)]
enum Answer {
    Body {
        bytes: Vec<u8>,
        announced: Option<u64>,
    },
    Status(u16),
    Network(String),
    /// Streams `bytes`, then fails with a read error, like a dropped connection.
    CutAfter {
        bytes: Vec<u8>,
        announced: Option<u64>,
    },
}

/// Scriptable fake of [`Fetch`]. An unknown URL answers 404.
#[derive(Debug, Default)]
pub struct FakeFetcher {
    answers: Mutex<HashMap<String, Answer>>,
    requests: Mutex<Vec<String>>,
}

impl FakeFetcher {
    /// A fetcher that knows no URL.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn answers(&self) -> MutexGuard<'_, HashMap<String, Answer>> {
        self.answers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `url` serves `bytes` and announces their exact length.
    pub fn serve(&self, url: &str, bytes: Vec<u8>) {
        let announced = u64::try_from(bytes.len()).ok();
        self.answers()
            .insert(url.to_owned(), Answer::Body { bytes, announced });
    }

    /// `url` serves `bytes` but announces `announced` (or nothing) as the length.
    pub fn serve_announcing(&self, url: &str, bytes: Vec<u8>, announced: Option<u64>) {
        self.answers()
            .insert(url.to_owned(), Answer::Body { bytes, announced });
    }

    /// `url` answers with an error status.
    pub fn status(&self, url: &str, code: u16) {
        self.answers().insert(url.to_owned(), Answer::Status(code));
    }

    /// `url` cannot be reached.
    pub fn unreachable(&self, url: &str, reason: &str) {
        self.answers()
            .insert(url.to_owned(), Answer::Network(reason.to_owned()));
    }

    /// `url` announces the full length but the connection drops after `bytes`.
    pub fn cut_after(&self, url: &str, bytes: Vec<u8>, full_length: u64) {
        self.answers().insert(
            url.to_owned(),
            Answer::CutAfter {
                bytes,
                announced: Some(full_length),
            },
        );
    }

    /// URLs requested so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Yields `bytes` and then a connection error.
struct CutReader {
    inner: Cursor<Vec<u8>>,
}

impl Read for CutReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(buf)? {
            0 => Err(io::Error::from(io::ErrorKind::ConnectionReset)),
            n => Ok(n),
        }
    }
}

impl Fetch for FakeFetcher {
    fn fetch(&self, url: &str) -> Result<Response, FetchError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(url.to_owned());
        let answer = self
            .answers()
            .get(url)
            .cloned()
            .unwrap_or(Answer::Status(404));
        match answer {
            Answer::Status(code) => Err(FetchError::Status(code)),
            Answer::Network(reason) => Err(FetchError::Network(reason)),
            Answer::Body { bytes, announced } => Ok(Response {
                content_length: announced,
                body: Box::new(Cursor::new(bytes)),
            }),
            Answer::CutAfter { bytes, announced } => Ok(Response {
                content_length: announced,
                body: Box::new(CutReader {
                    inner: Cursor::new(bytes),
                }),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(response: Response) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut body = response.body;
        body.read_to_end(&mut out).map(|_| out)
    }

    #[test]
    fn it_serves_bytes_and_records_requests() {
        let fetcher = FakeFetcher::new();
        fetcher.serve("https://a.test/x", b"abc".to_vec());
        let response = fetcher.fetch("https://a.test/x").unwrap();
        assert_eq!(response.content_length, Some(3));
        assert_eq!(read_all(response).unwrap(), b"abc");
        assert_eq!(fetcher.requests(), ["https://a.test/x"]);
    }

    #[test]
    fn unknown_urls_are_404_and_scripted_failures_are_reported() {
        let fetcher = FakeFetcher::new();
        assert_eq!(
            fetcher.fetch("https://a.test/nope").unwrap_err(),
            FetchError::Status(404)
        );
        fetcher.status("https://a.test/gone", 410);
        assert_eq!(
            fetcher.fetch("https://a.test/gone").unwrap_err(),
            FetchError::Status(410)
        );
        fetcher.unreachable("https://a.test/down", "refused");
        assert_eq!(
            fetcher.fetch("https://a.test/down").unwrap_err(),
            FetchError::Network("refused".into())
        );
    }

    #[test]
    fn a_cut_connection_fails_after_the_bytes() {
        let fetcher = FakeFetcher::new();
        fetcher.cut_after("https://a.test/x", b"1234".to_vec(), 100);
        let response = fetcher.fetch("https://a.test/x").unwrap();
        assert_eq!(response.content_length, Some(100));
        assert!(read_all(response).is_err());
    }

    #[test]
    fn the_announced_length_can_lie() {
        let fetcher = FakeFetcher::new();
        fetcher.serve_announcing("https://a.test/x", b"abc".to_vec(), Some(99));
        assert_eq!(
            fetcher.fetch("https://a.test/x").unwrap().content_length,
            Some(99)
        );
        fetcher.serve_announcing("https://a.test/y", b"abc".to_vec(), None);
        assert_eq!(
            fetcher.fetch("https://a.test/y").unwrap().content_length,
            None
        );
    }
}
