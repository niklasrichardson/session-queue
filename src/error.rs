//! Error type and exit codes.

use std::fmt;

/// Process exit codes. `cq --help` and README.md list the same values.
pub mod exit {
    /// Success.
    pub const OK: i32 = 0;
    /// `cq claim`: the message is no longer in the inbox (another claim won,
    /// or the event was a duplicate). The session ignores it.
    pub const GONE: i32 = 1;
    /// A rename still failed after 3 retries over 1 second.
    pub const RENAME_FAILED: i32 = 2;
    /// `cq claim`: the message is invalid and went to the dead letter queue.
    pub const INVALID: i32 = 3;
    /// `cq done`: the message is finished, but the reply was not sent.
    pub const REPLY_NOT_SENT: i32 = 4;
    /// A queue rule refused the request (name, size, hop limit, pair,
    /// recipient or sender limit, `PAUSE`, missing parent, watcher already
    /// running, sleeping another name without being a coordinator).
    pub const REFUSED: i32 = 5;
    /// `cq done`: the message is not claimed by this session.
    pub const NOT_FOUND: i32 = 6;
    /// `cq watch`: the name is asleep (`sleep/<name>` exists). The session
    /// does not re-arm.
    pub const ASLEEP: i32 = 7;
    /// `cq watch --to-inbox`: the session's inbox could not be reached
    /// after every retry. The session has probably ended.
    pub const INBOX_GONE: i32 = 8;
    /// Bad command-line arguments or bad JSON input.
    pub const USAGE: i32 = 64;
    /// Any other error (I/O, `CQ_ROOT` missing).
    pub const ERROR: i32 = 70;
}

#[derive(Debug)]
pub struct CqError {
    pub code: i32,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, CqError>;

impl CqError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(exit::REFUSED, message)
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(exit::USAGE, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(exit::ERROR, message)
    }

    /// An I/O error with the action and path that failed.
    pub fn io(context: impl fmt::Display, err: std::io::Error) -> Self {
        Self::new(exit::ERROR, format!("{context}: {err}"))
    }
}

impl fmt::Display for CqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CqError {}
