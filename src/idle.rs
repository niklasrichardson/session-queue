//! `cq idle-alert`: tell a lead when a worker session goes idle (issue #50).
//!
//! The Claude Code `Notification` hook (matcher `idle_prompt`) runs
//! `cq idle-alert` when a session waits for input. The owner lists the
//! workers to watch in `idle-alerts`, one `worker lead` pair per line. For a
//! listed worker that is awake, `cq` sends the lead one `info` message with
//! a fixed subject and body, at most once per interval, and records the
//! time in `idle/<worker>`. The message holds no text from the session's
//! work. Every limit of `cq send` applies; a refused send records nothing.

use std::fs;
use std::time::Duration;

use jiff::Timestamp;

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

/// What `alert` did.
#[derive(Debug)]
pub enum Outcome {
    /// The name is not listed in `idle-alerts`.
    NotListed,
    Asleep,
    /// The last alert was less than the interval ago (UTC, RFC 3339).
    Recent(String),
    /// Sent to the lead: the message ID.
    Sent {
        lead: String,
        id: String,
    },
    /// `cq send` refused or failed; nothing was recorded.
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
/// within `every`.
pub fn alert(root: &Root, name: &str, every: Duration) -> Outcome {
    let Some(lead) = lead_for(root, name) else {
        return Outcome::NotListed;
    };
    if sleep::is_asleep(root, name) {
        return Outcome::Asleep;
    }
    let now = timefmt::now();
    if let Some(last) = last_alert(root, name).filter(|t| is_recent(*t, now, every)) {
        return Outcome::Recent(rfc3339(last));
    }
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

/// Write the alert time to `idle/<name>` through `tmp/`.
fn record(root: &Root, name: &str, now: Timestamp) -> Result<()> {
    let dir = root.idle_dir();
    fs::create_dir_all(&dir).map_err(|e| CqError::io(format!("create {}", dir.display()), e))?;
    let path = root.idle_file(name);
    let bytes = format!("{}\n", rfc3339(now));
    fsutil::write_atomic(&root.tmp(), &path, bytes.as_bytes())
        .map_err(|e| CqError::io(format!("write {}", path.display()), e))
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
}
