//! `cq lock`: named locks that sessions read instead of messaging each other
//! (issue #99), for example one lock for the desktop that GUI tests drive.
//!
//! A lock is held while `locks/<lock>.lock` exists. `take` creates the file
//! with an exclusive create, so of many takes at once exactly one wins. The
//! file holds who took it, when and why (`Hold`). Only the holder releases
//! it; a coordinator may break any lock, with a reason. The reserved holder
//! `owner` is the user's hold: only a coordinator takes or releases it, with
//! `--owner`, and while it is held nobody else can take the lock.
//!
//! Release and break read the holder and remove the file while they hold
//! the OS lock on `locks/<lock>.guard`, so neither removes a lock that was
//! broken and taken again in between. Take creates, writes and, when the
//! write fails, removes its file under the same guard, so it never removes
//! a replacement hold; the exclusive create still decides which take wins.
//! A reader can find a file that its taker is still writing, so a hold that
//! does not parse is read again for a short while before it counts as
//! unreadable.
//!
//! A held lock is **orphaned** when its holder's watcher is stopped and the
//! last sign of life (the heartbeat, or the take itself) is at least
//! `ALIVE_SECS` old, and **stale** when its holder's watcher runs and it has
//! been held for at least the stale limit (`DEFAULT_STALE_MINUTES`, or
//! `CQ_LOCK_STALE_MINUTES`). A file that cannot be read as a hold is orphaned
//! once it is `ALIVE_SECS` old. The owner hold is never orphaned or stale. A
//! coordinator's watcher prints `CQ-LOCK-ORPHAN` and `CQ-LOCK-STALE` once per
//! take (see `watch`). `cq` never breaks a lock by itself.
//!
//! `locks.log` in the root gets one line per take, release and break, and
//! per orphan or stale report, appended as `watch-exits.log` is (see
//! `exitlog::append`). A line that cannot be written is reported on stderr;
//! the command's result does not change.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{CqError, Result, exit};
use crate::exitlog;
use crate::fsutil;
use crate::members;
use crate::message::{EVENT_SUBJECT_MAX, event_text};
use crate::names::{check_name, is_valid_id, is_valid_name};
use crate::overdue;
use crate::platform;
use crate::root::Root;
use crate::sleep as coordinators;
use crate::timefmt::{self, local_hhmm, rfc3339};
use crate::watch::{ALIVE_SECS, Liveness};

/// The holder name of the user's hold.
pub const OWNER: &str = "owner";
pub const DEFAULT_STALE_MINUTES: u64 = 120;
/// Minutes a live holder may keep a lock before it is stale.
pub const ENV_STALE: &str = "CQ_LOCK_STALE_MINUTES";
/// How often `cq lock wait` reads the lock file.
pub const DEFAULT_POLL: Duration = Duration::from_secs(15);
/// The longest `--for` text, in characters.
pub const FOR_MAX: usize = 200;
/// How long a hold that does not parse is read again before it counts as
/// unreadable: its taker may still be writing it.
const SETTLE_TRIES: u32 = 10;
const SETTLE_WAIT: Duration = Duration::from_millis(50);

/// The contents of `locks/<lock>.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub lock: String,
    /// This take, in the message ID format: `<UTC stamp>-<by>-<4 hex>`.
    pub id: String,
    /// The holding name, or `owner` for the user's hold.
    pub holder: String,
    /// The name that ran `cq lock take`: the holder, or for the user's hold
    /// the coordinator.
    pub by: String,
    /// When, in UTC (RFC 3339).
    pub taken: String,
    /// The same time in the local time zone, for a person reading the file.
    pub taken_local: String,
    #[serde(rename = "for")]
    pub purpose: String,
    /// The taker's session key, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
}

impl Hold {
    fn is_valid(&self) -> bool {
        is_valid_name(&self.lock)
            && (is_valid_name(&self.holder) || self.holder == OWNER)
            && is_valid_name(&self.by)
            && is_valid_id(&self.id)
    }
}

/// What a lock file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holding {
    Free,
    Held(Hold),
    /// The file exists but is not a hold (empty, damaged or made by hand).
    Unreadable,
}

/// How a held lock looks to the coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Live,
    Orphaned,
    Stale,
}

impl Health {
    /// The word for `cq lock status` and `list`, and the alert's kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Live => "live",
            Health::Orphaned => "orphaned",
            Health::Stale => "stale",
        }
    }
}

/// One held lock, as `status`, `list` and a coordinator's watcher show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    pub name: String,
    /// `None` when the file is unreadable.
    pub hold: Option<Hold>,
    /// When it was taken; the file's modified time when that is unknown.
    pub since: Timestamp,
    pub age: Duration,
    pub health: Health,
    /// Why the holder's watcher counts as stopped, for an orphaned lock.
    pub why: Option<String>,
    /// For an unreadable file: which file it is (`file_generation`), so a
    /// file made again under the same name is a new take.
    pub file: Option<String>,
}

impl Lock {
    pub fn holder(&self) -> &str {
        self.hold.as_ref().map_or("?", |h| h.holder.as_str())
    }

    /// The fields after the first word of a line, safe for one line.
    pub fn fields(&self) -> String {
        let mut s = format!(
            "lock={} holder={} since={} ({}) age={}m",
            self.name,
            self.holder(),
            rfc3339(self.since),
            local_hhmm(self.since),
            self.age.as_secs() / 60
        );
        if let Some(h) = &self.hold {
            if h.by != h.holder {
                s.push_str(&format!(" by={}", h.by));
            }
            s.push_str(&format!(" for={}", event_text(&h.purpose, FOR_MAX)));
        } else {
            s.push_str(" unreadable");
        }
        if let Some(why) = &self.why {
            s.push_str(&format!(" why={}", event_text(why, EVENT_SUBJECT_MAX)));
        }
        if self.health != Health::Live {
            s.push(' ');
            s.push_str(self.health.as_str());
        }
        s
    }

    /// A `cq lock status` or `list` line.
    pub fn line(&self) -> String {
        format!("held {}", self.fields())
    }

    /// A coordinator's `CQ-LOCK-ORPHAN` or `CQ-LOCK-STALE` line, timed
    /// `now`. `None` for a live lock.
    pub fn event_line(&self, now: Timestamp) -> Option<String> {
        let kind = match self.health {
            Health::Live => return None,
            Health::Orphaned => "CQ-LOCK-ORPHAN",
            Health::Stale => "CQ-LOCK-STALE",
        };
        Some(format!(
            "{kind} {} ({}) {}",
            rfc3339(now),
            local_hhmm(now),
            self.fields()
        ))
    }

    /// The take this lock is: its ID; for an unreadable file, which file it
    /// is, or `-` when that cannot be read.
    fn take_id(&self) -> &str {
        match (&self.hold, &self.file) {
            (Some(h), _) => &h.id,
            (None, Some(f)) => f,
            (None, None) => "-",
        }
    }

    /// `<orphaned|stale> <lock> <take ID>`: what a coordinator's watcher
    /// records once it has reported this lock in this state.
    pub fn alert_key(&self) -> String {
        format!("{} {} {}", self.health.as_str(), self.name, self.take_id())
    }
}

/// True for a line `Lock::alert_key` makes.
pub fn is_alert_key(line: &str) -> bool {
    let mut parts = line.split(' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(kind), Some(lock), Some(id), None) => {
            matches!(kind, "orphaned" | "stale")
                && is_valid_name(lock)
                && (id == "-" || is_valid_id(id) || is_file_generation(id))
        }
        _ => false,
    }
}

/// True for a value `file_generation` makes.
fn is_file_generation(s: &str) -> bool {
    s.strip_prefix("file-").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 80
            && rest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'.' || b == b'-')
    })
}

/// Which file `path` is, as `file-<volume>.<index>-<modified>-<size>` in
/// hex: the volume and file index (Windows) or device and inode (Unix),
/// the modified time in nanoseconds, and the size. A file removed and made
/// again under the same name differs in its index or its modified time,
/// even where the creation time is kept (NTFS tunnelling) or the inode is
/// reused. `None` when the file cannot be opened.
fn file_generation(path: &Path) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let meta = f.metadata().ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let (volume, index) = platform::file_id(&f).unwrap_or((0, 0));
    Some(format!(
        "file-{volume:x}.{index:x}-{modified:x}-{:x}",
        meta.len()
    ))
}

/// `<lock> <take ID>` of an alert key, to match it with a held lock.
fn key_take(key: &str) -> Option<(&str, &str)> {
    let (_, rest) = key.split_once(' ')?;
    rest.split_once(' ')
}

/// The minutes before a live holder's lock is stale: `CQ_LOCK_STALE_MINUTES`,
/// a whole number of minutes, at least 1. Unset or empty gives the default;
/// any other value is a usage error (exit 64).
pub fn stale_after_from_env() -> Result<Duration> {
    let minutes =
        overdue::minutes_var(|k| std::env::var(k).ok(), ENV_STALE, DEFAULT_STALE_MINUTES)?;
    Ok(Duration::from_secs(minutes.saturating_mul(60)))
}

/// What `take` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// This call took the lock.
    New(Hold),
    /// The holder already held it. Nothing changed.
    Already(Hold),
    /// Someone else holds it.
    HeldBy(Holding),
}

/// The name a lock command acts as: a valid name that is not `owner`, and,
/// when this session's key is recorded in `members/`, the name recorded for
/// it. So a session cannot take or release a lock under another session's
/// name by mistake.
fn check_actor(root: &Root, actor: &str) -> Result<()> {
    check_name("--as", actor)?;
    if actor == OWNER {
        return Err(CqError::refused(format!(
            "{OWNER} is the user's hold: a coordinator takes or releases it with --as <coordinator> --owner"
        )));
    }
    if let Some(key) = members::session_key()
        && let Some(own) = members::name_for(root, &key)
        && own != actor
    {
        return Err(CqError::refused(format!(
            "this session is recorded as {own} (members/{key}), not {actor}; use --as {own}"
        )));
    }
    Ok(())
}

/// Refuse `--owner` from a name that is not a coordinator.
fn check_owner(root: &Root, actor: &str, owner: bool) -> Result<()> {
    if owner && !coordinators::is_coordinator(root, actor) {
        return Err(CqError::refused(format!(
            "only a coordinator (listed in {}) may take or release the {OWNER} hold",
            root.coordinators().display()
        )));
    }
    Ok(())
}

/// Take `lock` as `actor`, or for the user with `owner`. Refused (exit 5)
/// under `PAUSE`, for a bad name, an over-long `--for`, or `--owner` from a
/// name that is not a coordinator.
pub fn take(root: &Root, lock: &str, actor: &str, owner: bool, purpose: &str) -> Result<Taken> {
    take_with(root, lock, actor, owner, purpose, &write_synced)
}

/// Write `bytes` to a new hold file and flush them to disk.
fn write_synced(f: &mut fs::File, bytes: &[u8]) -> io::Result<()> {
    f.write_all(bytes).and_then(|()| f.sync_all())
}

/// `take`, writing the new hold with `write`, so a test can make the write
/// fail.
fn take_with(
    root: &Root,
    lock: &str,
    actor: &str,
    owner: bool,
    purpose: &str,
    write: &dyn Fn(&mut fs::File, &[u8]) -> io::Result<()>,
) -> Result<Taken> {
    check_name("LOCK", lock)?;
    check_actor(root, actor)?;
    check_owner(root, actor, owner)?;
    let purpose = purpose.trim();
    if purpose.is_empty() {
        return Err(CqError::usage(
            "--for needs a short text saying what the lock is for",
        ));
    }
    if purpose.chars().count() > FOR_MAX {
        return Err(CqError::refused(format!(
            "--for is {} characters; the limit is {FOR_MAX}",
            purpose.chars().count()
        )));
    }
    // Checked after the arguments, so a bad command is still a usage error.
    if root.pause().exists() {
        return Err(CqError::refused(format!(
            "queue paused: remove PAUSE to take a lock ({})",
            root.pause().display()
        )));
    }
    let holder = if owner { OWNER } else { actor };
    let dir = root.locks_dir();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let path = root.lock_hold(lock);
    // A second round only when the lock was released between the failed
    // create and the read.
    for _ in 0..3 {
        let now = timefmt::now();
        let hold = Hold {
            lock: lock.to_owned(),
            id: format!(
                "{}-{actor}-{:04x}",
                timefmt::id_stamp(now),
                fsutil::random_u16()
            ),
            holder: holder.to_owned(),
            by: actor.to_owned(),
            taken: rfc3339(now),
            taken_local: timefmt::local_datetime(now),
            purpose: purpose.to_owned(),
            session_key: members::session_key(),
        };
        let bytes = [
            serde_json::to_vec_pretty(&hold).expect("a hold always serialises"),
            b"\n".to_vec(),
        ]
        .concat();
        let created = {
            let _guard = guard(root, lock)?;
            create_new(&path, &bytes, write)
        };
        match created {
            Ok(()) => {
                log(
                    root,
                    "take",
                    lock,
                    Some(&hold),
                    &format!(" for={}", event_text(&hold.purpose, FOR_MAX)),
                );
                return Ok(Taken::New(hold));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(CqError::io(format!("create {}", path.display()), e)),
        }
        match read(root, lock)? {
            Holding::Free => continue,
            Holding::Held(h) if h.holder == holder => return Ok(Taken::Already(h)),
            other => return Ok(Taken::HeldBy(other)),
        }
    }
    Err(CqError::error(format!(
        "{lock} was taken and released three times while this take ran; try again"
    )))
}

/// Create `path` only if it does not exist and `write` `bytes` to it. A
/// file left half-written by a failed write is removed. The caller holds
/// the lock's guard, so no break and new take can replace the file between
/// the create and that removal. On Windows a file that is being removed
/// cannot be created for a moment, so "access denied" is retried.
fn create_new(
    path: &Path,
    bytes: &[u8],
    write: &dyn Fn(&mut fs::File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut attempt = 0;
    let mut f = loop {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(f) => break f,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < fsutil::RETRIES => {
                attempt += 1;
                sleep(fsutil::RETRY_DELAY);
            }
            Err(e) => return Err(e),
        }
    };
    let written = write(&mut f, bytes);
    if written.is_err() {
        drop(f);
        let _ = fs::remove_file(path);
    }
    written
}

/// Read `locks/<lock>.lock`. A file that does not parse is read again for
/// a short while (its taker may still be writing it) before it counts as
/// unreadable.
pub fn read(root: &Root, lock: &str) -> Result<Holding> {
    let path = root.lock_hold(lock);
    for attempt in 0..SETTLE_TRIES {
        match fsutil::read_with_retry(&path) {
            Ok(bytes) => {
                if let Ok(h) = serde_json::from_slice::<Hold>(&bytes)
                    && h.is_valid()
                    && h.lock == lock
                {
                    return Ok(Holding::Held(h));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Holding::Free),
            Err(e) => return Err(CqError::io(format!("read {}", path.display()), e)),
        }
        if attempt + 1 < SETTLE_TRIES {
            sleep(SETTLE_WAIT);
        }
    }
    Ok(Holding::Unreadable)
}

/// Hold `locks/<lock>.guard` until the returned file is dropped. Never
/// remove the guard file: another process may hold an open handle to it.
fn guard(root: &Root, lock: &str) -> Result<fs::File> {
    let dir = root.locks_dir();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let path = root.lock_guard(lock);
    let f = fsutil::open_lock_file(&path)
        .map_err(|e| CqError::io(format!("open {}", path.display()), e))?;
    f.lock()
        .map_err(|e| CqError::io(format!("lock {}", path.display()), e))?;
    Ok(f)
}

fn remove(root: &Root, lock: &str) -> Result<()> {
    let path = root.lock_hold(lock);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CqError::io(format!("remove {}", path.display()), e)),
    }
}

/// Release `lock`, held by `actor`, or with `owner` the user's hold. Exit 1
/// when it is not held, or held by another name.
pub fn release(root: &Root, lock: &str, actor: &str, owner: bool) -> Result<Hold> {
    check_name("LOCK", lock)?;
    check_actor(root, actor)?;
    check_owner(root, actor, owner)?;
    let holder = if owner { OWNER } else { actor };
    let _guard = guard(root, lock)?;
    match read(root, lock)? {
        Holding::Free => Err(CqError::new(exit::LOCK_HELD, format!("{lock} is not held"))),
        Holding::Unreadable => Err(CqError::new(
            exit::LOCK_HELD,
            format!("{lock} has an unreadable holder; a coordinator can break it"),
        )),
        Holding::Held(h) if h.holder != holder => {
            let hint = if h.holder == OWNER {
                format!("; only a coordinator releases the {OWNER} hold, with --owner")
            } else {
                String::new()
            };
            Err(CqError::new(
                exit::LOCK_HELD,
                format!("{lock} is held by {}, not {holder}{hint}", h.holder),
            ))
        }
        Holding::Held(h) => {
            remove(root, lock)?;
            let by = if owner {
                format!(" by={actor}")
            } else {
                String::new()
            };
            log(
                root,
                "release",
                lock,
                Some(&h),
                &format!("{by}{}", held_for(&h)),
            );
            Ok(h)
        }
    }
}

/// Break `lock` as the coordinator `actor`, whoever holds it. Refused
/// (exit 5) for a name that is not a coordinator; exit 1 when it is not
/// held. Returns the hold it removed (`None` for an unreadable one).
pub fn break_lock(root: &Root, lock: &str, actor: &str, reason: &str) -> Result<Option<Hold>> {
    check_name("LOCK", lock)?;
    check_actor(root, actor)?;
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(CqError::usage("--reason needs a text saying why"));
    }
    if !coordinators::is_coordinator(root, actor) {
        return Err(CqError::refused(format!(
            "only a coordinator (listed in {}) may break a lock",
            root.coordinators().display()
        )));
    }
    let _guard = guard(root, lock)?;
    let hold = match read(root, lock)? {
        Holding::Free => {
            return Err(CqError::new(exit::LOCK_HELD, format!("{lock} is not held")));
        }
        Holding::Unreadable => None,
        Holding::Held(h) => Some(h),
    };
    remove(root, lock)?;
    let held = hold.as_ref().map_or_else(String::new, held_for);
    log(
        root,
        "break",
        lock,
        hold.as_ref(),
        &format!(
            " by={actor}{held} reason={}",
            event_text(reason, EVENT_SUBJECT_MAX)
        ),
    );
    Ok(hold)
}

/// ` age=Nm`: how long `hold` was held, when its time is readable.
fn held_for(hold: &Hold) -> String {
    timefmt::parse_rfc3339_z(&hold.taken).map_or_else(String::new, |t| {
        format!(" age={}m", age(t, timefmt::now()).as_secs() / 60)
    })
}

fn age(since: Timestamp, now: Timestamp) -> Duration {
    Duration::from_secs(u64::try_from(now.as_second() - since.as_second()).unwrap_or(0))
}

/// `lock`, if held, with its health at `now`. Reads only.
pub fn inspect(
    root: &Root,
    lock: &str,
    stale_after: Duration,
    now: Timestamp,
) -> Result<Option<Lock>> {
    inspect_by(root, lock, stale_after, now, None)
}

/// `inspect`, read by the watcher `watcher` when it is one: a lock that
/// watcher holds itself is judged by its own running process (see
/// `Liveness::read_by`).
fn inspect_by(
    root: &Root,
    lock: &str,
    stale_after: Duration,
    now: Timestamp,
    watcher: Option<&str>,
) -> Result<Option<Lock>> {
    check_name("LOCK", lock)?;
    let hold = match read(root, lock)? {
        Holding::Free => return Ok(None),
        Holding::Held(h) => Some(h),
        Holding::Unreadable => None,
    };
    let path = root.lock_hold(lock);
    let file = if hold.is_none() {
        file_generation(&path)
    } else {
        None
    };
    let modified = fsutil::modified(&path).and_then(timefmt::from_system_time);
    let since = hold
        .as_ref()
        .and_then(|h| timefmt::parse_rfc3339_z(&h.taken))
        .or(modified)
        .unwrap_or(now);
    let alive = Duration::from_secs(ALIVE_SECS.unsigned_abs());
    let (health, why) = match &hold {
        None if age(since, now) >= alive => {
            (Health::Orphaned, Some("unreadable lock file".to_owned()))
        }
        None => (Health::Live, None),
        Some(h) if h.holder == OWNER => (Health::Live, None),
        Some(h) => {
            let live = Liveness::read_by(root, &h.holder, watcher);
            match live.why_stopped() {
                Some(why) => {
                    let last = live.beat.map_or(since, |b| b.max(since));
                    if age(last, now) >= alive {
                        (Health::Orphaned, Some(why))
                    } else {
                        (Health::Live, None)
                    }
                }
                None if age(since, now) >= stale_after => (Health::Stale, None),
                None => (Health::Live, None),
            }
        }
    };
    Ok(Some(Lock {
        name: lock.to_owned(),
        hold,
        since,
        age: age(since, now),
        health,
        why,
        file,
    }))
}

/// The names of the lock files, sorted. A missing `locks/` gives none.
pub fn names(root: &Root) -> Vec<String> {
    let Ok(rd) = fs::read_dir(root.locks_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".lock")?;
            is_valid_name(stem).then(|| stem.to_owned())
        })
        .collect();
    out.sort();
    out
}

/// Every held lock, by name. Locks that cannot be read are skipped.
pub fn list(root: &Root, stale_after: Duration, now: Timestamp) -> Vec<Lock> {
    list_by(root, stale_after, now, None)
}

fn list_by(root: &Root, stale_after: Duration, now: Timestamp, watcher: Option<&str>) -> Vec<Lock> {
    names(root)
        .iter()
        .filter_map(|n| {
            inspect_by(root, n, stale_after, now, watcher)
                .ok()
                .flatten()
        })
        .collect()
}

/// The locks `name` holds, by lock name, for `cq status`.
pub fn held_by(root: &Root, name: &str) -> Vec<String> {
    names(root)
        .into_iter()
        .filter(|l| matches!(read(root, l), Ok(Holding::Held(h)) if h.holder == name))
        .collect()
}

/// What `wait` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    Free,
    /// Still held at `--max`, with the holder when it can be read.
    StillHeld(Option<Box<Lock>>),
}

/// Wait until `lock` is free, reading only whether its file exists, every
/// `poll`, for at most `max`. Sends nothing, takes nothing, and ignores
/// `PAUSE`.
pub fn wait(
    root: &Root,
    lock: &str,
    poll: Duration,
    max: Option<Duration>,
    stale_after: Duration,
) -> Result<Waited> {
    check_name("LOCK", lock)?;
    let path = root.lock_hold(lock);
    let until = max.map(|m| Instant::now() + m);
    loop {
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Waited::Free),
            // Held, or not readable now: wait on.
            _ => {}
        }
        let left = until.map(|u| u.saturating_duration_since(Instant::now()));
        if left == Some(Duration::ZERO) {
            let held = inspect(root, lock, stale_after, timefmt::now())?;
            return Ok(match held {
                None => Waited::Free,
                Some(l) => Waited::StillHeld(Some(Box::new(l))),
            });
        }
        sleep(left.map_or(poll, |l| l.min(poll)));
    }
}

/// For the coordinator `watcher`'s watcher, run in that process: the event
/// line of each orphaned or stale lock not reported before, with its alert
/// key. Keys of takes that are no longer held are dropped from `reported`.
/// Reads only; the caller records each key once its line is delivered.
pub fn alerts(
    root: &Root,
    watcher: &str,
    stale_after: Duration,
    reported: &mut HashSet<String>,
) -> Vec<(String, Lock)> {
    let now = timefmt::now();
    let held = list_by(root, stale_after, now, Some(watcher));
    let takes: HashSet<(&str, &str)> = held
        .iter()
        .map(|l| (l.name.as_str(), l.take_id()))
        .collect();
    reported.retain(|k| key_take(k).is_some_and(|t| takes.contains(&t)));
    held.iter()
        .filter(|l| l.health != Health::Live && !reported.contains(&l.alert_key()))
        .map(|l| (l.alert_key(), l.clone()))
        .collect()
}

/// Log the report of an orphaned or stale lock, seen by `seen_by`.
pub fn log_alert(root: &Root, lock: &Lock, seen_by: &str) {
    let event = match lock.health {
        Health::Live => return,
        Health::Orphaned => "orphan",
        Health::Stale => "stale",
    };
    log(
        root,
        event,
        &lock.name,
        lock.hold.as_ref(),
        &format!(" age={}m seen_by={seen_by}", lock.age.as_secs() / 60),
    );
}

/// One `locks.log` line: `<UTC> event=<event> lock=<lock> holder=<holder>
/// id=<take>` and `extra`.
fn log(root: &Root, event: &str, lock: &str, hold: Option<&Hold>, extra: &str) {
    let (holder, id) = hold.map_or(("?", "-"), |h| (h.holder.as_str(), h.id.as_str()));
    let line = format!(
        "{} event={event} lock={lock} holder={holder} id={id}{extra}",
        rfc3339(timefmt::now())
    );
    let path = root.locks_log();
    if let Err(e) = exitlog::append(&path, &line) {
        eprintln!("cq lock: could not write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::id_parts;

    const MIN: Duration = Duration::from_secs(60);

    fn root() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path().to_path_buf());
        root.init().unwrap();
        (dir, root)
    }

    fn taken(t: Taken) -> Hold {
        match t {
            Taken::New(h) => h,
            other => panic!("not taken: {other:?}"),
        }
    }

    #[test]
    fn take_writes_a_hold_and_a_second_take_finds_it() {
        let (_dir, root) = root();
        let h = taken(take(&root, "monitor", "worker-a", false, " GUI test ").unwrap());
        assert_eq!(h.holder, "worker-a");
        assert_eq!(h.by, "worker-a");
        assert_eq!(h.purpose, "GUI test");
        assert_eq!(id_parts(&h.id).map(|(_, by)| by), Some("worker-a"));
        assert!(timefmt::parse_rfc3339_z(&h.taken).is_some());
        assert_eq!(read(&root, "monitor").unwrap(), Holding::Held(h.clone()));

        // The holder again: already yours, nothing changes.
        let again = take(&root, "monitor", "worker-a", false, "other").unwrap();
        assert_eq!(again, Taken::Already(h.clone()));
        // Anyone else: held.
        let other = take(&root, "monitor", "worker-b", false, "x").unwrap();
        assert_eq!(other, Taken::HeldBy(Holding::Held(h)));

        let log = fs::read_to_string(root.locks_log()).unwrap();
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(log.contains(" event=take lock=monitor holder=worker-a id="));
        assert!(log.trim_end().ends_with(" for=\"GUI test\""), "{log}");
    }

    #[test]
    fn bad_names_long_text_and_pause_are_refused() {
        let (_dir, root) = root();
        let code = |r: Result<Taken>| r.unwrap_err().code;
        assert_eq!(code(take(&root, "../x", "worker-a", false, "x")), 5);
        assert_eq!(code(take(&root, "monitor", "Worker", false, "x")), 5);
        assert_eq!(code(take(&root, "monitor", OWNER, false, "x")), 5);
        assert_eq!(code(take(&root, "monitor", "worker-a", false, " ")), 64);
        let long = "x".repeat(FOR_MAX + 1);
        assert_eq!(code(take(&root, "monitor", "worker-a", false, &long)), 5);
        fs::write(root.pause(), "").unwrap();
        let e = take(&root, "monitor", "worker-a", false, "x").unwrap_err();
        assert_eq!(e.code, 5);
        assert!(e.message.contains("PAUSE"), "{}", e.message);
        assert!(!root.lock_hold("monitor").exists());
    }

    /// Threads that all take at once: exactly one wins, and every other
    /// one sees the winner. Each take is a separate create, as from a
    /// separate process.
    #[test]
    fn of_many_takes_at_once_exactly_one_wins() {
        const TAKERS: usize = 24;
        for round in 0..5 {
            let (_dir, root) = root();
            let start = std::sync::Barrier::new(TAKERS);
            let results: Vec<Taken> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..TAKERS)
                    .map(|i| {
                        let (root, start) = (&root, &start);
                        s.spawn(move || {
                            start.wait();
                            take(root, "monitor", &format!("worker-{i}"), false, "race").unwrap()
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let winners: Vec<&Hold> = results
                .iter()
                .filter_map(|t| match t {
                    Taken::New(h) => Some(h),
                    _ => None,
                })
                .collect();
            assert_eq!(winners.len(), 1, "round {round}: {results:?}");
            for t in &results {
                match t {
                    Taken::New(_) => {}
                    Taken::HeldBy(Holding::Held(h)) => assert_eq!(h, winners[0]),
                    other => panic!("round {round}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn only_the_holder_releases() {
        let (_dir, root) = root();
        taken(take(&root, "monitor", "worker-a", false, "x").unwrap());
        let e = release(&root, "monitor", "worker-b", false).unwrap_err();
        assert_eq!(e.code, exit::LOCK_HELD);
        assert!(e.message.contains("held by worker-a"), "{}", e.message);
        assert!(root.lock_hold("monitor").exists());
        release(&root, "monitor", "worker-a", false).unwrap();
        assert!(!root.lock_hold("monitor").exists());
        let e = release(&root, "monitor", "worker-a", false).unwrap_err();
        assert_eq!(e.code, exit::LOCK_HELD);
        assert!(e.message.contains("not held"), "{}", e.message);
        let log = fs::read_to_string(root.locks_log()).unwrap();
        assert!(log.lines().nth(1).unwrap().contains(" event=release "));
    }

    #[test]
    fn the_owner_hold_is_for_coordinators_only() {
        let (_dir, root) = root();
        let e = take(&root, "monitor", "lead", true, "user").unwrap_err();
        assert_eq!(e.code, 5);
        fs::write(root.coordinators(), "lead\n").unwrap();
        let h = taken(take(&root, "monitor", "lead", true, "user testing").unwrap());
        assert_eq!((h.holder.as_str(), h.by.as_str()), (OWNER, "lead"));
        // Nobody else takes it, and it is not the coordinator's own hold.
        assert!(matches!(
            take(&root, "monitor", "worker-a", false, "x").unwrap(),
            Taken::HeldBy(_)
        ));
        assert!(matches!(
            take(&root, "monitor", "lead", false, "x").unwrap(),
            Taken::HeldBy(_)
        ));
        assert_eq!(
            release(&root, "monitor", "lead", false).unwrap_err().code,
            exit::LOCK_HELD
        );
        assert_eq!(
            release(&root, "monitor", "worker-a", true)
                .unwrap_err()
                .code,
            5
        );
        // Another coordinator may release the user's hold.
        fs::write(root.coordinators(), "lead\nlead-2\n").unwrap();
        release(&root, "monitor", "lead-2", true).unwrap();
        assert!(!root.lock_hold("monitor").exists());
    }

    #[test]
    fn break_needs_a_coordinator_and_a_reason() {
        let (_dir, root) = root();
        taken(take(&root, "monitor", "worker-a", false, "x").unwrap());
        assert_eq!(
            break_lock(&root, "monitor", "worker-b", "gone")
                .unwrap_err()
                .code,
            5
        );
        fs::write(root.coordinators(), "lead\n").unwrap();
        assert_eq!(
            break_lock(&root, "monitor", "lead", " ").unwrap_err().code,
            64
        );
        let h = break_lock(&root, "monitor", "lead", "session ended").unwrap();
        assert_eq!(h.unwrap().holder, "worker-a");
        assert!(!root.lock_hold("monitor").exists());
        assert_eq!(
            break_lock(&root, "monitor", "lead", "again")
                .unwrap_err()
                .code,
            exit::LOCK_HELD
        );
        let log = fs::read_to_string(root.locks_log()).unwrap();
        let line = log.lines().nth(1).unwrap();
        assert!(
            line.contains(" event=break lock=monitor holder=worker-a id="),
            "{line}"
        );
        assert!(line.ends_with(" reason=\"session ended\""), "{line}");
    }

    /// A file made by hand, or left empty, is held by `?`: it blocks takes
    /// and cannot be released, and a coordinator can break it. Once it is
    /// old it reads as orphaned.
    #[test]
    fn an_unreadable_lock_is_held_and_can_be_broken() {
        let (_dir, root) = root();
        fs::create_dir_all(root.locks_dir()).unwrap();
        fs::write(root.lock_hold("monitor"), "").unwrap();
        assert_eq!(read(&root, "monitor").unwrap(), Holding::Unreadable);
        assert_eq!(
            take(&root, "monitor", "worker-a", false, "x").unwrap(),
            Taken::HeldBy(Holding::Unreadable)
        );
        let now = timefmt::now();
        let l = inspect(&root, "monitor", 120 * MIN, now).unwrap().unwrap();
        assert_eq!(l.health, Health::Live);
        assert!(l.line().contains(" holder=? "), "{}", l.line());
        let later = now + jiff::SignedDuration::from_secs(ALIVE_SECS);
        let l = inspect(&root, "monitor", 120 * MIN, later)
            .unwrap()
            .unwrap();
        assert_eq!(l.health, Health::Orphaned);
        let key = l.alert_key();
        assert!(key.starts_with("orphaned monitor file-"), "{key}");
        assert!(is_alert_key(&key), "{key}");
        fs::write(root.coordinators(), "lead\n").unwrap();
        assert_eq!(break_lock(&root, "monitor", "lead", "junk").unwrap(), None);
    }

    /// A holder with no watcher is live until 90 s after the take, then
    /// orphaned; a heartbeat later than the take counts from the beat.
    #[test]
    fn a_holder_without_a_watcher_is_orphaned_after_90_seconds() {
        let (_dir, root) = root();
        let h = taken(take(&root, "monitor", "worker-a", false, "x").unwrap());
        let t = timefmt::parse_rfc3339_z(&h.taken).unwrap();
        let at = |s: i64| t + jiff::SignedDuration::from_secs(s);
        let health = |now| {
            inspect(&root, "monitor", 120 * MIN, now)
                .unwrap()
                .unwrap()
                .health
        };
        assert_eq!(health(at(ALIVE_SECS - 1)), Health::Live);
        assert_eq!(health(at(ALIVE_SECS)), Health::Orphaned);
        fs::write(root.heartbeat("worker-a"), format!("{}\n", rfc3339(at(60)))).unwrap();
        assert_eq!(health(at(60 + ALIVE_SECS - 1)), Health::Live);
        let l = inspect(&root, "monitor", 120 * MIN, at(60 + ALIVE_SECS))
            .unwrap()
            .unwrap();
        assert_eq!(l.health, Health::Orphaned);
        assert_eq!(l.why.as_deref(), Some("no pid file"));
        assert!(
            l.line().ends_with(" why=\"no pid file\" orphaned"),
            "{}",
            l.line()
        );
        // The owner hold is never orphaned.
        release(&root, "monitor", "worker-a", false).unwrap();
        fs::write(root.coordinators(), "lead\n").unwrap();
        taken(take(&root, "monitor", "lead", true, "user").unwrap());
        assert_eq!(health(at(100_000)), Health::Live);
    }

    #[test]
    fn alerts_report_each_take_once_and_forget_released_ones() {
        let (_dir, root) = root();
        let h = taken(take(&root, "monitor", "worker-a", false, "x").unwrap());
        // Make the take old enough to be orphaned.
        let old = timefmt::now() - jiff::SignedDuration::from_secs(600);
        let old_hold = Hold {
            taken: rfc3339(old),
            ..h
        };
        fs::write(
            root.lock_hold("monitor"),
            serde_json::to_vec(&old_hold).unwrap(),
        )
        .unwrap();
        let mut reported = HashSet::new();
        let found = alerts(&root, "lead", 120 * MIN, &mut reported);
        assert_eq!(found.len(), 1);
        let (key, lock) = &found[0];
        assert!(is_alert_key(key), "{key}");
        assert_eq!(key, &format!("orphaned monitor {}", old_hold.id));
        let line = lock.event_line(timefmt::now()).unwrap();
        assert!(line.starts_with("CQ-LOCK-ORPHAN "), "{line}");
        assert!(
            line.contains(" lock=monitor holder=worker-a since="),
            "{line}"
        );
        assert!(line.contains(" age=10m for=\"x\" "), "{line}");
        reported.insert(key.clone());
        assert!(alerts(&root, "lead", 120 * MIN, &mut reported).is_empty());
        assert_eq!(reported.len(), 1);
        // Released: forgotten.
        release(&root, "monitor", "worker-a", false).unwrap();
        assert!(alerts(&root, "lead", 120 * MIN, &mut reported).is_empty());
        assert!(reported.is_empty());
    }

    /// Each unreadable file is its own take. One made again under the same
    /// name (a break, then a taker that stops before it writes) is reported
    /// again, though no scan saw the lock free in between.
    #[test]
    fn a_new_unreadable_file_is_reported_again() {
        let (_dir, root) = root();
        let path = root.lock_hold("monitor");
        let make = |secs_ago: u64| {
            let f = fs::File::create(&path).unwrap();
            f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs_ago))
                .unwrap();
        };
        make(600);
        let mut reported = HashSet::new();
        let found = alerts(&root, "lead", 120 * MIN, &mut reported);
        assert_eq!(found.len(), 1, "{found:?}");
        let first = found[0].0.clone();
        assert!(first.starts_with("orphaned monitor file-"), "{first}");
        assert!(is_alert_key(&first), "{first}");
        reported.insert(first.clone());
        // The same file: not again, and its key stays.
        assert!(alerts(&root, "lead", 120 * MIN, &mut reported).is_empty());
        assert!(reported.contains(&first));

        fs::remove_file(&path).unwrap();
        make(300);
        let found = alerts(&root, "lead", 120 * MIN, &mut reported);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_ne!(found[0].0, first);
        assert!(is_alert_key(&found[0].0), "{}", found[0].0);
        // The first file's key is dropped: it is no longer there.
        assert!(reported.is_empty(), "{reported:?}");
    }

    /// A coordinator's watcher judges a lock it holds itself by its own
    /// running process, so an old one is stale to it. Any other reader still
    /// treats a pid file that names its own process as a dead watcher's.
    #[test]
    fn a_watcher_finds_its_own_old_lock_stale() {
        let (_dir, root) = root();
        let h = taken(take(&root, "monitor", "lead", false, "x").unwrap());
        let old = Hold {
            taken: rfc3339(timefmt::now() - jiff::SignedDuration::from_secs(600)),
            ..h
        };
        fs::write(root.lock_hold("monitor"), serde_json::to_vec(&old).unwrap()).unwrap();
        // This process is lead's watcher, beating now.
        fs::write(root.pid_file("lead"), format!("{}\n", std::process::id())).unwrap();
        fs::write(
            root.heartbeat("lead"),
            format!("{}\n", rfc3339(timefmt::now())),
        )
        .unwrap();

        let found = alerts(&root, "lead", 2 * MIN, &mut HashSet::new());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].1.health, Health::Stale);
        assert_eq!(found[0].0, format!("stale monitor {}", old.id));

        assert!(Liveness::read_by(&root, "lead", Some("lead")).running());
        assert!(!Liveness::read(&root, "lead").running());
        assert!(!Liveness::read_by(&root, "lead", Some("lead-2")).running());
        // Another coordinator's watcher in this process: not lead's.
        assert!(alerts(&root, "lead-2", 2 * MIN, &mut HashSet::new()).is_empty());
    }

    /// A take whose write fails removes its file under the guard. A break
    /// and a new take that start after the failure wait for that removal,
    /// so the failed take never removes the new hold.
    #[test]
    fn a_failed_take_never_removes_a_replacement_hold() {
        let (_dir, root) = root();
        fs::write(root.coordinators(), "lead\n").unwrap();
        let (failed, on_fail) = std::sync::mpsc::channel();
        // The whole hold is written, so a reader finds it at once; then
        // the flush fails, and the take pauses before it cleans up.
        let fail_sync = move |f: &mut fs::File, bytes: &[u8]| {
            f.write_all(bytes)?;
            failed.send(()).unwrap();
            sleep(Duration::from_millis(500));
            Err(io::Error::other("injected sync failure"))
        };
        std::thread::scope(|s| {
            let failing =
                s.spawn(|| take_with(&root, "monitor", "worker-a", false, "x", &fail_sync));
            on_fail.recv().unwrap();
            let replacing = s.spawn(|| {
                let broken = break_lock(&root, "monitor", "lead", "stuck");
                (broken, take(&root, "monitor", "worker-b", false, "y"))
            });
            let e = failing.join().unwrap().unwrap_err();
            assert!(e.message.contains("injected sync failure"), "{}", e.message);
            let (broken, replaced) = replacing.join().unwrap();
            // The break waited for the cleanup, so it found the lock free.
            assert_eq!(broken.unwrap_err().code, exit::LOCK_HELD);
            let h = taken(replaced.unwrap());
            assert_eq!(h.holder, "worker-b");
            assert_eq!(read(&root, "monitor").unwrap(), Holding::Held(h));
        });
    }

    #[test]
    fn alert_keys_are_checked() {
        assert!(is_alert_key("stale monitor 20261111T100000Z-worker-a-ab12"));
        assert!(is_alert_key("orphaned monitor -"));
        assert!(is_alert_key("orphaned monitor file-1a.2b-17f3c-0"));
        assert!(!is_alert_key("orphaned monitor file-"));
        assert!(!is_alert_key("orphaned monitor file-x"));
        assert!(!is_alert_key("live monitor -"));
        assert!(!is_alert_key("stale ../x -"));
        assert!(!is_alert_key("stale monitor junk"));
        assert!(!is_alert_key("stale monitor - extra"));
        assert!(!is_alert_key(""));
    }

    #[test]
    fn wait_returns_when_free_or_at_its_limit() {
        let (_dir, root) = root();
        let fast = Duration::from_millis(20);
        assert_eq!(
            wait(&root, "monitor", fast, None, 120 * MIN).unwrap(),
            Waited::Free
        );
        taken(take(&root, "monitor", "worker-a", false, "x").unwrap());
        let started = Instant::now();
        let w = wait(
            &root,
            "monitor",
            fast,
            Some(Duration::from_millis(200)),
            120 * MIN,
        )
        .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(200));
        match w {
            Waited::StillHeld(Some(l)) => assert_eq!(l.holder(), "worker-a"),
            other => panic!("{other:?}"),
        }
        std::thread::scope(|s| {
            let waiting = s.spawn(|| wait(&root, "monitor", fast, None, 120 * MIN).unwrap());
            sleep(Duration::from_millis(150));
            assert!(!waiting.is_finished());
            release(&root, "monitor", "worker-a", false).unwrap();
            assert_eq!(waiting.join().unwrap(), Waited::Free);
        });
    }

    #[test]
    fn held_by_and_list() {
        let (_dir, root) = root();
        taken(take(&root, "monitor", "worker-a", false, "x").unwrap());
        taken(take(&root, "printer", "worker-b", false, "y").unwrap());
        taken(take(&root, "speaker", "worker-a", false, "z").unwrap());
        assert_eq!(held_by(&root, "worker-a"), ["monitor", "speaker"]);
        assert!(held_by(&root, "worker-c").is_empty());
        let all = list(&root, 120 * MIN, timefmt::now());
        let held: Vec<&str> = all.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(held, ["monitor", "printer", "speaker"]);
        // The guard file is not a lock.
        release(&root, "printer", "worker-b", false).unwrap();
        assert!(root.lock_guard("printer").exists());
        assert_eq!(names(&root), ["monitor", "speaker"]);
    }
}
