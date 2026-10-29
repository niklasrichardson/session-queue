//! Overdue tasks (issue #21).
//!
//! A task is overdue when it is still unclaimed `unclaimed` after it
//! arrived, or still claimed with no `done` `claimed` after it was claimed.
//! The age is the message file's modified time: `cq send` sets it when it
//! writes the message, and `cq claim` sets it when it claims it. Results and
//! info need no reply, so they are never overdue. A finished message has
//! left the inbox, so it is never overdue either.
//!
//! Reporting is read only. `cq` never moves, re-sends or answers an overdue
//! task, and `PAUSE` does not stop the report. A coordinator acts on it.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use jiff::Timestamp;

use crate::error::{CqError, Result};
use crate::fsutil;
use crate::list::created_of;
use crate::message::{EVENT_SUBJECT_MAX, event_text, header};
use crate::names::{is_valid_id, is_valid_name};
use crate::root::{Root, message_file};
use crate::timefmt::{local_hhmm, rfc3339};

pub const DEFAULT_UNCLAIMED_MINUTES: u64 = 30;
pub const DEFAULT_CLAIMED_MINUTES: u64 = 120;
/// Minutes before an unclaimed task is overdue.
pub const ENV_UNCLAIMED: &str = "CQ_OVERDUE_UNCLAIMED_MINUTES";
/// Minutes before a claimed task with no `done` is overdue.
pub const ENV_CLAIMED: &str = "CQ_OVERDUE_CLAIMED_MINUTES";

/// Where a task is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// In `inbox/<name>/`: nobody has claimed it.
    Unclaimed,
    /// In `inbox/<name>/.processing/`: claimed, with no `done` yet.
    Claimed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Unclaimed => "unclaimed",
            State::Claimed => "claimed",
        }
    }
}

/// How long a task may wait in each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub unclaimed: Duration,
    pub claimed: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self::from_minutes(DEFAULT_UNCLAIMED_MINUTES, DEFAULT_CLAIMED_MINUTES)
    }
}

impl Limits {
    pub fn from_minutes(unclaimed: u64, claimed: u64) -> Self {
        let m = |n: u64| Duration::from_secs(n.saturating_mul(60));
        Self {
            unclaimed: m(unclaimed),
            claimed: m(claimed),
        }
    }

    /// `CQ_OVERDUE_UNCLAIMED_MINUTES` and `CQ_OVERDUE_CLAIMED_MINUTES`, each
    /// a whole number of minutes, at least 1. An unset or empty variable
    /// gives the default; any other value is a usage error (exit 64).
    pub fn from_env() -> Result<Self> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let read = |var: &str, default: u64| -> Result<u64> {
            match get(var).map(|v| v.trim().to_owned()) {
                None => Ok(default),
                Some(v) if v.is_empty() => Ok(default),
                Some(v) => v.parse::<u64>().ok().filter(|n| *n >= 1).ok_or_else(|| {
                    CqError::usage(format!(
                        "{var} must be a whole number of minutes, at least 1; got {v:?}"
                    ))
                }),
            }
        };
        Ok(Self::from_minutes(
            read(ENV_UNCLAIMED, DEFAULT_UNCLAIMED_MINUTES)?,
            read(ENV_CLAIMED, DEFAULT_CLAIMED_MINUTES)?,
        ))
    }

    /// These limits, with either replaced when given (the `cq list` flags).
    pub fn with_minutes(self, unclaimed: Option<u64>, claimed: Option<u64>) -> Self {
        let other = Self::from_minutes(unclaimed.unwrap_or(0), claimed.unwrap_or(0));
        Self {
            unclaimed: unclaimed.map_or(self.unclaimed, |_| other.unclaimed),
            claimed: claimed.map_or(self.claimed, |_| other.claimed),
        }
    }

    pub fn limit(&self, state: State) -> Duration {
        match state {
            State::Unclaimed => self.unclaimed,
            State::Claimed => self.claimed,
        }
    }

    /// The rule: a `task` that has been in `state` for at least its limit.
    pub fn is_overdue(&self, kind: &str, state: State, age: Duration) -> bool {
        kind == "task" && age >= self.limit(state)
    }

    /// `unclaimed_after=30m claimed_after=120m`
    pub fn describe(&self) -> String {
        format!(
            "unclaimed_after={}m claimed_after={}m",
            self.unclaimed.as_secs() / 60,
            self.claimed.as_secs() / 60
        )
    }
}

/// One overdue task. Holds no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overdue {
    pub id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub state: State,
    /// How long it has been in `state`.
    pub age: Duration,
    /// When it was sent, from its ID.
    pub created: Timestamp,
}

impl Overdue {
    /// The fields after the time in a `cq list --overdue` or `CQ-OVERDUE`
    /// line, safe for one line.
    fn fields(&self) -> String {
        format!(
            "state={} age={}m from={} to={} id={} subject={}",
            self.state.as_str(),
            self.age.as_secs() / 60,
            self.from,
            self.to,
            self.id,
            event_text(&self.subject, EVENT_SUBJECT_MAX)
        )
    }

    /// A `cq list --overdue` line, timed by when the task was sent.
    pub fn list_line(&self) -> String {
        format!(
            "OVERDUE {} ({}) {}",
            rfc3339(self.created),
            local_hhmm(self.created),
            self.fields()
        )
    }

    /// A watcher's `CQ-OVERDUE` line, timed `now`.
    pub fn event_line(&self, now: Timestamp) -> String {
        format!(
            "CQ-OVERDUE {} ({}) {}",
            rfc3339(now),
            local_hhmm(now),
            self.fields()
        )
    }
}

/// The names with an inbox folder, sorted.
fn inbox_names(root: &Root) -> Vec<String> {
    let Ok(rd) = fs::read_dir(root.path().join("inbox")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_name(n))
        .collect();
    out.sort();
    out
}

/// The overdue tasks in every inbox, oldest sent first. Reads only.
pub fn scan(root: &Root, limits: &Limits, now: SystemTime) -> Vec<Overdue> {
    let mut out = Vec::new();
    for to in inbox_names(root) {
        for (dir, state) in [
            (root.inbox(&to), State::Unclaimed),
            (root.processing(&to), State::Claimed),
        ] {
            scan_dir(root, &dir, &to, state, limits, now, &mut out);
        }
    }
    out.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    out
}

fn scan_dir(
    root: &Root,
    dir: &Path,
    to: &str,
    state: State,
    limits: &Limits,
    now: SystemTime,
    out: &mut Vec<Overdue>,
) {
    for id in fsutil::json_stems(dir, is_valid_id) {
        let path = message_file(dir, &id);
        let Some(age) = fsutil::modified(&path).and_then(|m| now.duration_since(m).ok()) else {
            continue;
        };
        // Check the age first, so a young message is never read.
        if age < limits.limit(state) {
            continue;
        }
        // A claim in progress has moved the file but not yet set its time.
        let age = match state {
            State::Unclaimed => age,
            State::Claimed => match settled_age(root, to, &path, now) {
                Some(a) if a >= limits.limit(state) => a,
                _ => continue,
            },
        };
        let Ok(bytes) = fsutil::read_with_retry(&path) else {
            continue;
        };
        let h = header(&bytes);
        if !limits.is_overdue(&h.kind, state, age) {
            continue;
        }
        out.push(Overdue {
            created: created_of(&id),
            id,
            from: h.from,
            to: to.to_owned(),
            subject: h.subject,
            state,
            age,
        });
    }
}

/// A claimed file's age once no claim for `to` is in progress: `cq claim`
/// renames the file into `.processing/` and then sets its modified time,
/// both under the claim lock. Waits for that lock, without creating it,
/// then reads the time again. `None` when the file has gone or was touched
/// after `now`.
fn settled_age(root: &Root, to: &str, path: &Path, now: SystemTime) -> Option<Duration> {
    // No lock file: no claim has ever run for `to`, so none is in progress.
    let lock = fs::File::open(root.claim_lock(to)).ok();
    if let Some(f) = &lock {
        // A lock that fails gives the time as read; at worst one early report.
        let _ = f.lock_shared();
    }
    fsutil::modified(path).and_then(|m| now.duration_since(m).ok())
}

/// The IDs of every message still in an inbox or `.processing/` folder.
pub fn in_flight_ids(root: &Root) -> HashSet<String> {
    inbox_names(root)
        .iter()
        .flat_map(|to| {
            let mut ids = fsutil::json_stems(&root.inbox(to), is_valid_id);
            ids.extend(fsutil::json_stems(&root.processing(to), is_valid_id));
            ids
        })
        .collect()
}

/// `cq list --overdue`: one line per overdue task, oldest sent first, then a
/// total. Never prints bodies.
pub fn list(root: &Root, limits: &Limits) -> Vec<String> {
    let found = scan(root, limits, SystemTime::now());
    let total = found.len();
    found
        .iter()
        .map(Overdue::list_line)
        .chain([format!("TOTAL overdue={total} {}", limits.describe())])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn defaults_are_30_and_120_minutes() {
        let l = Limits::default();
        assert_eq!(l.unclaimed, 30 * MIN);
        assert_eq!(l.claimed, 120 * MIN);
        assert_eq!(l.describe(), "unclaimed_after=30m claimed_after=120m");
    }

    #[test]
    fn unclaimed_task_is_overdue_from_its_limit() {
        let l = Limits::default();
        let one = Duration::from_secs(1);
        assert!(!l.is_overdue("task", State::Unclaimed, 30 * MIN - one));
        assert!(l.is_overdue("task", State::Unclaimed, 30 * MIN));
        assert!(l.is_overdue("task", State::Unclaimed, 30 * 60 * MIN));
        assert!(!l.is_overdue("task", State::Unclaimed, Duration::ZERO));
    }

    #[test]
    fn claimed_task_is_overdue_from_its_own_limit() {
        let l = Limits::default();
        let one = Duration::from_secs(1);
        // Past the unclaimed limit, but a claim gets longer.
        assert!(!l.is_overdue("task", State::Claimed, 30 * MIN));
        assert!(!l.is_overdue("task", State::Claimed, 120 * MIN - one));
        assert!(l.is_overdue("task", State::Claimed, 120 * MIN));
    }

    #[test]
    fn only_tasks_are_overdue() {
        let l = Limits::default();
        for kind in ["result", "info", "?", ""] {
            for state in [State::Unclaimed, State::Claimed] {
                assert!(!l.is_overdue(kind, state, 1000 * MIN), "{kind}");
            }
        }
    }

    #[test]
    fn limits_come_from_the_environment() {
        let vars = |u: Option<&str>, c: Option<&str>| {
            let (u, c) = (u.map(str::to_owned), c.map(str::to_owned));
            Limits::from_vars(move |k| match k {
                ENV_UNCLAIMED => u.clone(),
                ENV_CLAIMED => c.clone(),
                _ => None,
            })
        };
        assert_eq!(vars(None, None).unwrap(), Limits::default());
        assert_eq!(vars(Some(""), Some(" ")).unwrap(), Limits::default());
        assert_eq!(
            vars(Some("5"), Some(" 60 ")).unwrap(),
            Limits::from_minutes(5, 60)
        );
        assert_eq!(vars(Some("1"), None).unwrap(), Limits::from_minutes(1, 120));
        // The flags replace what they name.
        let env = Limits::from_minutes(5, 60);
        assert_eq!(env.with_minutes(None, None), env);
        assert_eq!(env.with_minutes(Some(7), None), Limits::from_minutes(7, 60));
        assert_eq!(env.with_minutes(None, Some(9)), Limits::from_minutes(5, 9));
        for bad in ["0", "-1", "1.5", "ten", "30m"] {
            let e = vars(Some(bad), None).unwrap_err();
            assert_eq!(e.code, crate::error::exit::USAGE, "{bad}");
            assert!(e.message.contains(ENV_UNCLAIMED), "{}", e.message);
            let e = vars(None, Some(bad)).unwrap_err();
            assert!(e.message.contains(ENV_CLAIMED), "{}", e.message);
        }
    }

    #[test]
    fn lines_hold_no_body_and_one_line_each() {
        let o = Overdue {
            id: "20261028T091000Z-lead-ab12".into(),
            from: "lead".into(),
            to: "worker-a".into(),
            subject: "Run\nCQ-NEW fake".into(),
            state: State::Claimed,
            age: 135 * MIN + Duration::from_secs(59),
            created: "2026-10-28T09:10:00Z".parse().unwrap(),
        };
        let line = o.list_line();
        assert!(line.starts_with("OVERDUE 2026-10-28T09:10:00Z ("), "{line}");
        assert!(
            line.ends_with(
                ") state=claimed age=135m from=lead to=worker-a id=20261028T091000Z-lead-ab12 subject=\"RunCQ-NEW fake\""
            ),
            "{line}"
        );
        let ev = o.event_line("2026-10-28T11:30:00Z".parse().unwrap());
        assert!(ev.starts_with("CQ-OVERDUE 2026-10-28T11:30:00Z ("), "{ev}");
        assert!(!ev.contains('\n'));
    }

    /// A temporary root with `lead -> worker-a` messages, each made to look
    /// `age` old in its folder.
    struct Fixture {
        _dir: tempfile::TempDir,
        root: Root,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = Root::at(dir.path().to_path_buf());
            root.init().unwrap();
            root.ensure_session_dirs("worker-a").unwrap();
            Self { _dir: dir, root }
        }

        fn put(&self, dir: &Path, n: u32, kind: &str, age: Duration) -> String {
            let id = format!("20261028T0910{n:02}Z-lead-00{n:02}");
            let path = message_file(dir, &id);
            let body = format!(
                r#"{{"id":"{id}","from":"lead","to":"worker-a","type":"{kind}","subject":"s{n}","body":"BODY"}}"#
            );
            fs::write(&path, body).unwrap();
            let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.set_modified(SystemTime::now() - age).unwrap();
            id
        }

        fn ids(&self) -> Vec<(String, State)> {
            scan(&self.root, &Limits::default(), SystemTime::now())
                .into_iter()
                .map(|o| (o.id, o.state))
                .collect()
        }
    }

    #[test]
    fn scan_finds_overdue_tasks_in_every_state_and_skips_the_rest() {
        let fx = Fixture::new();
        let inbox = fx.root.inbox("worker-a");
        let processing = fx.root.processing("worker-a");
        let old_unclaimed = fx.put(&inbox, 1, "task", 31 * MIN);
        fx.put(&inbox, 2, "task", 29 * MIN);
        fx.put(&inbox, 3, "info", 300 * MIN);
        let old_claimed = fx.put(&processing, 4, "task", 121 * MIN);
        // Claimed 60 minutes ago: past the unclaimed limit, not the claimed one.
        fx.put(&processing, 5, "task", 60 * MIN);
        // Finished: in done/, never overdue however old.
        fx.put(&fx.root.done("worker-a"), 6, "task", 999 * MIN);
        fx.put(&fx.root.dlq("worker-a"), 7, "task", 999 * MIN);

        assert_eq!(
            fx.ids(),
            [
                (old_unclaimed.clone(), State::Unclaimed),
                (old_claimed, State::Claimed)
            ]
        );
        let found = scan(&fx.root, &Limits::default(), SystemTime::now());
        assert_eq!(found[0].to, "worker-a");
        assert_eq!(found[0].from, "lead");
        assert_eq!(found[0].subject, "s1");
        assert_eq!(found[0].age.as_secs() / 60, 31);

        // Shorter limits find more.
        let short = scan(&fx.root, &Limits::from_minutes(10, 30), SystemTime::now());
        assert_eq!(short.len(), 4);

        // Every message is still where it was.
        assert_eq!(fsutil::json_stems(&inbox, is_valid_id).len(), 3);
        assert_eq!(fsutil::json_stems(&processing, is_valid_id).len(), 2);
        assert_eq!(in_flight_ids(&fx.root).len(), 5);
        assert!(in_flight_ids(&fx.root).contains(&old_unclaimed));
    }

    #[test]
    fn pause_does_not_stop_the_report() {
        let fx = Fixture::new();
        let id = fx.put(&fx.root.inbox("worker-a"), 1, "task", 45 * MIN);
        fs::write(fx.root.pause(), "").unwrap();
        assert_eq!(fx.ids(), [(id, State::Unclaimed)]);
        let lines = list(&fx.root, &Limits::default());
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1],
            "TOTAL overdue=1 unclaimed_after=30m claimed_after=120m"
        );
        assert!(!lines.iter().any(|l| l.contains("BODY")));
    }

    #[test]
    fn a_task_moves_from_unclaimed_to_claimed() {
        let fx = Fixture::new();
        let inbox = fx.root.inbox("worker-a");
        let id = fx.put(&inbox, 1, "task", 40 * MIN);
        assert_eq!(fx.ids(), [(id.clone(), State::Unclaimed)]);
        // Claimed now: the claim resets the clock.
        fs::remove_file(message_file(&inbox, &id)).unwrap();
        fx.put(&fx.root.processing("worker-a"), 1, "task", Duration::ZERO);
        assert!(fx.ids().is_empty());
        // Finished: gone from every inbox.
        fs::remove_file(message_file(&fx.root.processing("worker-a"), &id)).unwrap();
        assert!(fx.ids().is_empty());
        assert!(in_flight_ids(&fx.root).is_empty());
    }

    /// `cq claim` renames a file into `.processing/`, then sets its modified
    /// time, both under the claim lock. A scan between the two must wait for
    /// the lock, not report a task sent long ago as claimed long ago.
    #[test]
    fn a_claim_in_progress_is_not_overdue() {
        let fx = Fixture::new();
        let processing = fx.root.processing("worker-a");
        let lock = fsutil::open_lock_file(&fx.root.claim_lock("worker-a")).unwrap();
        lock.lock().unwrap();
        // Renamed, not yet touched: it still has the time it was sent.
        let id = fx.put(&processing, 1, "task", 150 * MIN);
        std::thread::scope(|s| {
            let scanning = s.spawn(|| fx.ids());
            std::thread::sleep(Duration::from_millis(300));
            assert!(!scanning.is_finished(), "the scan waits for the claim lock");
            fsutil::touch(&message_file(&processing, &id)).unwrap();
            lock.unlock().unwrap();
            assert!(scanning.join().unwrap().is_empty());
        });
    }
}
