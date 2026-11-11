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
//! A coordinator's watcher also prints `CQ-LOCK-ORPHAN` and `CQ-LOCK-STALE`
//! once per take of a lock that is orphaned or stale (see `lock`), and logs
//! each to `locks.log`. Reported takes are kept in
//! `heartbeat/<name>.lock-alerts` as overdue tasks are. It never breaks a
//! lock.
//!
//! A watcher whose name is listed in `idle-alerts` also checks the turn end
//! that `cq turn-end` records, and sends the idle alert when the session
//! stays idle (see `idle::IdleCheck`). It prints nothing for this. Each
//! line it delivers wakes the session, so a turn end recorded before the
//! last delivery is dropped.
//!
//! With `--to-inbox` (experimental), the lines go into the session's own
//! Claude Code inbox instead of stdout (see `inbox`), and the watcher has no
//! default time limit. `--to-codex` instead wakes a Codex chat with
//! `codex queue` (see `codex`), and `--to-agy` wakes an Antigravity CLI
//! conversation with one `agy` turn per batch (see `agy`).
//!
//! Before its pid file, each run writes `heartbeat/<name>.watcher` with its
//! version and, with `--to-inbox`, the inbox address and a hash of the token
//! it posts with (see `record`).
//!
//! Each run that takes the lock writes one line to `watch-exits.log` when
//! it ends, and one for a predecessor that ended without writing its own
//! (see `exitlog`).
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

use crate::error::{CqError, Result, exit};
use crate::exitlog::{self, Exit};
use crate::fsutil;
use crate::idle;
use crate::lock;
use crate::members;
use crate::message::{EVENT_SUBJECT_MAX, event_text, header};
use crate::names::{check_name, is_valid_id};
use crate::overdue;
use crate::platform;
use crate::record::{self, Record};
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
/// How often a coordinator's watcher looks for orphaned and stale locks.
const LOCK_SCAN_EVERY: Duration = Duration::from_secs(15);
/// How often the watcher checks the turn end recorded by `cq turn-end`.
const IDLE_CHECK_EVERY: Duration = Duration::from_secs(5);
/// The reason a caught signal gives. The signal's name follows it.
const SIGNAL_REASON: &str = "stopped by a signal";

/// How a watcher run ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// `--max-seconds`, stdout closed, or (with `--to-inbox` or
    /// `--to-codex` or `--to-agy`) a signal: the session re-arms.
    Normal,
    /// The name is asleep: exit 7, and the session does not re-arm.
    Asleep,
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub name: String,
    pub takeover: bool,
    /// Stop after this many seconds. `None` runs until another reason
    /// stops the watcher (`--to-inbox`, `--to-codex`, `--to-agy`).
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
    /// Send the idle alert this long after a turn end with no change to the
    /// transcript (`--idle-after`).
    pub idle_after: Duration,
    /// The least time between two idle alerts (`--idle-every`).
    pub idle_every: Duration,
    /// Where `--to-inbox` posts, for the watcher record. `None` records no
    /// target.
    pub target: Option<record::Target>,
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
            idle_after: idle::DEFAULT_IDLE_AFTER,
            idle_every: idle::DEFAULT_EVERY,
            target: None,
        }
    }
}

/// Where a watcher's event lines go: stdout under a Monitor (`Lines`), the
/// session's own Claude Code inbox (`inbox::Inbox`), a Codex chat
/// (`codex::CodexQueue`), or an Antigravity CLI conversation
/// (`agy::AgyTurn`).
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
/// an exit is never silent. With `--to-inbox`, `--to-codex` or `--to-agy`,
/// a signal also stops it.
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
    let lock_stale = if coordinator {
        Some(lock::stale_after_from_env()?)
    } else {
        None
    };
    let started = Instant::now();
    let deadline = opts.max_seconds.map(|s| started + Duration::from_secs(s));

    let (_lock, took_over) = take_lock(root, name, opts.takeover)?;
    let pid = std::process::id();
    let exits = exitlog::Guard::arm(root, name, opts.mode);
    log_predecessor(root, name, pid, took_over);
    // Before the pid file, so a reader that finds this PID finds its record.
    // Without it, session-start asks for a restart, so a failure only costs
    // one.
    if let Err(e) = Record::this_process(opts.target.clone()).write(root, name) {
        eprintln!("cq watch: could not record the watcher for {name}: {e}");
    }
    let pid_file = root.pid_file(name);
    if let Err(e) = fsutil::write_atomic(&root.tmp(), &pid_file, format!("{pid}\n").as_bytes()) {
        let e = CqError::io(format!("write {}", pid_file.display()), e);
        exits.finish(e.code, None, &format!("error: {e}"));
        return Err(e);
    }
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
        read_marker(&root.overdue_marker(name), is_valid_id)
    } else {
        HashSet::new()
    };
    // What `heartbeat/<name>.overdue` holds: only tasks whose line the sink
    // has delivered, so a line lost in a failed post is reported again.
    let mut saved = reported.clone();
    let mut next_overdue_scan = started;
    // The same for orphaned and stale locks, in `<name>.lock-alerts`.
    let mut lock_alerts = if coordinator {
        read_marker(&root.lock_alerts(name), lock::is_alert_key)
    } else {
        HashSet::new()
    };
    let mut lock_alerts_saved = lock_alerts.clone();
    let mut next_lock_scan = started;
    let mut idle_check = idle::IdleCheck::default();
    let mut next_idle_check = started;
    let mut out = Counted {
        inner: out,
        delivered: None,
    };
    let out = &mut out;

    let end = loop {
        let now = Instant::now();
        if let (Some(d), Some(s)) = (deadline, opts.max_seconds)
            && now >= d
        {
            break Ok((format!("--max-seconds {s} reached"), Ended::Normal));
        }
        if platform::interrupted() {
            let reason = match platform::signal_name() {
                Some(sig) => format!("{SIGNAL_REASON} ({sig})"),
                None => SIGNAL_REASON.to_owned(),
            };
            break Ok((reason, Ended::Normal));
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
        if let Some(stale_after) = lock_stale
            && !paused
            && now >= next_lock_scan
        {
            next_lock_scan = now + LOCK_SCAN_EVERY;
            if let Err(e) = lock_scan(root, name, stale_after, &mut lock_alerts, out) {
                break out.failed(e).map(|r| (r, Ended::Normal));
            }
        }
        if let Err(e) = out.tick() {
            break out.failed(e).map(|r| (r, Ended::Normal));
        }
        if let Some(at) = out.delivered.take() {
            idle_check.delivered(at);
        }
        // A held line wakes the session once it is posted: no alert first.
        if !paused && !out.holding() && now >= next_idle_check {
            next_idle_check = now + IDLE_CHECK_EVERY;
            idle_check.run(root, name, opts.idle_after, opts.idle_every);
        }
        if reported != saved && !out.holding() {
            save_marker(root, &root.overdue_marker(name), &reported);
            saved = reported.clone();
        }
        if lock_alerts != lock_alerts_saved && !out.holding() {
            save_marker(root, &root.lock_alerts(name), &lock_alerts);
            lock_alerts_saved = lock_alerts.clone();
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

    // `finish` removes the pid file once the exit line is written, and
    // leaves it to a watcher that took over. A line it could not write
    // leaves the pid file, so the next start logs this exit.
    let (reason, ended) = match end {
        Ok(v) => v,
        Err(e) => {
            exits.finish(e.code, None, &format!("error: {e}"));
            return Err(e);
        }
    };
    let code = match ended {
        Ended::Normal => exit::OK,
        Ended::Asleep => exit::ASLEEP,
    };
    let signal = reason
        .starts_with(SIGNAL_REASON)
        .then(platform::signal_name)
        .flatten();
    // Before `stopped`, which can take seconds of retries.
    exits.finish(code, signal, &reason);
    // The last lines go out only now; save them once they are delivered.
    if out.stopped(&reason, ended) {
        if reported != saved {
            save_marker(root, &root.overdue_marker(name), &reported);
        }
        if lock_alerts != lock_alerts_saved {
            save_marker(root, &root.lock_alerts(name), &lock_alerts);
        }
    }
    // Ignore a failed write: stderr may be the same closed pipe as stdout.
    let _ = writeln!(io::stderr(), "cq watch: exiting: {reason}");
    Ok(ended)
}

/// A sink that records when it last delivered lines, so the idle check
/// can drop a turn end recorded before a line woke the session. A sink
/// that holds lines delivers them when a tick leaves it holding none; one
/// that holds none (stdout) delivers each line at once. A line a sink
/// drops by design counts too, which can only stop an alert. The time is
/// taken before the call: the session may wake and end a new turn before
/// the call returns, and that turn end must not look older than the
/// delivery.
struct Counted<'a, S: Sink> {
    inner: &'a mut S,
    delivered: Option<SystemTime>,
}

impl<S: Sink> Sink for Counted<'_, S> {
    fn line(&mut self, line: &str) -> io::Result<()> {
        let at = SystemTime::now();
        self.inner.line(line)?;
        if !self.inner.holding() {
            self.delivered = Some(at);
        }
        Ok(())
    }
    fn tick(&mut self) -> io::Result<()> {
        let held = self.inner.holding();
        let at = SystemTime::now();
        self.inner.tick()?;
        if held && !self.inner.holding() {
            self.delivered = Some(at);
        }
        Ok(())
    }
    fn holding(&self) -> bool {
        self.inner.holding()
    }
    fn stopped(&mut self, reason: &str, ended: Ended) -> bool {
        self.inner.stopped(reason, ended)
    }
    fn failed(&mut self, e: io::Error) -> Result<String> {
        self.inner.failed(e)
    }
}

/// Deliver the `CQ-SLEEP` line. A failed write is ignored: the watcher
/// exits either way.
fn say_asleep(out: &mut impl Sink, rec: &sleep::Sleep) {
    let _ = out.line(&format!("CQ-SLEEP {}", rec.describe()));
}

/// Take the exclusive lock on `heartbeat/<name>.lock`. The OS releases it
/// when the process exits, however it exits. The flag is true when the lock
/// was held and `takeover` stopped its holder.
pub(crate) fn take_lock(root: &Root, name: &str, takeover: bool) -> Result<(File, bool)> {
    let path = root.lock_file(name);
    let file = fsutil::open_lock_file(&path)
        .map_err(|e| CqError::io(format!("open {}", path.display()), e))?;
    match file.try_lock() {
        Ok(()) => return Ok((file, false)),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(e)) => {
            return Err(CqError::io(format!("lock {}", path.display()), e));
        }
    }

    let old = read_pid(root, name);
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
            Ok(()) => return Ok((file, true)),
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
/// exists, its heartbeat is less than `ALIVE_SECS` old (the rule cq-top
/// uses), and the process it names has not gone. It never opens the lock
/// file, so it cannot stop a watcher that is starting. A clean exit removes
/// the pid file, and a killed watcher's process is gone, so either reads as
/// stopped at once. Where the process cannot be checked, only the heartbeat
/// counts.
pub fn is_running(root: &Root, name: &str) -> bool {
    Liveness::read(root, name).running()
}

/// What the files of a name's watcher or dispatcher say, read only: the
/// facts behind `is_running`, for `cq status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Liveness {
    /// `heartbeat/<name>.pid` exists.
    pub pid_file: bool,
    /// The process ID in it, when it holds one.
    pub pid: Option<u32>,
    /// The time of the last heartbeat, if any.
    pub beat: Option<jiff::Timestamp>,
    /// Whether the process in the pid file runs: `None` when it cannot be
    /// checked, or there is no ID.
    pub alive: Option<bool>,
    /// `heartbeat/<name>.watcher`, when it names the same process.
    pub record: Option<Record>,
}

impl Liveness {
    /// For a reader that is not the watcher itself (`cq status`, `cq
    /// session-start`): a pid file that names this process belongs to a
    /// dead watcher whose ID was reused, which Windows does at once.
    pub fn read(root: &Root, name: &str) -> Self {
        Self::read_by(root, name, None)
    }

    /// As `read`, for a reader that may be the watcher `watcher` (a
    /// coordinator's lock scan): `name`'s own pid file naming this process
    /// is this watcher, so it runs. Any other name's pid file that names
    /// this process still counts as a dead watcher's.
    pub fn read_by(root: &Root, name: &str, watcher: Option<&str>) -> Self {
        let pid = read_pid(root, name);
        let me = std::process::id();
        let is_me = watcher == Some(name);
        Self {
            pid_file: root.pid_file(name).is_file(),
            pid,
            beat: last_beat(root, name),
            alive: pid.and_then(|p| {
                if p == me {
                    Some(is_me)
                } else {
                    platform::process_alive(p)
                }
            }),
            record: pid.and_then(|p| Record::read_for(root, name, p)),
        }
    }

    /// The version the watcher recorded; `None` for unknown.
    pub fn version(&self) -> Option<&str> {
        self.record.as_ref().and_then(|r| r.version.as_deref())
    }

    /// Seconds since the last heartbeat; 0 for one in the future.
    pub fn beat_age(&self) -> Option<i64> {
        self.beat
            .map(|t| (timefmt::now().as_second() - t.as_second()).max(0))
    }

    pub fn running(&self) -> bool {
        self.why_stopped().is_none()
    }

    /// Why it does not count as running, or `None` when it does.
    pub fn why_stopped(&self) -> Option<String> {
        match (self.pid_file, self.beat_age()) {
            (false, _) => Some("no pid file".to_owned()),
            (true, None) => Some("no heartbeat".to_owned()),
            (true, Some(age)) if age >= ALIVE_SECS => Some(format!("heartbeat {age}s old")),
            (true, Some(_)) => match (self.pid, self.alive) {
                (Some(pid), Some(false)) => Some(format!("process {pid} is not running")),
                _ => None,
            },
        }
    }
}

/// The process ID in `heartbeat/<name>.pid`, if it holds one.
fn read_pid(root: &Root, name: &str) -> Option<u32> {
    fs::read_to_string(root.pid_file(name))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// The time of the last heartbeat. `heartbeat` rewrites the file in place,
/// so a read can catch it half written; the modified time is then the beat.
fn last_beat(root: &Root, name: &str) -> Option<jiff::Timestamp> {
    let path = root.heartbeat(name);
    fs::read_to_string(&path)
        .ok()
        .and_then(|t| timefmt::parse_rfc3339_z(t.trim()))
        .or_else(|| fsutil::modified(&path).and_then(timefmt::from_system_time))
}

/// Called once the lock is held, before the pid file is written. Every exit
/// that writes its line removes the pid file (see `exitlog`), so one that
/// still names another process belongs to a holder that never wrote its
/// exit line: write the line for it. Holding the lock proves that the holder
/// is gone.
fn log_predecessor(root: &Root, name: &str, pid: u32, took_over: bool) {
    let Some(old) = read_pid(root, name).filter(|p| *p != pid) else {
        return;
    };
    let reason = if took_over {
        format!("stopped by --takeover from PID {pid}")
    } else {
        let beat = last_beat(root, name).map_or_else(|| "none".to_owned(), rfc3339);
        format!(
            "previous watcher ended without logging (last heartbeat {beat}); found by PID {pid}"
        )
    };
    // Read before this watcher writes its own record.
    let version = Record::read_for(root, name, old).and_then(|r| r.version);
    exitlog::write(
        root,
        &Exit {
            name,
            pid: old,
            // Read before this watcher records its own session.
            mode: members::mode_of_name(root, name),
            version: version.as_deref(),
            code: None,
            signal: None,
            reason: &reason,
        },
    );
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

/// For a coordinator: one `CQ-LOCK-ORPHAN` or `CQ-LOCK-STALE` line per
/// take of a lock not reported before in that state, each also logged to
/// `locks.log`. Takes no longer held are forgotten. The loop saves
/// `reported` once the sink has delivered the lines. Reads only; the lock
/// stays held.
fn lock_scan(
    root: &Root,
    name: &str,
    stale_after: Duration,
    reported: &mut HashSet<String>,
    out: &mut impl Sink,
) -> io::Result<()> {
    for (key, held) in lock::alerts(root, name, stale_after, reported) {
        let Some(line) = held.event_line(timefmt::now()) else {
            continue;
        };
        out.line(&line)?;
        lock::log_alert(root, &held, name);
        reported.insert(key);
    }
    Ok(())
}

/// What a coordinator's watcher has reported (overdue task IDs, or lock
/// alert keys): the lines of `path` that pass `valid`. A missing or
/// unreadable file gives none.
fn read_marker(path: &Path, valid: impl Fn(&str) -> bool) -> HashSet<String> {
    fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| valid(l))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Save what was reported to `path`, sorted. A failed write only risks one
/// repeated line after a restart, so it is reported on stderr and the
/// watcher runs on.
fn save_marker(root: &Root, path: &Path, reported: &HashSet<String>) {
    if reported.is_empty() {
        if let Err(e) = fs::remove_file(path)
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("cq watch: could not remove {}: {e}", path.display());
        }
        return;
    }
    let mut ids: Vec<&String> = reported.iter().collect();
    ids.sort();
    let text: String = ids.iter().map(|id| format!("{id}\n")).collect();
    if let Err(e) = fsutil::write_atomic(&root.tmp(), path, text.as_bytes()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idle::{IdleCheck, Marked, StopInput, mark_turn_end};

    /// A root where `worker-a` is listed, and Stop hook input with its
    /// transcript.
    fn listed() -> (tempfile::TempDir, Root, StopInput) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path().to_owned());
        root.init().unwrap();
        fs::write(root.idle_alerts(), "worker-a lead\n").unwrap();
        let tx = dir.path().join("transcript.jsonl");
        fs::write(&tx, "{\"type\":\"user\"}\n").unwrap();
        let input = StopInput {
            transcript_path: Some(tx),
            background_tasks: None,
        };
        (dir, root, input)
    }

    /// A sink that delivers in `line`, or holds the line and delivers it in
    /// `tick`. With `turn_ends`, the session wakes and ends a new turn while
    /// the line is delivered, before the call returns.
    struct Fake<'a> {
        root: &'a Root,
        input: &'a StopInput,
        hold: bool,
        held: bool,
        turn_ends: bool,
    }

    impl Fake<'_> {
        fn deliver(&mut self) {
            if self.turn_ends {
                sleep(Duration::from_millis(10));
                let marked = mark_turn_end(self.root, "worker-a", self.input);
                assert!(matches!(marked, Marked::Written), "{marked:?}");
                sleep(Duration::from_millis(10));
            }
        }
    }

    impl Sink for Fake<'_> {
        fn line(&mut self, _line: &str) -> io::Result<()> {
            if self.hold {
                self.held = true;
            } else {
                self.deliver();
            }
            Ok(())
        }
        fn tick(&mut self) -> io::Result<()> {
            if self.held {
                self.deliver();
                self.held = false;
            }
            Ok(())
        }
        fn holding(&self) -> bool {
            self.held
        }
        fn failed(&mut self, e: io::Error) -> Result<String> {
            Err(CqError::io("fake sink", e))
        }
    }

    /// Deliver one line through `Counted`, then run one idle check with
    /// the delivery time.
    fn deliver_and_check(root: &Root, input: &StopInput, hold: bool, turn_ends: bool) {
        let mut sink = Fake {
            root,
            input,
            hold,
            held: false,
            turn_ends,
        };
        let mut out = Counted {
            inner: &mut sink,
            delivered: None,
        };
        out.line("CQ-NEW x").unwrap();
        out.tick().unwrap();
        let at = out.delivered.expect("a delivery time");
        let mut check = IdleCheck::default();
        check.delivered(at);
        check.run(
            root,
            "worker-a",
            idle::DEFAULT_IDLE_AFTER,
            idle::DEFAULT_EVERY,
        );
    }

    #[test]
    fn a_turn_end_during_a_delivery_survives_it() {
        for hold in [false, true] {
            let (_dir, root, input) = listed();
            deliver_and_check(&root, &input, hold, true);
            assert!(root.turn_end_marker("worker-a").exists(), "hold={hold}");
        }
    }

    #[test]
    fn a_turn_end_before_a_delivery_is_cancelled() {
        for hold in [false, true] {
            let (_dir, root, input) = listed();
            assert!(matches!(
                mark_turn_end(&root, "worker-a", &input),
                Marked::Written
            ));
            sleep(Duration::from_millis(10));
            deliver_and_check(&root, &input, hold, false);
            assert!(!root.turn_end_marker("worker-a").exists(), "hold={hold}");
        }
    }

    /// A watcher that wrote its own line from a console event or a panic
    /// leaves no pid file, so the next start does not log it again as one
    /// that ended without logging.
    #[test]
    fn a_logged_console_or_panic_exit_is_not_logged_again_on_restart() {
        let _serial = exitlog::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path().to_owned());
        root.init().unwrap();
        root.ensure_session_dirs("worker-a").unwrap();
        let pid = std::process::id();
        let start = |root: &Root| {
            fs::write(root.pid_file("worker-a"), format!("{pid}\n")).unwrap();
            exitlog::Guard::arm(root, "worker-a", members::Mode::Inbox)
        };
        let restart = |root: &Root| log_predecessor(root, "worker-a", pid + 1, false);

        let guard = start(&root);
        exitlog::signalled("CTRL_CLOSE_EVENT");
        assert!(!root.pid_file("worker-a").exists());
        drop(guard);
        restart(&root);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = start(&root);
            panic!("test panic");
        }));
        assert!(panicked.is_err());
        assert!(!root.pid_file("worker-a").exists());
        restart(&root);

        let log = fs::read_to_string(root.watch_exits()).unwrap();
        let reasons: Vec<&str> = log
            .lines()
            .map(|l| l.split_once(" reason=").unwrap().1)
            .collect();
        assert_eq!(
            reasons,
            [r#""console event CTRL_CLOSE_EVENT""#, r#""panicked""#],
            "{log}"
        );
    }
}
