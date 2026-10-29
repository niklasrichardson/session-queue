//! `cq requeue`: move a message that a session rejected back into its own
//! inbox, with its original ID, so it can claim it again (for example after
//! its sender allow-list is fixed). Only the session's own `rejected/`.

use std::fs;
use std::io;

use crate::error::{CqError, Result, exit};
use crate::fsutil::{self, MoveError};
use crate::names::{check_id, check_name};
use crate::root::{Root, message_file, reason_file};

/// Move `rejected/<name>/<id>.json` to `inbox/<name>/` and remove its
/// reason file. Returns a warning when the reason file could not be
/// removed; the message has moved all the same.
pub fn requeue(root: &Root, name: &str, id: &str) -> Result<Option<String>> {
    check_name("--as", name)?;
    check_id("ID", id)?;
    root.ensure_session_dirs(name)?;

    // Hold the recipient's claim lock, as `cq claim` does, until the reason
    // file is gone. Otherwise a fast claim and reject of the requeued
    // message could write a new reason file that this one then removes.
    let claim_lock_path = root.claim_lock(name);
    let claim_lock =
        fsutil::open_lock_file(&claim_lock_path).map_err(|e| CqError::io("open claim lock", e))?;
    claim_lock
        .lock()
        .map_err(|e| CqError::io("lock claim transaction", e))?;

    let dir = root.rejected(name);
    let src = message_file(&dir, id);
    if !src.is_file() {
        return Err(CqError::new(
            exit::NOT_FOUND,
            format!(
                "message {id} is not in rejected/{name}/; requeue moves only messages that {name} rejected"
            ),
        ));
    }
    let dst = message_file(&root.inbox(name), id);
    match fsutil::move_with_retry(&src, &dst) {
        Ok(()) => {}
        Err(MoveError::Gone) => {
            return Err(CqError::new(
                exit::NOT_FOUND,
                format!("message {id} left rejected/{name}/ before it could be moved"),
            ));
        }
        Err(MoveError::Exists) => {
            return Err(CqError::error(format!(
                "inbox/{name}/{id}.json already exists"
            )));
        }
        Err(MoveError::Failed(e)) => {
            return Err(CqError::new(
                exit::RENAME_FAILED,
                format!("could not move {id} to inbox/{name}/ after 3 retries: {e}"),
            ));
        }
    }
    // A new modified time: the watcher announces the message again even if
    // it never saw the inbox without it, and a coordinator's CQ-WAKE counts
    // the wait from now.
    let _ = fsutil::touch(&dst);
    match fs::remove_file(reason_file(&dir, id)) {
        Ok(()) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Ok(Some(format!(
            "message {id} is back in inbox/{name}/, but its reason file could not be removed: {e}"
        ))),
    }
}
