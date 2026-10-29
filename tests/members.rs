//! Session membership: `cq watch` records the owner of a name, and
//! `cq session-start` (the SessionStart hook) and `cq leave` use the record.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::*;

const HOST: &str = "CLAUDE_CODE_HOST_SESSION_ID";
const SESSION: &str = "CLAUDE_CODE_SESSION_ID";

fn reminder(name: &str) -> String {
    format!(
        "You are in the cq session queue as {name}. Before anything else this turn: re-read {RULES} and follow it as {name} (skip choosing a name). Start your watcher Monitor (timeout_ms 1800000) on: cq watch --as {name} 2>&1. If it exits 5, your previous watcher belonged to the conversation before the clear: start it with --takeover. Then run cq list --as {name} and handle any UNCLAIMED messages. This reminder is for the main conversation only: a subagent ignores it and does not act as this queue name.\n"
    )
}

/// Run `c` to completion with no input.
fn run(mut c: Command) -> Output {
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run cq")
}

/// A one-second `cq watch --as <name>` with these session variables.
fn watch_once(env: &Env, name: &str, vars: &[(&str, &str)]) {
    let mut c = env.cmd(&["watch", "--as", name, "--max-seconds", "1"]);
    c.envs(vars.iter().copied());
    let out = run(c);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

fn session_start(env: &Env, vars: &[(&str, &str)]) -> Output {
    let mut c = env.cmd(&["session-start"]);
    c.envs(vars.iter().copied());
    run(c)
}

fn member_name(env: &Env, key: &str) -> Option<String> {
    let text = fs::read_to_string(env.path(&format!("members/{key}"))).ok()?;
    text.lines().next().map(str::to_owned)
}

#[test]
fn watch_records_the_session() {
    let env = Env::new();
    watch_once(&env, "b", &[(HOST, "local_aaa"), (SESSION, "s-1")]);
    assert_eq!(member_name(&env, "local_aaa").as_deref(), Some("b"));
    assert!(!env.path("members/s-1").exists(), "the host ID wins");

    let text = fs::read_to_string(env.path("members/local_aaa")).unwrap();
    let time = text.lines().nth(1).expect("a time line");
    assert!(time.ends_with('Z') && time.len() == 20, "{time:?}");
    // Line 3: the watcher's working folder, for cq-top.
    let here = std::env::current_dir().unwrap().display().to_string();
    assert_eq!(text.lines().nth(2), Some(here.as_str()));
    // Line 4: the watcher's mode, for session-start.
    assert_eq!(text.lines().nth(3), Some("monitor"));

    // An empty host ID falls back to the session ID, sanitised.
    watch_once(&env, "c", &[(HOST, ""), (SESSION, "s:2/x")]);
    assert_eq!(member_name(&env, "s_2_x").as_deref(), Some("c"));

    // No session ID: nothing recorded.
    let before = fs::read_dir(env.path("members")).unwrap().count();
    watch_once(&env, "d", &[]);
    assert_eq!(fs::read_dir(env.path("members")).unwrap().count(), before);
}

#[test]
fn a_new_session_takes_the_name() {
    let env = Env::new();
    watch_once(&env, "b", &[(HOST, "local_old")]);
    watch_once(&env, "e", &[(HOST, "local_other")]);
    watch_once(&env, "b", &[(HOST, "local_new")]);
    assert!(!env.path("members/local_old").exists());
    assert_eq!(member_name(&env, "local_new").as_deref(), Some("b"));
    assert_eq!(member_name(&env, "local_other").as_deref(), Some("e"));

    let out = env.run(&["members"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "local_new -> b\nlocal_other -> e\n");
}

#[test]
fn members_dir_is_created_on_demand() {
    let env = Env::new();
    assert!(env.path("members").is_dir(), "cq init creates members/");
    fs::remove_dir(env.path("members")).unwrap();

    let out = session_start(&env, &[(HOST, "local_aaa")]);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout(&out), "");
    assert_eq!(code(&env.run(&["members"], None)), 0);
    assert_eq!(code(&env.run(&["leave", "--as", "b"], None)), 0);

    watch_once(&env, "b", &[(HOST, "local_aaa")]);
    assert_eq!(member_name(&env, "local_aaa").as_deref(), Some("b"));
}

#[test]
fn session_start_reminds_a_member_only() {
    let env = Env::new();
    watch_once(&env, "worker-a", &[(HOST, "local_aaa")]);

    let out = session_start(&env, &[(HOST, "local_aaa")]);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout(&out), reminder("worker-a"));
    assert_eq!(stderr(&out), "");

    for vars in [
        &[(HOST, "local_bbb")][..],
        &[],
        &[(HOST, ""), (SESSION, "")],
    ] {
        let out = session_start(&env, vars);
        assert_eq!(code(&out), 0, "{vars:?}");
        assert_eq!(stdout(&out), "", "{vars:?}");
        assert_eq!(stderr(&out), "", "{vars:?}");
    }

    // A root that is not initialised: still exit 0 and silent.
    let empty = tempfile::tempdir().unwrap();
    let mut c = env.cmd(&["session-start"]);
    c.env("CQ_ROOT", empty.path()).env(HOST, "local_aaa");
    let out = run(c);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
}

/// The hook's stdin may be a pipe that stays open, with or without the
/// hook JSON in it. `session-start` must still return at once.
#[test]
fn session_start_ignores_an_open_stdin() {
    let env = Env::new();
    watch_once(&env, "b", &[(HOST, "local_aaa")]);

    for input in [None, Some(r#"{"session_id":"x","source":"clear"}"#)] {
        let mut c = env.cmd(&["session-start"]);
        c.env(HOST, "local_aaa")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = Instant::now();
        let mut child = c.spawn().unwrap();
        let mut pipe = child.stdin.take().unwrap();
        if let Some(text) = input {
            pipe.write_all(text.as_bytes()).unwrap();
            pipe.flush().unwrap();
        }
        let finished = wait_until(Duration::from_secs(5), || {
            child.try_wait().unwrap().is_some()
        });
        let took = started.elapsed();
        if !finished {
            let _ = child.kill();
        }
        drop(pipe);
        let out = child.wait_with_output().unwrap();
        assert!(
            finished,
            "session-start blocked on an open stdin ({input:?})"
        );
        assert!(took < Duration::from_secs(2), "took {took:?} ({input:?})");
        assert_eq!(code(&out), 0);
        assert_eq!(stdout(&out), reminder("b"), "{input:?}");
    }
}

#[test]
fn leave_removes_the_record() {
    let env = Env::new();
    watch_once(&env, "b", &[(HOST, "local_aaa")]);
    watch_once(&env, "c", &[(HOST, "local_ccc")]);

    let out = env.run(&["leave", "--as", "b"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!env.path("members/local_aaa").exists());
    assert_eq!(member_name(&env, "local_ccc").as_deref(), Some("c"));
    assert_eq!(stdout(&session_start(&env, &[(HOST, "local_aaa")])), "");

    let again = env.run(&["leave", "--as", "b"], None);
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    assert_eq!(code(&env.run(&["leave", "--as", "../x"], None)), 5);
}

#[test]
fn generic_session_id_precedes_claude_and_empty_falls_back() {
    let env = Env::new();
    watch_once(
        &env,
        "codex",
        &[
            ("CQ_SESSION_ID", "codex:thread/1"),
            (HOST, "local_old"),
            (SESSION, "old"),
        ],
    );
    assert_eq!(
        member_name(&env, "codex_thread_1").as_deref(),
        Some("codex")
    );
    assert!(!env.path("members/local_old").exists());
    assert!(
        stdout(&session_start(&env, &[("CQ_SESSION_ID", "codex:thread/1")])).contains("as codex")
    );
    watch_once(
        &env,
        "fallback",
        &[("CQ_SESSION_ID", "  "), (HOST, "local_fallback")],
    );
    assert_eq!(
        member_name(&env, "local_fallback").as_deref(),
        Some("fallback")
    );
    let long = "x".repeat(90);
    watch_once(&env, "long", &[("CQ_SESSION_ID", &long)]);
    assert_eq!(member_name(&env, &"x".repeat(64)).as_deref(), Some("long"));
}

/// The fake Claude Code CLI (`tests/support/fake_claude.rs`), built once per
/// test run as `claude` in a folder of its own.
fn fake_claude_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fake-claude");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        let out = Command::new("rustc")
            .args(["--edition=2024", "tests/support/fake_claude.rs", "-o"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        path
    })
}

/// One fake `claude` process that runs `cq` commands for its sessions.
struct FakeClaude {
    child: Child,
    lines: BufReader<ChildStdout>,
}

/// One `cq` run inside a `FakeClaude`.
struct Step {
    code: i32,
    stdout: String,
    stderr: String,
}

impl FakeClaude {
    /// Start it with these extra variables, which every `cq` it runs
    /// inherits.
    fn start(env: &Env, vars: &[(&str, &str)]) -> FakeClaude {
        let mut c = env.cmd_for(fake_claude_bin(), &[]);
        c.env("CQ_BIN", CQ)
            .envs(vars.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().expect("start the fake claude");
        let lines = BufReader::new(child.stdout.take().unwrap());
        FakeClaude { child, lines }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Run `cq ARGS` with `CLAUDE_CODE_SESSION_ID=session` and `hook` on
    /// stdin (see the fake for the format).
    fn run(&mut self, session: &str, hook: &str, args: &str) -> Step {
        let pipe = self.child.stdin.as_mut().unwrap();
        writeln!(pipe, "{session}\t{hook}\t{args}").unwrap();
        pipe.flush().unwrap();
        let (mut stdout, mut stderr, mut in_err) = (String::new(), String::new(), false);
        loop {
            let mut line = String::new();
            assert!(
                self.lines.read_line(&mut line).unwrap() > 0,
                "fake claude ended"
            );
            if let Some(code) = line.strip_prefix("\x01END ") {
                return Step {
                    code: code.trim().parse().unwrap(),
                    stdout,
                    stderr,
                };
            }
            if line == "\x01ERR\n" {
                in_err = true;
            } else if in_err {
                stderr.push_str(&line);
            } else {
                stdout.push_str(&line);
            }
        }
    }

    /// A one-second Monitor watcher for `name` in session `session`.
    fn watch(&mut self, session: &str, name: &str) {
        let step = self.run(session, "-", &format!("watch --as {name} --max-seconds 1"));
        assert_eq!(step.code, 0, "{}", step.stderr);
    }

    /// The SessionStart hook in session `session`, from `hook`.
    fn session_start(&mut self, session: &str, hook: &str) -> Step {
        let step = self.run(session, hook, "session-start");
        assert_eq!(step.code, 0);
        assert_eq!(step.stderr, "");
        step
    }
}

impl Drop for FakeClaude {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn record_line(env: &Env, key: &str, n: usize) -> Option<String> {
    let text = fs::read_to_string(env.path(&format!("members/{key}"))).ok()?;
    text.lines().nth(n).map(str::to_owned)
}

/// Replace line 5 of a record (or drop it, for a record from an older `cq`).
fn set_owner_line(env: &Env, key: &str, owner: Option<&str>) {
    let path = env.path(&format!("members/{key}"));
    let text = fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = text.lines().take(4).collect();
    lines.extend(owner);
    fs::write(&path, lines.join("\n") + "\n").unwrap();
}

/// The CLI starts a new session ID on `/clear` and keeps its process. The
/// hook finds the record by that process and moves it to the new ID.
#[test]
fn cli_clear_rejoins_through_the_claude_process() {
    let env = Env::new();
    let mut cli = FakeClaude::start(&env, &[]);
    cli.watch("s-old", "cli-worker");

    let owner = record_line(&env, "s-old", 4).expect("an owner line");
    let (pid, start) = owner.split_once(' ').unwrap();
    assert_eq!(pid, cli.pid().to_string());
    assert!(start.parse::<u64>().unwrap() > 0, "{owner:?}");

    let step = cli.session_start("s-new", "clear");
    assert_eq!(step.stdout, reminder("cli-worker"));
    assert!(!env.path("members/s-old").exists());
    assert_eq!(member_name(&env, "s-new").as_deref(), Some("cli-worker"));
    assert_eq!(record_line(&env, "s-new", 4), Some(owner.clone()));
    assert_eq!(record_line(&env, "s-new", 3).as_deref(), Some("monitor"));

    // The moved record keeps its owner, so a second /clear works too.
    let step = cli.session_start("s-newer", "clear");
    assert_eq!(step.stdout, reminder("cli-worker"));
    assert_eq!(member_name(&env, "s-newer").as_deref(), Some("cli-worker"));
    assert_eq!(
        stdout(&env.run(&["members"], None)),
        "s-newer -> cli-worker\n"
    );
}

/// Only a `/clear` moves a record, and only for the session the hook JSON
/// names: a resume may open another conversation in the same process.
#[test]
fn cli_rejoin_needs_a_clear_for_this_session() {
    let env = Env::new();
    let mut cli = FakeClaude::start(&env, &[]);
    cli.watch("s-old", "cli-worker");

    for hook in [
        "-",
        "startup",
        "resume",
        "compact",
        "{not json",
        r#"{"session_id":"s-other","source":"clear"}"#,
        r#"{"source":"resume"}"#,
    ] {
        let step = cli.session_start("s-new", hook);
        assert_eq!(step.stdout, "", "{hook:?}");
        assert!(!env.path("members/s-new").exists(), "{hook:?}");
        assert_eq!(member_name(&env, "s-old").as_deref(), Some("cli-worker"));
    }

    // Hook JSON without a session ID is enough with source "clear".
    let step = cli.session_start("s-new", r#"{"source":"clear"}"#);
    assert_eq!(step.stdout, reminder("cli-worker"));
}

/// A record is moved only to the same process, started at the same time,
/// and only when that process holds exactly one record.
#[test]
fn cli_rejoin_refuses_a_wrong_or_unclear_owner() {
    let env = Env::new();
    let mut cli = FakeClaude::start(&env, &[]);
    cli.watch("s-old", "cli-worker");
    let owner = record_line(&env, "s-old", 4).unwrap();

    // Another CLI process, for example another terminal in the same folder.
    let mut other = FakeClaude::start(&env, &[]);
    assert_eq!(other.session_start("s-new", "clear").stdout, "");
    assert_eq!(member_name(&env, "s-old").as_deref(), Some("cli-worker"));

    // The same process ID with another start time: a reused ID.
    let (pid, start) = owner.split_once(' ').unwrap();
    let later = format!("{pid} {}", start.parse::<u64>().unwrap() + 1);
    set_owner_line(&env, "s-old", Some(&later));
    assert_eq!(cli.session_start("s-new", "clear").stdout, "");
    assert!(!env.path("members/s-new").exists());

    // A record from an older cq, with no owner line.
    set_owner_line(&env, "s-old", None);
    assert_eq!(cli.session_start("s-new", "clear").stdout, "");
    assert_eq!(member_name(&env, "s-old").as_deref(), Some("cli-worker"));

    // Two records of one process cannot be told apart: neither moves.
    set_owner_line(&env, "s-old", Some(&owner));
    cli.watch("s-two", "worker-b");
    assert_eq!(record_line(&env, "s-two", 4), Some(owner));
    assert_eq!(cli.session_start("s-new", "clear").stdout, "");
    assert_eq!(
        stdout(&env.run(&["members"], None)),
        "s-old -> cli-worker\ns-two -> worker-b\n"
    );
}

/// The desktop app's host ID and an explicit CQ_SESSION_ID outlive
/// `/clear`, so their records get no owner line and never move.
#[test]
fn stable_keys_record_no_owner_and_never_move() {
    let env = Env::new();
    let mut desktop = FakeClaude::start(&env, &[(HOST, "local_aaa")]);
    desktop.watch("s-old", "lead");
    assert_eq!(
        record_line(&env, "local_aaa", 3).as_deref(),
        Some("monitor")
    );
    assert_eq!(record_line(&env, "local_aaa", 4), None);
    // After /clear the host ID still matches, as before.
    assert_eq!(
        desktop.session_start("s-new", "clear").stdout,
        reminder("lead")
    );
    drop(desktop);

    // A new desktop session in another process, with no record of its own.
    let mut fresh = FakeClaude::start(&env, &[(HOST, "local_bbb")]);
    assert_eq!(fresh.session_start("s-other", "clear").stdout, "");
    assert_eq!(member_name(&env, "local_aaa").as_deref(), Some("lead"));
    drop(fresh);

    // A host ID with no record never falls back to the process, even in
    // the process that holds a CLI record.
    let mut cli = FakeClaude::start(&env, &[]);
    cli.watch("s-cli", "cli-worker");
    assert!(record_line(&env, "s-cli", 4).is_some());
    assert_eq!(cli.session_start("s-new,local_ccc", "clear").stdout, "");
    assert!(!env.path("members/local_ccc").exists());
    assert_eq!(member_name(&env, "s-cli").as_deref(), Some("cli-worker"));
    drop(cli);

    let mut pinned = FakeClaude::start(&env, &[("CQ_SESSION_ID", "worker-a")]);
    pinned.watch("s-old", "worker-a");
    assert_eq!(record_line(&env, "worker-a", 4), None);
    assert_eq!(
        pinned.session_start("s-new", "clear").stdout,
        reminder("worker-a")
    );
}
