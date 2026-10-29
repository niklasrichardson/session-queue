//! `cq claim`: take a message from the inbox before working on it.

use serde_json::json;

use crate::error::{CqError, Result, exit};
use crate::fsutil::{self, MoveError};
use crate::message::{self, Invalid, MsgType, Status};
use crate::names::{check_id, check_name, id_parts};
use crate::root::{Root, message_file, reason_file};
use crate::send::{self, Hops, Outgoing};
use crate::timefmt;

/// Move `inbox/<name>/<id>.json` into `.processing/`, validate it and return
/// its bytes. An invalid message goes to `dlq/<name>/`.
pub fn claim(root: &Root, name: &str, id: &str) -> Result<Vec<u8>> {
    check_name("--as", name)?;
    check_id("ID", id)?;
    root.ensure_session_dirs(name)?;

    // On Windows, two rename calls can both open the same source before
    // either moves it, then both report success for the same destination.
    // Serialise the claim transaction per recipient; never remove this lock
    // file while another process could hold an open handle to it.
    let claim_lock_path = root.claim_lock(name);
    let claim_lock =
        fsutil::open_lock_file(&claim_lock_path).map_err(|e| CqError::io("open claim lock", e))?;
    claim_lock
        .lock()
        .map_err(|e| CqError::io("lock claim transaction", e))?;

    let src = message_file(&root.inbox(name), id);
    let dst = message_file(&root.processing(name), id);
    match fsutil::move_with_retry(&src, &dst) {
        Ok(()) => {}
        Err(MoveError::Gone) => {
            return Err(CqError::new(
                exit::GONE,
                format!(
                    "message {id} is not in inbox/{name}/ (already claimed, or a duplicate event); ignore it"
                ),
            ));
        }
        Err(MoveError::Exists) => {
            return Err(CqError::error(format!(
                "message {id} is already in inbox/{name}/.processing/; a duplicate ID was sent"
            )));
        }
        Err(MoveError::Failed(e)) => {
            return Err(CqError::new(
                exit::RENAME_FAILED,
                format!("could not claim {id} after 3 retries: {e}"),
            ));
        }
    }
    // The modified time records the claim, for `cq list --stale-minutes`
    // and overdue tasks. Without it, the message would look claimed since it
    // arrived, so put it back and fail. Overdue scans take the claim lock
    // before they trust a claimed file's time.
    if let Err(e) = fsutil::touch_with_retry(&dst) {
        let undo = match fsutil::move_with_retry(&dst, &src) {
            Ok(()) => format!("it is back in inbox/{name}/"),
            Err(m) => format!("it stays in inbox/{name}/.processing/ ({m:?})"),
        };
        return Err(CqError::io(
            format!("record the claim time of {id}; {undo}"),
            e,
        ));
    }

    let bytes = fsutil::read_with_retry(&dst)
        .map_err(|e| CqError::io(format!("read {}", dst.display()), e))?;
    match message::validate(&bytes, name, id) {
        Ok(_) => Ok(bytes),
        Err(invalid) => Err(dead_letter(root, name, id, invalid)),
    }
}

/// Move a claimed, invalid message to the DLQ, write the reason file and
/// tell the sender when `from` is readable. Returns the error to report.
fn dead_letter(root: &Root, name: &str, id: &str, invalid: Invalid) -> CqError {
    let src = message_file(&root.processing(name), id);
    let dlq = root.dlq(name);
    let dst = message_file(&dlq, id);
    if let Err(e) = fsutil::move_with_retry(&src, &dst) {
        return CqError::error(format!(
            "message {id} is invalid ({}) but could not be moved to dlq/{name}/: {e:?}",
            invalid.reason
        ));
    }
    let reason = json!({
        "id": id,
        "inbox": name,
        "reason": invalid.reason,
        "time": timefmt::rfc3339(timefmt::now()),
    });
    let mut text = serde_json::to_vec_pretty(&reason).expect("json value serialises");
    text.push(b'\n');
    let reason_note = match fsutil::write_atomic(&root.tmp(), &reason_file(&dlq, id), &text) {
        Ok(()) => String::new(),
        Err(e) => format!("; the reason file could not be written: {e}"),
    };

    // The ID names the sender too, which helps when the JSON is unreadable.
    let sender = invalid
        .sender
        .clone()
        .or_else(|| id_parts(id).map(|(_, from)| from.to_owned()));
    let sender_note = match &sender {
        None => "; the sender is unknown, so nobody was told".to_owned(),
        Some(sender) => {
            let notice = Outgoing {
                from: name.to_owned(),
                to: sender.clone(),
                kind: MsgType::Result,
                subject: format!("Not processed: {id}"),
                body: format!(
                    "Your message {id} to {name} was not processed. It is in the dead letter queue.\n\nReason: {}",
                    invalid.reason
                ),
                reply_to: Some(id.to_owned()),
                parent: None,
                status: Some(Status::Failed),
            };
            match send::send(root, notice, Hops::After(invalid.hops.unwrap_or(0))) {
                Ok(rid) => format!("; sent {sender} a failed result ({rid})"),
                Err(e) => format!("; could not tell {sender}: {e}"),
            }
        }
    };
    CqError::new(
        exit::INVALID,
        format!(
            "message {id} is invalid and was moved to dlq/{name}/: {}{reason_note}{sender_note}",
            invalid.reason
        ),
    )
}
