//! `cq list`: unclaimed, stale claimed and dead-lettered messages, after an
//! `ASLEEP` line when the name is asleep. `cq list --rejected`: rejected
//! messages, both ways.

use std::fs;
use std::time::{Duration, SystemTime};

use jiff::Timestamp;
use serde_json::Value;

use crate::error::Result;
use crate::fsutil;
use crate::message::{EVENT_SUBJECT_MAX, event_text, header};
use crate::names::{check_name, id_parts, is_valid_id, is_valid_name};
use crate::root::{Root, message_file, reason_file};
use crate::sleep;
use crate::timefmt::{self, local_hhmm, rfc3339};

/// Reason text in a `DLQ` line, in characters.
const REASON_MAX: usize = 200;

/// `ASLEEP by=... reason="..."` when the name is asleep, then one line per
/// message, oldest first (by creation time), then a total. Never prints
/// bodies.
pub fn list(root: &Root, name: &str, stale_minutes: u64) -> Result<Vec<String>> {
    check_name("--as", name)?;
    let now = SystemTime::now();
    let stale_after = Duration::from_secs(stale_minutes.saturating_mul(60));
    let mut rows: Vec<(Timestamp, String)> = Vec::new();
    let (mut unclaimed, mut stale, mut dead) = (0, 0, 0);

    let inbox = root.inbox(name);
    for id in fsutil::json_stems(&inbox, is_valid_id) {
        let path = message_file(&inbox, &id);
        let Ok(bytes) = fsutil::read_with_retry(&path) else {
            continue;
        };
        let created = created_of(&id);
        let h = header(&bytes);
        unclaimed += 1;
        rows.push((
            created,
            format!(
                "UNCLAIMED {} ({}) from={} type={} id={id} subject={}",
                rfc3339(created),
                local_hhmm(created),
                h.from,
                h.kind,
                event_text(&h.subject, EVENT_SUBJECT_MAX)
            ),
        ));
    }

    let processing = root.processing(name);
    for id in fsutil::json_stems(&processing, is_valid_id) {
        let path = message_file(&processing, &id);
        let Some(claimed) = fsutil::modified(&path) else {
            continue;
        };
        let age = now.duration_since(claimed).unwrap_or_default();
        if age < stale_after {
            continue;
        }
        let Ok(bytes) = fsutil::read_with_retry(&path) else {
            continue;
        };
        let created = created_of(&id);
        let claimed_ts = timefmt::from_system_time(claimed).unwrap_or(created);
        let h = header(&bytes);
        stale += 1;
        rows.push((
            created,
            format!(
                "STALE {} ({}) claimed={} age={}m from={} type={} id={id} subject={}",
                rfc3339(created),
                local_hhmm(created),
                rfc3339(claimed_ts),
                age.as_secs() / 60,
                h.from,
                h.kind,
                event_text(&h.subject, EVENT_SUBJECT_MAX)
            ),
        ));
    }

    let dlq = root.dlq(name);
    for id in fsutil::json_stems(&dlq, is_valid_id) {
        let created = created_of(&id);
        let reason = fsutil::read_with_retry(&reason_file(&dlq, &id))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| "no reason file".to_owned());
        dead += 1;
        rows.push((
            created,
            format!(
                "DLQ {} ({}) id={id} reason={}",
                rfc3339(created),
                local_hhmm(created),
                event_text(&reason, REASON_MAX)
            ),
        ));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut out: Vec<String> = sleep::read(root, name)
        .map(|rec| format!("ASLEEP {}", rec.describe()))
        .into_iter()
        .chain(rows.into_iter().map(|(_, line)| line))
        .collect();
    out.push(format!(
        "TOTAL unclaimed={unclaimed} stale={stale} dlq={dead}"
    ));
    Ok(out)
}

/// Creation time from the ID, which `cq` generates.
pub(crate) fn created_of(id: &str) -> Timestamp {
    id_parts(id)
        .and_then(|(stamp, _)| timefmt::parse_id_stamp(stamp))
        .unwrap_or(Timestamp::UNIX_EPOCH)
}

/// `cq list --rejected`: one line per message that `name` rejected
/// (`REJECTED-IN`, from `rejected/<name>/`), and per message from `name`
/// that another session rejected (`REJECTED-OUT`, by the sender in the
/// file name), oldest first (by creation time), then a total. Never prints
/// bodies.
pub fn rejected(root: &Root, name: &str) -> Result<Vec<String>> {
    check_name("--as", name)?;
    let mut rows: Vec<(Timestamp, String)> = Vec::new();
    let (mut rejected_in, mut rejected_out) = (0, 0);

    for by in rejecters(root) {
        let dir = root.rejected(&by);
        for id in fsutil::json_stems(&dir, is_valid_id) {
            let label = if by == name {
                "REJECTED-IN"
            } else if id_parts(&id).is_some_and(|(_, from)| from == name) {
                "REJECTED-OUT"
            } else {
                continue;
            };
            let Ok(bytes) = fsutil::read_with_retry(&message_file(&dir, &id)) else {
                continue;
            };
            let h = header(&bytes);
            let reason = fsutil::read_with_retry(&reason_file(&dir, &id))
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let field = |k: &str| {
                reason
                    .as_ref()
                    .and_then(|v| v.get(k).and_then(Value::as_str).map(str::to_owned))
            };
            let created = created_of(&id);
            if by == name {
                rejected_in += 1;
            } else {
                rejected_out += 1;
            }
            rows.push((
                created,
                format!(
                    "{label} {} ({}) rejected={} by={by} from={} type={} id={id} subject={} reason={}",
                    rfc3339(created),
                    local_hhmm(created),
                    field("time").unwrap_or_else(|| "?".to_owned()),
                    h.from,
                    h.kind,
                    event_text(&h.subject, EVENT_SUBJECT_MAX),
                    event_text(
                        &field("reason").unwrap_or_else(|| "no reason file".to_owned()),
                        REASON_MAX
                    )
                ),
            ));
        }
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut out: Vec<String> = rows.into_iter().map(|(_, line)| line).collect();
    out.push(format!(
        "TOTAL rejected_in={rejected_in} rejected_out={rejected_out}"
    ));
    Ok(out)
}

/// Every name with a directory under `rejected/`, sorted.
fn rejecters(root: &Root) -> Vec<String> {
    let Ok(rd) = fs::read_dir(root.path().join("rejected")) else {
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
