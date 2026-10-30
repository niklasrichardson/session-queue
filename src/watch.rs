//! `cq watch`: print one `CQ-NEW` line per new message in an inbox.
//!
//! Runs under a Claude Code Monitor, which wakes the session on each line.
//! One watcher per name, held by an OS file lock. The watcher exits on its
//! own before the Monitor's 30-minute cap, so an expired Monitor does not
//! leave a long-lived orphan behind; `--takeover` replaces one sooner.
//! With a session key, the watcher also records which Claude Code session
//! owns the name (see `members`).
//!
//! While `sleep/<name>` exists, the watcher prints `CQ-SLEEP` and exits 7
//! (see `sleep`). As a backup to the sender's own wake, a coordinator's
//! watcher also prints one `CQ-WAKE` line per message that has waited
//! `WAKE_AFTER` for a sleeping name, and never claims or moves it.
//!
//! A coordinator's watcher also prints one `CQ-OVERDUE` line per task that
//! has become overdue in any inbox (see `overdue`). A task is written to
//! `heartbeat/<name>.overdue` once its line has been delivered, so a
//! restarted watcher does not report it again; a line lost in a failed post
//! is reported again. It never moves the tasks.
//!
//! With `--to-inbox` (experimental), the lines go into the session's own
//! Claude Code inbox instead of stdout (see `inbox`), and the watcher has no
//! default time limit. `--to-codex` instead wakes a Codex chat with
//! `codex queue` (see `codex`).
//!
//! There is no parent-process check. Under Git Bash the parent of `cq.exe`
//! is a forked bash whose lifetime Cygwin controls, not the Monitor, and the
//! check was the only path that could end a watcher with exit 0 and no
//! output while its Monitor still ran.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, TryLockError};
use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::{CqError, Result};
use crate::fsutil;
use crate::members;
use crate::message::{EVENT_SUBJECT_MAX, event_text, header};
use crate::names::{check_name, is_valid_id};
use crate::overdue;
use crate::platform;
use crate::root::{Root, message_file};
use crate::sleep;
use crate::timefmt::{self, local_hhmm, rfc3339};

pub const DEFAULT_MAX_SECONDS: u64 = 1790;
pub const DEFAULT_RESCAN_SECONDS: u64 = 60;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);
/// A heartbeat this many seconds old or older means the watcher is gone:
/// three missed beats.
pub const ALIVE_SECS: i64 = 90;
const PRUNE_EVERY: Duration = Duration::from_secs(3600);
const PRUNE_AFTER: Duration = Duration::from_secs(14 * 24 * 3600);
/// Rescan interval when file-system events are not available.
const FALLBACK_RESCAN: Duration = Duration::from_secs(5);
/// How often the loop checks the clock and `PAUSE`.
const TICK: Duration = Duration::from_millis(250);
/// How long `--takeover` waits for the old watcher to release the lock.
const TAKEOVER_WAIT: Duration = Duration::from_secs(5);
/// How often a coordinator's watcher scans the inboxes of sleeping names.
const WAKE_SCAN_EVERY: Duration = Duration::from_secs(1);
/// A coordinator's watcher reports a message for a sleeping name once it
/// has been in the inbox this long (by the file's modified time), which
/// gives the sender time to wake the name itself.
pub const WAKE_AFTER: Duration = Duration::from_secs(120);
/// How often a coordinator's watcher looks for newly overdue tasks.
const OVERDUE_SCAN_EVERY: Duration = Duration::from_secs(30);

/// How a watcher run ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// `--max-seconds`, stdout closed, or (with `--to-inbox` or
    /// `--to-codex`) a signal: the session re-arms.
    Normal,
    /// The name is asleep: exit 7, and the session does not re-arm.
    Asleep,
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub name: String,
    pub takeover: bool,
    /// Stop after this many seconds. `None` runs until another reason
    /// stops the watcher (`--to-inbox`, `--to-codex`).
    pub max_seconds: Option<u64>,
    /// Full rescan interval. 60 seconds; shorter in tests.
    pub rescan: Duration,
    /// File-system events through `notify`. Off in tests that check the
    /// rescan on its own.
    pub fs_events: bool,
    /// The Claude Code session to record as the owner of the name. `None`
    /// records nothing.
    pub member_key: Option<String>,
    /// Where the lines go, recorded in the session's `members/` record, so
    /// that `cq session-start` does not ask for a Monitor after `/clear`.
    pub mode: members::Mode,
}

impl WatchOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            takeover: false,
            max_seconds: Some(DEFAULT_MAX_SECONDS),
            rescan: Duration::from_secs(DEFAULT_RESCAN_SECONDS),
            fs_events: true,
            member_key: None,
            mode: members::Mode::Monitor,
        }
    }
}

/// Where a watcher's event lines go: stdout under a Monitor (`Lines`), the
/// session's own Claude Code inbox (`inbox::Inbox`), or a Codex chat
/// (`codex::CodexQueue`).
pub trait Sink {
    /// Deliver one event line (without its newline), or hold it back.
    fn line(&mut self, line: &str) -> io::Result<()>;
    /// Called on every tick of the loop, to deliver lines held back.
    fn tick(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// True while a line given to `line` waits to be delivered. A line the
    /// sink drops by design is not held.
    fn holding(&self) -> bool {
        false
    }
    /// The watcher is stopping without an error: deliver what is held back
    /// and, where someone can act on it, say why. True when every line given
    /// to `line` has been delivered, or dropped by design.
    fn stopped(&mut self, _reason: &str, _ended: Ended) -> bool {
        true
    }
    /// A failed `line` or `tick`: `Ok` with the reason for a normal stop,
    /// or the error the watcher exits with.
    fn failed(&mut self, e: io::Error) -> Result<String>;
}

/// stdout under a Monitor: one line per event, flushed at once.
pub struct Lines<W: Write>(pub W);

impl<W: Write> Sink for Lines<W> {
    fn line(&mut self, line: &str) -> io::Result<()> {
        writeln!(self.0, "{line}")?;
        self.0.flush()
    }

    fn failed(&mut self, e: io::Error) -> Result<String> {
        // Nobody reads the output any more.
        Ok(format!("stdout can no longer be written ({e})"))
    }
}

/// Run the watcher until `--max-seconds`, until `out` fails, or until the
/// name is asleep. Each of these prints one line to stderr saying why, so
/// an exit is never silent. With `--to-inbox` or `--to-codex`, a signal
/// also stops it.
pub fn watch(root: &Root, opts: &WatchOptions, out: &mut impl Sink) -> Result<Ended> {
    let name = opts.name.as_str();
    check_name("--as", name)?;
    // Asleep at start: take no lock and record nothing.
    if let Some(rec) = sleep::read(root, name) {
        say_asleep(out, &rec);
        out.stopped("asleep", Ended::Asleep);
        let _ = writeln!(io::stderr(), "cq watch: exiting: asleep");
        return Ok(Ended::Asleep);
    }
    root.ensure_session_dirs(name)?;
    // Read before the lock, so a bad setting exits 64 with nothing to undo.
    let coordinator = sleep::is_coordinator(root, name);
    let overdue_limits = if coordinator {
        Some(overdue::Limits::from_env()?)
    } else {
        None
    };
    let started = Instant::now();
    let deadline = opts.max_seconds.map(|s| started + Duration::from_secs(s));

    let _lock = take_lock(root, name, opts.takeover)?;
    let pid = std::process::id();
    let pid_file = root.pid_file(name);
    fsutil::write_atomic(&root.tmp(), &pid_file, format!("{pid}\n").as_bytes())
        .map_err(|e| CqError::io(format!("write {}", pid_file.display()), e))?;
    if let Some(key) = &opts.member_key {
        // The queue still works without the record; only a rejoin after
        // `/clear` needs it.
        if let Err(e) = members::record(root, key, name, opts.mode) {
            eprintln!("cq watch: could not record the session for {name}: {e}");
        }
    }

    let inbox = root.inbox(name);
    let (_fs_watcher, mut events, mut rescan) = if opts.fs_events {
        match start_fs_events(&inbox) {
            Ok((w, rx)) => (Some(w), Some(rx), opts.rescan),
            Err(e) => {
                eprintln!(
                    "cq watch: file-system events unavailable ({e}); rescanning every {}s",
                    FALLBACK_RESCAN.as_secs()
                );
                (None, None, opts.rescan.min(FALLBACK_RESCAN))
            }
        }
    } else {
        (None, None, opts.rescan)
    };

    let mut announced: HashMap<String, Option<SystemTime>> = HashMap::new();
    let mut pending = true; // the start scan
    let mut was_paused = false;
    let mut next_rescan = started + rescan;
    let mut next_heartbeat = started;
    let mut next_prune = started;
    let mut woken: HashSet<String> = HashSet::new();
    let mut next_wake_scan = started;
    let mut reported = if coordinator {
        read_reported(root, name)
    } else {
        HashSet::new()
    };
    // What `heartbeat/<name>.overdue` holds: only tasks whose line the sink
    // has delivered, so a line lost in a failed post is reported again.
    let mut saved = reported.clone();
    let mut next_overdue_scan = started;

    let end = loop {
        let now = Instant::now();
        if let (Some(d), Some(s)) = (deadline, opts.max_seconds)
            && now >= d
        {
            break Ok((format!("--max-seconds {s} reached"), Ended::Normal));
        }
        if platform::interrupted() {
            break Ok(("stopped by a signal".to_owned(), Ended::Normal));
        }
        if let Some(rec) = sleep::read(root, name) {
            say_asleep(out, &rec);
            break Ok(("asleep".to_owned(), Ended::Asleep));
        }
        if now >= next_heartbeat {
            heartbeat(root, name);
            next_heartbeat = now + HEARTBEAT_EVERY;
        }
        if now >= next_prune {
            prune(&root.done(name));
            prune(&root.rejected(name));
            next_prune = now + PRUNE_EVERY;
        }
        if now >= next_rescan {
            pending = true;
            next_rescan = now + rescan;
        }

        let paused = root.pause().exists();
        if was_paused && !paused {
            pending = true;
        }
        was_paused = paused;
        if pending && !paused {
            pending = false;
            if let Err(e) = scan(root, name, &mut announced, out) {
                break out.failed(e).map(|r| (r, Ended::Normal));
            }
        }
        if coordinator && !paused && now >= next_wake_scan {
            next_wake_scan = now + WAKE_SCAN_EVERY;
            if let Err(e) = wake_scan(root, name, &mut woken, out) {
                break out.failed(e).map(|r| (r, Ended::Normal));
            }
        }
        if let Some(limits) = &overdue_limits
            && !paused
            && now >= next_overdue_scan
        {
            next_overdue_scan = now + OVERDUE_SCAN_EVERY;
            if let Err(e) = overdue_scan(root, limits, &mut reported, out) {
                break out.failed(e).map(|r| (r, Ended::Normal));
            }
        }
        if let Err(e) = out.tick() {
            break out.failed(e).map(|r| (r, Ended::Normal));
        }
        if reported != saved && !out.holding() {
            save_reported(root, name, &reported);
            saved = reported.clone();
        }

        let wait = deadline.map_or(TICK, |d| {
            TICK.min(d.saturating_duration_since(Instant::now()))
        });
        let stopped = match &events {
            Some(rx) => match rx.recv_timeout(wait) {
                Ok(()) => {
                    pending = true;
                    while rx.try_recv().is_ok() {}
                    false
                }
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => true,
            },
            None => {
                sleep(wait);
                false
            }
        };
        if stopped {
            eprintln!(
                "cq watch: file-system events stopped; rescanning every {}s",
                FALLBACK_RESCAN.as_secs()
            );
            events = None;
            rescan = rescan.min(FALLBACK_RESCAN);
            next_rescan = next_rescan.min(Instant::now() + rescan);
        }
    };

    // Leave the pid file to a watcher that took over.
    if fs::read_to_string(&pid_file).is_ok_and(|s| s.trim() == pid.to_string()) {
        let _ = fs::remove_file(&pid_file);
    }
    let (reason, ended) = end?;
    // The last lines go out only now; save them once they are delivered.
    if out.stopped(&reason, ended) && reported != saved {
        save_reported(root, name, &reported);
    }
    // Ignore a failed write: stderr may be the same closed pipe as stdout.
    let _ = writeln!(io::stderr(), "cq watch: exiting: {reason}");
    Ok(ended)
}

/// Deliver the `CQ-SLEEP` line. A failed write is ignored: the watcher
/// exits either way.
fn say_asleep(out: &mut impl Sink, rec: &sleep::Sleep) {
    let _ = out.line(&format!("CQ-SLEEP {}", rec.describe()));
}

/// Take the exclusive lock on `heartbeat/<name>.lock`. The OS releases it
/// when the process exits, however it exits.
pub(crate) fn take_lock(root: &Root, name: &str, takeover: bool) -> Result<File> {
    let path = root.lock_file(name);
    let file = fsutil::open_lock_file(&path)
        .map_err(|e| CqError::io(format!("open {}", path.display()), e))?;
    match file.try_lock() {
        Ok(()) => return Ok(file),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(e)) => {
            return Err(CqError::io(format!("lock {}", path.display()), e));
        }
    }

    let old = fs::read_to_string(root.pid_file(name))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let old_text = old.map_or_else(|| "unknown process".to_owned(), |p| format!("process {p}"));
    if !takeover {
        return Err(CqError::refused(format!(
            "a watcher or dispatcher for {name} is already running ({old_text}); use --takeover to replace it"
        )));
    }
    if let Some(p) = old.filter(|p| *p != std::process::id()) {
        platform::terminate(p)?;
    }
    let until = Instant::now() + TAKEOVER_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < until => {
                sleep(Duration::from_millis(100));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(CqError::refused(format!(
                    "--takeover: the old watcher for {name} ({old_text}) still holds the lock after {}s",
                    TAKEOVER_WAIT.as_secs()
                )));
            }
            Err(TryLockError::Error(e)) => {
                return Err(CqError::io(format!("lock {}", path.display()), e));
            }
        }
    }
}

/// True while a watcher or dispatcher for `name` looks alive: its pid file
/// exists and its heartbeat is less than `ALIVE_SECS` old, the rule cq-top
/// uses. It never opens the lock file, so it cannot stop a watcher that is
/// starting. A clean exit removes the pid file, so it reads as stopped at
/// once; a killed watcher still looks alive until its heartbeat is too old.
pub fn is_running(root: &Root, name: &str) -> bool {
    if !root.pid_file(name).is_file() {
        return false;
    }
    let path = root.heartbeat(name);
    // `heartbeat` rewrites the file in place, so a read can catch it half
    // written; the modified time is then the beat.
    let beat = fs::read_to_string(&path)
        .ok()
        .and_then(|t| timefmt::parse_rfc3339_z(t.trim()))
        .or_else(|| fsutil::modified(&path).and_then(timefmt::from_system_time));
    beat.is_some_and(|t| timefmt::now().as_second() - t.as_second() < ALIVE_SECS)
}

/// Any change in the inbox triggers a scan; the scan decides what is new.
fn start_fs_events(inbox: &Path) -> notify::Result<(RecommendedWatcher, Receiver<()>)> {
    let (tx, rx) = mpsc::channel();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })?;
    w.watch(inbox, RecursiveMode::NonRecursive)?;
    Ok((w, rx))
}

/// Announce each message in the inbox that has not been announced yet,
/// oldest first. Forgets messages that have left the inbox. A message is
/// known by its ID and modified time, so one that left and came back
/// between two scans (`cq requeue` sets a new time) is announced again.
fn scan(
    root: &Root,
    name: &str,
    announced: &mut HashMap<String, Option<SystemTime>>,
    out: &mut impl Sink,
) -> io::Result<()> {
    let inbox = root.inbox(name);
    let present = fsutil::json_stems(&inbox, is_valid_id);
    announced.retain(|id, _| present.binary_search(id).is_ok());
    for id in present {
        let path = message_file(&inbox, &id);
        let modified = fsutil::modified(&path);
        if announced.get(&id) == Some(&modified) {
            continue;
        }
        let bytes = match fsutil::read_with_retry(&path) {
            Ok(b) => b,
            // Claimed since the directory was listed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            // Announce it anyway; `cq claim` decides what happens to it.
            Err(_) => Vec::new(),
        };
        out.line(&event_line("CQ-NEW", name, &id, &bytes))?;
        announced.insert(id, modified);
    }
    Ok(())
}

/// For a coordinator: one `CQ-WAKE` line per message that has waited at
/// least `WAKE_AFTER` in the inbox of a name that is still asleep, at most
/// once per message per watcher run. Reads only; the message stays where it
/// is.
fn wake_scan(
    root: &Root,
    name: &str,
    woken: &mut HashSet<String>,
    out: &mut impl Sink,
) -> io::Result<()> {
    for to in sleep::asleep(root) {
        if to == name {
            continue;
        }
        let inbox = root.inbox(&to);
        for id in fsutil::json_stems(&inbox, is_valid_id) {
            if woken.contains(&id) {
                continue;
            }
            let path = message_file(&inbox, &id);
            let waited = fsutil::modified(&path)
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .is_some_and(|age| age >= WAKE_AFTER);
            if !waited {
                continue;
            }
            let bytes = match fsutil::read_with_retry(&path) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => Vec::new(),
            };
            out.line(&event_line("CQ-WAKE", &to, &id, &bytes))?;
            woken.insert(id);
        }
    }
    Ok(())
}

/// For a coordinator: one `CQ-OVERDUE` line per overdue task not reported
/// before, in any inbox. Tasks that have left every inbox are forgotten.
/// The loop saves `reported` once the sink has delivered the lines, so a
/// restarted watcher does not repeat them. Reads only; the task stays where
/// it is.
fn overdue_scan(
    root: &Root,
    limits: &overdue::Limits,
    reported: &mut HashSet<String>,
    out: &mut impl Sink,
) -> io::Result<()> {
    let now = SystemTime::now();
    for task in overdue::scan(root, limits, now) {
        if reported.contains(&task.id) {
            continue;
        }
        out.line(&task.event_line(timefmt::now()))?;
        reported.insert(task.id);
    }
    let in_flight = overdue::in_flight_ids(root);
    reported.retain(|id| in_flight.contains(id));
    Ok(())
}

/// The tasks this coordinator has reported overdue. A missing or unreadable
/// file gives none.
fn read_reported(root: &Root, name: &str) -> HashSet<String> {
    fs::read_to_string(root.overdue_marker(name))
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| is_valid_id(l))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Save the reported tasks, sorted. A failed write only risks one repeated
/// line after a restart, so it is reported on stderr and the watcher runs on.
fn save_reported(root: &Root, name: &str, reported: &HashSet<String>) {
    let path = root.overdue_marker(name);
    if reported.is_empty() {
        if let Err(e) = fs::remove_file(&path)
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("cq watch: could not remove {}: {e}", path.display());
        }
        return;
    }
    let mut ids: Vec<&String> = reported.iter().collect();
    ids.sort();
    let text: String = ids.iter().map(|id| format!("{id}\n")).collect();
    if let Err(e) = fsutil::write_atomic(&root.tmp(), &path, text.as_bytes()) {
        eprintln!("cq watch: could not write {}: {e}", path.display());
    }
}

/// `<kind> <UTC> (<local>) to=... from=... type=... id=... subject="..."`,
/// timed now.
pub(crate) fn event_line(kind: &str, to: &str, id: &str, bytes: &[u8]) -> String {
    let h = header(bytes);
    let now = timefmt::now();
    format!(
        "{kind} {} ({}) to={to} from={} type={} id={id} subject={}",
        rfc3339(now),
        local_hhmm(now),
        h.from,
        h.kind,
        event_text(&h.subject, EVENT_SUBJECT_MAX)
    )
}

pub(crate) fn heartbeat(root: &Root, name: &str) {
    let path = root.heartbeat(name);
    let text = format!("{}\n", rfc3339(timefmt::now()));
    if let Err(e) = fs::write(&path, text) {
        eprintln!("cq watch: could not write {}: {e}", path.display());
    }
}

/// Delete files older than 14 days. Used for `done/` and `rejected/`, never
/// for `dlq/`.
fn prune(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in rd.filter_map(|e| e.ok()) {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > PRUNE_AFTER);
        if meta.is_file() && old {
            let _ = fs::remove_file(entry.path());
        }
    }
}
