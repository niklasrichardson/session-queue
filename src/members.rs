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
//! Line 5, only when the key is `CLAUDE_CODE_SESSION_ID` (the Claude Code
//! CLI), is the owner process: the ID and start time of the nearest `claude`
//! process above the watcher. The CLI starts a new session ID on `/clear` but
//! keeps its process, so `cq session-start` can find the record by the
//! process and move it to the new key (see `rejoin`). Any other start in that
//! process, such as `/resume`, drops line 5 from the records of other keys
//! (see `disarm`). Every change to a record holds `members/.lock`. Each
//! `claude` process also has a mark, `members/.procs/<pid>-<start>`, with the
//! key it last ran; a move needs the mark to name the record's key.
//! Readers of the name use line 1 only, so a record from an older
//! `cq` still works; a missing or unknown mode reads as `monitor`.
//! `members/` is created on demand, so an older root without it still works.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{CqError, Result};
use crate::fsutil;
use crate::names::is_valid_name;
use crate::platform;
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
    key_and_source().map(|(key, _)| key)
}

/// The session key when it comes from `CLAUDE_CODE_SESSION_ID` alone, as in
/// the Claude Code CLI: no CQ_SESSION_ID and no host ID. Only such a key
/// changes on `/clear` while its `claude` process stays.
pub fn cli_session_key() -> Option<String> {
    key_and_source()
        .filter(|(_, var)| *var == ENV_SESSION)
        .map(|(key, _)| key)
}

fn key_and_source() -> Option<(String, &'static str)> {
    [ENV_GENERIC_SESSION, ENV_HOST_SESSION, ENV_SESSION]
        .into_iter()
        .find_map(|var| Some((sanitise_key(&std::env::var(var).ok()?)?, var)))
}

/// The process that holds a Claude Code CLI session: line 5 of the record.
/// The start time guards against a reused process ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub pid: u32,
    pub start: u64,
}

impl Owner {
    /// The nearest `claude` process above this one, if any.
    pub fn find() -> Option<Owner> {
        platform::claude_ancestor().map(|(pid, start)| Owner { pid, start })
    }

    /// Line 5 of a record: `<pid> <start>`. `None` for anything else.
    pub fn parse(line: Option<&str>) -> Option<Owner> {
        let mut parts = line?.split_whitespace();
        let owner = Owner {
            pid: parts.next()?.parse().ok()?,
            start: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(owner)
    }
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

/// Record that `key` owns `name` with a watcher in `mode`, held by `owner`
/// when known, and remove any other key's record of `name`.
pub fn record(root: &Root, key: &str, name: &str, mode: Mode, owner: Option<Owner>) -> Result<()> {
    let dir = root.members();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let folder = std::env::current_dir()
        .map(|d| d.display().to_string().replace(['\r', '\n'], ""))
        .unwrap_or_default();
    let mut text = format!(
        "{name}\n{}\n{folder}\n{}\n",
        rfc3339(timefmt::now()),
        mode.as_str()
    );
    if let Some(o) = owner {
        text.push_str(&format!("{} {}\n", o.pid, o.start));
    }
    let _lock = lock(root, None)?;
    write_record(root, key, name, &text)?;
    if let Some(o) = owner {
        // A failed mark only stops a later rejoin.
        mark_current(root, key, o);
    }
    Ok(())
}

/// Where each `claude` process's current conversation is marked:
/// `members/.procs/<pid>-<start>` holds the key that process was last seen
/// to run. A directory, so `members` never reads a mark as a record.
const PROCS_DIR: &str = ".procs";

fn mark_path(root: &Root, owner: Owner) -> PathBuf {
    root.members()
        .join(PROCS_DIR)
        .join(format!("{}-{}", owner.pid, owner.start))
}

/// Mark `key` as the conversation the process `owner` runs now. Without
/// the `members/` lock, so a busy lock cannot stop it: one atomic rename of
/// the process's own file. If that fails, it removes the old mark, since no
/// mark also stops `rejoin`. Does nothing when `members/` does not exist.
fn mark_current(root: &Root, key: &str, owner: Owner) {
    if !root.members().is_dir() {
        return;
    }
    let path = mark_path(root, owner);
    let Some(dir) = path.parent() else {
        return;
    };
    let written = fs::create_dir_all(dir).is_ok()
        && fsutil::write_atomic(
            dir,
            &path,
            format!(
                "{key}
"
            )
            .as_bytes(),
        )
        .is_ok();
    if !written {
        let _ = fs::remove_file(&path);
    }
}

/// The key last marked for the process `owner`, if any.
fn marked_key(root: &Root, owner: Owner) -> Option<String> {
    let text = fs::read_to_string(mark_path(root, owner)).ok()?;
    sanitise_key(text.lines().next()?)
}

/// The lock file that serialises every change to `members/`: `record`,
/// `rejoin`, `disarm` and `leave`. A key never starts with `.`, and an
/// empty file is no record. Never removed: another process may hold it.
const LOCK_FILE: &str = ".lock";

/// How long `rejoin` and `disarm` wait for the lock before they give up:
/// the SessionStart hook must not hang.
const HOOK_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Hold the `members/` lock until the file is dropped. `None` waits as long
/// as it takes; `Some(limit)` gives up after `limit`. `members/` must exist.
fn lock(root: &Root, limit: Option<Duration>) -> Result<fs::File> {
    let path = root.members().join(LOCK_FILE);
    let file = fsutil::open_lock_file(&path)
        .map_err(|e| CqError::io(format!("open {}", path.display()), e))?;
    let Some(limit) = limit else {
        file.lock()
            .map_err(|e| CqError::io(format!("lock {}", path.display()), e))?;
        return Ok(file);
    };
    let until = Instant::now() + limit;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < until => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err(CqError::error(format!("{} is busy", path.display()))),
        }
    }
}

/// Write `text` as the record of `key`, then remove any other key's record
/// of `name`.
fn write_record(root: &Root, key: &str, name: &str, text: &str) -> Result<()> {
    let dir = root.members();
    let path = dir.join(key);
    fsutil::write_atomic(&root.tmp(), &path, text.as_bytes())
        .map_err(|e| CqError::io(format!("write {}", path.display()), e))?;
    for (other, n) in members(root) {
        if n == name && other != key {
            let _ = fs::remove_file(dir.join(other));
        }
    }
    Ok(())
}

/// After `/clear` in the Claude Code CLI: move the one record held by
/// `owner` to the new `key`, and return its name. The caller checks that
/// the key is a CLI key with no record of its own. `None`, with nothing
/// changed, unless exactly one record names this process ID and start
/// time: two would mean the records cannot tell the sessions apart. The
/// process's mark must also name that record's key, which proves that no
/// other start (such as `/resume`) happened in the process since the
/// record or the last move: with no mark, or another key, nothing moves.
///
/// It reads and writes under the `members/` lock, so a watcher that takes
/// the name meanwhile wins: its record either replaces the moved one, or
/// is there first and holds another owner.
pub fn rejoin(root: &Root, key: &str, owner: Owner) -> Option<String> {
    let dir = root.members();
    if !dir.is_dir() {
        return None;
    }
    let _lock = lock(root, Some(HOOK_LOCK_WAIT)).ok()?;
    if name_for(root, key).is_some() {
        return None;
    }
    let current = marked_key(root, owner)?;
    let mut held = members(root).into_iter().filter_map(|(k, name)| {
        let text = fs::read_to_string(dir.join(&k)).ok()?;
        (k != key && Owner::parse(text.lines().nth(4)) == Some(owner)).then_some((k, name, text))
    });
    let (from, name, text) = held.next()?;
    if held.next().is_some() || from != current {
        return None;
    }
    write_record(root, key, &name, &text).ok()?;
    // A failed mark only stops the next rejoin.
    mark_current(root, key, owner);
    Some(name)
}

/// The process `owner` now runs the conversation `key`, so a later `/clear`
/// must not move a record of another conversation to it. Used after
/// `/resume` or any other start that is not a `/clear`. First it marks
/// `key` as current without the lock, which alone stops `rejoin`. Then,
/// under the lock, it drops the owner line from every record of another
/// key held by `owner`, so the record cannot move even if the mark is lost.
/// If either step fails, the other still stops the move. A record gets an
/// owner again when its conversation starts its own watcher.
pub fn disarm(root: &Root, key: &str, owner: Owner) {
    let dir = root.members();
    if !dir.is_dir() {
        return;
    }
    mark_current(root, key, owner);
    let Ok(_lock) = lock(root, Some(HOOK_LOCK_WAIT)) else {
        return;
    };
    for (k, _) in members(root) {
        let path = dir.join(&k);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if k == key || Owner::parse(text.lines().nth(4)) != Some(owner) {
            continue;
        }
        let kept: String = text.lines().take(4).map(|l| format!("{l}\n")).collect();
        let _ = fsutil::write_atomic(&root.tmp(), &path, kept.as_bytes());
    }
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
    if !dir.is_dir() {
        return Ok(removed);
    }
    let _lock = lock(root, None)?;
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

    #[test]
    fn owner_line_parses_strictly() {
        let o = Owner {
            pid: 71597,
            start: 1730212345,
        };
        assert_eq!(Owner::parse(Some("71597 1730212345")), Some(o));
        assert_eq!(Owner::parse(Some(" 71597  1730212345 ")), Some(o));
        for bad in ["", "71597", "71597 x", "-1 5", "1 2 3", "monitor"] {
            assert_eq!(Owner::parse(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(Owner::parse(None), None);
    }
}
