//! `cq`: a file-based message queue for Claude Code sessions.
//!
//! One binary with subcommands `init`, `send`, `watch`, `dispatch`, `claim`,
//! `done`, `list`, `requeue`, `leave`, `members`, `sleep`, `wake`, `status`, `lock`, `session-start`, `turn-end` and `idle-alert`. The library holds all queue logic so the integration tests can
//! check the message validator directly.

pub mod agy;
pub mod claim;
pub mod codex;
pub mod config;
pub mod dispatch;
pub mod done;
pub mod error;
pub mod exchange;
pub mod exitlog;
pub mod fsutil;
pub mod idle;
pub mod inbox;
pub mod job;
pub mod list;
pub mod lock;
pub mod members;
pub mod message;
pub mod names;
pub mod overdue;
pub mod platform;
pub mod record;
pub mod requeue;
pub mod root;
pub mod send;
pub mod sleep;
pub mod timefmt;
pub mod watch;

pub use error::{CqError, Result};
pub use root::Root;
