//! Command-line interface for `cq`. All queue logic lives in the library.

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde::de::DeserializeOwned;

use cq::done::{self, DoneRequest};
use cq::error::{CqError, Result, exit};
use cq::message::{
    DoneInput, MAX_BODY_BYTES, MAX_HOPS, MsgType, RATE_LIMIT, RATE_WINDOW, RECIPIENT_LIMIT,
    SENDER_LIMIT, SendInput, Status,
};
use cq::send::{self, Hops, Outgoing};
use cq::watch::{self, DEFAULT_MAX_SECONDS, DEFAULT_RESCAN_SECONDS, WatchOptions};
use cq::{Root, claim, list, members, platform, sleep, timefmt};

const EXIT_CODES: &str = "\
Exit codes:
  0   ok
  1   claim: the message is no longer in the inbox (already handled); ignore it
  2   a rename still failed after 3 retries
  3   claim: the message is invalid and went to the dead letter queue
  4   done: the message is finished, but the reply was not sent
  5   refused (name, size, hop or send limit, PAUSE, missing parent, watcher
      running, sleeping another name without being a coordinator)
  6   done: the message is not claimed by this session
  7   watch: the name is asleep; do not re-arm
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
and CQ_TZ_LABEL; here: {}.",
        timefmt::local_description()
    )
}

/// The limits `cq send` enforces (exit 5, nothing written), then the exit
/// codes. Built from the constants, so the text cannot drift from them.
fn send_help() -> String {
    let minutes = RATE_WINDOW.as_secs() / 60;
    format!(
        "Limits (each refusal exits 5 and writes nothing):
  body             at most {MAX_BODY_BYTES} bytes
  hops             refused at {MAX_HOPS} (from --parent and --reply-to)
  replies          only task messages get replies
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
    /// name.
    #[command(after_help = exit_help())]
    Watch {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// Stop the running watcher for this name first.
        #[arg(long)]
        takeover: bool,
        /// Exit after this many seconds (just under the Monitor's 1800 s cap).
        #[arg(long, default_value_t = DEFAULT_MAX_SECONDS,
              value_parser = clap::value_parser!(u64).range(1..))]
        max_seconds: u64,
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
        /// Decline the message: read {"reason": "..."}; a task's sender gets a
        /// rejected result.
        #[arg(long)]
        reject: bool,
        /// Read the JSON from this file instead of stdin ("-" for stdin).
        /// Needs --reply or --reject.
        #[arg(long, value_name = "FILE")]
        input: Option<PathBuf>,
    },

    /// List unclaimed, stale claimed and dead-lettered messages, oldest first.
    #[command(after_help = exit_help())]
    List {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// A claimed message counts as stale after this many minutes.
        #[arg(long, default_value_t = 60)]
        stale_minutes: u64,
    },

    /// Leave the queue: forget which Claude Code session owns this name.
    /// Exit 0 when there is nothing to forget.
    #[command(after_help = exit_help())]
    Leave {
        /// Your session name.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
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

    /// For a Claude Code SessionStart hook (clear|compact|resume). If this
    /// session owns a queue name, print a reminder to rejoin as it; else
    /// print nothing. Always exits 0.
    SessionStart {
        /// Print the session key and the lookup to stderr.
        #[arg(long)]
        debug: bool,
    },
}

/// How long `session-start` waits for the hook's JSON once bytes are
/// waiting. The content is not used.
const HOOK_INPUT_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, ValueEnum)]
enum ReplyStatus {
    Ok,
    Failed,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            // clap uses exit 2 for usage errors, which is RENAME_FAILED here.
            let code = if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            };
            return ExitCode::from(code as u8);
        }
    };
    match run(cli.cmd) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("cq: {e}");
            ExitCode::from(e.code as u8)
        }
    }
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
        } => {
            let root = Root::open()?;
            let opts = WatchOptions {
                name,
                takeover,
                max_seconds,
                rescan: Duration::from_secs(rescan_seconds),
                fs_events: !no_fs_events,
                member_key: members::session_key(),
            };
            if watch::watch(&root, &opts, &mut io::stdout().lock())? == watch::Ended::Asleep {
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
        } => {
            let root = Root::open()?;
            for line in list::list(&root, &name, stale_minutes)? {
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
        Cmd::SessionStart { debug } => session_start(debug),
    }
    Ok(exit::OK)
}

/// The SessionStart hook. Never fails, never blocks and, without
/// `--debug`, never writes to stderr: a hook must not break session start.
fn session_start(debug: bool) {
    drain_hook_input();
    let key = members::session_key();
    let name = match (&key, Root::from_env()) {
        (Some(k), Ok(root)) => members::name_for(&root, k),
        _ => None,
    };
    if debug {
        eprintln!("cq session-start: key={key:?} name={name:?}");
    }
    let asleep = match (&name, Root::from_env()) {
        (Some(n), Ok(root)) => sleep::read(&root, n),
        _ => None,
    };
    if let (Some(name), Some(rec)) = (&name, asleep) {
        // Ignore a failed write, as stdout may already be closed.
        let _ = writeln!(
            io::stdout(),
            "You are in the cq session queue as {name}, and {name} is asleep ({}). Do not start a watcher. Messages wait in your inbox. To wake, run /cq-start when the user or a coordinator's message asks you to.",
            rec.describe()
        );
    } else if let Some(name) = name {
        // Ignore a failed write, as stdout may already be closed.
        let _ = writeln!(
            io::stdout(),
            "You are in the cq session queue as {name}. Before anything else this turn: re-read {} and follow it as {name} (skip choosing a name). Start your watcher Monitor (timeout_ms 1800000) on: cq watch --as {name} 2>&1. If it exits 5, your previous watcher belonged to the conversation before the clear: start it with --takeover. Then run cq list --as {name} and handle any UNCLAIMED messages.",
            rules_ref(std::env::var(ENV_RULES).ok().as_deref(), BUILD_RULES_PATH)
        );
    }
}

/// The session rules file named in the `session-start` reminder: `CQ_RULES`
/// at run time, else the build's `CQ_RULES_PATH`, else a generic pointer.
const ENV_RULES: &str = "CQ_RULES";
const BUILD_RULES_PATH: Option<&str> = option_env!("CQ_RULES_PATH");
const GENERIC_RULES: &str = "your cq session rules file (docs/SESSION-RULES.md in your cq setup)";

/// The first non-empty of the run-time and build-time paths, else the
/// generic wording.
fn rules_ref<'a>(runtime: Option<&'a str>, build: Option<&'a str>) -> &'a str {
    [runtime, build]
        .into_iter()
        .flatten()
        .find(|p| !p.trim().is_empty())
        .unwrap_or(GENERIC_RULES)
}

/// Read and drop the hook's JSON, if any, so the writer never sees a broken
/// pipe. The read runs on its own thread with a time limit, so a pipe that
/// never closes cannot hold up the hook; the thread ends with the process.
fn drain_hook_input() {
    if !platform::stdin_has_input() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(read_stdin().is_ok());
    });
    let _ = rx.recv_timeout(HOOK_INPUT_WAIT);
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
    fn rules_ref_prefers_runtime_then_build_then_generic() {
        assert_eq!(
            rules_ref(Some("/r/run.md"), Some("/r/build.md")),
            "/r/run.md"
        );
        assert_eq!(rules_ref(Some(""), Some("/r/build.md")), "/r/build.md");
        assert_eq!(rules_ref(None, Some("/r/build.md")), "/r/build.md");
        assert_eq!(rules_ref(Some(" "), Some("")), GENERIC_RULES);
        assert_eq!(rules_ref(None, None), GENERIC_RULES);
    }
}
