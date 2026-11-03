//! Codex chats and Antigravity CLI (Gemini) conversations, for rows beside the Claude Code
//! sessions.
//!
//! - Codex: the thread list is the `threads` table in Codex's state database
//!   (`state_N.sqlite` in its SQLite home). A thread's context and tokens come from the
//!   `token_count` lines of its rollout file, read like a Claude transcript: the tail first,
//!   then only the lines added since the last scan.
//! - Antigravity CLI: the conversation list is the `conversation_summaries` table in
//!   `~/.gemini/antigravity-cli/conversation_summaries.db`. Its model and token data are in
//!   undocumented protobuf blobs, which are not parsed, so those rows show only activity.
//!
//! Both databases are opened read-only. With no write-ahead log waiting, the file is opened
//! `immutable`: SQLite takes no lock and creates no file. With a log and its `-shm` file
//! (the app has the database open), it is opened `mode=ro`, so the newest writes in the log
//! are seen; a read-only connection never checkpoints. A missing file, table or column gives
//! no rows, never an error.
//!
//! Message text is never read: not Codex's `first_user_message` or `preview`, not the
//! Antigravity CLI's `preview` or `raw_summary`, and not the message lines of a rollout.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use super::{iso_secs, mtime_of, read_lines, tail_find};

/// How long a read waits for a lock the app holds, before it gives up for this scan.
const DB_BUSY: Duration = Duration::from_millis(100);

// ---------- SQLite ----------

/// `path` as an SQLite `file:` URI with `query`. Bytes other than letters, digits and
/// `/-._~:` are percent-encoded, so `?`, `#` and `%` in a folder name stay part of the path.
fn sqlite_uri(path: &Path, query: &str) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push('?');
    out.push_str(query);
    out
}

/// `db` opened read-only, as the module docs say; `None` when there is no such file.
fn open(db: &Path) -> Option<Connection> {
    if !db.is_file() {
        return None;
    }
    let beside = |suffix: &str| {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        PathBuf::from(p)
    };
    let wal = fs::metadata(beside("-wal")).is_ok_and(|m| m.len() > 0);
    // Without `-shm`, `mode=ro` would create it; `immutable` reads the main file only.
    let query = if wal && beside("-shm").is_file() {
        "mode=ro"
    } else {
        "mode=ro&immutable=1"
    };
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(sqlite_uri(db, query), flags).ok()?;
    conn.busy_timeout(DB_BUSY).ok()?;
    Some(conn)
}

/// The rows `sql` gives, each mapped by `map`; a row `map` cannot read is skipped. Empty
/// when the database cannot be opened or the query fails (no table, another schema, a lock
/// held past `DB_BUSY`).
fn rows<T>(db: &Path, sql: &str, map: impl Fn(&rusqlite::Row) -> Option<T>) -> Vec<T> {
    let Some(conn) = open(db) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(mut found) = stmt.query([]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Ok(Some(row)) = found.next() {
        out.extend(map(row));
    }
    out
}

/// A text column as a string; `None` for NULL, another type or an empty string.
fn text(row: &rusqlite::Row, i: usize) -> Option<String> {
    let s = row.get_ref(i).ok()?.as_str().ok()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// An integer column; `None` for NULL or another type.
fn int(row: &rusqlite::Row, i: usize) -> Option<i64> {
    row.get_ref(i).ok()?.as_i64().ok()
}

/// A Windows path without its `\\?\` prefix (`\\?\UNC\server` becomes `\\server`). Codex
/// writes its folders and rollout paths with the prefix.
fn plain_path(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(s))
}

// ---------- Codex ----------

/// One Codex thread, as far as the Sessions screen uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct CodexThread {
    pub id: String,
    /// The thread's rollout file (`.jsonl`).
    pub rollout: PathBuf,
    pub folder: Option<PathBuf>,
    /// The name given to the thread. Codex's own `title` is not read: for an unnamed thread it
    /// is often the start of the first message.
    pub title: Option<String>,
    /// The thread's name alone, which `cq watch --to-codex` also accepts.
    pub name: Option<String>,
    pub model: String,
    /// `updated_at_ms`, in epoch seconds; 0 when unknown.
    pub updated: i64,
}

/// The newest state database in Codex's SQLite home: `state_N.sqlite` with the highest N,
/// so a schema version change in the file name is followed.
pub fn codex_state_db(home: &Path) -> Option<PathBuf> {
    fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

/// A thread a parent thread started for itself (a subagent or a Guardian review): its
/// `source` is JSON with a `subagent` key.
fn is_subagent(source: Option<&str>) -> bool {
    source
        .filter(|s| s.trim_start().starts_with('{'))
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .is_some_and(|v| v.get("subagent").is_some())
}

/// The threads in Codex's state database that are not archived and not subagents.
pub fn codex_threads(db: &Path) -> Vec<CodexThread> {
    const SQL: &str = "SELECT id, rollout_path, cwd, name, model, updated_at_ms, source \
                       FROM threads WHERE archived = 0";
    rows(db, SQL, |row| {
        if is_subagent(text(row, 6).as_deref()) {
            return None;
        }
        let name = text(row, 3);
        Some(CodexThread {
            id: text(row, 0)?,
            rollout: plain_path(&text(row, 1)?),
            folder: text(row, 2).map(|c| plain_path(&c)),
            title: name.clone(),
            name,
            model: text(row, 4).unwrap_or_default(),
            updated: int(row, 5).map_or(0, |ms| ms.div_euclid(1000)),
        })
    })
}

#[derive(Deserialize, Default)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
struct TokenInfo {
    #[serde(default)]
    total_token_usage: Option<TokenUsage>,
    #[serde(default)]
    last_token_usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    info: Option<TokenInfo>,
}

/// The parts of a rollout line cq-top uses. Everything else is skipped by the parser.
#[derive(Deserialize)]
struct RolloutLine {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<Payload>,
}

/// One `token_count` line of a rollout.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TokenCount {
    at: Option<i64>,
    /// The thread's running total: `total_token_usage.total_tokens`.
    total: u64,
    /// `last_token_usage.input_tokens`. Codex counts cached input inside `input_tokens`
    /// (`cached_input_tokens` is a part of it), so this alone is the newest prompt.
    context: u64,
}

/// A `token_count` line's figures; `None` for any other line and for one whose `info` is
/// null (Codex writes that before the first reply).
fn token_count(line: &str) -> Option<TokenCount> {
    // Cheap test first: most of a rollout's bytes are messages and tool output.
    if !line.contains("\"token_count\"") {
        return None;
    }
    let l: RolloutLine = serde_json::from_str(line).ok()?;
    let p = l.payload?;
    if p.kind != "token_count" {
        return None;
    }
    let info = p.info?;
    Some(TokenCount {
        at: l.timestamp.as_deref().and_then(iso_secs),
        total: info.total_token_usage?.total_tokens?,
        context: info.last_token_usage?.input_tokens?,
    })
}

/// Running figures for one rollout file. Only bytes appended since the last read are read;
/// a file that shrank is read again from the start.
#[derive(Default, Debug)]
pub struct RolloutTally {
    /// Bytes read so far; always just after a newline.
    offset: u64,
    /// The local midnight that `tokens` counts from.
    day: i64,
    /// The thread's running total at the newest `token_count` line before `day`.
    before: Option<u64>,
    /// The running total at the newest `token_count` line from today.
    today_total: Option<u64>,
    /// The newest prompt's tokens.
    pub context: Option<u64>,
}

impl RolloutTally {
    /// Tokens since `day`: the difference between the thread's running totals. A total
    /// that went down (Codex started counting again) gives 0 rather than a guess.
    pub fn tokens(&self) -> u64 {
        self.today_total
            .map_or(0, |t| t.saturating_sub(self.before.unwrap_or(0)))
    }

    /// Brings the tally up to date with the file. `today` is local midnight, epoch seconds.
    pub fn update(&mut self, path: &Path, today: i64) {
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        let (len, mtime) = (meta.len(), mtime_of(&meta));
        if len < self.offset {
            *self = RolloutTally::default();
        }
        if self.day != today {
            // Everything already read was written before this midnight.
            self.day = today;
            self.before = self.today_total.or(self.before);
            self.today_total = None;
        }
        if self.offset == 0 && mtime < today {
            // Nothing in the file is from today: the tail is enough.
            let last = tail_find(path, len, token_count);
            self.context = last.map(|c| c.context);
            self.before = last.map(|c| c.total);
            self.offset = len;
            return;
        }
        let mut offset = self.offset;
        read_lines(path, &mut offset, len, |line| self.add(line));
        self.offset = offset;
    }

    fn add(&mut self, line: &str) {
        let Some(c) = token_count(line) else {
            return;
        };
        self.context = Some(c.context);
        if c.at.is_none_or(|t| t < self.day) {
            self.before = Some(c.total);
        } else {
            self.today_total = Some(c.total);
        }
    }
}

// ---------- Antigravity CLI ----------

/// One Antigravity CLI conversation, as far as the Sessions screen uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct GeminiConversation {
    pub id: String,
    pub title: Option<String>,
    pub steps: i64,
    /// `last_modified_time`, in epoch seconds; 0 when unknown.
    pub modified: i64,
    /// The first folder in `workspace_uris`.
    pub folder: Option<PathBuf>,
}

/// `2026-11-02 18:04:20.9701445+00:00` (as the Antigravity CLI writes times) -> epoch
/// seconds.
fn gemini_time(s: &str) -> Option<i64> {
    s.trim()
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_second())
}

/// `%XX` escapes decoded; a broken escape is kept as it is.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `file://` URI as a path: `file:///D:/work/demo` -> `D:/work/demo`,
/// `file:///home/me/demo` -> `/home/me/demo`. `None` for another scheme.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let path = percent_decode(rest);
    // `/D:/x`: a Windows drive after the root slash.
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(PathBuf::from(&path[1..]));
    }
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The first folder in a `workspace_uris` value (a JSON list of `file://` URIs).
fn first_workspace(uris: &str) -> Option<PathBuf> {
    serde_json::from_str::<Vec<String>>(uris)
        .ok()?
        .iter()
        .find_map(|u| file_uri_path(u))
}

/// The Antigravity CLI's database under the home folder.
pub fn gemini_db(home: &Path) -> PathBuf {
    home.join(".gemini")
        .join("antigravity-cli")
        .join("conversation_summaries.db")
}

/// The top-level conversations in the Antigravity CLI's database. One with a parent
/// conversation or a nesting depth above 0 was started by another and is skipped.
/// `not_fully_idle`, `killed` and `status` are not used: their meaning is not documented,
/// and every value seen so far was 0, 0 and an idle status.
pub fn gemini_conversations(db: &Path) -> Vec<GeminiConversation> {
    const SQL: &str = "SELECT conversation_id, title, step_count, last_modified_time, \
                       workspace_uris, parent_conversation_id, nesting_depth \
                       FROM conversation_summaries";
    rows(db, SQL, |row| {
        if text(row, 5).is_some() || int(row, 6).is_some_and(|d| d > 0) {
            return None;
        }
        Some(GeminiConversation {
            id: text(row, 0)?,
            title: text(row, 1),
            steps: int(row, 2).unwrap_or(0),
            modified: text(row, 3).as_deref().and_then(gemini_time).unwrap_or(0),
            folder: text(row, 4).as_deref().and_then(first_workspace),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn uris_are_sqlite_and_file_uris() {
        assert_eq!(
            sqlite_uri(Path::new(r"C:\work\demo #1\state_5.sqlite"), "mode=ro"),
            "file:///C:/work/demo%20%231/state_5.sqlite?mode=ro"
        );
        assert_eq!(
            sqlite_uri(Path::new("/home/me/a?b.db"), "mode=ro&immutable=1"),
            "file:///home/me/a%3Fb.db?mode=ro&immutable=1"
        );
        assert_eq!(
            file_uri_path("file:///D:/work/my%20demo"),
            Some(PathBuf::from("D:/work/my demo"))
        );
        assert_eq!(
            file_uri_path("file:///d%3A/work/demo"),
            Some(PathBuf::from("d:/work/demo"))
        );
        assert_eq!(
            file_uri_path("file:///home/me/demo"),
            Some(PathBuf::from("/home/me/demo"))
        );
        assert_eq!(file_uri_path("https://example.com/x"), None);
        assert_eq!(
            first_workspace(r#"["file:///C:/work/demo","file:///C:/work/other"]"#),
            Some(PathBuf::from("C:/work/demo"))
        );
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn windows_prefixes_are_dropped() {
        assert_eq!(
            plain_path(r"\\?\C:\work\demo"),
            PathBuf::from(r"C:\work\demo")
        );
        assert_eq!(
            plain_path(r"\\?\UNC\server\share"),
            PathBuf::from(r"\\server\share")
        );
        assert_eq!(plain_path("/home/me"), PathBuf::from("/home/me"));
    }

    #[test]
    fn gemini_times_parse() {
        assert_eq!(
            gemini_time("2026-11-02 18:04:20.9701445+00:00"),
            Some(1_793_642_660)
        );
        assert_eq!(
            gemini_time("2026-11-02T19:04:20+01:00"),
            Some(1_793_642_660)
        );
        assert_eq!(gemini_time("not a time"), None);
    }

    fn codex_db(dir: &Path) -> PathBuf {
        let db = dir.join("state_5.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT, cwd TEXT, title TEXT, \
             name TEXT, model TEXT, updated_at_ms INTEGER, archived INTEGER, source TEXT, \
             first_user_message TEXT, preview TEXT);
             INSERT INTO threads VALUES ('t1', '\\\\?\\C:\\work\\demo\\r1.jsonl', \
               '\\\\?\\C:\\work\\demo', 'Demo title', 'demo-codex', 'gpt-demo', 1793729060123, \
               0, 'vscode', 'SECRET-TEXT', 'SECRET-TEXT');
             INSERT INTO threads VALUES ('t2', 'C:/work/r2.jsonl', NULL, 'Only a title', NULL, \
               NULL, NULL, 0, 'cli', 'SECRET-TEXT', 'SECRET-TEXT');
             INSERT INTO threads VALUES ('t3', 'C:/work/r3.jsonl', 'C:/work', 'Archived', NULL, \
               'gpt-demo', 1, 1, 'cli', '', '');
             INSERT INTO threads VALUES ('t4', 'C:/work/r4.jsonl', 'C:/work', 'Review', NULL, \
               'gpt-demo', 1, 0, '{\"subagent\":{\"other\":\"review\"}}', '', '');",
        )
        .unwrap();
        db
    }

    #[test]
    fn codex_threads_skip_archived_and_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let db = codex_db(dir.path());
        // The newest state database is found by its number.
        fs::write(dir.path().join("state_4.sqlite"), b"").unwrap();
        fs::write(dir.path().join("state_x.sqlite"), b"").unwrap();
        assert_eq!(codex_state_db(dir.path()), Some(db.clone()));

        let threads = codex_threads(&db);
        assert_eq!(threads.len(), 2, "{threads:?}");
        assert_eq!(
            threads[0],
            CodexThread {
                id: "t1".into(),
                rollout: PathBuf::from(r"C:\work\demo\r1.jsonl"),
                folder: Some(PathBuf::from(r"C:\work\demo")),
                title: Some("demo-codex".into()),
                name: Some("demo-codex".into()),
                model: "gpt-demo".into(),
                updated: 1_793_729_060,
            }
        );
        assert_eq!(threads[1].title, None);
        assert_eq!((threads[1].folder.as_ref(), threads[1].updated), (None, 0));
        assert!(!format!("{threads:?}").contains("SECRET"));
    }

    #[test]
    fn missing_databases_and_tables_give_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        assert!(codex_threads(&dir.path().join("none.sqlite")).is_empty());
        assert!(gemini_conversations(&dir.path().join("none.db")).is_empty());
        assert_eq!(codex_state_db(&dir.path().join("none")), None);
        let db = dir.path().join("other.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE threads (id TEXT)")
            .unwrap();
        assert!(codex_threads(&db).is_empty());
        assert!(gemini_conversations(&db).is_empty());
        // Opened read-only: no journal or shared-memory file was made beside it.
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["other.db"]);
    }

    #[test]
    fn a_database_in_wal_mode_is_read_with_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let db = codex_db(dir.path());
        // An open writer keeps its newest rows in the log, as a running Codex does.
        let writer = Connection::open(&db).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                 INSERT INTO threads VALUES ('t5', 'C:/work/r5.jsonl', 'C:/work', 'New', \
                   NULL, 'gpt-demo', 2, 0, 'cli', '', '');",
            )
            .unwrap();
        assert!(dir.path().join("state_5.sqlite-shm").is_file());
        let ids: Vec<String> = codex_threads(&db).into_iter().map(|t| t.id).collect();
        assert!(ids.contains(&"t5".to_string()), "{ids:?}");
        drop(writer);
    }

    #[test]
    fn gemini_conversations_skip_children_and_never_read_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("conversation_summaries.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE conversation_summaries (conversation_id TEXT, title TEXT, \
                 preview TEXT, step_count INTEGER, last_modified_time datetime, \
                 workspace_uris TEXT, parent_conversation_id TEXT, nesting_depth INTEGER, \
                 raw_summary BLOB);
                 INSERT INTO conversation_summaries VALUES ('g1', 'Demo chat', 'SECRET-TEXT', \
                   12, '2026-11-02 18:04:20.9701445+00:00', '[\"file:///C:/work/demo\"]', '', 0, \
                   X'00');
                 INSERT INTO conversation_summaries VALUES ('g2', '', 'SECRET-TEXT', 0, \
                   '0001-01-01 00:00:00+00:00', '[]', '', 0, NULL);
                 INSERT INTO conversation_summaries VALUES ('g3', 'Child', '', 1, \
                   '2026-11-02 18:04:20+00:00', '[]', 'g1', 1, NULL);",
            )
            .unwrap();
        let convs = gemini_conversations(&db);
        assert_eq!(
            convs,
            [
                GeminiConversation {
                    id: "g1".into(),
                    title: Some("Demo chat".into()),
                    steps: 12,
                    modified: 1_793_642_660,
                    folder: Some(PathBuf::from("C:/work/demo")),
                },
                GeminiConversation {
                    id: "g2".into(),
                    title: None,
                    steps: 0,
                    modified: -62_135_596_800,
                    folder: None,
                },
            ]
        );
    }

    fn token_line(ts: &str, total: u64, input: u64) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total},"cached_input_tokens":0,"output_tokens":0,"total_tokens":{total}}},"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{input},"cache_write_input_tokens":0,"output_tokens":5,"total_tokens":{input}}},"model_context_window":400000}}}}}}"#
        ) + "\n"
    }

    fn append(path: &Path, text: &str) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    /// 2026-11-02T00:00:00Z, standing in for local midnight.
    const DAY: i64 = 1_793_577_600;

    #[test]
    fn a_rollout_gives_context_and_todays_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        append(
            &path,
            r#"{"timestamp":"2026-11-01T10:00:00.000Z","type":"response_item","payload":{"type":"message","content":"SECRET-TEXT token_count"}}"#,
        );
        append(&path, "\n");
        append(
            &path,
            r#"{"timestamp":"2026-11-01T10:00:01.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#,
        );
        append(&path, "\n");
        append(&path, &token_line("2026-11-01T23:00:00.000Z", 1_000, 900));
        append(&path, &token_line("2026-11-02T09:00:00.000Z", 1_500, 1_400));

        let mut t = RolloutTally::default();
        t.update(&path, DAY);
        assert_eq!((t.context, t.tokens()), (Some(1_400), 500));

        // Appended lines only; a line still being written waits for its newline.
        let line = token_line("2026-11-02T09:05:00.000Z", 2_100, 1_900);
        let (head, tail) = line.split_at(40);
        append(&path, head);
        t.update(&path, DAY);
        assert_eq!((t.context, t.tokens()), (Some(1_400), 500));
        append(&path, tail);
        t.update(&path, DAY);
        assert_eq!((t.context, t.tokens()), (Some(1_900), 1_100));

        // The next day counts from the total at midnight.
        t.update(&path, DAY + 86_400);
        assert_eq!((t.context, t.tokens()), (Some(1_900), 0));
        append(&path, &token_line("2026-11-03T08:00:00.000Z", 2_400, 2_000));
        t.update(&path, DAY + 86_400);
        assert_eq!(t.tokens(), 300);
    }

    #[test]
    fn an_old_rollout_is_read_from_its_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        append(&path, &token_line("2026-10-30T09:00:00.000Z", 1_000, 800));
        append(&path, &token_line("2026-10-30T09:05:00.000Z", 1_200, 950));
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs((DAY - 3600) as u64))
            .unwrap();
        let mut t = RolloutTally::default();
        t.update(&path, DAY);
        assert_eq!((t.context, t.tokens()), (Some(950), 0));
        // A reply today counts from the old total.
        append(&path, &token_line("2026-11-02T09:00:00.000Z", 1_700, 1_100));
        t.update(&path, DAY);
        assert_eq!((t.context, t.tokens()), (Some(1_100), 500));
    }
}
