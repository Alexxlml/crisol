//! Errors on the wire.

use serde::{Deserialize, Serialize};

use crate::text::{lenient_boxed_text, Text};

/// What went wrong, as a number a client can act on (JSON-RPC style: the negative ones are the
/// standard codes, the positive ones are Crisol's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i32", try_from = "i32")]
pub enum ErrorCode {
    /// The line is not JSON (`-32700`).
    ParseError,
    /// The JSON is not a valid request (`-32600`).
    InvalidRequest,
    /// The method does not exist (`-32601`).
    MethodNotFound,
    /// The parameters are not valid for the method (`-32602`).
    InvalidParams,
    /// The daemon failed in a way that is its fault (`-32603`).
    Internal,
    /// The client speaks a protocol version the daemon does not (`1`).
    UnsupportedProtocol,
    /// Something named in the request is not there: a version, a site, a service (`2`).
    NotFound,
    /// The request is fine but cannot be done in the current state (`3`): a version in use, a
    /// service that is not running, a site that already exists.
    Conflict,
    /// The home is open to other accounts and Crisol will not run or generate anything from it
    /// (`4`); `data.fix` has the command.
    HomeExposed,
    /// A file or the operating system failed (`5`).
    Io,
    /// The operation needs something that is not installed or not set up (`6`).
    NotAvailable,
}

impl ErrorCode {
    /// The number.
    #[must_use]
    pub fn number(self) -> i32 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::Internal => -32603,
            Self::UnsupportedProtocol => 1,
            Self::NotFound => 2,
            Self::Conflict => 3,
            Self::HomeExposed => 4,
            Self::Io => 5,
            Self::NotAvailable => 6,
        }
    }
}

impl From<ErrorCode> for i32 {
    fn from(code: ErrorCode) -> Self {
        code.number()
    }
}

impl TryFrom<i32> for ErrorCode {
    type Error = String;

    fn try_from(number: i32) -> Result<Self, String> {
        Ok(match number {
            -32700 => Self::ParseError,
            -32600 => Self::InvalidRequest,
            -32601 => Self::MethodNotFound,
            -32602 => Self::InvalidParams,
            -32603 => Self::Internal,
            1 => Self::UnsupportedProtocol,
            2 => Self::NotFound,
            3 => Self::Conflict,
            4 => Self::HomeExposed,
            5 => Self::Io,
            6 => Self::NotAvailable,
            other => return Err(format!("unknown error code {other}")),
        })
    }
}

/// An error answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// The kind.
    pub code: ErrorCode,
    /// A sentence for the user, ready to show as it is.
    pub message: String,
    /// What to do about it, when there is an exact thing to do (the `icacls` command, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// `message` as a key and data, for a client to say in its language (ADR-0030). Boxed so an
    /// `Err(RpcError)` stays small. Absent
    /// from `--json` and from a daemon older than the field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_boxed_text"
    )]
    pub text: Option<Box<Text>>,
    /// `fix` as a key and data.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_boxed_text"
    )]
    pub fix_text: Option<Box<Text>>,
}

impl RpcError {
    /// An error with no suggested fix.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            fix: None,
            text: None,
            fix_text: None,
        }
    }

    /// With the sentence also as a key and data (the `message` stays in English).
    #[must_use]
    pub fn with_text(mut self, text: Text) -> Self {
        self.text = Some(Box::new(text));
        self
    }

    /// With the fix also as a key and data.
    #[must_use]
    pub fn with_fix_text(mut self, text: Text) -> Self {
        self.fix_text = Some(Box::new(text));
        self
    }

    /// Takes out the `*_text` fields, which `--json` does not show (ADR-0030 §5).
    pub fn strip_texts(&mut self) {
        self.text = None;
        self.fix_text = None;
    }

    /// With the exact thing to do about it.
    #[must_use]
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RpcError {}
