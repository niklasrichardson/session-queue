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
//! watcher for the name then finds the old pid file, which a clean exit
//! removes, and writes the line for it: after `--takeover` it says so, else
//! the old watcher ended without logging. Holding the lock proves that the
//! old process is gone, so no process check is needed.
//!
//! The file only grows by appends. When it is over `MAX_BYTES`, it is
//! renamed to `watch-exits.log.old`, replacing the last one, before the next
//! line. A failed write goes to stderr; the watcher's exit does not change.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::members::Mode;
use crate::message::event_text;
use crate::root::Root;
use crate::timefmt::{self, rfc3339};

/// The log is renamed to `.old` once it is larger than this.
pub const MAX_BYTES: u64 = 1024 * 1024;
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

fn write_to(path: &Path, exit: &Exit) {
    if let Err(e) = append(path, &exit.line()) {
        let _ = writeln!(
            io::stderr(),
            "cq watch: could not write {}: {e}",
            path.display()
        );
    }
}

/// Append `line` and a newline, after renaming a log over `MAX_BYTES` to
/// `<path>.old`. One `write_all` per line, so lines from two watchers do not
/// mix.
pub fn append(path: &Path, line: &str) -> io::Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        // Another watcher may have rotated it first; the append then goes
        // to the new file either way.
        let _ = fs::rename(path, old_path(path));
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(format!("{line}\n").as_bytes())
}

/// `<path>.old`
pub fn old_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// The watcher this process runs, while it holds the lock. The console
/// handler reads it from another thread (see `signalled`).
struct Running {
    path: PathBuf,
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

fn log_once(code: Option<i32>, signal: Option<&str>, reason: &str) {
    let Some(r) = running().take() else {
        return;
    };
    write_to(
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
}
