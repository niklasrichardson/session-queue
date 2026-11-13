//! Command-line interface for `cq`. All queue logic lives in the library.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde::de::DeserializeOwned;

use cq::agy::AgyTurn;
use cq::codex::{CHECK_AFTER, CodexQueue};
use cq::config::{self, Config};
use cq::done::{self, DoneRequest};
use cq::error::{CqError, Result, exit};
use cq::inbox::Inbox;
use cq::message::{
    DoneInput, EVENT_SUBJECT_MAX, MAX_BODY_BYTES, MAX_HOPS, MsgType, RATE_LIMIT, RATE_WINDOW,
    RECIPIENT_LIMIT, SENDER_LIMIT, SendInput, Status, event_text,
};
use cq::send::{self, Hops, Outgoing};
use cq::watch::{
    self, DEFAULT_MAX_SECONDS, DEFAULT_RESCAN_SECONDS, Ended, Lines, Sink, WatchOptions,
};
use cq::{Root, claim, idle, list, lock, members, overdue, platform, record, sleep, timefmt};

const EXIT_CODES: &str = "\
Exit codes:
  0   ok
  1   claim: the message is no longer in the inbox (already handled); ignore it.
      lock: held by another name, or (release, break) not held or not yours
  2   a rename still failed after 3 retries. lock wait: still held at --max
  3   claim: the message is invalid and went to the dead letter queue
  4   done or dispatch: the message is finished, but the reply was not sent
  5   refused (name, size, hop or send limit, PAUSE, missing parent, a reply
      to a result or info, watcher running, sleeping another name without
      being a coordinator, an agy broker job record that needs review or is
      torn)
  6  done: the message is not claimed by this session; requeue: the
      message is not in this session's rejected/
  7   watch: the name is asleep; do not re-arm. status: the name is asleep
  8   watch --to-inbox: the session's inbox cannot be reached (session gone)
  9   watch --to-codex: codex queue failed 4 times; the Codex chat is not woken
  10  status: no watcher runs for the name
  11  watch --to-agy: an agy turn failed 4 times in a row, or ran in another
      conversation; the conversation is not woken. With --agy-broker: a job
      needs review (see its record), or agy could not start 4 times
  64  bad arguments or bad JSON input
  70  any other error (I/O, CQ_ROOT missing or not initialised)";

/// The exit codes, then where the queue root and the local time come from,
/// with the values this build uses.
fn exit_help() -> String {
    let root = cq::root::default_root().map_or_else(
        || "none (the per-user data folder is unknown)".to_owned(),
        |p| p.display().to_string(),
    );
    format!(
        "{EXIT_CODES}

The queue root is CQ_ROOT (an absolute path). When it is unset, the root is
the build's CQ_DEFAULT_ROOT, else cq in the per-user data folder. Default
here: {root}
The local time in CQ-NEW and list lines uses CQ_TZ (an IANA time zone name)
and CQ_TZ_LABEL, else tz and tz_label in config.toml in the queue root;
here: {}.",
        timefmt::local_description()
    )
}

/// The overdue rule for `cq list --overdue`, then the exit codes.
fn overdue_help() -> String {
    format!(
        "Overdue (--overdue): a task is overdue when it is still unclaimed {}
minutes after it arrived, or claimed with no done {} minutes after the claim
(the defaults; set {} and {}, or the flags).
Results and info are never overdue. It reads only, and works while PAUSE
exists; cq never moves, re-sends or answers an overdue task.

{}",
        overdue::DEFAULT_UNCLAIMED_MINUTES,
        overdue::DEFAULT_CLAIMED_MINUTES,
        overdue::ENV_UNCLAIMED,
        overdue::ENV_CLAIMED,
        exit_help()
    )
}

/// The lock rules and exit codes for `cq lock`, then the general exit codes.
fn lock_help() -> String {
    format!(
        "A lock is held while CQ_ROOT/locks/<LOCK>.lock exists. Of many takes at
once exactly one wins. Only the holder releases it; a coordinator may break
any lock with a reason. The holder \"{}\" is the user's hold: a coordinator
takes or releases it with --owner, and nobody else can take the lock while it
is held. take is refused (exit 5) while PAUSE exists.

A held lock is orphaned when its holder's watcher is stopped and its last
heartbeat (or the take) is over 90 s old, and stale when its holder's watcher
runs and it has been held {} minutes (set {}). A coordinator's watcher prints
CQ-LOCK-ORPHAN and CQ-LOCK-STALE once per take; cq never breaks a lock itself.
Each take, release, break and report is a line in CQ_ROOT/locks.log.

Lock exit codes:
  take     0 taken or already yours; 1 held by another name (the holder is printed)
  release  0 released; 1 not held, or not yours
  status   0 free; 1 held (holder, time taken, --for text, age)
  wait     0 free; 2 still held at --max
  break    0 broken; 1 not held
  list     0
  5        refused (bad name, --owner or break without being a coordinator, PAUSE)

{}",
        lock::OWNER,
        lock::DEFAULT_STALE_MINUTES,
        lock::ENV_STALE,
        exit_help()
    )
}

/// The limits `cq send` enforces (exit 5, nothing written), then the exit
/// codes. Built from the constants, so the text cannot drift from them.
fn send_help() -> String {
    let minutes = RATE_WINDOW.as_secs() / 60;
    format!(
        "Limits (each refusal exits 5 and writes nothing):
  body             at most {MAX_BODY_BYTES} bytes
  hops             refused at {MAX_HOPS} (from --parent and --reply-to); a result that
                   replies to a task may have {MAX_HOPS}, and nothing can follow it
  replies          only task messages get replies (--reply-to must name a task)
  pair             at most {RATE_LIMIT} messages from one sender to one recipient per {minutes} minutes
  recipient        at most {RECIPIENT_LIMIT} messages to one recipient, all senders, per {minutes} minutes
  sender           at most {SENDER_LIMIT} messages from one sender, all recipients, per {minutes} minutes
  PAUSE            while CQ_ROOT/PAUSE exists, every send is refused
The per-hour counts cover the recipient's inbox, .processing/, done/,
rejected/ and dlq/, by the time stamp and sender in each file name.

{}",
        exit_help()
    )
}

#[derive(Parser)]
#[command(
    name = "cq",
    bin_name = "cq",
    version,
    about = "File-based message queue for AI coding sessions",
    after_help = exit_help()
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

// Parsed once per run, so the size of the `watch` variant does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    /// Create the queue layout under CQ_ROOT. Safe to run more than once.
    Init,

    /// Send a message. Reads {"subject": "...", "body": "..."} as JSON from
    /// stdin or --input, and prints the new message ID.
    #[command(after_help = send_help())]
    Send {
        /// Your session name.
        #[arg(long, value_name = "NAME")]
        from: String,
        /// The recipient's session name.
        #[arg(long, value_name = "NAME")]
        to: String,
        #[arg(long = "type", value_name = "TYPE")]
        kind: MsgType,
        /// The message this answers (a message you have claimed).
        #[arg(long, value_name = "ID")]
        reply_to: Option<String>,
        /// The message this follows from (a message you have claimed).
        #[arg(long, value_name = "ID")]
        parent: Option<String>,
        /// Required for --type result.
        #[arg(long)]
        status: Option<Status>,
        /// Read the JSON from this file instead of stdin ("-" for stdin).
        #[arg(long, value_name = "FILE")]
        input: Option<PathBuf>,
    },

    /// Print one CQ-NEW line per new message in your inbox. Run it under a
    /// Monitor. Never prints message bodies. While your name is asleep, it
    /// prints CQ-SLEEP and exits 7. A coordinator's watcher also prints one
    /// CQ-WAKE line per message that has waited 2 minutes for a sleeping
    /// name. Experimental: --to-inbox posts the lines into this Claude Code
    /// session's inbox instead, --to-codex wakes a Codex chat, and --to-agy
    /// wakes an Antigravity CLI conversation. When your
    /// name is listed in CQ_ROOT/idle-alerts, it also sends the idle alert
    /// after a turn end recorded by `cq turn-end`, if the session stays idle.
    #[command(after_help = exit_help())]
    Watch {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// Stop the running watcher for this name first.
        #[arg(long)]
        takeover: bool,
        /// Exit after this many seconds. Default 1790, just under the
        /// Monitor's 1800 s cap; no limit with --to-inbox, --to-codex or
        /// --to-agy.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max_seconds: Option<u64>,
        /// Experimental. Post the lines into this Claude Code session's own
        /// inbox (CLAUDE_CODE_MESSAGING_SOCKET and CLAUDE_CODE_MESSAGING_TOKEN)
        /// instead of stdout, batched, and post a last message when the
        /// watcher stops. Start it once in the background with the Bash
        /// tool, not under a Monitor.
        #[arg(long)]
        to_inbox: bool,
        /// Experimental. Wake this Codex chat (a thread ID or exact thread
        /// name) when messages arrive: run `codex queue` once per batch with
        /// a fixed prompt, never message text. Run it in the background,
        /// outside the chat.
        #[arg(long, value_name = "THREAD", conflicts_with = "to_inbox")]
        to_codex: Option<String>,
        /// The Codex executable for --to-codex. Default: codex on PATH.
        #[arg(long, value_name = "PATH", requires = "to_codex")]
        codex_bin: Option<OsString>,
        /// The message --to-codex queues, instead of the default queue check
        /// prompt. It must not contain anything a queue message wrote.
        #[arg(long, value_name = "TEXT", requires = "to_codex")]
        codex_prompt: Option<String>,
        /// How long after each queued check --to-codex looks, read-only, in
        /// Codex's queue database whether the chat has taken it. 0 turns the
        /// look-up off. Default: 60.
        #[arg(long, value_name = "SECS", requires = "to_codex")]
        codex_check_seconds: Option<u64>,
        /// Codex's queue database for that look-up, instead of
        /// queue_1.sqlite in Codex's SQLite home.
        #[arg(long, value_name = "PATH", requires = "to_codex")]
        codex_queue_db: Option<PathBuf>,
        /// With --to-codex, print to stderr, once, why the look-up in Codex's
        /// queue database was skipped.
        #[arg(long, requires = "to_codex")]
        debug: bool,
        /// Experimental. Wake this Antigravity CLI conversation (its ID) when
        /// messages arrive: run one `agy --conversation ID -p PROMPT` turn per
        /// batch with a fixed prompt, never message text. Run it in the
        /// background, outside the conversation.
        #[arg(long, value_name = "CONVERSATION_ID",
              conflicts_with_all = ["to_inbox", "to_codex"])]
        to_agy: Option<String>,
        /// The Antigravity CLI executable for --to-agy. Default: agy on PATH.
        #[arg(long, value_name = "PATH", requires = "to_agy")]
        agy_bin: Option<OsString>,
        /// The prompt each --to-agy turn runs, instead of the default queue
        /// check prompt. It must not contain anything a queue message wrote.
        #[arg(long, value_name = "TEXT", requires = "to_agy")]
        agy_prompt: Option<String>,
        /// Experimental, Windows only. With --to-agy: the watcher claims and
        /// finishes each message itself, and each agy turn only reads a
        /// request file and writes a reply file in the exchange folder. One
        /// turn per message.
        #[arg(long, requires = "to_agy")]
        agy_broker: bool,
        /// With --agy-broker: a sender whose messages get an agy turn. Repeat
        /// it for each sender. Other senders' tasks are rejected unread.
        #[arg(long, value_name = "NAME", requires = "agy_broker")]
        agy_allow_from: Vec<String>,
        /// With --agy-broker: the exchange folder. Default:
        /// CQ_ROOT/agy-exchange/NAME.
        #[arg(long, value_name = "DIR", requires = "agy_broker")]
        agy_exchange: Option<PathBuf>,
        /// With --agy-broker, on Windows: the SID of agy's sandbox, which gets
        /// Modify access to the exchange folder.
        #[arg(long, value_name = "SID", requires = "agy_broker")]
        agy_sandbox_sid: Option<String>,
        /// Experimental, Windows only. Run inside an Antigravity session (CLI
        /// or App) as one fixed background command, and wake that session
        /// with `agentapi send-message` for each message, through the
        /// exchange folder. The root is the build's CQ_DEFAULT_ROOT and the
        /// policy is [agy-session.NAME] in its config.toml; no setting comes
        /// from the environment. Takes no other option except --takeover.
        #[arg(long, conflicts_with_all = SESSION_CONFLICTS)]
        to_agy_session: bool,
        /// Send the idle alert this long after a turn end recorded by
        /// `cq turn-end`, when no activity is observed since: no transcript
        /// write after the first 30 s, and no line from this watcher. For
        /// example 10m; more than 30s. Only for a name listed in
        /// CQ_ROOT/idle-alerts.
        #[arg(long, value_name = "DURATION", default_value = "5m",
              value_parser = parse_idle_after)]
        idle_after: Duration,
        /// The least time between two idle alerts for this name.
        #[arg(long, value_name = "DURATION", default_value = "1h",
              value_parser = parse_every)]
        idle_every: Duration,
        /// Full rescan interval (for tests).
        #[arg(long, hide = true, default_value_t = DEFAULT_RESCAN_SECONDS,
              value_parser = clap::value_parser!(u64).range(1..))]
        rescan_seconds: u64,
        /// Rely on the rescan alone (for tests).
        #[arg(long, hide = true)]
        no_fs_events: bool,
    },

    /// Run a fresh Codex task for each inbox message. Requires trusted scope
    /// instructions and allowed senders. Does not wake an existing chat.
    #[command(after_help = exit_help())]
    Dispatch {
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// Absolute path to the Codex executable (not a shell command).
        #[arg(long, value_name = "FILE")]
        codex: PathBuf,
        /// Absolute path to the repository Codex will work in.
        #[arg(long, value_name = "DIR")]
        workspace: PathBuf,
        /// Trusted file defining allowed tasks, read once at startup.
        #[arg(long, value_name = "FILE")]
        instructions: PathBuf,
        /// Allowed sender. Repeat for more names. Sender names are not authentication.
        #[arg(long, required = true, value_name = "NAME")]
        allow_from: Vec<String>,
        /// Codex sandbox for this invocation. Never bypasses approvals.
        #[arg(long, value_enum, default_value_t = cq::dispatch::Sandbox::ReadOnly)]
        sandbox: cq::dispatch::Sandbox,
        /// Stop and leave the task claimed if Codex exceeds this limit.
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..))]
        timeout_seconds: u64,
        /// Process the current inbox, then exit. Under PAUSE, exit 5.
        #[arg(long)]
        once: bool,
    },

    /// Claim a message before working on it, and print its JSON.
    /// Exit 1 means another claim won or the event was a duplicate: ignore it.
    #[command(after_help = exit_help())]
    Claim {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// The message ID from the CQ-NEW line.
        id: String,
    },

    /// Finish a claimed message. Reads JSON from stdin or --input only with
    /// --reply ({"reply": "..."}, sent to a task's sender) or --reject
    /// ({"reason": "..."}). With neither, it reads no input, sends nothing,
    /// and refuses (exit 64, nothing moved) if input is waiting on stdin.
    #[command(after_help = exit_help())]
    Done {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// The claimed message ID.
        id: String,
        /// Status of the reply (default ok). Needs --reply.
        #[arg(long, requires = "reply")]
        status: Option<ReplyStatus>,
        /// Reply to a task: read {"reply": "..."}. Refused for a result or info.
        #[arg(long, conflicts_with = "reject")]
        reply: bool,
        /// Decline the message: read {"reason": "..."}. A task's sender gets a
        /// rejected result; an info or result's sender gets an info notice.
        #[arg(long)]
        reject: bool,
        /// Read the JSON from this file instead of stdin ("-" for stdin).
        /// Needs --reply or --reject.
        #[arg(long, value_name = "FILE")]
        input: Option<PathBuf>,
    },

    /// List unclaimed, stale claimed and dead-lettered messages, oldest first.
    /// With --overdue, list the overdue tasks in every inbox instead.
    #[command(after_help = overdue_help())]
    List {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME", required_unless_present = "overdue")]
        name: Option<String>,
        /// A claimed message counts as stale after this many minutes.
        #[arg(long, default_value_t = 60, conflicts_with = "overdue")]
        stale_minutes: u64,
        /// List rejected messages instead: those you rejected (REJECTED-IN)
        /// and those you sent that others rejected (REJECTED-OUT).
        #[arg(long, conflicts_with_all = ["stale_minutes", "overdue"])]
        rejected: bool,
        /// List overdue tasks across all inboxes: unclaimed or claimed for
        /// too long. Read only; works while PAUSE exists.
        #[arg(long, conflicts_with = "name")]
        overdue: bool,
        /// With --overdue: an unclaimed task is overdue after this many
        /// minutes. Default: CQ_OVERDUE_UNCLAIMED_MINUTES, else 30.
        #[arg(long, value_name = "N",
              value_parser = clap::value_parser!(u64).range(1..))]
        unclaimed_minutes: Option<u64>,
        /// With --overdue: a claimed task with no done is overdue after this
        /// many minutes. Default: CQ_OVERDUE_CLAIMED_MINUTES, else 120.
        #[arg(long, value_name = "M",
              value_parser = clap::value_parser!(u64).range(1..))]
        claimed_minutes: Option<u64>,
    },

    /// Leave the queue: forget which Claude Code session owns this name.
    /// Exit 0 when there is nothing to forget.
    #[command(after_help = exit_help())]
    Leave {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
    },

    /// Move a message you rejected back to your inbox, with its original
    /// ID, so you can claim it again. Only from your own rejected/ folder.
    #[command(after_help = exit_help())]
    Requeue {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// The rejected message ID.
        id: String,
    },

    /// Print one "<session key> -> <name>" line per recorded session, with
    /// " (asleep)" after a sleeping name. A sleeping name with no recorded
    /// session prints as "- -> <name> (asleep)".
    Members,

    /// Put a name to sleep: its watcher prints CQ-SLEEP and exits 7, and the
    /// session stops re-arming. Messages still arrive in its inbox. A name
    /// may sleep itself; only a coordinator (listed in CQ_ROOT/coordinators)
    /// may sleep another name.
    #[command(after_help = exit_help())]
    Sleep {
        /// Your session name.
        #[arg(long = "as", value_name = "ACTOR")]
        actor: String,
        /// The name to put to sleep.
        name: String,
        /// Shown in the CQ-SLEEP line.
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,
    },

    /// Wake a sleeping name, so its watcher runs again. Any name may wake
    /// any name. Exit 0 when it was not asleep. The session itself still
    /// needs a message telling it to rejoin.
    #[command(after_help = exit_help())]
    Wake {
        /// Your session name.
        #[arg(long = "as", value_name = "ACTOR")]
        actor: String,
        /// The name to wake.
        name: String,
    },

    /// Print one line saying whether NAME's watcher runs: "running" (exit
    /// 0), "stopped" with why (exit 10), or "asleep" (exit 7), with the
    /// heartbeat age, the process ID and the recorded mode. The rule is
    /// the one `cq session-start` uses: a pid file and a heartbeat under
    /// 90 s old. Reads only; any session or the user may check any name.
    #[command(after_help = exit_help())]
    Status {
        /// The name to check.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
    },

    /// Take, release, wait for and list named locks, such as one for the
    /// desktop that GUI tests drive. Sessions read a lock instead of
    /// messaging its holder.
    #[command(after_help = lock_help())]
    Lock {
        #[command(subcommand)]
        cmd: LockCmd,
    },

    /// For a Claude Code SessionStart hook (startup|clear|compact|resume). If this
    /// session owns a queue name, print a reminder to rejoin as it; else
    /// print nothing. Always exits 0.
    SessionStart {
        /// Print the session key and the lookup to stderr.
        #[arg(long)]
        debug: bool,
    },

    /// For a Claude Code Notification hook (idle_prompt), or manual use. If
    /// this session owns a name listed in CQ_ROOT/idle-alerts as "worker
    /// lead", send the lead one info message "<name> is idle", at most once
    /// per --every. Skipped while the name is asleep. Exits 0, except 64 for
    /// a bad --every. The desktop app does not send idle_prompt: use the
    /// Stop hook with `cq turn-end` instead.
    IdleAlert {
        /// The least time between two alerts for one name, for example 30m
        /// or 2h.
        #[arg(long, value_name = "DURATION", default_value = "1h",
              value_parser = parse_every)]
        every: Duration,
        /// Print the session key, the name and the result to stderr.
        #[arg(long)]
        debug: bool,
        /// Wait this long after the check, before the send (for tests).
        #[arg(long, hide = true, default_value_t = 0)]
        hold_ms: u64,
    },

    /// For a Claude Code Stop hook. If this session owns a name listed in
    /// CQ_ROOT/idle-alerts and awake, record the turn end and the hook's
    /// transcript_path in CQ_ROOT/idle/<name>.turn-end. The name's watcher
    /// then sends the idle alert if the session stays idle (see cq watch
    /// --idle-after). Records nothing when the hook reports background work
    /// other than a cq watcher. Prints nothing and always exits 0.
    TurnEnd {
        /// Print the session key, the name and the result to stderr.
        #[arg(long)]
        debug: bool,
    },
}

#[derive(Subcommand)]
enum LockCmd {
    /// Take LOCK. Exit 0 when taken or already yours, 1 when another name
    /// holds it (the holder is printed).
    #[command(after_help = lock_help())]
    Take {
        /// The lock's name, as a queue name: a-z, 0-9 and -.
        lock: String,
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        actor: String,
        /// What you hold it for, in a few words (at most 200 characters).
        #[arg(long = "for", value_name = "TEXT")]
        purpose: String,
        /// Take the user's hold ("owner") instead of your own. Coordinators
        /// only.
        #[arg(long)]
        owner: bool,
    },

    /// Release LOCK. Exit 0 when released, 1 when it is not held or not
    /// yours.
    #[command(after_help = lock_help())]
    Release {
        lock: String,
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        actor: String,
        /// Release the user's hold ("owner"). Coordinators only.
        #[arg(long)]
        owner: bool,
    },

    /// Print whether LOCK is free (exit 0) or held (exit 1), with the
    /// holder, the time taken, the --for text and the age, and "orphaned"
    /// or "stale" when it is. Reads only.
    #[command(after_help = lock_help())]
    Status { lock: String },

    /// Wait until LOCK is free (exit 0), or until --max minutes have passed
    /// (exit 2). Reads the lock every 15 s; sends nothing and takes nothing.
    /// Run it in the background.
    #[command(after_help = lock_help())]
    Wait {
        lock: String,
        /// Give up after this many minutes. Without it, waits until free.
        #[arg(long, value_name = "MINUTES",
              value_parser = clap::value_parser!(u64).range(1..))]
        max: Option<u64>,
        /// Read the lock this often, in milliseconds (for tests).
        #[arg(long, hide = true, default_value_t = 15_000)]
        poll_ms: u64,
        /// Give up after this many milliseconds (for tests).
        #[arg(long, hide = true, conflicts_with = "max")]
        max_ms: Option<u64>,
    },

    /// Print one line per held lock, with its holder. Reads only.
    #[command(after_help = lock_help())]
    List,

    /// Break LOCK, whoever holds it, and log why. Coordinators only. Exit 1
    /// when it is not held.
    #[command(after_help = lock_help())]
    Break {
        lock: String,
        /// Your session name, a coordinator.
        #[arg(long = "as", value_name = "NAME")]
        actor: String,
        /// Why, in a few words. Goes into locks.log.
        #[arg(long, value_name = "TEXT")]
        reason: String,
    },
}

/// How long `session-start` waits for the hook's JSON once bytes are
/// waiting. The content is not used.
const HOOK_INPUT_WAIT: Duration = Duration::from_millis(100);
/// How long `turn-end` waits for the whole of the hook's JSON, from the
/// first byte to the end. It needs the transcript path, so it waits longer;
/// the read ends as soon as Claude Code closes stdin.
const TURN_END_INPUT_WAIT: Duration = Duration::from_secs(2);

/// The `watch` options `--to-agy-session` refuses: all but `--as` and
/// `--takeover`. A default value does not count as given.
const SESSION_CONFLICTS: [&str; 19] = [
    "max_seconds",
    "to_inbox",
    "to_codex",
    "codex_bin",
    "codex_prompt",
    "codex_check_seconds",
    "codex_queue_db",
    "debug",
    "to_agy",
    "agy_bin",
    "agy_prompt",
    "agy_broker",
    "agy_allow_from",
    "agy_exchange",
    "agy_sandbox_sid",
    "idle_after",
    "idle_every",
    "rescan_seconds",
    "no_fs_events",
];

/// The flag that makes this process a session-mode watcher, which reads
/// no setting from the environment.
const SESSION_FLAG: &str = "--to-agy-session";

#[derive(Clone, Copy, ValueEnum)]
enum ReplyStatus {
    Ok,
    Failed,
}

fn main() -> ExitCode {
    // Before parsing: the help text shows the local time zone, which is
    // fixed once it is first read.
    // Session mode takes its root and config.toml from the build only
    // (`cq::session`), so neither is read here from the environment.
    let session_mode = std::env::args_os().any(|a| a == SESSION_FLAG);
    let config_warning = if session_mode {
        config::ignore_env();
        None
    } else {
        load_config()
    };
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            if session_mode && e.use_stderr() {
                println!(
                    "cq watch stopped: the command line is not valid. Do not restart; tell the user."
                );
            }
            // clap uses exit 2 for usage errors, which is RENAME_FAILED here.
            let code = if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            };
            return ExitCode::from(code as u8);
        }
    };
    if let Some(w) = config_warning
        && (!is_hook(&cli.cmd) || hook_debug(&cli.cmd))
    {
        eprintln!("cq: warning: {w}");
    }
    match run(cli.cmd) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("cq: {e}");
            ExitCode::from(e.code as u8)
        }
    }
}

/// `cq watch --to-inbox`. An error that the session can still hear about
/// (not exit 8, which means the inbox is gone) is posted as the stopped
/// message, so the session knows it has no watcher.
fn watch_to_inbox(opts: &WatchOptions) -> Result<Ended> {
    let mut inbox = Inbox::from_env(&opts.name)?;
    if let Err(e) = platform::install_interrupt_handler() {
        eprintln!("cq watch: a signal will stop the watcher without a stopped message ({e})");
    }
    let result = Root::open().and_then(|root| watch::watch(&root, opts, &mut inbox));
    if let Err(e) = &result
        && e.code != exit::INBOX_GONE
    {
        inbox.stopped(&format!("error: {e}"), Ended::Normal);
    }
    result
}

/// `cq watch --to-codex`. Errors go to stderr only: the Codex chat is not
/// told that its watcher stopped.
fn watch_to_codex(
    opts: &WatchOptions,
    thread: &str,
    bin: Option<OsString>,
    prompt: Option<String>,
    check_after: Duration,
    db: Option<PathBuf>,
    debug: bool,
) -> Result<Ended> {
    let codex = CodexQueue::start(&opts.name, thread, bin, prompt, check_after, db, debug)?;
    if let Err(e) = platform::install_interrupt_handler() {
        eprintln!("cq watch: a signal will stop the watcher before it queues held messages ({e})");
    }
    let root = Root::open()?;
    let mut codex = codex.alerting(root.clone());
    watch::watch(&root, opts, &mut codex)
}

/// `cq watch --to-agy`. Errors go to stderr only: the conversation is not
/// told that its watcher stopped.
fn watch_to_agy(
    opts: &WatchOptions,
    conversation: &str,
    bin: Option<OsString>,
    prompt: Option<String>,
) -> Result<Ended> {
    let mut agy = AgyTurn::start(&opts.name, conversation, bin, prompt)?;
    if let Err(e) = platform::install_interrupt_handler() {
        eprintln!(
            "cq watch: a signal will stop the watcher before it starts a turn for held messages ({e})"
        );
    }
    let root = Root::open()?;
    watch::watch(&root, opts, &mut agy)
}

/// `cq watch --to-agy --agy-broker`.
fn watch_agy_broker(
    opts: &WatchOptions,
    conversation: &str,
    bin: Option<OsString>,
    prompt: Option<String>,
    broker: BrokerFlags,
) -> Result<Ended> {
    let root = Root::open()?;
    let exchange = broker
        .exchange
        .unwrap_or_else(|| cq::broker::default_exchange(&root, &opts.name));
    let cfg = cq::broker::Config {
        name: opts.name.clone(),
        conversation: conversation.to_owned(),
        allow_from: broker.allow_from,
        exchange,
        sandbox_sid: broker.sandbox_sid,
        bin: bin.unwrap_or_else(|| cq::agy::DEFAULT_BIN.into()),
        prompt,
    };
    let mut sink = cq::broker::AgyBroker::start(root.clone(), cfg)?;
    if let Err(e) = platform::install_interrupt_handler() {
        eprintln!("cq watch: a signal may stop the watcher without stopping a running turn ({e})");
    }
    watch::watch(&root, opts, &mut sink)
}

/// `cq watch --to-agy-session`. Whatever ends it, the last stdout line is
/// the stop line, which the session reads as the command's output.
fn watch_agy_session(name: &str, takeover: bool) -> Result<i32> {
    let result = run_agy_session(name, takeover);
    println!("{}", stop_line(name, &result));
    let _ = io::stdout().flush();
    result
}

/// The last stdout line of a session-mode watcher.
fn stop_line(name: &str, result: &Result<i32>) -> String {
    format!("cq watch ({name}) stopped: {}", stop_advice(name, result))
}

/// The stop line after `cq watch (NAME) stopped: `: why, and what to do.
fn stop_advice(name: &str, result: &Result<i32>) -> String {
    let e = match result {
        Ok(_) => return "it was ended. Do not restart; tell the user.".into(),
        Err(e) => e,
    };
    match e.code {
        // A restart would stop again at once: only a person can give the job
        // a new round.
        exit::INBOX_GONE if cq::broker::is_not_taken(e) => format!(
            "{}. A restart stops again at once. For a new round, the user puts {name} to sleep (cq sleep) or creates PAUSE, has this session start the watcher, then runs cq wake {name} or removes PAUSE. Do not restart; tell the user.",
            e.message
        ),
        exit::INBOX_GONE => format!("{}. Start the watcher again with the same line.", e.message),
        exit::REFUSED if e.message.contains("already running") => {
            if same_session_holds(name) {
                "already running for this conversation; nothing to do.".into()
            } else {
                format!("{name} is held by another watcher; ask the user.")
            }
        }
        _ => format!("{}. Do not restart; tell the user.", e.message),
    }
}

/// The watcher holding `name` is a session-mode watcher for the
/// conversation in this build's policy.
fn same_session_holds(name: &str) -> bool {
    let Some(root) = Root::build_default() else {
        return false;
    };
    let policy = std::fs::read(root.path().join(config::FILE))
        .ok()
        .and_then(|b| config::text_of(&b).ok().map(str::to_owned))
        .and_then(|t| policy_conversation(&t, name));
    let holder = std::fs::read_to_string(root.pid_file(name))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .and_then(|pid| record::Record::read_for(&root, name, pid))
        .and_then(|r| r.session);
    matches!((policy, holder), (Some(c), Some(h)) if c == h.conversation)
}

#[cfg(windows)]
fn policy_conversation(text: &str, name: &str) -> Option<String> {
    let policy = cq::session::Policy::parse(text, name).ok()?;
    cq::session::conversation_of(&policy, &|var: &str| std::env::var_os(var)).ok()
}

#[cfg(not(windows))]
fn policy_conversation(_text: &str, _name: &str) -> Option<String> {
    None
}

#[cfg(not(windows))]
fn run_agy_session(_name: &str, _takeover: bool) -> Result<i32> {
    Err(CqError::usage(
        "--to-agy-session is supported on Windows only",
    ))
}

/// Build the trusted inputs, then run the broker with session wakes.
#[cfg(windows)]
fn run_agy_session(name: &str, takeover: bool) -> Result<i32> {
    use cq::broker::{AgyBroker, SessionWake, Timing, default_exchange};
    use cq::session::{Os, SessionInputs, Sources};
    use cq::wake::AgentApi;

    cq::names::check_name("--as", name)?;
    let env = |var: &str| std::env::var_os(var);
    let inputs = SessionInputs::build(
        name,
        Sources {
            build_root: Root::build_default().map(|r| r.path().to_owned()),
            exe: std::env::current_exe().map_err(|e| CqError::io("find this cq.exe", e))?,
            pid: std::process::id(),
            env: &env,
            system: Box::new(Os),
        },
    )?;
    let SessionInputs {
        root,
        policy,
        settings,
        warning,
        conversation,
        agentapi,
        link,
        pins: _pins,
        ..
    } = inputs;
    if let Some(w) = warning {
        eprintln!("cq: warning: {w}");
    }
    config::install(settings);
    let minutes = |m: u64| Duration::from_secs(m * 60);
    let timing = Timing {
        ack: minutes(policy.ack_minutes),
        reply: minutes(policy.reply_minutes),
        resume: minutes(policy.resume_minutes),
    };
    let wake = AgentApi::new(agentapi.clone(), conversation.clone(), env);
    let cfg = cq::broker::Config {
        name: name.to_owned(),
        conversation: conversation.clone(),
        allow_from: policy.allow_from,
        exchange: default_exchange(&root, name),
        sandbox_sid: policy.sandbox_sids.into_iter().next(),
        bin: agentapi.clone().into_os_string(),
        prompt: None,
    };
    let mut sink: AgyBroker<SessionWake<AgentApi, _>, _> = AgyBroker::session(
        root.clone(),
        cfg,
        wake,
        link,
        timing,
        cq::inbox::SystemClock,
        timefmt::now(),
        Box::new(io::stderr()),
    )?;
    if let Err(e) = platform::install_interrupt_handler() {
        eprintln!("cq watch: a signal may stop the watcher without a stop line ({e})");
    }
    let mut opts = WatchOptions::new(name);
    opts.takeover = takeover;
    opts.max_seconds = None;
    opts.member_key = members::sanitise_key(&format!("agy-{name}"));
    opts.mode = members::Mode::AgySession;
    opts.session = Some(record::SessionTarget {
        conversation,
        agentapi: agentapi.display().to_string(),
    });
    watch::watch(&root, &opts, &mut sink)?;
    Ok(exit::OK)
}

/// The `--agy-broker` options.
struct BrokerFlags {
    allow_from: Vec<String>,
    exchange: Option<PathBuf>,
    sandbox_sid: Option<String>,
}

/// Read `config.toml` in the queue root and use it for this process.
/// Returns the warning for a file with problems; the usable settings are
/// still used. Without a usable root there is no file; the command reports
/// the root problem itself.
fn load_config() -> Option<String> {
    let root = Root::from_env().ok()?;
    let (config, warning) = Config::load(&root);
    config::install(config);
    warning
}

/// Run one command. Returns the exit code of a run that did not fail:
/// 0, or 7 for a watcher that stopped because its name is asleep.
fn run(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Init => {
            let root = Root::from_env()?;
            root.init()?;
            println!("initialised {}", root.path().display());
        }
        Cmd::Send {
            from,
            to,
            kind,
            reply_to,
            parent,
            status,
            input,
        } => {
            let root = Root::open()?;
            let text = read_input(input.as_ref())?.ok_or_else(|| {
                CqError::usage(
                    "no input: pass {\"subject\": \"...\", \"body\": \"...\"} on stdin (a quoted heredoc) or with --input FILE",
                )
            })?;
            let SendInput { subject, body } = parse_input(&text)?;
            let recipient = to.clone();
            let out = Outgoing {
                from,
                to,
                kind,
                subject,
                body,
                reply_to,
                parent,
                status,
            };
            let id = send::send(&root, out, Hops::Lookup)?;
            println!("{id}");
            if sleep::is_asleep(&root, &recipient) {
                eprintln!("cq: {recipient} is asleep; wake it with /cq-start {recipient}");
            }
        }
        Cmd::Watch {
            name,
            takeover,
            max_seconds,
            rescan_seconds,
            no_fs_events,
            to_inbox,
            to_codex,
            codex_bin,
            codex_prompt,
            codex_check_seconds,
            codex_queue_db,
            debug,
            to_agy,
            agy_bin,
            agy_prompt,
            agy_broker,
            agy_allow_from,
            agy_exchange,
            agy_sandbox_sid,
            to_agy_session,
            idle_after,
            idle_every,
        } => {
            if to_agy_session {
                return watch_agy_session(&name, takeover);
            }
            // clap does not enforce `requires` once a conflicting mode flag
            // is given, so check that each mode's options come with it.
            if to_codex.is_none()
                && (codex_bin.is_some()
                    || codex_prompt.is_some()
                    || codex_check_seconds.is_some()
                    || codex_queue_db.is_some()
                    || debug)
            {
                return Err(CqError::usage(
                    "--codex-bin, --codex-prompt, --codex-check-seconds, --codex-queue-db and --debug need --to-codex",
                ));
            }
            if to_agy.is_none() && (agy_bin.is_some() || agy_prompt.is_some()) {
                return Err(CqError::usage("--agy-bin and --agy-prompt need --to-agy"));
            }
            if (to_agy.is_none() && agy_broker)
                || (!agy_broker
                    && (!agy_allow_from.is_empty()
                        || agy_exchange.is_some()
                        || agy_sandbox_sid.is_some()))
            {
                return Err(CqError::usage(
                    "--agy-broker needs --to-agy, and --agy-allow-from, --agy-exchange and --agy-sandbox-sid need --agy-broker",
                ));
            }
            // The Unix trust rule reads only owners and mode bits, so an ACL
            // (macOS extended ACLs, POSIX ACLs) could still let another user
            // swap a folder of the exchange path.
            if agy_broker && !cfg!(windows) {
                return Err(CqError::usage(
                    "--agy-broker is supported on Windows only: on Unix the exchange folder needs trust checks that read ACLs (macOS extended ACLs, POSIX ACLs), which cq does not have yet",
                ));
            }
            let mode = match (&to_codex, &to_agy, to_inbox) {
                (Some(_), _, _) => members::Mode::Codex,
                (None, Some(_), _) => members::Mode::Agy,
                (None, None, true) => members::Mode::Inbox,
                (None, None, false) => members::Mode::Monitor,
            };
            let opts = WatchOptions {
                member_key: match mode {
                    members::Mode::Codex => members::codex_key(&name),
                    members::Mode::Agy => members::agy_key(&name),
                    _ => members::session_key(),
                },
                name,
                takeover,
                max_seconds: max_seconds
                    .or((mode == members::Mode::Monitor).then_some(DEFAULT_MAX_SECONDS)),
                rescan: Duration::from_secs(rescan_seconds),
                fs_events: !no_fs_events,
                mode,
                idle_after,
                idle_every,
                target: if mode == members::Mode::Inbox {
                    cq::record::Target::from_env()
                } else {
                    None
                },
                session: None,
            };
            let ended = if let Some(thread) = &to_codex {
                let check_after = codex_check_seconds.map_or(CHECK_AFTER, Duration::from_secs);
                watch_to_codex(
                    &opts,
                    thread,
                    codex_bin,
                    codex_prompt,
                    check_after,
                    codex_queue_db,
                    debug,
                )?
            } else if let Some(conversation) = &to_agy
                && agy_broker
            {
                let flags = BrokerFlags {
                    allow_from: agy_allow_from,
                    exchange: agy_exchange,
                    sandbox_sid: agy_sandbox_sid,
                };
                watch_agy_broker(&opts, conversation, agy_bin, agy_prompt, flags)?
            } else if let Some(conversation) = &to_agy {
                watch_to_agy(&opts, conversation, agy_bin, agy_prompt)?
            } else if to_inbox {
                watch_to_inbox(&opts)?
            } else {
                let root = Root::open()?;
                // Silent on failure: under a Monitor, each stderr line wakes
                // the session.
                let _ = platform::install_exit_log_handler();
                watch::watch(&root, &opts, &mut Lines(io::stdout().lock()))?
            };
            if ended == Ended::Asleep {
                return Ok(exit::ASLEEP);
            }
        }
        Cmd::Dispatch {
            name,
            codex,
            workspace,
            instructions,
            allow_from,
            sandbox,
            timeout_seconds,
            once,
        } => {
            let root = Root::open()?;
            cq::dispatch::dispatch(
                &root,
                &cq::dispatch::Options {
                    name,
                    codex,
                    workspace,
                    instructions,
                    allow_from,
                    sandbox,
                    timeout: Duration::from_secs(timeout_seconds),
                    once,
                },
            )?;
        }
        Cmd::Claim { name, id } => {
            let root = Root::open()?;
            let bytes = claim::claim(&root, &name, &id)?;
            let mut out = io::stdout().lock();
            let _ = out.write_all(&bytes);
            if !bytes.ends_with(b"\n") {
                let _ = out.write_all(b"\n");
            }
            let _ = out.flush();
        }
        Cmd::Done {
            name,
            id,
            status,
            reply,
            reject,
            input,
        } => {
            let root = Root::open()?;
            // Input is read only when a flag asks for it, so a stdin pipe
            // that never closes cannot block a plain `cq done`. Without a
            // flag, stdin is only checked, without blocking, so that a reply
            // given without --reply is refused instead of lost.
            let input: DoneInput = if reply || reject {
                match read_input(input.as_ref())? {
                    Some(text) => parse_input(&text)?,
                    None => DoneInput::default(),
                }
            } else if input.is_some() {
                return Err(CqError::usage("--input needs --reply or --reject"));
            } else if platform::stdin_has_input() {
                return Err(CqError::usage(
                    "input given without --reply or --reject; nothing moved. Add --reply for {\"reply\": \"...\"} or --reject for {\"reason\": \"...\"}",
                ));
            } else {
                DoneInput::default()
            };
            let status = status.map(|s| match s {
                ReplyStatus::Ok => Status::Ok,
                ReplyStatus::Failed => Status::Failed,
            });
            let req = DoneRequest {
                name,
                id: id.clone(),
                status,
                reply,
                reject,
                input,
            };
            let report = done::done(&root, req)?;
            println!(
                "done id={id} moved={} reply={}",
                report.moved_to,
                report.reply_id.as_deref().unwrap_or("none")
            );
            if let Some(note) = report.note {
                eprintln!("cq: {note}");
            }
        }
        Cmd::List {
            name,
            stale_minutes,
            rejected,
            overdue: show_overdue,
            unclaimed_minutes,
            claimed_minutes,
        } => {
            let lines = if show_overdue {
                let limits =
                    overdue::Limits::from_env()?.with_minutes(unclaimed_minutes, claimed_minutes);
                overdue::list(&Root::open()?, &limits)
            } else if unclaimed_minutes.is_some() || claimed_minutes.is_some() {
                // Checked here: clap counts a flag's default as present.
                return Err(CqError::usage(
                    "--unclaimed-minutes and --claimed-minutes need --overdue",
                ));
            } else {
                let name = name.ok_or_else(|| CqError::usage("--as is required"))?;
                if rejected {
                    list::rejected(&Root::open()?, &name)?
                } else {
                    list::list(&Root::open()?, &name, stale_minutes)?
                }
            };
            for line in lines {
                println!("{line}");
            }
        }
        Cmd::Leave { name } => {
            cq::names::check_name("--as", &name)?;
            let root = Root::open()?;
            let removed = members::leave(&root, &name)?;
            if removed.is_empty() {
                println!("no session recorded for {name}");
            } else {
                println!("left: {name} (session {})", removed.join(", "));
            }
        }
        Cmd::Requeue { name, id } => {
            let root = Root::open()?;
            let note = cq::requeue::requeue(&root, &name, &id)?;
            println!("requeued id={id} to=inbox/{name}/");
            if let Some(note) = note {
                eprintln!("cq: {note}");
            }
        }
        Cmd::Members => {
            let root = Root::open()?;
            let records = members::members(&root);
            let asleep = sleep::asleep(&root);
            let mark = |name: &str| {
                if asleep.iter().any(|n| n == name) {
                    " (asleep)"
                } else {
                    ""
                }
            };
            for (key, name) in &records {
                println!("{key} -> {name}{}", mark(name));
            }
            for name in asleep
                .iter()
                .filter(|n| !records.iter().any(|r| &r.1 == *n))
            {
                println!("- -> {name} (asleep)");
            }
        }
        Cmd::Sleep {
            actor,
            name,
            reason,
        } => {
            let root = Root::open()?;
            sleep::sleep(&root, &actor, &name, reason.as_deref())?;
            println!("asleep: {name} (by {actor})");
        }
        Cmd::Wake { actor, name } => {
            let root = Root::open()?;
            if sleep::wake(&root, &actor, &name)? {
                println!("awake: {name} (by {actor})");
            } else {
                println!("{name} was not asleep");
            }
        }
        Cmd::Status { name } => return status(&name),
        Cmd::Lock { cmd } => return lock_cmd(cmd),
        Cmd::SessionStart { debug } => session_start(debug),
        Cmd::IdleAlert {
            every,
            debug,
            hold_ms,
        } => idle_alert(every, debug, Duration::from_millis(hold_ms)),
        Cmd::TurnEnd { debug } => turn_end(debug),
    }
    Ok(exit::OK)
}

/// `cq status`: one line, and the exit code for the state. Asleep wins over
/// running: a sleeping name's watcher is about to stop, and the session
/// must not start one.
fn status(name: &str) -> Result<i32> {
    cq::names::check_name("--as", name)?;
    let root = Root::open()?;
    let live = watch::Liveness::read(&root, name);
    let why = live.why_stopped();
    let mut facts = String::new();
    if let Some(why) = &why {
        facts.push_str(&format!(" why={}", event_text(why, EVENT_SUBJECT_MAX)));
    }
    if let Some(pid) = live.pid {
        facts.push_str(&format!(" pid={pid}"));
    }
    if let Some(age) = live.beat_age() {
        facts.push_str(&format!(" heartbeat={age}s"));
    }
    let known_mode = members::mode_of_name(&root, name);
    let mode = known_mode.map_or("unknown", members::Mode::as_str);
    // What the watcher recorded about itself; only for a pid file.
    let mut tags = String::new();
    if live.pid_file {
        let version = live.version();
        tags.push_str(&format!(" version={}", version.unwrap_or("unknown")));
        if record::is_outdated(version) {
            tags.push_str(" outdated");
        }
        let target = live.record.as_ref().and_then(|r| r.target.as_ref());
        // The caller's inbox is only comparable when the caller is this
        // name's session: its session key is recorded for this name (as
        // `cq session-start` finds it, also after /clear). Any other caller,
        // such as a coordinator checking a worker, has its own inbox, which
        // never matches.
        let own_session = members::session_key()
            .and_then(|k| members::name_for(&root, &k))
            .is_some_and(|n| n == name);
        if known_mode == Some(members::Mode::Inbox)
            && own_session
            && let Some(here) = record::Target::from_env()
            && target != Some(&here)
        {
            tags.push_str(" stale-target");
        }
    }
    let held = lock::held_by(&root, name);
    if !held.is_empty() {
        tags.push_str(&format!(" locks={}", held.join(",")));
    }
    let (line, code) = match sleep::read(&root, name) {
        Some(rec) => {
            let watcher = if why.is_none() { "running" } else { "stopped" };
            (
                format!(
                    "asleep name={name} {} watcher={watcher}{facts} mode={mode}{tags}",
                    rec.describe()
                ),
                exit::ASLEEP,
            )
        }
        None if why.is_none() => (
            format!("running name={name}{facts} mode={mode}{tags}"),
            exit::OK,
        ),
        None => (
            format!("stopped name={name}{facts} mode={mode}{tags}"),
            exit::NOT_RUNNING,
        ),
    };
    println!("{line}");
    Ok(code)
}

/// `cq lock`: one line on stdout, and the exit code `lock_help` lists.
fn lock_cmd(cmd: LockCmd) -> Result<i32> {
    let root = Root::open()?;
    let stale_after = lock::stale_after_from_env()?;
    // The held lock as `status` shows it; `None` once it is free again.
    let show = |name: &str| lock::inspect(&root, name, stale_after, timefmt::now());
    match cmd {
        LockCmd::Take {
            lock: name,
            actor,
            purpose,
            owner,
        } => match lock::take(&root, &name, &actor, owner, &purpose)? {
            lock::Taken::New(_) | lock::Taken::Already(_) => {
                let fields = match show(&name)? {
                    Some(l) => l.fields(),
                    None => format!("lock={name}"),
                };
                println!("taken {fields}");
            }
            lock::Taken::HeldBy(_) => {
                match show(&name)? {
                    Some(l) => println!("{}", l.line()),
                    // Released since: say so, still not taken.
                    None => println!("free lock={name} (released during the take; try again)"),
                }
                return Ok(exit::LOCK_HELD);
            }
        },
        LockCmd::Release {
            lock: name,
            actor,
            owner,
        } => {
            lock::release(&root, &name, &actor, owner)?;
            println!("released lock={name}");
        }
        LockCmd::Status { lock: name } => match show(&name)? {
            Some(l) => {
                println!("{}", l.line());
                return Ok(exit::LOCK_HELD);
            }
            None => println!("free lock={name}"),
        },
        LockCmd::Wait {
            lock: name,
            max,
            poll_ms,
            max_ms,
        } => {
            let max = max_ms
                .map(Duration::from_millis)
                .or(max.map(|m| Duration::from_secs(m.saturating_mul(60))));
            let poll = Duration::from_millis(poll_ms.max(1));
            match lock::wait(&root, &name, poll, max, stale_after)? {
                lock::Waited::Free => println!("free lock={name}"),
                lock::Waited::StillHeld(l) => {
                    match l {
                        Some(l) => println!("{}", l.line()),
                        None => println!("held lock={name}"),
                    }
                    return Ok(exit::LOCK_WAIT_TIMEOUT);
                }
            }
        }
        LockCmd::List => {
            for l in lock::list(&root, stale_after, timefmt::now()) {
                println!("{}", l.line());
            }
        }
        LockCmd::Break {
            lock: name,
            actor,
            reason,
        } => {
            let hold = lock::break_lock(&root, &name, &actor, &reason)?;
            let holder = hold.as_ref().map_or("?", |h| h.holder.as_str());
            println!("broken lock={name} holder={holder} by={actor}");
        }
    }
    Ok(exit::OK)
}

/// The SessionStart hook. Never fails, never blocks and, without
/// `--debug`, never writes to stderr: a hook must not break session start.
fn session_start(debug: bool) {
    drain_hook_input();
    let key = members::session_key();
    let root = Root::from_env().ok();
    let name = match (&key, &root) {
        (Some(k), Some(root)) => members::name_for(root, k),
        _ => None,
    };
    let mode = match (&key, &root, &name) {
        (Some(k), Some(root), Some(_)) => Some(members::mode_for(root, k)),
        _ => None,
    };
    if debug {
        eprintln!("cq session-start: key={key:?} name={name:?} mode={mode:?}");
    }
    let (Some(root), Some(name), Some(mode)) = (root, name, mode) else {
        return;
    };
    let candidates = config::candidates(
        ENV_RULES,
        config::installed().rules.as_deref(),
        BUILD_RULES_PATH,
    );
    let rules = rules_ref(&candidates);
    // A running inbox watcher whose record does not match this session's
    // inbox address and token, or this cq's version, may post into nothing:
    // Claude Code can give a resumed session a new token. Hooks get the same
    // variables as Bash commands.
    let inbox_live = (mode == members::Mode::Inbox)
        .then(|| watch::Liveness::read(&root, &name))
        .filter(watch::Liveness::running);
    let restart = inbox_live.as_ref().and_then(|live| {
        record::restart_reason(live.record.as_ref(), record::Target::from_env().as_ref())
    });
    let text = if let Some(rec) = sleep::read(&root, &name) {
        format!(
            "You are in the cq session queue as {name}, and {name} is asleep ({}). Do not start a watcher. Messages wait in your inbox. To wake, run /cq-start when the user or a coordinator's message asks you to.",
            rec.describe()
        )
    } else if let Some(why) = &restart {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). Your inbox watcher still runs, but {why}, so its posts may not reach this session. Restart it in the background with the Bash tool (not the Monitor tool): cq watch --as {name} --to-inbox --takeover 2>&1. Then run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if inbox_live.is_some() {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). Your inbox watcher (cq watch --as {name} --to-inbox) still runs and posts queue events into this session's inbox: do not start a watcher Monitor and do not use --takeover. Run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if mode == members::Mode::Inbox {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). Your inbox watcher has stopped. Start it again in the background with the Bash tool (not the Monitor tool): cq watch --as {name} --to-inbox. If it exits 5, an old watcher still holds the name: start it with --takeover. Then run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if mode == members::Mode::Codex && watch::is_running(&root, &name) {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). A Codex watcher (cq watch --as {name} --to-codex) still runs and wakes the Codex chat for {name} with codex queue: do not start a watcher for {name} here and do not use --takeover. Run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if mode == members::Mode::Codex {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). The Codex watcher for {name} (cq watch --as {name} --to-codex) has stopped, so nothing wakes the Codex chat for {name}. Do not start a watcher for {name} here: tell the user, who restarts it outside the chat (see docs/agents/CODEX.md). Run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if mode == members::Mode::Agy && watch::is_running(&root, &name) {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). An Antigravity CLI watcher (cq watch --as {name} --to-agy) still runs and wakes the conversation for {name} with agy turns: do not start a watcher for {name} here and do not use --takeover. Run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if mode == members::Mode::AgySession && watch::is_running(&root, &name) {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). An Antigravity session watcher (cq watch --as {name} --to-agy-session) still runs and wakes the Antigravity session for {name}: do not start a watcher for {name} here and do not use --takeover. Run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if mode == members::Mode::AgySession {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). The Antigravity session watcher for {name} (cq watch --as {name} --to-agy-session) has stopped, so nothing wakes the Antigravity session for {name}. Do not start a watcher for {name} here: tell the user (see docs/agents/ANTIGRAVITY.md). Run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else if mode == members::Mode::Agy {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). The Antigravity CLI watcher for {name} (cq watch --as {name} --to-agy) has stopped, so nothing wakes the conversation for {name}. Do not start a watcher for {name} here: tell the user, who restarts it outside the conversation (see docs/agents/ANTIGRAVITY.md). Run cq list --as {name} and handle any UNCLAIMED messages."
        )
    } else {
        format!(
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {rules} and follow it as {name} (skip choosing a name). Start your watcher Monitor (timeout_ms 1800000) on: cq watch --as {name} 2>&1. If it exits 5, your previous watcher belonged to the conversation before the clear: start it with --takeover. Then run cq list --as {name} and handle any UNCLAIMED messages."
        )
    };
    // Ignore a failed write, as stdout may already be closed.
    let _ = writeln!(io::stdout(), "{text} {SUBAGENT_NOTE}");
}

/// The idle_prompt Notification hook. Like `session-start`, it never
/// fails, never blocks for long and, without `--debug`, never writes to
/// stderr. It finds the name the same way.
fn idle_alert(every: Duration, debug: bool, hold: Duration) {
    drain_hook_input();
    let key = members::session_key();
    let root = Root::open().ok();
    let name = match (&key, &root) {
        (Some(k), Some(root)) => members::name_for(root, k),
        _ => None,
    };
    let outcome = match (&root, &name) {
        (Some(root), Some(name)) => Some(idle::alert(root, name, every, hold)),
        _ => None,
    };
    if debug {
        eprintln!("cq idle-alert: key={key:?} name={name:?} outcome={outcome:?}");
    }
}

/// The Stop hook. Like `session-start`, it never fails, never blocks for
/// long and, without `--debug`, never writes to stdout or stderr. It finds
/// the name the same way. Without valid hook input it records nothing.
fn turn_end(debug: bool) {
    // Not `read_hook_input`: its first-byte check gives up after a short
    // grace, and here one time limit covers the whole read.
    let input = (!io::stdin().is_terminal())
        .then(|| read_stdin_within(TURN_END_INPUT_WAIT))
        .flatten()
        .and_then(|b| idle::StopInput::parse(&b));
    let key = members::session_key();
    let root = Root::open().ok();
    let name = match (&key, &root) {
        (Some(k), Some(root)) => members::name_for(root, k),
        _ => None,
    };
    let outcome = match (&root, &name, &input) {
        (Some(root), Some(name), Some(input)) => Some(idle::mark_turn_end(root, name, input)),
        _ => None,
    };
    // Never the input itself: it holds text from the session's work.
    if debug {
        eprintln!(
            "cq turn-end: key={key:?} name={name:?} input={} outcome={outcome:?}",
            input.is_some()
        );
    }
}

/// A positive duration such as `90s`, `30m` or `1h` (jiff's friendly or
/// ISO 8601 format).
fn parse_every(s: &str) -> std::result::Result<Duration, String> {
    let d: jiff::SignedDuration = s.parse().map_err(|e| format!("{e}"))?;
    if d.is_positive() {
        Ok(d.unsigned_abs())
    } else {
        Err("must be more than 0".to_owned())
    }
}

/// `--idle-after`: a duration longer than the settle time after a turn
/// end, during which transcript writes do not count.
fn parse_idle_after(s: &str) -> std::result::Result<Duration, String> {
    let d = parse_every(s)?;
    if d > idle::SETTLE {
        Ok(d)
    } else {
        Err(format!("must be more than {}s", idle::SETTLE.as_secs()))
    }
}

/// Ends every `session-start` reminder. Subagents share the host session ID,
/// so the hook also fires in a subagent after it compacts (issue #5).
const SUBAGENT_NOTE: &str = "This reminder is for the main conversation only: a subagent ignores it and does not act as this queue name.";

/// The session rules file named in the `session-start` reminder: `CQ_RULES`
/// at run time, else `rules` in the root's `config.toml`, else the build's
/// `CQ_RULES_PATH`, else a generic pointer.
const ENV_RULES: &str = "CQ_RULES";
const BUILD_RULES_PATH: Option<&str> = option_env!("CQ_RULES_PATH");
const GENERIC_RULES: &str = "your cq session rules file (docs/SESSION-RULES.md in your cq setup)";

/// The first of the candidate paths (see `config::candidates`) that has no
/// character that can end or forge a line, else the generic wording. The
/// reminder tells each session to follow this path, so a candidate from any
/// source (`CQ_RULES`, `config.toml` after the root is joined, the build
/// default) with such a character is skipped whole, not cleaned.
fn rules_ref(candidates: &[String]) -> &str {
    candidates
        .iter()
        .find(|c| !c.chars().any(cq::message::breaks_line))
        .map_or(GENERIC_RULES, String::as_str)
}

/// Whether `cmd` is a hook. A hook prints a config warning only with
/// `--debug`.
fn is_hook(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::SessionStart { .. } | Cmd::IdleAlert { .. } | Cmd::TurnEnd { .. }
    )
}

/// Whether a hook command runs with `--debug`.
fn hook_debug(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::SessionStart { debug: true }
            | Cmd::IdleAlert { debug: true, .. }
            | Cmd::TurnEnd { debug: true }
    )
}

/// Read and drop the hook's JSON, if any, so the writer never sees a broken
/// pipe.
fn drain_hook_input() {
    let _ = read_hook_input(HOOK_INPUT_WAIT);
}

/// The hook's JSON, if any arrives within `wait`. The read runs on its own
/// thread with that time limit, so a pipe that never closes cannot hold up
/// the hook; the thread ends with the process.
fn read_hook_input(wait: Duration) -> Option<Vec<u8>> {
    if !platform::stdin_has_input() {
        return None;
    }
    read_stdin_within(wait)
}

/// All of stdin, if it ends within `wait`. The read runs on its own thread,
/// which ends with the process.
fn read_stdin_within(wait: Duration) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(read_stdin().ok());
    });
    rx.recv_timeout(wait).ok().flatten()
}

/// The JSON input text: from `--input FILE` (or `-`), else from stdin when
/// it is not a terminal. `None` when there is no input or it is empty.
/// With `--input FILE`, stdin is never touched.
fn read_input(input: Option<&PathBuf>) -> Result<Option<String>> {
    let bytes = match input {
        Some(p) if p.as_os_str() != "-" => std::fs::read(p)
            .map_err(|e| CqError::new(exit::USAGE, format!("read --input {}: {e}", p.display())))?,
        Some(_) => read_stdin()?,
        None if io::stdin().is_terminal() => return Ok(None),
        None => read_stdin()?,
    };
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| CqError::usage("the input is not valid UTF-8"))?;
    Ok((!text.trim().is_empty()).then_some(text))
}

fn read_stdin() -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    io::stdin()
        .read_to_end(&mut buf)
        .map_err(|e| CqError::io("read stdin", e))?;
    Ok(buf)
}

fn parse_input<T: DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| CqError::usage(format!("bad input JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_agy_session_takes_only_as_and_takeover() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["cq", "watch", "--as", "w", "--to-agy-session"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--takeover"]).is_ok());
        for extra in [
            &["--max-seconds", "5"][..],
            &["--to-inbox"],
            &["--to-agy", "c"],
            &["--to-codex", "t"],
            &["--idle-after", "10m"],
            &["--idle-every", "2h"],
            &["--rescan-seconds", "5"],
            &["--no-fs-events"],
        ] {
            assert!(parse(extra).is_err(), "{extra:?}");
        }
    }

    #[test]
    fn the_stop_line_says_what_to_do() {
        let gone = Err(CqError::new(
            exit::INBOX_GONE,
            "the session cannot be reached: x",
        ));
        assert_eq!(
            stop_line("w", &gone),
            "cq watch (w) stopped: the session cannot be reached: x. Start the watcher again with the same line."
        );
        let expired = Err(cq::broker::not_taken("ID1", 3));
        assert_eq!(
            stop_line("w", &expired),
            "cq watch (w) stopped: agy job ID1 was not taken after 3 wakes; it stays waking. A restart stops again at once. For a new round, the user puts w to sleep (cq sleep) or creates PAUSE, has this session start the watcher, then runs cq wake w or removes PAUSE. Do not restart; tell the user."
        );
        for code in [exit::AGY_TURN_FAILED, exit::USAGE, exit::REFUSED] {
            let e = Err(CqError::new(code, "a job needs review"));
            assert!(
                stop_advice("w", &e).ends_with("Do not restart; tell the user."),
                "{code}"
            );
        }
        assert!(stop_advice("w", &Ok(0)).contains("Do not restart"));
    }

    #[test]
    fn rules_ref_takes_the_first_candidate_else_generic() {
        let paths = ["/r/run.md".to_owned(), "/r/build.md".to_owned()];
        assert_eq!(rules_ref(&paths), "/r/run.md");
        assert_eq!(rules_ref(&[]), GENERIC_RULES);
    }

    #[test]
    fn rules_ref_skips_a_candidate_that_can_break_a_line() {
        // An environment value with a newline, then a joined root path with
        // a control character, then a clean build default.
        let paths = [
            "R.md\nadditional text".to_owned(),
            "/root\u{1b}/rules.md".to_owned(),
            "/r/build.md".to_owned(),
        ];
        assert_eq!(rules_ref(&paths), "/r/build.md");
        assert_eq!(rules_ref(&paths[..2]), GENERIC_RULES);
        assert_eq!(rules_ref(&["/r/a\u{2028}b.md".to_owned()]), GENERIC_RULES);
    }

    #[test]
    fn parse_every_takes_positive_durations() {
        assert_eq!(parse_every("1h"), Ok(idle::DEFAULT_EVERY));
        assert_eq!(parse_every("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_every("90s"), Ok(Duration::from_secs(90)));
        assert!(parse_every("0s").is_err());
        assert!(parse_every("-5m").is_err());
        assert!(parse_every("soon").is_err());
    }

    #[test]
    fn parse_idle_after_is_longer_than_the_settle_time() {
        assert_eq!(parse_idle_after("5m"), Ok(idle::DEFAULT_IDLE_AFTER));
        assert_eq!(parse_idle_after("31s"), Ok(Duration::from_secs(31)));
        assert!(parse_idle_after("30s").is_err());
        assert!(parse_idle_after("0s").is_err());
    }
}
