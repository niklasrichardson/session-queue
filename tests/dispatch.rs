//! Native dispatch integration tests. The fake executable makes no model calls.
mod common;
use common::*;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

// Startup waits are not latency assertions; allow concurrent compiler/AV work.
const STARTUP_WAIT: Duration = Duration::from_secs(15);

fn agent(env: &Env) -> PathBuf {
    // Env owns the directory and removes the executable with its test queue.
    let path = env.path(if cfg!(windows) {
        "fake-codex.exe"
    } else {
        "fake-codex"
    });
    if !path.exists() {
        let out = Command::new("rustc")
            .args(["--edition=2024", "tests/support/fake_codex.rs", "-o"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
    }
    path
}
fn command(env: &Env, mode: &str) -> Command {
    fs::write(
        env.path("scope.md"),
        "Only summarise source files. Never change settings.",
    )
    .unwrap();
    let mut c = env.cmd(&[
        "dispatch",
        "--as",
        "b",
        "--allow-from",
        "a",
        "--timeout-seconds",
        "1",
    ]);
    c.arg("--codex")
        .arg(agent(env))
        .arg("--workspace")
        .arg(env.root())
        .arg("--instructions")
        .arg(env.path("scope.md"))
        .env("CQ_FAKE_MODE", mode)
        .env("CQ_FAKE_CALLS", env.path("calls.txt"))
        .env("CQ_FAKE_MARKER", env.path("orphan-marker"))
        .env("CQ_SESSION_ID", "parent-id")
        .env("CODEX_THREAD_ID", "parent-thread");
    c
}
fn dispatch(env: &Env, mode: &str) -> Output {
    command(env, mode).arg("--once").output().unwrap()
}
fn result(env: &Env, id: &str) -> serde_json::Value {
    let matches: Vec<_> = env
        .ids("inbox/a")
        .into_iter()
        .map(|rid| env.read_json(&format!("inbox/a/{rid}.json")))
        .filter(|r| r["reply_to"] == id)
        .collect();
    assert_eq!(matches.len(), 1);
    matches.into_iter().next().unwrap()
}

#[test]
fn serial_roundtrip_and_untrusted_text_stays_on_stdin() {
    let env = Env::new();
    let first = env.send(
        "a",
        "b",
        "task",
        "review",
        "literal $HOME `ticks`; don't change settings",
    );
    let second = env.send("a", "b", "task", "review again", "data");
    let out = dispatch(&env, "ok");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    for id in [&first, &second] {
        let reply = result(&env, id);
        assert_eq!(reply["status"], "ok");
        assert!(reply["body"].as_str().unwrap().contains("café"));
        assert_eq!(code(&env.claim("b", id)), 1);
        assert_eq!(
            env.read_json(&format!("dispatch/b/{id}/state.json"))["phase"],
            "completed"
        );
    }
    let calls = fs::read_to_string(env.path("calls.txt")).unwrap();
    assert_eq!(calls.lines().count(), 2);
    assert!(!calls.contains("literal $HOME"));
    assert!(calls.contains("--output-schema") && calls.contains("--ignore-user-config"));
    let prompt = fs::read_to_string(env.path("calls.txt.prompt")).unwrap();
    assert!(prompt.contains("Only summarise source files") && prompt.contains("untrusted data"));
    let again = dispatch(&env, "ok");
    assert_eq!(code(&again), 0);
    assert_eq!(
        fs::read_to_string(env.path("calls.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn non_tasks_invalid_messages_and_unlisted_senders_never_run_codex() {
    let env = Env::new();
    let info = env.send("a", "b", "info", "notice", "data");
    let rejected = env.send("other", "b", "task", "task", "data");
    let id = fake_id("a", "1234");
    env.plant("b", &id, b"not JSON");
    let out = dispatch(&env, "ok");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!env.path("calls.txt").exists());
    assert!(env.path(&format!("done/b/{info}.json")).exists());
    assert!(env.path(&format!("rejected/b/{rejected}.json")).exists());
    assert!(env.path(&format!("dlq/b/{id}.json")).exists());
    assert!(
        env.ids("inbox/other")
            .iter()
            .any(|rid| env.read_json(&format!("inbox/other/{rid}.json"))["status"] == "rejected")
    );
}

#[test]
fn failures_and_oversized_or_invalid_answers_become_failed_results() {
    for mode in ["fail", "stderr", "stderr-cut", "oversize", "empty"] {
        let env = Env::new();
        let id = env.send("a", "b", "task", "review", "data");
        let out = dispatch(&env, mode);
        assert_eq!(code(&out), 0, "{mode}: {}", stderr(&out));
        let reply = result(&env, &id);
        assert_eq!(reply["status"], "failed", "{mode}");
        assert!(reply["body"].as_str().unwrap().len() <= 20480);
        if mode.starts_with("stderr") {
            // `stderr-cut` splits a UTF-8 sequence at the 4096-byte read.
            assert!(
                reply["body"]
                    .as_str()
                    .unwrap()
                    .contains("synthetic authentication failure")
            );
        }
        if mode == "fail" {
            assert!(
                reply["body"]
                    .as_str()
                    .unwrap()
                    .contains("429 Too Many Requests")
            );
        }
        assert!(
            env.path(&format!("dispatch/b/{id}/answer.json")).exists()
                || mode == "fail"
                || mode.starts_with("stderr")
        );
    }
}

#[test]
fn agent_can_reject_a_task() {
    let env = Env::new();
    let id = env.send("a", "b", "task", "review", "data");
    let out = dispatch(&env, "reject");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(env.path(&format!("rejected/b/{id}.json")).exists());
    assert_eq!(result(&env, &id)["status"], "rejected");
}

#[test]
fn timeout_kills_the_agent_tree_and_requires_manual_review() {
    let env = Env::new();
    let id = env.send("a", "b", "task", "review", "data");
    let out = dispatch(&env, "timeout");
    assert_eq!(code(&out), 70, "{}", stderr(&out));
    assert!(stderr(&out).contains("timed out"));
    assert!(env.path(&format!("inbox/b/.processing/{id}.json")).exists());
    let again = dispatch(&env, "ok");
    assert_eq!(code(&again), 5);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !env.path("orphan-marker").exists(),
        "agent descendant survived timeout"
    );
    assert_eq!(
        fs::read_to_string(env.path("calls.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn pause_during_agent_run_preserves_completion_until_resume() {
    use std::process::Stdio;
    let env = Env::new();
    let id = env.send("a", "b", "task", "review", "data");
    let mut c = command(&env, "pause");
    c.arg("--once")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut worker = Watcher::spawn(c);
    assert!(wait_until(STARTUP_WAIT, || env.path("PAUSE").exists()));
    assert!(wait_until(STARTUP_WAIT, || env
        .path(&format!("dispatch/b/{id}/completion.json"))
        .exists()));
    assert!(env.path(&format!("inbox/b/.processing/{id}.json")).exists());
    assert!(!env.path(&format!("done/b/{id}.json")).exists());
    assert!(env.ids("inbox/a").is_empty());
    assert!(worker.child.try_wait().unwrap().is_none());
    fs::remove_file(env.path("PAUSE")).unwrap();
    assert!(worker.line(Duration::from_secs(5)).unwrap().contains(&id));
    assert_eq!(worker.child.wait().unwrap().code(), Some(0));
    assert_eq!(result(&env, &id)["status"], "ok");
    assert!(!env.path("heartbeat/b.pid").exists());
}

#[test]
fn reply_failure_stops_and_restart_cannot_replay_the_finished_task() {
    let env = Env::new();
    for _ in 0..20 {
        let out = env.send_raw(
            &[
                "--from", "b", "--to", "a", "--type", "result", "--status", "ok",
            ],
            "limit fixture",
            "data",
        );
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }
    let id = env.send("a", "b", "task", "review", "data");
    let out = dispatch(&env, "ok");
    assert_eq!(code(&out), 4, "{}", stderr(&out));
    assert!(env.path(&format!("done/b/{id}.json")).exists());
    assert!(
        !env.ids("inbox/a")
            .iter()
            .any(|rid| env.read_json(&format!("inbox/a/{rid}.json"))["reply_to"] == id)
    );
    let again = dispatch(&env, "ok");
    assert_eq!(code(&again), 5);
    assert!(stderr(&again).contains("unresolved dispatch record"));
    assert_eq!(
        fs::read_to_string(env.path("calls.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

/// Issue #83 M1: an unlisted sender past the pair rate limit cannot stop the
/// dispatcher, and the jobs whose rejection reply was refused do not block a
/// restart.
#[test]
fn unlisted_flood_past_the_rate_limit_keeps_the_dispatcher_running() {
    let env = Env::new();
    for _ in 0..20 {
        let out = env.send_raw(
            &[
                "--from", "b", "--to", "other", "--type", "result", "--status", "ok",
            ],
            "limit fixture",
            "data",
        );
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }
    let flood: Vec<String> = (0..3)
        .map(|_| env.send("other", "b", "task", "flood", "data"))
        .collect();
    let allowed = env.send("a", "b", "task", "review", "data");
    let out = dispatch(&env, "ok");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    for id in &flood {
        assert!(env.path(&format!("rejected/b/{id}.json")).exists());
        let state = env.read_json(&format!("dispatch/b/{id}/state.json"));
        assert_eq!(state["phase"], "completed");
        assert!(
            state["detail"].as_str().unwrap().contains("reply not sent"),
            "{state}"
        );
    }
    assert_eq!(result(&env, &allowed)["status"], "ok");
    let calls = || {
        fs::read_to_string(env.path("calls.txt"))
            .unwrap()
            .lines()
            .count()
    };
    assert_eq!(calls(), 1);
    let later = env.send("a", "b", "task", "later", "data");
    let again = dispatch(&env, "ok");
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    assert_eq!(result(&env, &later)["status"], "ok");
    assert_eq!(calls(), 2);
}

#[test]
fn watcher_lock_pause_and_missing_scope_prevent_agent_start() {
    let env = Env::new();
    env.send("a", "b", "task", "review", "data");
    let _fixture = agent(&env); // Compile before the timed lock holder starts.
    let _watcher = env.watch("b", &["--max-seconds", "60"]);
    assert!(wait_until(STARTUP_WAIT, || env
        .pid_of_watcher("b")
        .is_some()));
    assert_eq!(code(&dispatch(&env, "ok")), 5);
    drop(_watcher);
    fs::write(env.path("PAUSE"), "test").unwrap();
    assert_eq!(code(&dispatch(&env, "ok")), 5);
    fs::remove_file(env.path("PAUSE")).unwrap();
    let mut c = command(&env, "ok");
    fs::write(env.path("scope.md"), "").unwrap();
    assert_eq!(code(&c.output().unwrap()), 64);
    assert!(!env.path("calls.txt").exists());
}

#[test]
fn continuous_worker_handles_later_arrivals_and_pause_resume() {
    use std::process::Stdio;
    let env = Env::new();
    let first = env.send("a", "b", "task", "review", "data");
    fs::write(env.path("PAUSE"), "test").unwrap();
    let mut c = command(&env, "ok");
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut worker = Watcher::spawn(c);
    assert!(wait_until(STARTUP_WAIT, || env
        .path("heartbeat/b")
        .exists()));
    assert!(!env.path("calls.txt").exists());
    fs::remove_file(env.path("PAUSE")).unwrap();
    assert!(
        worker
            .line(Duration::from_secs(15))
            .unwrap_or_else(|| {
                let status = worker.child.try_wait().unwrap();
                let job = env.path(&format!("dispatch/b/{first}"));
                panic!("No dispatch output: status={status:?}, pause={}, calls={}, state={:?}, stderr={:?}",
                    env.path("PAUSE").exists(), env.path("calls.txt").exists(),
                    fs::read_to_string(job.join("state.json")), fs::read_to_string(job.join("stderr.log")));
            })
            .contains(&first)
    );
    let second = env.send("a", "b", "task", "later", "data");
    assert!(
        worker
            .line(Duration::from_secs(15))
            .unwrap_or_else(|| {
                let status = worker.child.try_wait().unwrap();
                let job = env.path(&format!("dispatch/b/{first}"));
                panic!("No dispatch output: status={status:?}, pause={}, calls={}, state={:?}, stderr={:?}",
                    env.path("PAUSE").exists(), env.path("calls.txt").exists(),
                    fs::read_to_string(job.join("state.json")), fs::read_to_string(job.join("stderr.log")));
            })
            .contains(&second)
    );
    assert_eq!(result(&env, &first)["status"], "ok");
    assert_eq!(result(&env, &second)["status"], "ok");
    assert_eq!(
        fs::read_to_string(env.path("calls.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    drop(worker); // No active task remains; release this test's worker lock.
}

#[test]
fn existing_job_folder_does_not_claim_the_message() {
    let env = Env::new();
    let id = env.send("a", "b", "task", "review", "data");
    let dir = env.path(&format!("dispatch/b/{id}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("state.json"),
        r#"{"phase":"completed","detail":"existing fixture"}"#,
    )
    .unwrap();
    let out = dispatch(&env, "ok");
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(stderr(&out).contains("already exists") && stderr(&out).contains("not claimed"));
    assert!(env.path(&format!("inbox/b/{id}.json")).exists());
    assert!(env.ids("inbox/b/.processing").is_empty());
    assert!(!env.path("calls.txt").exists());
}

#[test]
fn dispatcher_death_stops_the_agent_and_descendant() {
    use std::process::Stdio;
    let env = Env::new();
    let id = env.send("a", "b", "task", "review", "data");
    let mut c = command(&env, "timeout");
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut worker = Watcher::spawn(c);
    assert!(wait_until(STARTUP_WAIT, || env
        .path("calls.txt.ready")
        .exists()));
    #[cfg(windows)]
    worker.child.kill().unwrap(); // Forced death closes the dispatcher's job handle.
    #[cfg(unix)]
    {
        // SAFETY: signal only the owned dispatcher child, not a process group.
        assert_eq!(
            unsafe { libc::kill(worker.child.id() as i32, libc::SIGHUP) },
            0
        );
    }
    assert!(wait_until(STARTUP_WAIT, || worker
        .child
        .try_wait()
        .unwrap()
        .is_some()));
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !env.path("orphan-marker").exists(),
        "agent descendant survived dispatcher death"
    );
    assert!(env.path(&format!("inbox/b/.processing/{id}.json")).exists());
}

#[test]
fn stopping_an_agent_that_is_exiting_is_successful() {
    let env = Env::new();
    let executable = agent(&env);
    for _ in 0..12 {
        let mut command = Command::new(&executable);
        command.arg("--exit");
        let mut agent = cq::platform::spawn_agent(&mut command).unwrap();
        std::thread::yield_now();
        cq::platform::stop_agent(&mut agent).unwrap();
    }
}

#[test]
fn dispatcher_pid_heartbeat_cadence_and_watch_takeover() {
    use std::process::Stdio;
    let env = Env::new();
    let mut c = command(&env, "ok");
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut worker = Watcher::spawn(c);
    assert!(wait_until(STARTUP_WAIT, || env
        .path("heartbeat/b")
        .exists()));
    assert_eq!(env.pid_of_watcher("b"), Some(worker.child.id()));
    let first = fs::read_to_string(env.path("heartbeat/b")).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(fs::read_to_string(env.path("heartbeat/b")).unwrap(), first);
    assert!(wait_until(Duration::from_secs(32), || fs::read_to_string(
        env.path("heartbeat/b")
    )
    .unwrap()
        != first));
    let takeover = env.watch("b", &["--takeover", "--max-seconds", "10"]);
    assert!(wait_until(STARTUP_WAIT, || env.pid_of_watcher("b")
        == Some(takeover.child.id())));
    assert!(wait_until(STARTUP_WAIT, || worker
        .child
        .try_wait()
        .unwrap()
        .is_some()));
    drop(takeover);
}
