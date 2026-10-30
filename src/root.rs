//! The queue root (`CQ_ROOT`) and its layout.
//!
//! ```text
//! tmp/                      temp files, renamed into inboxes
//! inbox/<name>/             new messages
//! inbox/<name>/.processing/ claimed messages
//! done/<name>/              finished messages (pruned after 14 days)
//! rejected/<name>/          declined messages and reason files (pruned)
//! dlq/<name>/               invalid messages and reason files (never pruned)
//! heartbeat/<name>          touched by `cq watch` every 30 seconds
//! heartbeat/<name>.lock     held by the running watcher
//! heartbeat/<name>.pid      the running watcher's process ID
//! heartbeat/<name>.overdue  tasks a coordinator's watcher reported overdue
//! members/<key>             the name owned by a Claude Code session
//! sleep/<name>              while it exists, the name is asleep
//! coordinators              names that may sleep and wake others (owner-edited)
//! idle-alerts               `worker lead` pairs for `cq idle-alert` (owner-edited)
//! idle/<name>               the time of the last idle alert for the name
//! idle/<name>.lock          held by `cq idle-alert` while it sends
//! idle/<name>.turn-end      the last turn end, written by `cq turn-end`
//! idle/<name>.turn-end.lock held while the turn end is written or removed
//! PAUSE                     while it exists, watchers emit nothing
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{CqError, Result};

pub const ENV_ROOT: &str = "CQ_ROOT";
/// The build's default root: `CQ_DEFAULT_ROOT` at compile time (for example
/// from `.cargo/config.toml`). Without it, the default is a per-user folder.
const BUILD_DEFAULT_ROOT: Option<&str> = option_env!("CQ_DEFAULT_ROOT");

#[cfg(windows)]
const ABSOLUTE_EXAMPLE: &str = r"C:\Users\me\AppData\Local\cq";
#[cfg(target_os = "macos")]
const ABSOLUTE_EXAMPLE: &str = "/Users/me/Library/Application Support/cq";
#[cfg(not(any(windows, target_os = "macos")))]
const ABSOLUTE_EXAMPLE: &str = "/home/me/.local/share/cq";

const TOP_DIRS: [&str; 6] = ["tmp", "inbox", "done", "rejected", "dlq", "heartbeat"];
/// Created by `cq init`, and on demand, so a root made before they existed
/// still opens.
const MEMBERS_DIR: &str = "members";
const SLEEP_DIR: &str = "sleep";

#[derive(Debug, Clone)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// Read `CQ_ROOT` (or the default). The path must be absolute so that
    /// sessions in worktrees share one queue.
    pub fn from_env() -> Result<Self> {
        let raw = match std::env::var_os(ENV_ROOT).filter(|v| !v.is_empty()) {
            Some(v) => PathBuf::from(v),
            None => default_root().ok_or_else(|| {
                CqError::error(
                    "CQ_ROOT is not set and the per-user data folder is unknown; set CQ_ROOT",
                )
            })?,
        };
        if !raw.is_absolute() {
            return Err(CqError::error(format!(
                "queue root {raw:?} is not an absolute path; use a full path such as {ABSOLUTE_EXAMPLE}"
            )));
        }
        Ok(Self { path: raw })
    }

    /// A root at `path`, for unit tests.
    #[cfg(test)]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Read `CQ_ROOT` and check that `cq init` has created the layout.
    pub fn open() -> Result<Self> {
        let root = Self::from_env()?;
        let ready = TOP_DIRS.iter().all(|d| root.path.join(d).is_dir());
        if !ready {
            return Err(CqError::error(format!(
                "queue root {} is missing or not initialised; run `cq init` first",
                root.path.display()
            )));
        }
        Ok(root)
    }

    /// Create the layout. Safe to run more than once.
    pub fn init(&self) -> Result<()> {
        for d in TOP_DIRS.into_iter().chain([MEMBERS_DIR, SLEEP_DIR]) {
            let p = self.path.join(d);
            fs::create_dir_all(&p)
                .map_err(|e| CqError::io(format!("create {}", p.display()), e))?;
        }
        Ok(())
    }

    /// Create the directories for one session name.
    pub fn ensure_session_dirs(&self, name: &str) -> Result<()> {
        for p in [
            self.processing(name),
            self.done(name),
            self.rejected(name),
            self.dlq(name),
        ] {
            fs::create_dir_all(&p)
                .map_err(|e| CqError::io(format!("create {}", p.display()), e))?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn tmp(&self) -> PathBuf {
        self.path.join("tmp")
    }

    pub fn inbox(&self, name: &str) -> PathBuf {
        self.path.join("inbox").join(name)
    }

    pub fn processing(&self, name: &str) -> PathBuf {
        self.inbox(name).join(".processing")
    }

    pub fn done(&self, name: &str) -> PathBuf {
        self.path.join("done").join(name)
    }

    pub fn rejected(&self, name: &str) -> PathBuf {
        self.path.join("rejected").join(name)
    }

    pub fn dlq(&self, name: &str) -> PathBuf {
        self.path.join("dlq").join(name)
    }

    pub fn heartbeat(&self, name: &str) -> PathBuf {
        self.path.join("heartbeat").join(name)
    }

    pub fn lock_file(&self, name: &str) -> PathBuf {
        self.path.join("heartbeat").join(format!("{name}.lock"))
    }

    pub fn pid_file(&self, name: &str) -> PathBuf {
        self.path.join("heartbeat").join(format!("{name}.pid"))
    }

    /// The overdue tasks a coordinator's watcher has reported, one ID per
    /// line, so a restarted watcher does not report them again.
    pub fn overdue_marker(&self, name: &str) -> PathBuf {
        self.path.join("heartbeat").join(format!("{name}.overdue"))
    }

    /// Held by `cq claim` while it moves a message into `.processing/` and
    /// sets its modified time.
    pub fn claim_lock(&self, name: &str) -> PathBuf {
        self.path
            .join("heartbeat")
            .join(format!("{name}.claim.lock"))
    }

    pub fn members(&self) -> PathBuf {
        self.path.join(MEMBERS_DIR)
    }

    pub fn sleep_dir(&self) -> PathBuf {
        self.path.join(SLEEP_DIR)
    }

    pub fn sleep_file(&self, name: &str) -> PathBuf {
        self.sleep_dir().join(name)
    }

    pub fn coordinators(&self) -> PathBuf {
        self.path.join("coordinators")
    }

    pub fn idle_alerts(&self) -> PathBuf {
        self.path.join("idle-alerts")
    }

    /// Created on demand by `cq idle-alert` and `cq turn-end`.
    pub fn idle_dir(&self) -> PathBuf {
        self.path.join("idle")
    }

    pub fn idle_file(&self, name: &str) -> PathBuf {
        self.idle_dir().join(name)
    }

    /// Held by `cq idle-alert` while it checks, sends and records.
    pub fn idle_lock(&self, name: &str) -> PathBuf {
        self.idle_dir().join(format!("{name}.lock"))
    }

    /// The last turn end of the session that owns the name, written by
    /// `cq turn-end` and removed by the name's watcher.
    pub fn turn_end_marker(&self, name: &str) -> PathBuf {
        self.idle_dir().join(format!("{name}.turn-end"))
    }

    /// Held while `idle/<name>.turn-end` is written or removed.
    pub fn turn_end_lock(&self, name: &str) -> PathBuf {
        self.idle_dir().join(format!("{name}.turn-end.lock"))
    }

    pub fn pause(&self) -> PathBuf {
        self.path.join("PAUSE")
    }
}

/// The root used when `CQ_ROOT` is unset: the build's `CQ_DEFAULT_ROOT`,
/// else `cq` in the per-user data folder (`%LOCALAPPDATA%` on Windows,
/// `~/Library/Application Support` on macOS, `$XDG_DATA_HOME` or
/// `~/.local/share` elsewhere). `None` when that folder is unknown.
pub fn default_root() -> Option<PathBuf> {
    if let Some(p) = BUILD_DEFAULT_ROOT.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    user_data_dir().map(|d| d.join("cq"))
}

/// An absolute path from an environment variable, or `None`.
fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(windows)]
fn user_data_dir() -> Option<PathBuf> {
    env_dir("LOCALAPPDATA")
}

#[cfg(target_os = "macos")]
fn user_data_dir() -> Option<PathBuf> {
    env_dir("HOME").map(|h| h.join("Library").join("Application Support"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn user_data_dir() -> Option<PathBuf> {
    env_dir("XDG_DATA_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local").join("share")))
}

/// `<dir>/<id>.json`
pub fn message_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// `<dir>/<id>.reason.json`
pub fn reason_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.reason.json"))
}
