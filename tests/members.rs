//! Session membership: `cq watch` records the owner of a name, and
//! `cq session-start` (the SessionStart hook) and `cq leave` use the record.

mod common;

use std::fs;
use std::io::Write;
use std::process::{Command, Output, Stdio};
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
