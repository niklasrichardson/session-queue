//! `cq idle-alert`: tell a lead when a worker session goes idle (issue #50).
//!
//! The Claude Code `Notification` hook (matcher `idle_prompt`) runs
//! `cq idle-alert` when a session waits for input. The owner lists the
//! workers to watch in `idle-alerts`, one `worker lead` pair per line. For a
//! listed worker that is awake, `cq` sends the lead one `info` message with
//! a fixed subject and body, at most once per interval, and records the
//! time in `idle/<worker>`. The message holds no text from the session's
//! work. Every limit of `cq send` applies; a refused send records nothing.
//!
//! Claude Code runs matching hooks in parallel, so the check, the send and
//! the record run under an exclusive lock on `idle/<worker>.lock`. A hook
//! that finds the lock taken sends nothing.
//!
//! The desktop app does not send `idle_prompt`, so a `Stop` hook runs
//! `cq turn-end` instead. A turn end alone does not mean idle: the session
//! may be woken again at once. So `cq turn-end` only records the time, the
//! transcript and its length in `idle/<worker>.turn-end`, and the worker's
//! `cq watch` sends the alert when no activity is observed for
//! `--idle-after`: no new `user` entry in the transcript, no transcript
//! write after `SETTLE`, and no line delivered by the watcher since the
//! turn end (see `IdleCheck`).

use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{CqError, Result};
use crate::fsutil;
use crate::message::MsgType;
use crate::names::is_valid_name;
use crate::root::Root;
use crate::send::{self, Hops, Outgoing};
use crate::sleep;
use crate::timefmt::{self, rfc3339};

/// The default time between two alerts for one worker.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(60 * 60);
/// The default time after a turn end, with no change to the transcript,
/// before the watcher sends the alert.
pub const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(5 * 60);
/// Claude Code may still write to the transcript just after the `Stop`
/// hook. A write within this long after the turn end is not a new turn.
pub const SETTLE: Duration = Duration::from_secs(30);

/// What `alert` did.
#[derive(Debug)]
pub enum Outcome {
    /// The name is not listed in `idle-alerts`.
    NotListed,
    Asleep,
    /// Another `cq idle-alert` for the name holds the lock.
    Busy,
    /// The last alert was less than the interval ago (UTC, RFC 3339).
    Recent(String),
    /// Sent to the lead: the message ID.
    Sent {
        lead: String,
        id: String,
    },
    /// The lock or `cq send` failed, or the send was refused; nothing was
    /// recorded.
    NotSent(CqError),
    /// Sent, but the time could not be recorded.
    NotRecorded {
        id: String,
        error: CqError,
    },
}

/// The lead for `name`: the first valid `worker lead` line for it in
/// `idle-alerts`. Blank lines, `#` comments and lines that are not two
/// valid names are skipped. A missing file means no one is listed.
pub fn lead_for(root: &Root, name: &str) -> Option<String> {
    let text = fs::read_to_string(root.idle_alerts()).ok()?;
    pairs(&text)
        .find(|(worker, _)| *worker == name)
        .map(|(_, lead)| lead.to_owned())
}

/// The valid `worker lead` pairs in the config text.
fn pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let mut words = line.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some(w), Some(l), None) if is_valid_name(w) && is_valid_name(l) => Some((w, l)),
            _ => None,
        }
    })
}

/// The subject and body of the alert for `name` at `now`.
pub fn text(name: &str, now: Timestamp) -> (String, String) {
    (
        format!("{name} is idle"),
        format!("Idle since {}.", timefmt::local_hhmm(now)),
    )
}

/// The time of the last alert for `name`, if `idle/<name>` holds one.
fn last_alert(root: &Root, name: &str) -> Option<Timestamp> {
    let text = fs::read_to_string(root.idle_file(name)).ok()?;
    timefmt::parse_rfc3339_z(text.trim())
}

/// True when `last` is less than `every` before `now`. A time in the
/// future does not count, so a wrong clock cannot stop alerts for long.
fn is_recent(last: Timestamp, now: Timestamp, every: Duration) -> bool {
    let age = now.duration_since(last);
    !age.is_negative() && age.unsigned_abs() < every
}

/// Send the idle alert for `name` if it is listed, awake and not alerted
/// within `every`. `hold` waits that long after the check, under the
/// lock, so tests can run hooks that overlap; it is zero otherwise.
pub fn alert(root: &Root, name: &str, every: Duration, hold: Duration) -> Outcome {
    let Some(lead) = lead_for(root, name) else {
        return Outcome::NotListed;
    };
    if sleep::is_asleep(root, name) {
        return Outcome::Asleep;
    }
    // Held until this function returns, so no other hook for the name can
    // pass the check below before this one records its alert.
    let dir = root.idle_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        return Outcome::NotSent(CqError::io(format!("create {}", dir.display()), e));
    }
    let _lock = match try_lock(&root.idle_lock(name)) {
        Ok(Some(file)) => file,
        Ok(None) => return Outcome::Busy,
        Err(e) => return Outcome::NotSent(e),
    };
    let now = timefmt::now();
    if let Some(last) = last_alert(root, name).filter(|t| is_recent(*t, now, every)) {
        return Outcome::Recent(rfc3339(last));
    }
    std::thread::sleep(hold);
    let (subject, body) = text(name, now);
    let out = Outgoing {
        from: name.to_owned(),
        to: lead.clone(),
        kind: MsgType::Info,
        subject,
        body,
        reply_to: None,
        parent: None,
        status: None,
    };
    let id = match send::send(root, out, Hops::Lookup) {
        Ok(id) => id,
        Err(e) => return Outcome::NotSent(e),
    };
    match record(root, name, now) {
        Ok(()) => Outcome::Sent { lead, id },
        Err(error) => Outcome::NotRecorded { id, error },
    }
}

/// Take the exclusive lock on `path` in `idle/` without waiting. `None`
/// when another process holds it. The OS releases it when the process
/// exits, however it exits.
fn try_lock(path: &Path) -> Result<Option<File>> {
    let file = fsutil::open_lock_file(path)
        .map_err(|e| CqError::io(format!("open {}", path.display()), e))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(CqError::io(format!("lock {}", path.display()), e)),
    }
}

/// Write the alert time to `idle/<name>` through `tmp/`.
fn record(root: &Root, name: &str, now: Timestamp) -> Result<()> {
    let dir = root.idle_dir();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let path = root.idle_file(name);
    let bytes = format!("{}\n", rfc3339(now));
    fsutil::write_atomic(&root.tmp(), &path, bytes.as_bytes())
        .map_err(|e| CqError::io(format!("write {}", path.display()), e))
}

/// `idle/<name>.turn-end`: the last turn end, as `cq turn-end` records it.
/// It holds nothing from the hook's input except the transcript path, and
/// nothing from the transcript except its length.
#[derive(Debug, Serialize, Deserialize)]
pub struct TurnEnd {
    /// Unique per turn end, so a consumer never removes a newer marker.
    pub generation: String,
    /// UTC, RFC 3339, with fractions of a second, so it orders against the
    /// watcher's deliveries.
    pub turn_end: String,
    /// The session's transcript, from the hook's input.
    pub transcript_path: PathBuf,
    /// The transcript's length in bytes at the turn end. The watcher reads
    /// only what is added after it.
    pub transcript_len: u64,
}

/// The parts of the `Stop` hook's input that `cq turn-end` uses.
/// `stop_hook_active` is not used: a newer marker is harmless.
#[derive(Debug, Default, Deserialize)]
pub struct StopInput {
    pub transcript_path: Option<PathBuf>,
    /// In-flight background work. Absent when Claude Code does not report
    /// it (older versions, or no task registry): then it is unknown.
    pub background_tasks: Option<Vec<BackgroundTask>>,
}

/// One entry of the hook's `background_tasks`. Read only to decide; never
/// copied anywhere.
#[derive(Debug, Default, Deserialize)]
pub struct BackgroundTask {
    pub status: Option<String>,
    /// Present only for shell tasks.
    pub command: Option<String>,
}

impl StopInput {
    /// The hook's JSON, or `None` when it is not valid.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        serde_json::from_slice(bytes).ok()
    }

    /// True when the hook reports background work other than the cq
    /// watcher, which may wake the session later. Scheduled wakeups
    /// (`session_crons`) do not count: the watcher's transcript check
    /// catches the wake.
    pub fn has_other_background_work(&self) -> bool {
        self.background_tasks
            .iter()
            .flatten()
            .any(|t| !t.is_finished() && !t.is_cq_watcher())
    }
}

impl BackgroundTask {
    fn is_finished(&self) -> bool {
        self.status.as_deref().is_some_and(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "completed" | "failed" | "killed" | "stopped" | "cancelled" | "canceled"
            )
        })
    }

    /// A `cq watch` run: the command's program is `cq` or `cq.exe`, at any
    /// path, and its first argument is `watch`. A task with no command
    /// (a subagent, or a tool that reports none) is never the watcher,
    /// whatever its description says.
    fn is_cq_watcher(&self) -> bool {
        let Some(command) = &self.command else {
            return false;
        };
        let mut words = Words(command);
        let program = words.next().unwrap_or_default();
        let file = program.rsplit(['/', '\\']).next().unwrap_or_default();
        matches!(file.to_ascii_lowercase().as_str(), "cq" | "cq.exe")
            && words.next().as_deref() == Some("watch")
    }
}

/// The words of a command line, split at spaces. A word in double or single
/// quotes may hold spaces; the quotes are dropped.
struct Words<'a>(&'a str);

impl Iterator for Words<'_> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        let rest = self.0.trim_start();
        let mut word = String::new();
        let mut quote = None;
        let mut end = rest.len();
        for (i, c) in rest.char_indices() {
            match (quote, c) {
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), _) if c == q => quote = None,
                (None, c) if c.is_whitespace() => {
                    end = i;
                    break;
                }
                (_, c) => word.push(c),
            }
        }
        self.0 = &rest[end..];
        (!rest.is_empty()).then_some(word)
    }
}

/// What `mark_turn_end` did.
#[derive(Debug)]
pub enum Marked {
    /// The name is not listed in `idle-alerts`.
    NotListed,
    Asleep,
    /// The hook's input has no transcript path, or the transcript cannot be
    /// read.
    NoTranscript,
    /// The hook reports other background work, so the session may be woken
    /// later: no marker, and an older one is removed.
    Background,
    Written,
    /// The marker lock stayed taken, or the write or removal failed. The
    /// marker was not touched, or the removal failed.
    NotWritten(CqError),
}

/// Record a turn end for `name` in `idle/<name>.turn-end`, through `tmp/`,
/// if it is listed and awake. A newer turn end replaces an older one.
pub fn mark_turn_end(root: &Root, name: &str, input: &StopInput) -> Marked {
    if lead_for(root, name).is_none() {
        return Marked::NotListed;
    }
    if sleep::is_asleep(root, name) {
        return Marked::Asleep;
    }
    let Some(transcript) = input
        .transcript_path
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return Marked::NoTranscript;
    };
    // Held while writing or removing, so a watcher never decides on a
    // marker that changes under it.
    let lock_path = root.turn_end_lock(name);
    let _lock = match lock_marker(root, name, MARKER_LOCK_WAIT) {
        Ok(Some(file)) => file,
        Ok(None) => {
            let e = io::Error::new(io::ErrorKind::WouldBlock, "taken");
            return Marked::NotWritten(CqError::io(format!("lock {}", lock_path.display()), e));
        }
        Err(e) => return Marked::NotWritten(e),
    };
    let path = root.turn_end_marker(name);
    if input.has_other_background_work() {
        return match remove(&path) {
            Ok(()) => Marked::Background,
            Err(e) => Marked::NotWritten(CqError::io(format!("remove {}", path.display()), e)),
        };
    }
    let meta = match fs::metadata(transcript) {
        Ok(m) if m.is_file() => m,
        _ => return Marked::NoTranscript,
    };
    let now = timefmt::now();
    let marker = TurnEnd {
        generation: format!(
            "{}-{}-{:04x}",
            timefmt::id_stamp(now),
            std::process::id(),
            fsutil::random_u16()
        ),
        turn_end: now.to_string(),
        transcript_path: transcript.to_owned(),
        transcript_len: meta.len(),
    };
    let bytes = serde_json::to_vec(&marker).expect("a marker serialises");
    match fsutil::write_atomic(&root.tmp(), &path, &bytes) {
        Ok(()) => Marked::Written,
        Err(e) => Marked::NotWritten(CqError::io(format!("write {}", path.display()), e)),
    }
}

/// How long `cq turn-end` waits for the marker lock. A watcher holds it
/// only for one check: a read, maybe a send, and a removal.
const MARKER_LOCK_WAIT: Duration = Duration::from_secs(1);

/// The lock on `idle/<name>.turn-end.lock`, waiting up to `wait`. `None`
/// when it stays taken. It is separate from `idle/<name>.lock`, which
/// `alert` takes, so a check can send under it.
fn lock_marker(root: &Root, name: &str, wait: Duration) -> Result<Option<File>> {
    let dir = root.idle_dir();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let until = Instant::now() + wait;
    loop {
        match try_lock(&root.turn_end_lock(name))? {
            Some(file) => return Ok(Some(file)),
            None if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            None => return Ok(None),
        }
    }
}

/// Remove `path`. A file that is already gone counts as removed.
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What the watcher does with a turn end on one check.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Wait,
    /// Send the alert, then remove the marker.
    Alert,
    /// Remove the marker without an alert: the session was active.
    Cancel,
}

/// The step for a turn end at `turn_end`, checked at `now`. `modified` is
/// the transcript's modified time, `None` when it cannot be read. A write
/// later than `SETTLE` after the turn end means a new turn. The check
/// needs no earlier look, so a watcher that starts late decides the same.
fn step(
    turn_end: SystemTime,
    now: SystemTime,
    idle_after: Duration,
    modified: Option<SystemTime>,
) -> Step {
    let Some(modified) = modified else {
        return Step::Cancel;
    };
    if turn_end.checked_add(SETTLE).is_none_or(|t| modified > t) {
        return Step::Cancel;
    }
    // A turn end in the future waits; the next turn end replaces it.
    match now.duration_since(turn_end) {
        Ok(age) if age >= idle_after => Step::Alert,
        _ => Step::Wait,
    }
}

/// The most transcript the watcher reads after a turn end. More than this
/// added counts as a new turn.
const MAX_ADDED: u64 = 1024 * 1024;

/// True when the transcript at `path` shows a new turn after it was `len`
/// bytes long: an added entry has type `user` (a prompt, a tool result or
/// a queued notification), more than `MAX_ADDED` was added, or it shrank or
/// cannot be read. Only each entry's `type` is parsed; the text is never
/// kept. A last line with no newline is still being written and is skipped.
fn new_turn(path: &Path, len: u64) -> bool {
    #[derive(Deserialize)]
    struct Entry {
        #[serde(rename = "type")]
        kind: Option<String>,
    }
    let read = || -> io::Result<bool> {
        let mut file = File::open(path)?;
        let now = file.metadata()?.len();
        if now < len || now - len > MAX_ADDED {
            return Ok(true);
        }
        file.seek(SeekFrom::Start(len))?;
        let mut added = Vec::new();
        file.take(now - len).read_to_end(&mut added)?;
        let complete = added
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(&[][..], |end| &added[..end]);
        Ok(complete.split(|b| *b == b'\n').any(|line| {
            serde_json::from_slice::<Entry>(line).is_ok_and(|e| e.kind.as_deref() == Some("user"))
        }))
    };
    read().unwrap_or(true)
}

/// Waits before the second and third try when the alert was not sent
/// (`Busy`, `NotSent`). After the third, the turn end is dropped.
const RETRY_AFTER: [Duration; 2] = [Duration::from_secs(90), Duration::from_secs(180)];

/// The watcher's side of the `Stop` hook: on each check, look at
/// `idle/<name>.turn-end` and send the idle alert once `idle_after` has
/// passed since the turn ended with no sign of a new turn. It never prints
/// anything, so it cannot break the watcher's lines.
#[derive(Debug, Default)]
pub struct IdleCheck {
    /// The generation that failed to send, the number of failed tries and
    /// when to try again.
    retry: Option<(String, usize, Instant)>,
    /// A generation that is over (sent, cancelled or dropped) but whose
    /// marker could not be removed yet. It is never sent; each check tries
    /// the removal again.
    finished: Option<String>,
    /// When the watcher last delivered a line, which wakes the session. A
    /// turn end before it is over.
    delivered: Option<SystemTime>,
}

impl IdleCheck {
    /// One check for `name`. Does nothing unless the name is listed in
    /// `idle-alerts`, read again on each check. The read, the decision, the
    /// send and the removal all run under the marker lock, so a turn end
    /// that replaces the marker waits for them; when a hook holds the lock,
    /// this check is skipped.
    pub fn run(&mut self, root: &Root, name: &str, idle_after: Duration, every: Duration) {
        if lead_for(root, name).is_none() {
            return;
        }
        let path = root.turn_end_marker(name);
        if !path.exists() {
            self.retry = None;
            self.finished = None;
            return;
        }
        let Ok(Some(_lock)) = lock_marker(root, name, Duration::ZERO) else {
            return;
        };
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.retry = None;
                self.finished = None;
                return;
            }
            Err(_) => return,
        };
        let parsed = serde_json::from_str::<TurnEnd>(&text)
            .ok()
            .and_then(|m| Some((timefmt::parse_rfc3339_z(&m.turn_end)?, m)));
        let Some((at, marker)) = parsed else {
            self.retry = None;
            let _ = remove(&path);
            return;
        };
        let generation = marker.generation.as_str();
        if self.finished.as_deref() == Some(generation) {
            self.finish(&path, generation);
            return;
        }
        self.finished = None;
        let tries = match &self.retry {
            Some((g, _, next)) if g == generation && Instant::now() < *next => return,
            Some((g, n, _)) if g == generation => *n,
            _ => 0,
        };
        let at = SystemTime::from(at);
        let modified =
            fsutil::modified(&marker.transcript_path).filter(|_| marker.transcript_path.is_file());
        let mut next = step(at, SystemTime::now(), idle_after, modified);
        if self.delivered.is_some_and(|d| at < d)
            || (next == Step::Alert && new_turn(&marker.transcript_path, marker.transcript_len))
        {
            next = Step::Cancel;
        }
        match next {
            Step::Wait => {}
            Step::Cancel => self.finish(&path, generation),
            Step::Alert => match alert(root, name, every, Duration::ZERO) {
                Outcome::Busy | Outcome::NotSent(_) if tries < RETRY_AFTER.len() => {
                    let next = Instant::now() + RETRY_AFTER[tries];
                    self.retry = Some((generation.to_owned(), tries + 1, next));
                }
                // Sent, recorded or not; within `every` of the last alert;
                // no longer listed; asleep; or out of tries.
                _ => self.finish(&path, generation),
            },
        }
    }

    /// The turn end `generation` is over: remove its marker, which the
    /// caller has read under the lock. Until the removal works, the
    /// generation stays finished, so it is never sent.
    fn finish(&mut self, path: &Path, generation: &str) {
        self.retry = None;
        self.finished = match remove(path) {
            Ok(()) => None,
            Err(_) => Some(generation.to_owned()),
        };
    }

    /// The watcher delivered a line at `at`, which wakes the session: a
    /// turn end before it is not idle.
    pub fn delivered(&mut self, at: SystemTime) {
        self.delivered = Some(self.delivered.map_or(at, |d| d.max(at)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn pairs_skip_comments_blanks_and_bad_lines() {
        let text = "# idle alerts\n\n  worker-a   lead  \nworker-b\nWorker-C lead\nworker-d lead extra\nworker-e ../x\n#worker-f lead\nworker-g lead\n";
        let got: Vec<_> = pairs(text).collect();
        assert_eq!(got, [("worker-a", "lead"), ("worker-g", "lead")]);
    }

    #[test]
    fn recent_is_within_the_interval_and_not_in_the_future() {
        let now = ts("2026-10-29T12:00:00Z");
        let hour = DEFAULT_EVERY;
        assert!(is_recent(ts("2026-10-29T11:00:01Z"), now, hour));
        assert!(is_recent(now, now, hour));
        assert!(!is_recent(ts("2026-10-29T11:00:00Z"), now, hour));
        assert!(!is_recent(ts("2026-10-29T12:00:01Z"), now, hour));
    }

    #[test]
    fn text_is_fixed() {
        let (subject, body) = text("worker-a", ts("2026-10-29T12:00:00Z"));
        assert_eq!(subject, "worker-a is idle");
        assert!(
            body.starts_with("Idle since ") && body.ends_with('.'),
            "{body}"
        );
    }

    const SEC: Duration = Duration::from_secs(1);
    const MIN: Duration = Duration::from_secs(60);

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    #[test]
    fn an_idle_turn_end_alerts_after_idle_after() {
        let (t, five) = (t0(), DEFAULT_IDLE_AFTER);
        let written = Some(t + 5 * SEC);
        assert_eq!(step(t, t, five, written), Step::Wait);
        assert_eq!(step(t, t + 4 * MIN, five, written), Step::Wait);
        assert_eq!(step(t, t + five, five, written), Step::Alert);
        // A turn end in the future never alerts.
        assert_eq!(step(t + MIN, t, five, written), Step::Wait);
    }

    #[test]
    fn a_final_write_inside_the_settle_time_still_alerts() {
        let (t, five) = (t0(), DEFAULT_IDLE_AFTER);
        assert_eq!(step(t, t + five, five, Some(t + SETTLE)), Step::Alert);
        assert_eq!(step(t, t + five, five, Some(t + 29 * SEC)), Step::Alert);
    }

    #[test]
    fn a_write_after_the_settle_time_cancels() {
        let (t, five) = (t0(), DEFAULT_IDLE_AFTER);
        let later = Some(t + SETTLE + SEC);
        assert_eq!(step(t, t + 31 * SEC, five, later), Step::Cancel);
        assert_eq!(step(t, t + five, five, later), Step::Cancel);
        // A watcher that starts late decides from the modified time alone.
        assert_eq!(step(t, t + 10 * MIN, five, Some(t + 3 * MIN)), Step::Cancel);
        assert_eq!(step(t, t + 10 * MIN, five, Some(t + 10 * SEC)), Step::Alert);
    }

    #[test]
    fn a_missing_transcript_cancels() {
        let (t, five) = (t0(), DEFAULT_IDLE_AFTER);
        assert_eq!(step(t, t + SEC, five, None), Step::Cancel);
        assert_eq!(step(t, t + five, five, None), Step::Cancel);
    }

    fn task(status: &str, command: Option<&str>) -> BackgroundTask {
        BackgroundTask {
            status: Some(status.to_owned()),
            command: command.map(str::to_owned),
        }
    }

    #[test]
    fn background_work_other_than_the_watcher_counts() {
        let input = |tasks: Option<Vec<BackgroundTask>>| StopInput {
            transcript_path: None,
            background_tasks: tasks,
        };
        let busy = |command: Option<&str>| {
            input(Some(vec![task("running", command)])).has_other_background_work()
        };
        // Absent: unknown, so it does not count.
        assert!(!input(None).has_other_background_work());
        assert!(!input(Some(vec![])).has_other_background_work());
        for watcher in [
            r#""C:\tools\cq.exe" watch --as worker-a --to-inbox"#,
            r"'C:\Program Files\cq\CQ.EXE' watch --as worker-a",
            "/usr/local/bin/cq watch --as worker-a 2>&1",
            "  cq   watch",
        ] {
            assert!(!busy(Some(watcher)), "{watcher}");
        }
        for work in [
            r#"grep "cq watch" src/watch.rs"#,
            "echo cq watch",
            "cq list --as worker-a",
            "cq",
            "cqx watch",
            r#""C:\tools\cq.exe watch""#,
            "cargo build",
        ] {
            assert!(busy(Some(work)), "{work}");
        }
        // No command: a subagent, or a watcher run through a tool that
        // reports none. It counts, whatever its description says.
        let monitor: StopInput = serde_json::from_str(
            r#"{"background_tasks":[{"type":"monitor","status":"running","description":"cq watch --as worker-a"}]}"#,
        )
        .unwrap();
        assert!(monitor.has_other_background_work());
        let agent: StopInput = serde_json::from_str(
            r#"{"background_tasks":[{"type":"subagent","status":"running","description":"review cq watch races"}]}"#,
        )
        .unwrap();
        assert!(agent.has_other_background_work());
        assert!(
            !input(Some(vec![task("completed", Some("cargo build"))])).has_other_background_work()
        );
    }

    #[test]
    fn stop_input_parses_the_hook_json() {
        let json = br#"{"session_id":"s","transcript_path":"/t/x.jsonl","stop_hook_active":true,
            "background_tasks":[{"id":"1","type":"subagent","status":"running","description":"d","agent_type":"x"}],
            "session_crons":[{"id":"c","schedule":"0 9 * * *","recurring":true,"prompt":"p"}]}"#;
        let input = StopInput::parse(json).unwrap();
        assert_eq!(input.transcript_path, Some(PathBuf::from("/t/x.jsonl")));
        assert!(input.has_other_background_work());
        let bom = b"\xEF\xBB\xBF{\"transcript_path\":\"/t/x.jsonl\"}";
        let input = StopInput::parse(bom).unwrap();
        assert!(input.background_tasks.is_none());
        assert!(StopInput::parse(b"not json").is_none());
    }

    /// A root where `worker-a` is listed, with a transcript file.
    fn listed_root() -> (tempfile::TempDir, Root, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path().to_owned());
        root.init().unwrap();
        fs::write(root.idle_alerts(), "worker-a lead\n").unwrap();
        let tx = dir.path().join("transcript.jsonl");
        fs::write(&tx, "{\"type\":\"user\"}\n").unwrap();
        (dir, root, tx)
    }

    fn set_modified_ago(path: &Path, secs: u64) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    fn append(path: &Path, text: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        io::Write::write_all(&mut f, text.as_bytes()).unwrap();
    }

    /// Write a marker whose turn end was `secs` ago, with the transcript
    /// last written then. Returns its generation.
    fn plant(root: &Root, tx: &Path, secs: u64, generation: &str) -> String {
        set_modified_ago(tx, secs);
        let at = timefmt::now() - jiff::SignedDuration::from_secs(secs as i64);
        let marker = TurnEnd {
            generation: generation.to_owned(),
            turn_end: at.to_string(),
            transcript_path: tx.to_owned(),
            transcript_len: fs::metadata(tx).unwrap().len(),
        };
        fs::create_dir_all(root.idle_dir()).unwrap();
        fs::write(
            root.turn_end_marker("worker-a"),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        marker.generation
    }

    fn generation(root: &Root) -> Option<String> {
        let text = fs::read_to_string(root.turn_end_marker("worker-a")).ok()?;
        Some(serde_json::from_str::<TurnEnd>(&text).unwrap().generation)
    }

    fn alerts(root: &Root) -> usize {
        fsutil::json_stems(&root.inbox("lead"), |_| true).len()
    }

    fn check(c: &mut IdleCheck, root: &Root) {
        c.run(root, "worker-a", DEFAULT_IDLE_AFTER, DEFAULT_EVERY);
    }

    fn input(tx: &Path, tasks: Option<Vec<BackgroundTask>>) -> StopInput {
        StopInput {
            transcript_path: Some(tx.to_owned()),
            background_tasks: tasks,
        }
    }

    #[test]
    fn mark_turn_end_writes_only_time_generation_and_transcript() {
        let (_dir, root, tx) = listed_root();
        assert!(matches!(
            mark_turn_end(&root, "worker-a", &input(&tx, None)),
            Marked::Written
        ));
        let text = fs::read_to_string(root.turn_end_marker("worker-a")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "generation",
                "transcript_len",
                "transcript_path",
                "turn_end"
            ]
        );
        assert_eq!(v["transcript_len"], fs::metadata(&tx).unwrap().len());

        // Other background work: no marker, and the older one goes.
        let busy = input(&tx, Some(vec![task("running", None)]));
        assert!(matches!(
            mark_turn_end(&root, "worker-a", &busy),
            Marked::Background
        ));
        assert!(!root.turn_end_marker("worker-a").exists());

        // A transcript that cannot be read: nothing is recorded.
        let gone = input(&tx.with_extension("gone"), None);
        assert!(matches!(
            mark_turn_end(&root, "worker-a", &gone),
            Marked::NoTranscript
        ));
        assert!(!root.turn_end_marker("worker-a").exists());
    }

    #[test]
    fn mark_turn_end_touches_nothing_without_the_marker_lock() {
        let (_dir, root, tx) = listed_root();
        let lock = lock_marker(&root, "worker-a", Duration::ZERO)
            .unwrap()
            .unwrap();
        assert!(matches!(
            mark_turn_end(&root, "worker-a", &input(&tx, None)),
            Marked::NotWritten(_)
        ));
        assert!(!root.turn_end_marker("worker-a").exists());
        plant(&root, &tx, 600, "g1");
        let busy = input(&tx, Some(vec![task("running", None)]));
        assert!(matches!(
            mark_turn_end(&root, "worker-a", &busy),
            Marked::NotWritten(_)
        ));
        assert_eq!(generation(&root).as_deref(), Some("g1"));
        drop(lock);
    }

    #[test]
    fn the_check_alerts_once_and_removes_the_marker() {
        let (_dir, root, tx) = listed_root();
        plant(&root, &tx, 600, "g1");
        // A new check, as after a watcher restart.
        check(&mut IdleCheck::default(), &root);
        assert!(!root.turn_end_marker("worker-a").exists());
        assert_eq!(alerts(&root), 1);
    }

    #[test]
    fn a_recent_alert_discards_the_turn_end() {
        let (_dir, root, tx) = listed_root();
        record(&root, "worker-a", timefmt::now()).unwrap();
        plant(&root, &tx, 600, "g1");
        check(&mut IdleCheck::default(), &root);
        assert!(!root.turn_end_marker("worker-a").exists());
        assert_eq!(alerts(&root), 0);
    }

    #[test]
    fn a_failed_send_is_tried_three_times() {
        let (_dir, root, tx) = listed_root();
        fs::write(root.pause(), "").unwrap();
        plant(&root, &tx, 600, "g1");
        let mut c = IdleCheck::default();
        check(&mut c, &root);
        assert!(
            root.turn_end_marker("worker-a").exists(),
            "kept for a retry"
        );
        // Not before the wait is over.
        check(&mut c, &root);
        assert_eq!(c.retry.as_ref().map(|r| r.1), Some(1));
        for tries in [2, 3] {
            c.retry.as_mut().unwrap().2 = Instant::now();
            check(&mut c, &root);
            if tries < 3 {
                assert_eq!(c.retry.as_ref().map(|r| r.1), Some(tries));
            }
        }
        assert!(!root.turn_end_marker("worker-a").exists(), "dropped");
        assert_eq!(alerts(&root), 0);
    }

    #[test]
    fn a_held_marker_lock_skips_the_check() {
        let (_dir, root, tx) = listed_root();
        plant(&root, &tx, 600, "g1");
        let lock = lock_marker(&root, "worker-a", Duration::ZERO)
            .unwrap()
            .unwrap();
        let mut c = IdleCheck::default();
        check(&mut c, &root);
        assert_eq!(generation(&root).as_deref(), Some("g1"));
        assert_eq!(alerts(&root), 0);
        drop(lock);
        check(&mut c, &root);
        assert_eq!(alerts(&root), 1);
    }

    #[test]
    fn a_marker_replaced_before_the_send_is_decided_again() {
        let (_dir, root, tx) = listed_root();
        plant(&root, &tx, 600, "g1");
        let mut c = IdleCheck::default();
        // A hook holds the lock and replaces the old turn end with a new
        // one while the check runs: the check skips, then waits for g2.
        let lock = lock_marker(&root, "worker-a", Duration::ZERO)
            .unwrap()
            .unwrap();
        check(&mut c, &root);
        plant(&root, &tx, 1, "g2");
        drop(lock);
        check(&mut c, &root);
        assert_eq!(generation(&root).as_deref(), Some("g2"));
        assert_eq!(alerts(&root), 0);
    }

    #[test]
    fn a_failed_removal_is_tried_again_without_a_send() {
        let (dir, root, tx) = listed_root();
        let mut c = IdleCheck::default();
        // A directory cannot be removed as a file.
        let stuck = dir.path().join("stuck");
        fs::create_dir(&stuck).unwrap();
        c.finish(&stuck, "g1");
        assert_eq!(c.finished.as_deref(), Some("g1"));
        // The same turn end, idle long enough: removed, never sent.
        plant(&root, &tx, 600, "g1");
        check(&mut c, &root);
        assert!(!root.turn_end_marker("worker-a").exists());
        assert_eq!(c.finished, None);
        assert_eq!(alerts(&root), 0);
        // Already gone counts as removed.
        c.finish(&root.turn_end_marker("worker-a"), "g2");
        assert_eq!(c.finished, None);
    }

    #[test]
    fn a_line_delivered_after_the_turn_end_cancels_it() {
        let (_dir, root, tx) = listed_root();
        plant(&root, &tx, 600, "g1");
        let mut c = IdleCheck::default();
        c.delivered(SystemTime::now() - Duration::from_secs(300));
        check(&mut c, &root);
        assert!(!root.turn_end_marker("worker-a").exists());
        assert_eq!(alerts(&root), 0);

        // A line delivered before the turn end does not.
        plant(&root, &tx, 600, "g2");
        let mut c = IdleCheck::default();
        c.delivered(SystemTime::now() - Duration::from_secs(700));
        check(&mut c, &root);
        assert_eq!(alerts(&root), 1);
    }

    #[test]
    fn a_new_turn_inside_the_settle_time_cancels() {
        let (_dir, root, tx) = listed_root();
        plant(&root, &tx, 600, "g1");
        // A new prompt 10 seconds after the turn end, and no later write.
        append(
            &tx,
            "{\"type\":\"assistant\"}\n{\"type\":\"user\",\"message\":\"x\"}\n",
        );
        set_modified_ago(&tx, 590);
        check(&mut IdleCheck::default(), &root);
        assert!(!root.turn_end_marker("worker-a").exists());
        assert_eq!(alerts(&root), 0);
    }

    #[test]
    fn only_other_entries_inside_the_settle_time_still_alert() {
        let (_dir, root, tx) = listed_root();
        plant(&root, &tx, 600, "g1");
        append(&tx, "{\"type\":\"assistant\"}\r\n{\"type\":\"system\"}\n");
        set_modified_ago(&tx, 590);
        check(&mut IdleCheck::default(), &root);
        assert!(!root.turn_end_marker("worker-a").exists());
        assert_eq!(alerts(&root), 1);
    }

    #[test]
    fn new_turn_reads_only_complete_added_entries() {
        let dir = tempfile::tempdir().unwrap();
        let tx = dir.path().join("t.jsonl");
        fs::write(&tx, "{\"type\":\"user\"}\n").unwrap();
        let len = fs::metadata(&tx).unwrap().len();
        assert!(!new_turn(&tx, len), "entries before the turn end");
        append(&tx, "{\"type\":\"user\"");
        assert!(!new_turn(&tx, len), "a line still being written");
        append(&tx, "}\n");
        assert!(new_turn(&tx, len));
        assert!(new_turn(&tx, len + 1_000), "shrank");
        assert!(new_turn(&dir.path().join("gone"), 0), "cannot be read");
        fs::write(&tx, vec![b' '; MAX_ADDED as usize + 1]).unwrap();
        assert!(new_turn(&tx, 0), "too much added");
    }

    #[test]
    fn the_check_drops_a_bad_marker_and_ignores_an_unlisted_name() {
        let (_dir, root, tx) = listed_root();
        fs::create_dir_all(root.idle_dir()).unwrap();
        fs::write(root.turn_end_marker("worker-a"), "not json").unwrap();
        check(&mut IdleCheck::default(), &root);
        assert!(!root.turn_end_marker("worker-a").exists());

        // Not listed: the marker stays and nothing is sent.
        fs::write(root.idle_alerts(), "worker-b lead\n").unwrap();
        plant(&root, &tx, 600, "g1");
        check(&mut IdleCheck::default(), &root);
        assert!(root.turn_end_marker("worker-a").is_file());
        assert_eq!(alerts(&root), 0);
    }
}
