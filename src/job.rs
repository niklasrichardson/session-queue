//! A claimed job's record, shared by `cq dispatch` and
//! `cq watch --to-agy --agy-broker`: reserve the record folder before the
//! claim, claim, and build the `done` request for the finish chosen.
//!
//! The record folder is `CQ_ROOT/dispatch/NAME/ID/`. It is created before
//! the claim, so a claimed message always has a record; a folder that
//! already exists is never reused.
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::done::DoneRequest;
use crate::error::{CqError, Result, exit};
use crate::message::{DoneInput, MAX_BODY_BYTES, Message, Status};
use crate::{Root, claim};

/// How a claimed message is finished.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum Finish {
    /// `cq done --reply --status STATUS` with `text`. Only for a task.
    Reply { status: Status, text: String },
    /// `cq done --reject` with `text` as the reason.
    Reject { text: String },
    /// Plain `cq done`.
    Done,
}

/// The folder that holds the records of `name`'s jobs.
pub fn records(root: &Root, name: &str) -> PathBuf {
    root.path().join("dispatch").join(name)
}

/// Reserve `jobs/ID/`, then claim `id`. `None` when the message is gone or
/// invalid (claim exit 1 or 3): the empty folder is removed again. `what`
/// names the worker in errors ("dispatch", "agy").
pub fn reserve_and_claim(
    root: &Root,
    name: &str,
    jobs: &Path,
    id: &str,
    what: &str,
) -> Result<Option<(PathBuf, Message)>> {
    let job = jobs.join(id);
    fs::create_dir(&job).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            CqError::refused(format!("{what} job {} already exists; message {id} was not claimed; inspect the existing record", job.display()))
        } else {
            CqError::io(format!("reserve {what} job; message was not claimed"), e)
        }
    })?;
    let bytes = match claim::claim(root, name, id) {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = fs::remove_dir(&job); // Only the empty directory reserved above.
            if e.code == exit::GONE || e.code == exit::INVALID {
                return Ok(None);
            }
            return Err(e);
        }
    };
    let message: Message = serde_json::from_slice(&bytes)
        .map_err(|e| CqError::error(format!("read claimed message: {e}")))?;
    Ok(Some((job, message)))
}

/// The `done` request that finishes `id` as `finish` says.
pub fn done_request(name: &str, id: &str, finish: &Finish) -> DoneRequest {
    let (status, reply, reject, input) = match finish {
        Finish::Reply { status, text } => (
            Some(*status),
            true,
            false,
            DoneInput {
                reply: Some(text.clone()),
                reason: None,
            },
        ),
        Finish::Reject { text } => (
            None,
            false,
            true,
            DoneInput {
                reply: None,
                reason: Some(text.clone()),
            },
        ),
        Finish::Done => (None, false, false, DoneInput::default()),
    };
    DoneRequest {
        name: name.to_owned(),
        id: id.to_owned(),
        status,
        reply,
        reject,
        input,
    }
}

/// `reply` cut to fit a message body, with a note that it was cut.
pub fn fit_reply(reply: impl Into<String>) -> String {
    let mut reply = reply.into();
    if reply.len() > MAX_BODY_BYTES {
        let mut end = MAX_BODY_BYTES - 32;
        while !reply.is_char_boundary(end) {
            end -= 1;
        }
        reply.truncate(end);
        reply.push_str("\n[truncated; see dispatch logs]");
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_finish_gives_its_done_request() {
        let r = done_request(
            "w",
            "id",
            &Finish::Reply {
                status: Status::Failed,
                text: "t".into(),
            },
        );
        assert!(r.reply && !r.reject);
        assert_eq!(r.status, Some(Status::Failed));
        assert_eq!(r.input.reply.as_deref(), Some("t"));
        let r = done_request("w", "id", &Finish::Reject { text: "no".into() });
        assert!(r.reject && !r.reply && r.status.is_none());
        assert_eq!(r.input.reason.as_deref(), Some("no"));
        let r = done_request("w", "id", &Finish::Done);
        assert!(!r.reply && !r.reject && r.input.reply.is_none() && r.input.reason.is_none());
    }

    #[test]
    fn a_long_reply_is_cut_at_a_char_boundary() {
        let long = "é".repeat(MAX_BODY_BYTES);
        let cut = fit_reply(long);
        assert!(cut.len() <= MAX_BODY_BYTES, "{}", cut.len());
        assert!(cut.ends_with("[truncated; see dispatch logs]"));
        assert_eq!(fit_reply("short"), "short");
    }

    #[test]
    fn finish_round_trips_as_json() {
        for f in [
            Finish::Reply {
                status: Status::Ok,
                text: "x".into(),
            },
            Finish::Reject { text: "y".into() },
            Finish::Done,
        ] {
            let s = serde_json::to_string(&f).unwrap();
            assert_eq!(serde_json::from_str::<Finish>(&s).unwrap(), f);
        }
        assert!(serde_json::from_str::<Finish>(r#"{"action":"reject","text":"y","x":1}"#).is_err());
    }
}
