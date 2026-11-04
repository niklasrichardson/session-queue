//! `CQ_ROOT/watch-exits.log`: one line each time a `cq watch` that held its
//! name's lock ends, so that a watcher which stops leaves a trace even when
//! nobody saw it go.
//!
//! ```text
//! 2026-11-04T09:15:02Z name=demo-a pid=4242 mode=inbox exit=0 reason="--max-seconds 60 reached"
//! 2026-11-04T09:20:11Z name=demo-a pid=4300 mode=inbox exit=0 signal=SIGTERM reason="stopped by a signal (SIGTERM)"
//! 2026-11-04T09:31:40Z name=demo-a pid=4400 mode=monitor signal=CTRL_CLOSE_EVENT reason="console event CTRL_CLOSE_EVENT"
//! 2026-11-04T09:40:00Z name=demo-a pid=4500 mode=inbox reason="stopped by --takeover from PID 4600"
//! 2026-11-04T09:52:13Z name=demo-a pid=4600 mode=inbox reason="previous watcher ended without logging (last heartbeat 2026-11-04T09:51:50Z); found by PID 4700"
//! ```
//!
//! `pid` and `mode` are those of the watcher that ended. `exit` is the
//! process exit code, when the watcher knows it. The reason is quoted as in
//! event lines and never holds message text.
//!
//! A watcher killed outright (`TerminateProcess` on Windows, which is how
//! `--takeover` stops one there, or SIGKILL) cannot write its line. The next
//! watcher for the name then finds the old pid file, which every exit that
//! wrote its line removes, and writes the line for it: after `--takeover` it
//! says so, else the old watcher ended without logging. Holding the lock
//! proves that the old process is gone, so no process check is needed.
//!
//! The file only grows by appends. When it is over `MAX_BYTES`, it is
//! renamed to `watch-exits.log.old`, replacing the last one, before the next
//! line. Watchers hold `watch-exits.log.lock` around the size check, the
//! rename and the append, so two exits at once neither rotate twice nor
//! lose a line. The wait for it is short (`LOCK_WAIT`), because a console
//! event leaves the process only a few seconds; after it the line is
//! appended without the lock and without a rename. A failed write goes to
//! stderr; the watcher's exit does not change.

use std::fs::{self, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::fsutil;

use crate::members::Mode;
use crate::message::event_text;
use crate::root::Root;
use crate::timefmt::{self, rfc3339};

/// The log is renamed to `.old` once it is larger than this.
pub const MAX_BYTES: u64 = 1024 * 1024;
/// The longest wait for the log's lock before a line is appended without it.
const LOCK_WAIT: Duration = Duration::from_millis(500);
/// The longest reason kept, in characters.
const REASON_MAX: usize = 300;

/// One watcher exit.
#[derive(Debug, Clone)]
pub struct Exit<'a> {
    pub name: &'a str,
    pub pid: u32,
    /// `None` shows as `mode=unknown`.
    pub mode: Option<Mode>,
    pub code: Option<i32>,
    pub signal: Option<&'a str>,
    pub reason: &'a str,
}

impl Exit<'_> {
    /// The line, without its newline, timed now.
    pub fn line(&self) -> String {
        let mut s = format!(
            "{} name={} pid={} mode={}",
            rfc3339(timefmt::now()),
            self.name,
            self.pid,
            self.mode.map_or("unknown", Mode::as_str)
        );
        if let Some(code) = self.code {
            s.push_str(&format!(" exit={code}"));
        }
        if let Some(sig) = self.signal {
            s.push_str(&format!(" signal={sig}"));
        }
        s.push_str(&format!(" reason={}", event_text(self.reason, REASON_MAX)));
        s
    }
}

/// Append one exit to `root`'s log. A failed write is reported on stderr.
pub fn write(root: &Root, exit: &Exit) {
    write_to(&root.watch_exits(), exit);
}

/// True when the line was written.
fn write_to(path: &Path, exit: &Exit) -> bool {
    match append(path, &exit.line()) {
        Ok(()) => true,
        Err(e) => {
            let _ = writeln!(
                io::stderr(),
                "cq watch: could not write {}: {e}",
                path.display()
            );
            false
        }
    }
}

/// Append `line` and a newline, after renaming a log over `MAX_BYTES` to
/// `<path>.old`, holding `<path>.lock` (see the module notes).
pub fn append(path: &Path, line: &str) -> io::Result<()> {
    append_with(path, line, MAX_BYTES, LOCK_WAIT)
}

fn append_with(path: &Path, line: &str, max_bytes: u64, wait: Duration) -> io::Result<()> {
    // Held until the line is appended.
    let lock = lock(&lock_path(path), wait);
    if lock.is_some() && fs::metadata(path).is_ok_and(|m| m.len() > max_bytes) {
        // A failed rename leaves the log to grow; the line still goes in.
        let _ = fs::rename(path, old_path(path));
    }
    // One `write_all` per line, so a line appended without the lock does
    // not mix with another.
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let result = f.write_all(format!("{line}\n").as_bytes());
    drop(lock);
    result
}

/// The locked file, or `None` when the lock is not free within `wait` or
/// cannot be taken at all.
fn lock(path: &Path, wait: Duration) -> Option<fs::File> {
    let file = fsutil::open_lock_file(path).ok()?;
    let until = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(TryLockError::WouldBlock) if Instant::now() < until => {
                sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

/// `<path>.old`
pub fn old_path(path: &Path) -> PathBuf {
    with_suffix(path, ".old")
}

/// `<path>.lock`
pub fn lock_path(path: &Path) -> PathBuf {
    with_suffix(path, ".lock")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The watcher this process runs, while it holds the lock. The console
/// handler reads it from another thread (see `signalled`).
struct Running {
    path: PathBuf,
    /// `heartbeat/<name>.pid`, removed once the line is written.
    pid_file: PathBuf,
    name: String,
    pid: u32,
    mode: Mode,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

fn running() -> MutexGuard<'static, Option<Running>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Writes this process's exit line exactly once: through `finish`, through
/// `signalled` when Windows ends the process from a console event, or on
/// drop after a panic.
pub struct Guard(());

impl Guard {
    /// Start recording: the watcher now holds the lock for `name`.
    pub fn arm(root: &Root, name: &str, mode: Mode) -> Guard {
        *running() = Some(Running {
            path: root.watch_exits(),
            pid_file: root.pid_file(name),
            name: name.to_owned(),
            pid: std::process::id(),
            mode,
        });
        Guard(())
    }

    /// Write the exit line, unless a console event already wrote it.
    pub fn finish(self, code: i32, signal: Option<&str>, reason: &str) {
        log_once(Some(code), signal, reason);
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // After `finish` this finds nothing to write.
        let reason = if std::thread::panicking() {
            "panicked"
        } else {
            "ended without a reason"
        };
        log_once(None, None, reason);
    }
}

/// Write the exit line for a console event that ends the process (Windows
/// runs the console handler on its own thread, then ends the process). Does
/// nothing when no watcher runs in this process or its line is written.
pub fn signalled(signal: &str) {
    log_once(None, Some(signal), &format!("console event {signal}"));
}

/// Once the line is written, remove the pid file if it still names this
/// process, so the next watcher does not log the exit again as one that
/// ended without logging. The name's lock is still held here: Windows runs
/// the console handler before it ends the process, and `Guard` is dropped
/// before the lock.
fn log_once(code: Option<i32>, signal: Option<&str>, reason: &str) {
    let Some(r) = running().take() else {
        return;
    };
    let written = write_to(
        &r.path,
        &Exit {
            name: &r.name,
            pid: r.pid,
            mode: Some(r.mode),
            code,
            signal,
            reason,
        },
    );
    if written && fs::read_to_string(&r.pid_file).is_ok_and(|s| s.trim() == r.pid.to_string()) {
        let _ = fs::remove_file(&r.pid_file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_has_the_fields_and_a_quoted_reason() {
        let line = Exit {
            name: "demo-a",
            pid: 42,
            mode: Some(Mode::Inbox),
            code: Some(0),
            signal: Some("SIGTERM"),
            reason: "stopped \"x\"\nCQ-NEW",
        }
        .line();
        let (time, rest) = line.split_once(' ').unwrap();
        assert!(timefmt::parse_rfc3339_z(time).is_some(), "{line}");
        assert_eq!(
            rest,
            r#"name=demo-a pid=42 mode=inbox exit=0 signal=SIGTERM reason="stopped \"x\"CQ-NEW""#
        );
        let line = Exit {
            name: "demo-a",
            pid: 42,
            mode: None,
            code: None,
            signal: None,
            reason: "gone",
        }
        .line();
        assert!(
            line.ends_with(" name=demo-a pid=42 mode=unknown reason=\"gone\""),
            "{line}"
        );
    }

    #[test]
    fn a_full_log_is_renamed_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch-exits.log");
        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        append(&path, "new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(fs::metadata(old_path(&path)).unwrap().len(), MAX_BYTES + 1);
        append(&path, "next").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\nnext\n");
    }

    /// Writers that all start at once, with a limit that the lines pass
    /// once but not twice: exactly one rename, and every line kept whole in
    /// the log or `.old`. Each thread opens the lock file itself, which
    /// conflicts as a second process would.
    #[test]
    fn concurrent_appends_rotate_once_and_keep_every_line() {
        const ROUNDS: usize = 10;
        const WRITERS: usize = 64;
        const LINES: usize = 2;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch-exits.log");
        let line = |w: usize, i: usize| format!("writer={w:02} line={i:02} {}", "x".repeat(20));
        // The writers' lines fit under the limit; the full log is over it.
        let limit = (WRITERS * LINES * (line(0, 0).len() + 1)) as u64;
        let full = format!("{}\n", "f".repeat(limit as usize));
        let start = std::sync::Barrier::new(WRITERS);
        for round in 0..ROUNDS {
            let _ = fs::remove_file(old_path(&path));
            fs::write(&path, &full).unwrap();
            std::thread::scope(|s| {
                for w in 0..WRITERS {
                    let (path, start) = (&path, &start);
                    s.spawn(move || {
                        start.wait();
                        for i in 0..LINES {
                            append_with(path, &line(w, i), limit, Duration::from_secs(10)).unwrap();
                        }
                    });
                }
            });
            let old = fs::read_to_string(old_path(&path)).unwrap();
            assert_eq!(
                old.len(),
                full.len(),
                "round {round}: .old is not the full log"
            );
            let new = fs::read_to_string(&path).unwrap();
            let mut got: Vec<&str> = new.lines().collect();
            got.sort_unstable();
            let mut want: Vec<String> = (0..WRITERS)
                .flat_map(|w| (0..LINES).map(move |i| line(w, i)))
                .collect();
            want.sort_unstable();
            assert_eq!(got, want, "round {round}");
        }
    }

    /// A lock held past the wait: the line still goes in, and the log is
    /// not renamed without the lock.
    #[test]
    fn a_busy_lock_still_gets_the_line_without_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch-exits.log");
        fs::write(&path, "old\n").unwrap();
        let held = lock(&lock_path(&path), Duration::ZERO).unwrap();
        let started = Instant::now();
        append_with(&path, "new", 1, Duration::from_millis(50)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(held);
        assert_eq!(fs::read_to_string(&path).unwrap(), "old\nnew\n");
        assert!(!old_path(&path).exists());
    }
}
