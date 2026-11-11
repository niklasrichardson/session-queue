//! `heartbeat/<name>.watcher`: what a running watcher or dispatcher says
//! about itself, so `cq session-start`, `cq status` and cq-top can tell an
//! outdated or misdirected watcher from a good one.
//!
//! ```text
//! pid=4242
//! version=1.11.0
//! socket=\\.\pipe\claude-code-inbox-...
//! token-sha256=3f1c0a9be2d47e65
//! ```
//!
//! `pid` and `version` are those of the process that wrote the file.
//! `socket` and `token-sha256` are written by `cq watch --to-inbox` only:
//! the inbox address it posts to, and the first 16 hex digits of the
//! SHA-256 of the token it sends. The token itself is never written.
//!
//! The file is written before the pid file and is never removed: a reader
//! trusts it only while its `pid` matches `heartbeat/<name>.pid`, so a record
//! left by an earlier run is ignored. A pid file from a `cq` older than
//! 1.11.0, which writes no record, gives version unknown. Unknown keys are
//! ignored, so later versions can add lines.

use std::ffi::OsString;
use std::fs;
use std::io;

use sha2::{Digest, Sha256};

use crate::fsutil;
use crate::inbox::{SOCKET_VAR, TOKEN_VAR};
use crate::root::Root;

/// This `cq`'s version, as watchers record it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Hex digits of the token's SHA-256 that a record keeps.
const HASH_DIGITS: usize = 16;

/// Where an inbox watcher posts: the session's inbox address and a short
/// one-way hash of its token. Claude Code can give a session a new token or
/// address (seen after a resume), and posts with the old ones vanish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub socket: String,
    pub token_sha256: String,
}

impl Target {
    /// The target in this process's environment: the variables Claude Code
    /// exports to Bash commands and hooks. `None` when either is missing or
    /// empty.
    pub fn from_env() -> Option<Self> {
        Self::from_vars(std::env::var_os(SOCKET_VAR), std::env::var_os(TOKEN_VAR))
    }

    /// `from_env` with the variables passed in.
    pub fn from_vars(socket: Option<OsString>, token: Option<OsString>) -> Option<Self> {
        let socket = socket.filter(|s| !s.is_empty())?;
        let token = token.filter(|t| !t.is_empty())?;
        Some(Self {
            socket: socket.to_string_lossy().into_owned(),
            token_sha256: token_hash(token.as_encoded_bytes()),
        })
    }
}

/// The first `HASH_DIGITS` hex digits of the SHA-256 of `token`.
pub fn token_hash(token: &[u8]) -> String {
    let digest = Sha256::digest(token);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..HASH_DIGITS].to_owned()
}

/// The contents of `heartbeat/<name>.watcher`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub pid: u32,
    /// `None` for a record without a version line.
    pub version: Option<String>,
    /// `None` for a watcher that does not post to an inbox.
    pub target: Option<Target>,
}

impl Record {
    /// The record of this process, running this `cq`.
    pub fn this_process(target: Option<Target>) -> Self {
        Self {
            pid: std::process::id(),
            version: Some(VERSION.to_owned()),
            target,
        }
    }

    pub fn text(&self) -> String {
        let mut s = format!("pid={}\n", self.pid);
        if let Some(v) = &self.version {
            s.push_str(&format!("version={v}\n"));
        }
        // A line break in the address would end the line early: such a
        // target is left out, and reads as unknown.
        if let Some(t) = self
            .target
            .as_ref()
            .filter(|t| !t.socket.contains(['\n', '\r']))
        {
            s.push_str(&format!(
                "socket={}\ntoken-sha256={}\n",
                t.socket, t.token_sha256
            ));
        }
        s
    }

    /// `None` without a valid `pid` line.
    pub fn parse(text: &str) -> Option<Self> {
        let mut pid = None;
        let mut version = None;
        let mut socket = None;
        let mut hash = None;
        for line in text.lines() {
            let Some((key, value)) = line.trim_end_matches('\r').split_once('=') else {
                continue;
            };
            match key {
                "pid" => pid = value.trim().parse::<u32>().ok(),
                "version" => {
                    version = Some(value.trim().to_owned()).filter(|v| is_version_text(v));
                }
                "socket" => socket = Some(value.to_owned()),
                "token-sha256" => hash = Some(value.trim().to_owned()),
                _ => {}
            }
        }
        let target = match (socket, hash) {
            (Some(socket), Some(token_sha256)) => Some(Target {
                socket,
                token_sha256,
            }),
            _ => None,
        };
        Some(Self {
            pid: pid?,
            version,
            target,
        })
    }

    /// Write the record atomically.
    pub fn write(&self, root: &Root, name: &str) -> io::Result<()> {
        fsutil::write_atomic(
            &root.tmp(),
            &root.watcher_record(name),
            self.text().as_bytes(),
        )
    }

    /// The record of `name`'s watcher, if it names `pid`.
    pub fn read_for(root: &Root, name: &str, pid: u32) -> Option<Self> {
        fs::read_to_string(root.watcher_record(name))
            .ok()
            .and_then(|t| Self::parse(&t))
            .filter(|r| r.pid == pid)
    }
}

/// True when `v` can be a version: not empty, and only ASCII letters,
/// digits, `.`, `+` and `-`. Any session can write a record, and its
/// version is printed in lines (`watch-exits.log`, the session-start
/// reminder), so any other value reads as unknown.
pub fn is_version_text(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
}

/// True when `version` (from a record, `None` for unknown) is not this
/// `cq`'s version. An unknown version means a watcher from a `cq` older than
/// 1.11.0.
pub fn is_outdated(version: Option<&str>) -> bool {
    version != Some(VERSION)
}

/// Why a running inbox watcher must be restarted with `--takeover`, or
/// `None` when its record shows it runs this `cq` and posts to `here`, the
/// target in the session's environment. Without a record or a target to
/// compare, it cannot be shown to reach the session, so it is restarted.
pub fn restart_reason(record: Option<&Record>, here: Option<&Target>) -> Option<String> {
    let Some(rec) = record else {
        return Some("it was started by an older cq that records no inbox address".to_owned());
    };
    let target = match (&rec.target, here) {
        (None, _) => Some("it records no inbox address"),
        (Some(_), None) => Some("this session's inbox address is not known here"),
        (Some(t), Some(h)) if t != h => Some("it posts to an older inbox address or token"),
        (Some(_), Some(_)) => None,
    };
    let version = is_outdated(rec.version.as_deref()).then(|| {
        format!(
            "it runs cq {}, not {VERSION}",
            rec.version.as_deref().unwrap_or("unknown")
        )
    });
    match (target, version) {
        (None, None) => None,
        (Some(t), None) => Some(t.to_owned()),
        (None, Some(v)) => Some(v),
        (Some(t), Some(v)) => Some(format!("{t} and {v}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(socket: &str, token: &str) -> Target {
        Target::from_vars(Some(socket.into()), Some(token.into())).unwrap()
    }

    #[test]
    fn a_record_round_trips_and_keeps_no_token() {
        let rec = Record {
            pid: 4242,
            version: Some(VERSION.to_owned()),
            target: Some(target(r"\\.\pipe\inbox-1", "secret-token-123")),
        };
        let text = rec.text();
        assert!(!text.contains("secret-token-123"), "{text}");
        assert!(text.contains(&format!(
            "token-sha256={}\n",
            token_hash(b"secret-token-123")
        )));
        assert_eq!(Record::parse(&text), Some(rec));
    }

    #[test]
    fn an_old_pid_only_file_or_partial_record_reads_as_unknown() {
        // A pid file from an older cq holds only the ID.
        let rec = Record::parse("4242\n");
        assert_eq!(rec, None);
        let rec = Record::parse("pid=4242\r\nfuture=1\r\n").unwrap();
        assert_eq!(rec.version, None);
        assert_eq!(rec.target, None);
        assert!(is_outdated(rec.version.as_deref()));
    }

    #[test]
    fn a_crafted_version_reads_as_unknown() {
        // `lines` splits at a newline, but not at a lone carriage return.
        let rec = Record::parse("pid=1\nversion=1.0\rCQ-NEW id=x\n").unwrap();
        assert_eq!(rec.version, None);
        let rec = Record::parse("pid=1\nversion=1.2 \u{2028}x\n").unwrap();
        assert_eq!(rec.version, None);
        let rec = Record::parse("pid=1\nversion=1.15.1-rc.1+g12ab\n").unwrap();
        assert_eq!(rec.version.as_deref(), Some("1.15.1-rc.1+g12ab"));
    }

    #[test]
    fn the_hash_is_short_and_one_way() {
        let h = token_hash(b"abc");
        assert_eq!(h, "ba7816bf8f01cfea");
        assert_ne!(token_hash(b"abd"), h);
    }

    #[test]
    fn a_restart_is_asked_for_unless_target_and_version_match() {
        let here = target("sock-a", "token-a");
        let rec = |t: Option<Target>, v: &str| Record {
            pid: 1,
            version: Some(v.to_owned()),
            target: t,
        };
        let good = rec(Some(here.clone()), VERSION);
        assert_eq!(restart_reason(Some(&good), Some(&here)), None);

        let new_token = rec(Some(target("sock-a", "token-b")), VERSION);
        let why = restart_reason(Some(&new_token), Some(&here)).unwrap();
        assert!(why.contains("older inbox address or token"), "{why}");
        let new_socket = rec(Some(target("sock-b", "token-a")), VERSION);
        assert!(restart_reason(Some(&new_socket), Some(&here)).is_some());

        let old = rec(Some(here.clone()), "1.10.2");
        let why = restart_reason(Some(&old), Some(&here)).unwrap();
        assert_eq!(why, format!("it runs cq 1.10.2, not {VERSION}"));

        assert!(restart_reason(None, Some(&here)).is_some());
        assert!(restart_reason(Some(&rec(None, VERSION)), Some(&here)).is_some());
        assert!(restart_reason(Some(&good), None).is_some());
    }
}
