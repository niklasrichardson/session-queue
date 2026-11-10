//! `cq send`: write one message into a recipient's inbox.

use std::collections::BTreeSet;
use std::fs;

use serde_json::Value;

use crate::error::{CqError, Result, exit};
use crate::fsutil::{self, MoveError};
use crate::message::{
    MAX_BODY_BYTES, MAX_HOPS, Message, MsgType, RATE_LIMIT, RATE_WINDOW, RECIPIENT_LIMIT,
    SCHEMA_VERSION, SENDER_LIMIT, Status,
};
use crate::names::{check_id, check_name, id_parts, is_valid_name};
use crate::root::{Root, message_file};
use crate::timefmt;

/// Where `hops` comes from.
#[derive(Debug, Clone, Copy)]
pub enum Hops {
    /// Look up `--parent` and `--reply-to` in the sender's `.processing/`
    /// and `done/` (the `cq send` command).
    Lookup,
    /// The caller already knows the hops of the message it answers
    /// (`cq done` and `cq claim`); the new message gets one more.
    After(u32),
}

#[derive(Debug, Clone)]
pub struct Outgoing {
    pub from: String,
    pub to: String,
    pub kind: MsgType,
    pub subject: String,
    pub body: String,
    pub reply_to: Option<String>,
    pub parent: Option<String>,
    pub status: Option<Status>,
}

/// Validate, enforce the limits, write the message and return its ID.
pub fn send(root: &Root, out: Outgoing, hops: Hops) -> Result<String> {
    check_name("--from", &out.from)?;
    check_name("--to", &out.to)?;
    for (what, id) in [("--reply-to", &out.reply_to), ("--parent", &out.parent)] {
        if let Some(id) = id {
            check_id(what, id)?;
        }
    }
    match (out.kind, out.status) {
        (MsgType::Result, None) => {
            return Err(CqError::usage(
                "--type result needs --status ok|failed|rejected",
            ));
        }
        (MsgType::Task | MsgType::Info, Some(_)) => {
            return Err(CqError::usage("--status is only for --type result"));
        }
        _ => {}
    }
    if out.body.len() > MAX_BODY_BYTES {
        return Err(CqError::refused(format!(
            "body is {} bytes; the limit is {MAX_BODY_BYTES}. Put large content in a file and send its path",
            out.body.len()
        )));
    }

    // Checked after the arguments, so a bad command is still a usage error.
    if root.pause().exists() {
        return Err(CqError::refused(format!(
            "queue paused: remove PAUSE to send ({})",
            root.pause().display()
        )));
    }

    // An answer ends its chain, so it may have one hop more than any other
    // message. Otherwise a task with the highest stored count could never
    // be answered. Answers are what `cq done` and `cq claim` send, and a
    // result that replies to a task.
    let (hops, highest) = match hops {
        Hops::After(h) => (h.saturating_add(1), MAX_HOPS),
        Hops::Lookup => {
            let hops = lookup_hops(root, &out.from, &out.reply_to, &out.parent)?;
            let answer = out.kind == MsgType::Result && out.reply_to.is_some();
            (hops, if answer { MAX_HOPS } else { MAX_HOPS - 1 })
        }
    };
    if hops > highest {
        return Err(CqError::refused(format!(
            "hop limit reached: this message would have {hops} hops (at most {highest} allowed)"
        )));
    }

    let now = timefmt::now();
    let recent = recent_messages(root, now);
    let pair = recent
        .iter()
        .filter(|(to, from)| *to == out.to && *from == out.from)
        .count();
    if pair >= RATE_LIMIT {
        return Err(CqError::refused(format!(
            "rate limit reached: {pair} messages from {} to {} in the last hour (limit {RATE_LIMIT})",
            out.from, out.to
        )));
    }
    let to_recipient = recent.iter().filter(|(to, _)| *to == out.to).count();
    if to_recipient >= RECIPIENT_LIMIT {
        return Err(CqError::refused(format!(
            "recipient limit reached: {to_recipient} messages to {} from all senders in the last hour (limit {RECIPIENT_LIMIT})",
            out.to
        )));
    }
    let from_sender = recent.iter().filter(|(_, from)| *from == out.from).count();
    if from_sender >= SENDER_LIMIT {
        return Err(CqError::refused(format!(
            "sender limit reached: {from_sender} messages from {} to all recipients in the last hour (limit {SENDER_LIMIT})",
            out.from
        )));
    }

    let inbox = root.inbox(&out.to);
    fs::create_dir_all(&inbox)
        .map_err(|e| CqError::io(format!("create {}", inbox.display()), e))?;

    let env_id = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut msg = Message {
        schema_version: SCHEMA_VERSION,
        id: String::new(),
        from: out.from,
        to: out.to,
        kind: out.kind,
        subject: out.subject,
        body: out.body,
        reply_to: out.reply_to,
        parent: out.parent,
        status: out.status,
        hops,
        created: timefmt::rfc3339(now),
        from_session_id: env_id("CLAUDE_CODE_SESSION_ID"),
        from_host_session_id: env_id("CLAUDE_CODE_HOST_SESSION_ID"),
    };

    // A new random suffix if the ID is already taken.
    for _ in 0..5 {
        msg.id = format!(
            "{}-{}-{:04x}",
            timefmt::id_stamp(now),
            msg.from,
            fsutil::random_u16()
        );
        let bytes = [
            serde_json::to_vec_pretty(&msg).expect("a message always serialises"),
            b"\n".to_vec(),
        ]
        .concat();
        let tmp = fsutil::write_tmp(&root.tmp(), &msg.id, &bytes)
            .map_err(|e| CqError::io(format!("write to {}", root.tmp().display()), e))?;
        let dst = message_file(&inbox, &msg.id);
        match fsutil::move_with_retry(&tmp, &dst) {
            Ok(()) => return Ok(msg.id),
            Err(MoveError::Exists) => {
                let _ = fs::remove_file(&tmp);
            }
            Err(MoveError::Gone) => {
                return Err(CqError::error(format!(
                    "temp file {} vanished before it was moved",
                    tmp.display()
                )));
            }
            Err(MoveError::Failed(e)) => {
                let _ = fs::remove_file(&tmp);
                return Err(CqError::new(
                    exit::RENAME_FAILED,
                    format!("could not move the message into {}: {e}", inbox.display()),
                ));
            }
        }
    }
    Err(CqError::error("could not find a free message ID"))
}

/// 1 + the highest `hops` of the named messages, read from the sender's
/// `.processing/` or `done/`; 0 when none is named. Refuses a `reply_to`
/// that does not name a task: only task messages get replies.
fn lookup_hops(
    root: &Root,
    from: &str,
    reply_to: &Option<String>,
    parent: &Option<String>,
) -> Result<u32> {
    let mut hops = None;
    for (id, is_reply) in [(reply_to, true), (parent, false)] {
        let Some(id) = id else { continue };
        let found = [root.processing(from), root.done(from)]
            .into_iter()
            .map(|d| message_file(&d, id))
            .find(|p| p.is_file());
        let Some(path) = found else {
            return Err(CqError::refused(format!(
                "message {id} is not in {from}'s .processing/ or done/; send only about messages you have claimed"
            )));
        };
        let bytes = fsutil::read_with_retry(&path)
            .map_err(|e| CqError::io(format!("read {}", path.display()), e))?;
        let value = serde_json::from_slice::<Value>(&bytes).ok();
        let field = |key: &str| value.as_ref().and_then(|v| v.get(key));
        let h = field("hops")
            .and_then(Value::as_u64)
            .ok_or_else(|| CqError::refused(format!("message {id} has no readable hops")))?;
        let kind = field("type").and_then(Value::as_str);
        if is_reply && kind != Some(MsgType::Task.as_str()) {
            return Err(CqError::refused(format!(
                "--reply-to refused: {id} is a {}, and only task messages get replies. Use --parent to send a new message about it",
                kind.unwrap_or("message of unknown type")
            )));
        }
        let h = u32::try_from(h).unwrap_or(u32::MAX);
        hops = Some(hops.map_or(h, |m: u32| m.max(h)));
    }
    Ok(hops.map_or(0, |h| h.saturating_add(1)))
}

/// `(to, from)` for every message created in the last `RATE_WINDOW`,
/// counted across each recipient's inbox, `.processing/`, `done/`,
/// `rejected/` and `dlq/`. Sender and time come from the file name, which
/// `cq` generates; the recipient is the directory name.
fn recent_messages(root: &Root, now: jiff::Timestamp) -> Vec<(String, String)> {
    let since = now - RATE_WINDOW;
    let mut out = Vec::new();
    for to in recipients(root) {
        let dirs = [
            root.inbox(&to),
            root.processing(&to),
            root.done(&to),
            root.rejected(&to),
            root.dlq(&to),
        ];
        for dir in &dirs {
            for stem in fsutil::json_stems(dir, |_| true) {
                if let Some((stamp, from)) = id_parts(&stem)
                    && timefmt::parse_id_stamp(stamp).is_some_and(|t| t > since)
                {
                    out.push((to.clone(), from.to_owned()));
                }
            }
        }
    }
    out
}

/// Every name with a directory under `inbox/`, `done/`, `rejected/` or
/// `dlq/`, once each.
fn recipients(root: &Root) -> BTreeSet<String> {
    ["inbox", "done", "rejected", "dlq"]
        .iter()
        .filter_map(|top| fs::read_dir(root.path().join(top)).ok())
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_name(n))
        .collect()
}
