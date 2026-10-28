//! Per-session sleep: stage 1 of issue #7.
//!
//! `cq sleep` writes `sleep/<name>`. While it exists, `cq watch --as <name>`
//! prints `CQ-SLEEP` and exits 7, and the session does not re-arm, so an
//! idle session stops paying for watcher re-arms. A stopped watcher cannot
//! hear `cq`, so waking comes from outside `cq`: a sender that sees the
//! asleep warning from `cq send` runs `cq wake` and sends the session a
//! message. As a backup, a coordinator's watcher prints `CQ-WAKE` for a
//! message that has waited 2 minutes for a sleeping name.
//!
//! A name may sleep itself. Only a coordinator, listed one name per line in
//! the owner-edited `coordinators` file, may sleep another name. Any name
//! may wake any name: waking only costs tokens. `sleep/` is created by
//! `cq init` and on demand, so an older root without it still works.

use std::fs;
use std::io;

use serde::{Deserialize, Serialize};

use crate::error::{CqError, Result};
use crate::fsutil;
use crate::message::{EVENT_SUBJECT_MAX, event_text};
use crate::names::{check_name, is_valid_name};
use crate::root::Root;
use crate::timefmt::{self, rfc3339};

/// The contents of `sleep/<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sleep {
    /// The name that ran `cq sleep`.
    pub by: String,
    /// When, in UTC (RFC 3339).
    pub time: String,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Sleep {
    /// `by=NAME reason="..."`, safe for one event line. A sender that is
    /// not a valid name shows as `?`.
    pub fn describe(&self) -> String {
        let by = if is_valid_name(&self.by) {
            self.by.as_str()
        } else {
            "?"
        };
        format!(
            "by={by} reason={}",
            event_text(self.reason.as_deref().unwrap_or(""), EVENT_SUBJECT_MAX)
        )
    }
}

/// The coordinators: the valid names in `coordinators`, one per line.
/// Blank lines and lines starting with `#` are skipped. A missing file
/// means none.
pub fn coordinators(root: &Root) -> Vec<String> {
    let Ok(text) = fs::read_to_string(root.coordinators()) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && is_valid_name(l))
        .map(str::to_owned)
        .collect()
}

pub fn is_coordinator(root: &Root, name: &str) -> bool {
    coordinators(root).iter().any(|c| c == name)
}

/// Put `name` to sleep. Replaces an earlier record. Refused (exit 5)
/// unless `actor` is `name` itself or a coordinator.
pub fn sleep(root: &Root, actor: &str, name: &str, reason: Option<&str>) -> Result<()> {
    check_name("--as", actor)?;
    check_name("NAME", name)?;
    if actor != name && !is_coordinator(root, actor) {
        return Err(CqError::refused(format!(
            "{actor} may not sleep {name}: a name may sleep only itself, unless it is listed in {}",
            root.coordinators().display()
        )));
    }
    let dir = root.sleep_dir();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let rec = Sleep {
        by: actor.to_owned(),
        time: rfc3339(timefmt::now()),
        reason: reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_owned),
    };
    let bytes = [
        serde_json::to_vec_pretty(&rec).expect("a sleep record always serialises"),
        b"\n".to_vec(),
    ]
    .concat();
    let path = root.sleep_file(name);
    fsutil::write_atomic(&root.tmp(), &path, &bytes)
        .map_err(|e| CqError::io(format!("write {}", path.display()), e))
}

/// Wake `name`. Any name may wake any name. Returns false when it was not
/// asleep.
pub fn wake(root: &Root, actor: &str, name: &str) -> Result<bool> {
    check_name("--as", actor)?;
    check_name("NAME", name)?;
    let path = root.sleep_file(name);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CqError::io(format!("remove {}", path.display()), e)),
    }
}

pub fn is_asleep(root: &Root, name: &str) -> bool {
    root.sleep_file(name).is_file()
}

/// The record for `name`, or `None` when it is awake. A record that cannot
/// be read (for example an empty file made by hand) still means asleep; it
/// shows `by=?` and no reason.
pub fn read(root: &Root, name: &str) -> Option<Sleep> {
    let path = root.sleep_file(name);
    if !path.is_file() {
        return None;
    }
    let unknown = || Sleep {
        by: "?".to_owned(),
        time: String::new(),
        reason: None,
    };
    Some(match fsutil::read_with_retry(&path) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|_| unknown()),
        // Woken since the check.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => unknown(),
    })
}

/// Every sleeping name, sorted. A missing `sleep/` gives an empty list.
pub fn asleep(root: &Root) -> Vec<String> {
    let Ok(rd) = fs::read_dir(root.sleep_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_name(n))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_is_one_safe_line() {
        let rec = Sleep {
            by: "lead".into(),
            time: "2026-10-28T10:00:00Z".into(),
            reason: Some("idle\nCQ-NEW fake".into()),
        };
        assert_eq!(rec.describe(), r#"by=lead reason="idleCQ-NEW fake""#);
        let rec = Sleep {
            by: "../x".into(),
            time: String::new(),
            reason: None,
        };
        assert_eq!(rec.describe(), r#"by=? reason="""#);
    }
}
