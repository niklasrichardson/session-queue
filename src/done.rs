//! `cq done`: finish a claimed message, with a reply (`--reply`), a
//! rejection (`--reject`) or neither.
//!
//! A rejected task's sender gets a `rejected` result. A rejected info or
//! result's sender gets a short `info` notice instead, unless the message
//! was itself a rejection, so two sessions cannot reject back and forth.

use serde_json::json;

use crate::error::{CqError, Result, exit};
use crate::fsutil::{self, MoveError};
use crate::message::{
    DoneInput, EVENT_SUBJECT_MAX, MAX_BODY_BYTES, Message, MsgType, Status, event_text,
};
use crate::names::{check_id, check_name, id_parts};
use crate::root::{Root, message_file, reason_file};
use crate::send::{self, Hops, Outgoing};
use crate::timefmt;

#[derive(Debug)]
pub struct DoneRequest {
    pub name: String,
    pub id: String,
    /// Reply status, `ok` or `failed`. Defaults to `ok`. Needs `reply`.
    pub status: Option<Status>,
    /// Send `input.reply` to a task's sender. Refused for a result or info.
    pub reply: bool,
    pub reject: bool,
    /// Read only with `reply` or `reject`; empty otherwise.
    pub input: DoneInput,
}

/// What `cq done` did.
#[derive(Debug)]
pub struct DoneReport {
    pub moved_to: &'static str,
    pub reply_id: Option<String>,
    pub note: Option<String>,
}

/// Subject prefix of a `rejected` result and of a reject notice.
const REJECTED_PREFIX: &str = "Rejected: ";

pub fn done(root: &Root, req: DoneRequest) -> Result<DoneReport> {
    check_name("--as", &req.name)?;
    check_id("ID", &req.id)?;
    let DoneInput { reply, reason } = req.input;
    if req.reply && req.reject {
        return Err(CqError::usage(
            "--reply and --reject cannot be used together",
        ));
    }
    if req.reject {
        if req.status.is_some() {
            return Err(CqError::usage("--status cannot be used with --reject"));
        }
        if reply.is_some() {
            return Err(CqError::usage(
                "--reject takes a \"reason\", not a \"reply\", in the input JSON",
            ));
        }
        if reason.as_deref().is_none_or(|r| r.trim().is_empty()) {
            return Err(CqError::usage(
                "--reject needs {\"reason\": \"...\"} as JSON on stdin or in --input",
            ));
        }
    } else if req.reply {
        if reason.is_some() {
            return Err(CqError::usage(
                "\"reason\" is only for --reject; --reply takes {\"reply\": \"...\"}",
            ));
        }
        if reply.as_deref().is_none_or(|r| r.trim().is_empty()) {
            return Err(CqError::usage(
                "--reply needs {\"reply\": \"...\"} as JSON on stdin or in --input",
            ));
        }
    } else if reply.is_some() || reason.is_some() {
        return Err(CqError::usage(
            "input JSON needs --reply or --reject; without them cq done reads no input",
        ));
    }
    if req.status.is_some() && !req.reply {
        return Err(CqError::usage("--status needs --reply"));
    }
    for text in [&reply, &reason].into_iter().flatten() {
        if text.len() > MAX_BODY_BYTES {
            return Err(CqError::refused(format!(
                "reply or reason is {} bytes; the limit is {MAX_BODY_BYTES}",
                text.len()
            )));
        }
    }

    let (name, id) = (req.name.as_str(), req.id.as_str());
    root.ensure_session_dirs(name)?;
    let src = message_file(&root.processing(name), id);
    if !src.is_file() {
        return Err(CqError::new(
            exit::NOT_FOUND,
            format!("message {id} is not claimed by {name} (not in inbox/{name}/.processing/)"),
        ));
    }
    let original: Option<Message> = fsutil::read_with_retry(&src)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    // Refuse before the move, so the message stays claimed and the session
    // can run `cq done` again without --reply.
    if req.reply
        && let Some(m) = &original
        && m.kind != MsgType::Task
    {
        return Err(CqError::refused(format!(
            "--reply refused: {id} is a {}, and only task messages get replies. Nothing was moved; run cq done without --reply",
            m.kind.as_str()
        )));
    }

    let (dir, moved_to) = if req.reject {
        (root.rejected(name), "rejected")
    } else {
        (root.done(name), "done")
    };
    match fsutil::move_with_retry(&src, &message_file(&dir, id)) {
        Ok(()) => {}
        Err(MoveError::Gone) => {
            return Err(CqError::new(
                exit::NOT_FOUND,
                format!("message {id} left inbox/{name}/.processing/ before it could be moved"),
            ));
        }
        Err(MoveError::Exists) => {
            return Err(CqError::error(format!(
                "{moved_to}/{name}/{id}.json already exists"
            )));
        }
        Err(MoveError::Failed(e)) => {
            return Err(CqError::new(
                exit::RENAME_FAILED,
                format!("could not move {id} to {moved_to}/{name}/ after 3 retries: {e}"),
            ));
        }
    }

    let mut note = None;
    if req.reject {
        let text = json!({
            "id": id,
            "inbox": name,
            "reason": reason,
            "time": timefmt::rfc3339(timefmt::now()),
        });
        let mut bytes = serde_json::to_vec_pretty(&text).expect("json value serialises");
        bytes.push(b'\n');
        if let Err(e) = fsutil::write_atomic(&root.tmp(), &reason_file(&dir, id), &bytes) {
            note = Some(format!("the reason file could not be written: {e}"));
        }
    }

    let mut report = DoneReport {
        moved_to,
        reply_id: None,
        note,
    };
    let Some(original) = original else {
        if reply.is_some() || req.reject {
            report.note = Some(format!(
                "message {id} could not be read, so no result was sent"
            ));
        }
        return Ok(report);
    };

    if req.reject && original.kind != MsgType::Task {
        if is_rejection(&original) {
            return Ok(report);
        }
        let notice = Outgoing {
            from: name.to_owned(),
            to: original.from.clone(),
            kind: MsgType::Info,
            subject: format!("{REJECTED_PREFIX}{}", original.subject),
            body: notice_body(name, id, &original, reason.as_deref().unwrap_or_default()),
            reply_to: Some(id.to_owned()),
            parent: None,
            status: None,
        };
        // The reject stands even when the notice cannot be sent.
        match send::send(root, notice, Hops::After(original.hops)) {
            Ok(nid) => report.reply_id = Some(nid),
            Err(e) => add_note(
                &mut report.note,
                format!(
                    "message {id} moved to rejected/{name}/, but {} was not told: {e}",
                    original.from
                ),
            ),
        }
        return Ok(report);
    }

    let outgoing = match original.kind {
        MsgType::Task if req.reject => Some(Outgoing {
            from: name.to_owned(),
            to: original.from.clone(),
            kind: MsgType::Result,
            subject: format!("{REJECTED_PREFIX}{}", original.subject),
            body: reason.unwrap_or_default(),
            reply_to: Some(id.to_owned()),
            parent: None,
            status: Some(Status::Rejected),
        }),
        MsgType::Task => reply.map(|body| Outgoing {
            from: name.to_owned(),
            to: original.from.clone(),
            kind: MsgType::Result,
            subject: format!("Re: {}", original.subject),
            body,
            reply_to: Some(id.to_owned()),
            parent: None,
            status: Some(req.status.unwrap_or(Status::Ok)),
        }),
        MsgType::Result | MsgType::Info => None,
    };
    if let Some(out) = outgoing {
        match send::send(root, out, Hops::After(original.hops)) {
            Ok(rid) => report.reply_id = Some(rid),
            Err(e) => {
                return Err(CqError::new(
                    exit::REPLY_NOT_SENT,
                    format!(
                        "message {id} moved to {moved_to}/{name}/, but the reply was not sent: {e}"
                    ),
                ));
            }
        }
    }
    Ok(report)
}

/// True for a message that reports a rejection: a `rejected` result, or a
/// reject notice (an info with the `Rejected: ` subject whose `reply_to` is
/// a message its recipient sent). Rejecting one sends nothing back.
fn is_rejection(m: &Message) -> bool {
    match m.kind {
        MsgType::Result => m.status == Some(Status::Rejected),
        MsgType::Info => {
            m.subject.starts_with(REJECTED_PREFIX)
                && m.reply_to
                    .as_deref()
                    .and_then(id_parts)
                    .is_some_and(|(_, from)| from == m.to)
        }
        MsgType::Task => false,
    }
}

/// The body of a reject notice: who rejected which message, and why. Cut
/// to `MAX_BODY_BYTES`, as the reason alone may use the whole limit.
fn notice_body(name: &str, id: &str, original: &Message, reason: &str) -> String {
    let mut body = format!(
        "{name} rejected your {} {id} (subject {}).\nReason: {reason}",
        original.kind.as_str(),
        event_text(&original.subject, EVENT_SUBJECT_MAX)
    );
    if body.len() > MAX_BODY_BYTES {
        let mut end = MAX_BODY_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    body
}

/// Add `text` to the note, after any note already there.
fn add_note(note: &mut Option<String>, text: String) {
    *note = Some(match note.take() {
        Some(first) => format!("{first}; {text}"),
        None => text,
    });
}
