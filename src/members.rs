//! Which Claude Code session owns which queue name.
//!
//! `cq watch` records its name under `members/<key>`, where the key is the
//! session's ID from the environment. After `/clear` or a compaction the
//! conversation forgets its name; the SessionStart hook (`cq session-start`)
//! looks the key up and reminds it. A name belongs to one session at a time.
//!
//! The file holds the name on line 1, the UTC time it was written on
//! line 2, the watcher's working folder on line 3 (cq-top uses it to show a
//! sleeping name beside its session), and the watcher's mode on line 4:
//! `monitor`, `inbox` for `cq watch --to-inbox`, whose watcher outlives
//! `/clear`, or `codex` for `cq watch --to-codex`, which wakes a Codex chat.
//! Readers of the name use line 1 only, so a record from an older
//! `cq` still works; a missing or unknown mode reads as `monitor`.
//! `members/` is created on demand, so an older root without it still works.

use std::fs;
use std::path::Path;

use crate::error::{CqError, Result};
use crate::fsutil;
use crate::names::is_valid_name;
use crate::root::Root;
use crate::timefmt::{self, rfc3339};

/// Explicit agent-neutral identity, ahead of agent-specific fallbacks.
pub const ENV_GENERIC_SESSION: &str = "CQ_SESSION_ID";
/// The Claude Desktop app's `local_...` ID, intended to outlive `/clear`.
pub const ENV_HOST_SESSION: &str = "CLAUDE_CODE_HOST_SESSION_ID";
pub const ENV_SESSION: &str = "CLAUDE_CODE_SESSION_ID";
pub const MAX_KEY_LEN: usize = 64;

/// How the name's watcher delivers its lines: line 4 of the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Under a Claude Code Monitor, which ends with the conversation.
    Monitor,
    /// `--to-inbox`: a background process that survives `/clear`.
    Inbox,
    /// `--to-codex`: a background process, usually outside the Codex chat
    /// it wakes.
    Codex,
}

impl Mode {
    /// The word on line 4 of the record.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Monitor => "monitor",
            Mode::Inbox => "inbox",
            Mode::Codex => "codex",
        }
    }

    /// Line 4 of a record. A missing or unknown word reads as `Monitor`.
    pub fn parse(line: Option<&str>) -> Mode {
        match line.map(str::trim) {
            Some("inbox") => Mode::Inbox,
            Some("codex") => Mode::Codex,
            _ => Mode::Monitor,
        }
    }
}

/// The first non-empty key: CQ_SESSION_ID, Claude host ID, Claude session ID.
pub fn session_key() -> Option<String> {
    [ENV_GENERIC_SESSION, ENV_HOST_SESSION, ENV_SESSION]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find_map(|v| sanitise_key(&v))
}

/// The key of a `--to-codex` watcher for `name`: CQ_SESSION_ID when set,
/// else `codex-<name>`. Never a Claude Code ID: the watcher wakes a Codex
/// chat, not the Claude Code session that may have started it, and that
/// session's own record must survive.
pub fn codex_key(name: &str) -> Option<String> {
    std::env::var(ENV_GENERIC_SESSION)
        .ok()
        .and_then(|v| sanitise_key(&v))
        .or_else(|| sanitise_key(&format!("codex-{name}")))
}

/// Keep `[A-Za-z0-9_-]` (any other character becomes `_`), at most 64
/// characters. `None` for an empty value.
pub fn sanitise_key(raw: &str) -> Option<String> {
    let key: String = raw
        .trim()
        .chars()
        .take(MAX_KEY_LEN)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    (!key.is_empty()).then_some(key)
}

/// Record that `key` owns `name` with a watcher in `mode`, and remove any
/// other key's record of `name`.
pub fn record(root: &Root, key: &str, name: &str, mode: Mode) -> Result<()> {
    let dir = root.members();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let path = dir.join(key);
    let folder = std::env::current_dir()
        .map(|d| d.display().to_string().replace(['\r', '\n'], ""))
        .unwrap_or_default();
    let text = format!(
        "{name}\n{}\n{folder}\n{}\n",
        rfc3339(timefmt::now()),
        mode.as_str()
    );
    fsutil::write_atomic(&root.tmp(), &path, text.as_bytes())
        .map_err(|e| CqError::io(format!("write {}", path.display()), e))?;
    for (other, n) in members(root) {
        if n == name && other != key {
            let _ = fs::remove_file(dir.join(other));
        }
    }
    Ok(())
}

/// The name `key` owns, if any.
pub fn name_for(root: &Root, key: &str) -> Option<String> {
    read_name(&root.members().join(key))
}

/// The watcher mode `key` last recorded. `Monitor` when the record has no
/// mode line (an older `cq`) or no record exists.
pub fn mode_for(root: &Root, key: &str) -> Mode {
    let text = fs::read_to_string(root.members().join(key)).unwrap_or_default();
    Mode::parse(text.lines().nth(3))
}

/// Remove every record of `name`. Returns the keys removed.
pub fn leave(root: &Root, name: &str) -> Result<Vec<String>> {
    let dir = root.members();
    let mut removed = Vec::new();
    for (key, n) in members(root) {
        if n != name {
            continue;
        }
        let path = dir.join(&key);
        match fs::remove_file(&path) {
            Ok(()) => removed.push(key),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(CqError::io(format!("remove {}", path.display()), e)),
        }
    }
    Ok(removed)
}

/// Every `(key, name)` record, sorted by key. A missing `members/` gives an
/// empty list; unreadable records are skipped.
pub fn members(root: &Root) -> Vec<(String, String)> {
    let Ok(rd) = fs::read_dir(root.members()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let key = e.file_name().into_string().ok()?;
            let name = read_name(&e.path())?;
            Some((key, name))
        })
        .collect();
    out.sort();
    out
}

fn read_name(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let name = text.lines().next()?.trim();
    is_valid_name(name).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(
            sanitise_key("local_ab12-CD").as_deref(),
            Some("local_ab12-CD")
        );
        assert_eq!(sanitise_key("a:b/c\\d").as_deref(), Some("a_b_c_d"));
        assert_eq!(sanitise_key(&"x".repeat(80)).map(|k| k.len()), Some(64));
        assert_eq!(sanitise_key(""), None);
        assert_eq!(sanitise_key("  "), None);
    }

    #[test]
    fn modes_round_trip_and_unknown_reads_as_monitor() {
        for m in [Mode::Monitor, Mode::Inbox, Mode::Codex] {
            assert_eq!(Mode::parse(Some(m.as_str())), m);
        }
        assert_eq!(Mode::parse(Some(" codex ")), Mode::Codex);
        assert_eq!(Mode::parse(Some("other")), Mode::Monitor);
        assert_eq!(Mode::parse(None), Mode::Monitor);
    }
}
