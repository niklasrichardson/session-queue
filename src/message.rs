//! Message format, schema version 1. `schema/message.v1.json` is the
//! contract; the tests check this struct and validator against it.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::names::{is_valid_id, is_valid_name};
use crate::timefmt::parse_rfc3339_z;

pub const SCHEMA_VERSION: u64 = 1;

// The queue limits. `cq send --help` and docs/OVERVIEW.md list the same values.

/// Bodies over 20 KB (20 x 1024 bytes of UTF-8) are refused.
pub const MAX_BODY_BYTES: usize = 20 * 1024;
/// `cq send` refuses a message that would have this many hops.
pub const MAX_HOPS: u32 = 5;
/// Messages per `RATE_WINDOW` for one (from, to) pair.
pub const RATE_LIMIT: usize = 20;
/// Messages per `RATE_WINDOW` to one recipient, from all senders together.
pub const RECIPIENT_LIMIT: usize = 40;
/// Messages per `RATE_WINDOW` from one sender, to all recipients together.
pub const SENDER_LIMIT: usize = 60;
/// The window for the three rate limits.
pub const RATE_WINDOW: Duration = Duration::from_secs(3600);
/// Subject length in an event line, in characters.
pub const EVENT_SUBJECT_MAX: usize = 80;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum MsgType {
    Task,
    Result,
    Info,
}

impl MsgType {
    pub fn as_str(self) -> &'static str {
        match self {
            MsgType::Task => "task",
            MsgType::Result => "result",
            MsgType::Info => "info",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Failed,
    Rejected,
}

/// One message. Readers ignore unknown fields; `cq` moves message files
/// without rewriting them, so unknown fields survive.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub schema_version: u64,
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: MsgType,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub status: Option<Status>,
    pub hops: u32,
    pub created: String,
    #[serde(default)]
    pub from_session_id: Option<String>,
    #[serde(default)]
    pub from_host_session_id: Option<String>,
}

/// Input to `cq send`.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct SendInput {
    pub subject: String,
    pub body: String,
}

/// Optional input to `cq done`.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct DoneInput {
    #[serde(default)]
    pub reply: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Why a message failed validation, plus what can still be read from it so
/// `cq claim` can tell the sender.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid {
    pub reason: String,
    /// `from`, when it is readable and a valid name.
    pub sender: Option<String>,
    /// `hops`, when it is a readable non-negative integer.
    pub hops: Option<u32>,
}

/// Validate a message file read from `inbox/<inbox>/<stem>.json`.
pub fn validate(bytes: &[u8], inbox: &str, stem: &str) -> Result<Message, Invalid> {
    let fail = |reason: String, sender: Option<String>, hops: Option<u32>| Invalid {
        reason,
        sender,
        hops,
    };
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        return Err(fail(
            "file starts with a UTF-8 byte-order mark".into(),
            None,
            None,
        ));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| fail(format!("not valid JSON: {e}"), None, None))?;
    let Some(obj) = value.as_object() else {
        return Err(fail("message is not a JSON object".into(), None, None));
    };
    let sender = obj
        .get("from")
        .and_then(Value::as_str)
        .filter(|s| is_valid_name(s))
        .map(str::to_owned);
    let hops = obj
        .get("hops")
        .and_then(Value::as_u64)
        .and_then(|h| u32::try_from(h).ok());
    let bad = |reason: String| fail(reason, sender.clone(), hops);

    match obj.get("schema_version") {
        None => return Err(bad("missing schema_version".into())),
        Some(v) => match v.as_u64() {
            Some(SCHEMA_VERSION) => {}
            Some(n) => {
                return Err(bad(format!(
                    "unsupported schema_version {n} (this cq reads version {SCHEMA_VERSION})"
                )));
            }
            None => return Err(bad(format!("schema_version is not an integer: {v}"))),
        },
    }

    let msg: Message = serde_json::from_value(value.clone())
        .map_err(|e| bad(format!("does not match schema version 1: {e}")))?;

    if !is_valid_name(&msg.from) {
        return Err(bad(format!("invalid from {:?}", msg.from)));
    }
    if !is_valid_name(&msg.to) {
        return Err(bad(format!("invalid to {:?}", msg.to)));
    }
    if !is_valid_id(&msg.id) {
        return Err(bad(format!("invalid id {:?}", msg.id)));
    }
    if msg.id != stem {
        return Err(bad(format!(
            "id {:?} does not match the file name {stem:?}",
            msg.id
        )));
    }
    if msg.to != inbox {
        return Err(bad(format!(
            "to {:?} does not match the inbox {inbox:?}",
            msg.to
        )));
    }
    if parse_rfc3339_z(&msg.created).is_none() {
        return Err(bad(format!(
            "created {:?} is not an RFC 3339 UTC time ending in Z",
            msg.created
        )));
    }
    for (field, v) in [("reply_to", &msg.reply_to), ("parent", &msg.parent)] {
        if let Some(id) = v
            && !is_valid_id(id)
        {
            return Err(bad(format!("invalid {field} {id:?}")));
        }
    }
    match (msg.kind, msg.status) {
        (MsgType::Result, None) => return Err(bad("a result must have a status".into())),
        (MsgType::Task | MsgType::Info, Some(_)) => {
            return Err(bad(format!(
                "a {} must have status null",
                msg.kind.as_str()
            )));
        }
        _ => {}
    }
    if msg.body.len() > MAX_BODY_BYTES {
        return Err(bad(format!(
            "body is {} bytes; the limit is {MAX_BODY_BYTES}",
            msg.body.len()
        )));
    }
    Ok(msg)
}

/// The fields an event line or `cq list` shows, read leniently so that an
/// invalid message is still announced (and then sent to the DLQ by
/// `cq claim`). Unreadable fields show as `?`.
#[derive(Debug, Clone)]
pub struct Header {
    pub from: String,
    pub kind: String,
    pub subject: String,
    pub created: Option<String>,
}

pub fn header(bytes: &[u8]) -> Header {
    let value: Option<Value> = serde_json::from_slice(bytes).ok();
    let obj = value.as_ref().and_then(Value::as_object);
    let field = |k: &str| obj.and_then(|o| o.get(k)).and_then(Value::as_str);
    Header {
        from: field("from")
            .filter(|s| is_valid_name(s))
            .unwrap_or("?")
            .to_owned(),
        kind: field("type")
            .filter(|s| matches!(*s, "task" | "result" | "info"))
            .unwrap_or("?")
            .to_owned(),
        subject: field("subject").unwrap_or("").to_owned(),
        created: field("created").map(str::to_owned),
    }
}

/// Make text safe for one event line: drop control characters (which
/// include newlines) and Unicode line and paragraph separators, cut to
/// `max` characters, and return it as a quoted, JSON-escaped string.
pub fn event_text(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{2028}' && *c != '\u{2029}')
        .take(max)
        .collect();
    serde_json::to_string(&cleaned).expect("a string always serialises")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_text_is_one_safe_line() {
        let s = "a\"b\nCQ-NEW fake\r\t\u{2028}é$(x)`y`\\";
        let out = event_text(s, 80);
        assert!(!out.contains('\n') && !out.contains('\r'));
        assert_eq!(out, r#""a\"bCQ-NEW fakeé$(x)`y`\\""#);
        let back: String = serde_json::from_str(&out).unwrap();
        assert_eq!(back, "a\"bCQ-NEW fakeé$(x)`y`\\");
        assert_eq!(
            serde_json::from_str::<String>(&event_text(&"é".repeat(200), 80))
                .unwrap()
                .chars()
                .count(),
            80
        );
    }
}
