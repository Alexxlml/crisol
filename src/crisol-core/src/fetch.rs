//! Getting bytes from a URL, behind a small trait.
//!
//! The installer only needs "open this URL and give me a stream", so everything above this
//! module is tested with [`fake::FakeFetcher`], with no network. [`HttpFetcher`] is the real
//! thing.
//!
//! [`HttpFetcher`] only talks `https` (also for every redirect: `ureq` checks the scheme of
//! each hop before connecting, `run.rs` in `ureq` 3.4). That is not what makes a download
//! trustworthy, the SHA-256 in the catalog is, but it keeps the bytes private and rules out
//! downgrade tricks.
//!
//! `ureq` honours `HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` from the environment, which is what
//! most corporate networks need. A proxy sees only the encrypted tunnel and the host name,
//! and cannot change the outcome: the bytes are checked against the catalog's hash anyway.

pub mod fake;

use std::io::Read;
use std::time::Duration;

/// An open download.
pub struct Response {
    /// Size announced by the server, if any.
    pub content_length: Option<u64>,
    /// The body, streamed.
    pub body: Box<dyn Read + Send>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("content_length", &self.content_length)
            .finish_non_exhaustive()
    }
}

/// Why a URL could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The server answered with an error status.
    #[error("the server answered {0}")]
    Status(u16),
    /// The URL is not `https`.
    #[error("only https downloads are allowed: {0}")]
    NotHttps(String),
    /// The connection failed.
    #[error("network error: {0}")]
    Network(String),
}

/// Opens URLs.
pub trait Fetch: Send + Sync {
    /// Starts a GET request and returns the response stream.
    fn fetch(&self, url: &str) -> Result<Response, FetchError>;
}

/// Downloads over HTTPS with `ureq` and `rustls`.
#[derive(Debug, Clone)]
pub struct HttpFetcher {
    agent: ureq::Agent,
    https_only: bool,
}

impl HttpFetcher {
    /// How long a whole download may take before it is abandoned. `ureq` has no idle timeout,
    /// so this also bounds a connection that goes silent.
    pub const BODY_TIMEOUT: Duration = Duration::from_secs(60 * 60);

    /// A fetcher that refuses anything but `https`.
    #[must_use]
    pub fn new() -> Self {
        Self::build(true)
    }

    /// Accepts plain `http`, for tests that talk to a server on this machine.
    #[cfg(test)]
    fn allowing_http() -> Self {
        Self::build(false)
    }

    fn build(https_only: bool) -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(https_only)
            .max_redirects(5)
            .user_agent(concat!("crisol/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Self::BODY_TIMEOUT))
            .build();
        Self {
            agent: config.into(),
            https_only,
        }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for HttpFetcher {
    fn fetch(&self, url: &str) -> Result<Response, FetchError> {
        if self.https_only && !url.starts_with("https://") {
            return Err(FetchError::NotHttps(url.to_owned()));
        }
        let response = self.agent.get(url).call().map_err(|e| match e {
            ureq::Error::StatusCode(code) => FetchError::Status(code),
            other => FetchError::Network(other.to_string()),
        })?;
        let content_length = response.body().content_length();
        let body = response
            .into_body()
            .into_with_config()
            .limit(u64::MAX)
            .reader();
        Ok(Response {
            content_length,
            body: Box::new(body),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    /// A one-off HTTP server on this machine: `handler` gets the request line and the
    /// stream, and answers however the test wants.
    fn serve(handler: impl Fn(&str, &mut TcpStream) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned();
                handler(&line, &mut stream);
            }
        });
        format!("http://{address}")
    }

    fn read_all(response: Response) -> Vec<u8> {
        let mut out = Vec::new();
        let mut body = response.body;
        body.read_to_end(&mut out).unwrap();
        out
    }

    fn respond(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
        let head = format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }

    #[test]
    fn it_streams_a_body_and_reports_its_length() {
        let url = serve(|_, s| respond(s, "200 OK", "Content-Length: 11\r\n", b"hello world"));
        let response = HttpFetcher::allowing_http().fetch(&url).unwrap();
        assert_eq!(response.content_length, Some(11));
        assert_eq!(read_all(response), b"hello world");
    }

    #[test]
    fn there_is_no_hidden_size_limit_on_the_body() {
        // The library caps in-memory reads at 10 MB by default; a download must not be.
        let big = vec![7_u8; 12 * 1024 * 1024];
        let expected = big.len();
        let url = serve(move |_, s| {
            respond(
                s,
                "200 OK",
                &format!("Content-Length: {}\r\n", big.len()),
                &big,
            );
        });
        let response = HttpFetcher::allowing_http().fetch(&url).unwrap();
        assert_eq!(read_all(response).len(), expected);
    }

    #[test]
    fn error_statuses_are_reported_by_code() {
        let url = serve(|_, s| respond(s, "404 Not Found", "Content-Length: 0\r\n", b""));
        assert_eq!(
            HttpFetcher::allowing_http().fetch(&url).unwrap_err(),
            FetchError::Status(404)
        );
        let url = serve(|_, s| respond(s, "503 Service Unavailable", "Content-Length: 0\r\n", b""));
        assert_eq!(
            HttpFetcher::allowing_http().fetch(&url).unwrap_err(),
            FetchError::Status(503)
        );
    }

    #[test]
    fn redirects_are_followed() {
        let url = serve(|line, s| {
            if line.contains("/final") {
                respond(s, "200 OK", "Content-Length: 4\r\n", b"done");
            } else {
                respond(
                    s,
                    "302 Found",
                    "Location: /final\r\nContent-Length: 0\r\n",
                    b"",
                );
            }
        });
        let response = HttpFetcher::allowing_http()
            .fetch(&format!("{url}/start"))
            .unwrap();
        assert_eq!(read_all(response), b"done");
    }

    #[test]
    fn a_redirect_loop_gives_up() {
        let url = serve(|_, s| {
            respond(
                s,
                "302 Found",
                "Location: /again\r\nContent-Length: 0\r\n",
                b"",
            );
        });
        let err = HttpFetcher::allowing_http().fetch(&url).unwrap_err();
        assert!(matches!(err, FetchError::Network(_)), "{err}");
    }

    #[test]
    fn plain_http_is_refused_before_connecting() {
        // Nothing listens here: the refusal must come from the check, not from the network.
        for url in [
            "http://example.invalid/a.zip",
            "ftp://example.invalid/a.zip",
            "example.invalid/a.zip",
        ] {
            assert_eq!(
                HttpFetcher::new().fetch(url).unwrap_err(),
                FetchError::NotHttps(url.to_owned())
            );
        }
    }

    #[test]
    fn an_unreachable_server_is_a_network_error() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        }; // closed again: nothing listens on `port`
        let err = HttpFetcher::allowing_http()
            .fetch(&format!("http://127.0.0.1:{port}/"))
            .unwrap_err();
        assert!(matches!(err, FetchError::Network(_)), "{err}");
    }

    #[test]
    fn a_connection_cut_mid_body_shows_up_as_a_read_error() {
        let url = serve(|_, s| {
            // Promises 1000 bytes, sends 10 and hangs up.
            respond(s, "200 OK", "Content-Length: 1000\r\n", b"0123456789");
        });
        let response = HttpFetcher::allowing_http().fetch(&url).unwrap();
        assert_eq!(response.content_length, Some(1000));
        let mut out = Vec::new();
        let mut body = response.body;
        assert!(
            body.read_to_end(&mut out).is_err(),
            "a truncated body must not look complete"
        );
    }
}
