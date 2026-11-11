//! `cq lock`: take, release, status, wait, list and break, the owner hold,
//! and a coordinator's orphan and stale reports.

mod common;

use std::fs;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use serde_json::Value;

use common::*;

fn lock(env: &Env, args: &[&str]) -> (i32, String, String) {
    let mut all = vec!["lock"];
    all.extend_from_slice(args);
    let out = env.run(&all, None);
    (
        code(&out),
        stdout(&out).trim_end().to_owned(),
        stderr(&out).trim_end().to_owned(),
    )
}

fn take(env: &Env, name: &str, actor: &str) -> (i32, String, String) {
    lock(env, &["take", name, "--as", actor, "--for", "GUI test"])
}

fn hold(env: &Env, name: &str) -> Value {
    env.read_json(&format!("locks/{name}.lock"))
}

/// Rewrite the take time of `name`'s hold to `secs` seconds ago.
fn age_hold(env: &Env, name: &str, secs: i64) {
    let mut h = hold(env, name);
    let then = Timestamp::now() - SignedDuration::from_secs(secs);
    h["taken"] = Value::String(then.strftime("%Y-%m-%dT%H:%M:%SZ").to_string());
    fs::write(
        env.path(&format!("locks/{name}.lock")),
        serde_json::to_vec_pretty(&h).unwrap(),
    )
    .unwrap();
}

/// Make `name`'s watcher look alive: the test process's PID (which `cq`
/// finds running) and a fresh heartbeat.
fn seem_alive(env: &Env, name: &str) {
    fs::write(
        env.path(&format!("heartbeat/{name}.pid")),
        format!("{}\n", std::process::id()),
    )
    .unwrap();
    let now = Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    fs::write(env.path(&format!("heartbeat/{name}")), format!("{now}\n")).unwrap();
}

/// The `CQ-LOCK-` lines a watcher prints within `d`.
fn lock_lines(w: &Watcher, d: Duration) -> Vec<String> {
    w.lines_for(d)
        .into_iter()
        .filter(|l| l.starts_with("CQ-LOCK-"))
        .collect()
}

fn locks_log(env: &Env) -> Vec<String> {
    fs::read_to_string(env.path("locks.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn take_status_release_and_the_log() {
    let env = Env::new();
    let (c, out, _) = lock(&env, &["status", "monitor"]);
    assert_eq!((c, out.as_str()), (0, "free lock=monitor"));

    let (c, out, err) = take(&env, "monitor", "worker-a");
    assert_eq!(c, 0, "{err}");
    assert!(
        out.starts_with("taken lock=monitor holder=worker-a since="),
        "{out}"
    );
    let h = hold(&env, "monitor");
    assert_eq!(h["holder"], "worker-a");
    assert_eq!(h["for"], "GUI test");
    assert!(h["taken"].as_str().unwrap().ends_with('Z'));
    assert!(h["taken_local"].as_str().unwrap().ends_with(" UK"), "{h}");
    assert!(h.get("session_key").is_none(), "{h}");

    // The holder again: exit 0, the first take kept.
    let (c, out, _) = lock(
        &env,
        &["take", "monitor", "--as", "worker-a", "--for", "other"],
    );
    assert_eq!(c, 0);
    assert!(out.contains(" for=\"GUI test\""), "{out}");

    // Anyone else: exit 1 with the holder on stdout.
    let (c, out, _) = take(&env, "monitor", "worker-b");
    assert_eq!(c, 1);
    assert!(
        out.starts_with("held lock=monitor holder=worker-a since="),
        "{out}"
    );
    assert!(
        out.contains(" (") && out.contains(" UK) age=0m for=\"GUI test\""),
        "{out}"
    );

    let (c, out, _) = lock(&env, &["status", "monitor"]);
    assert_eq!(c, 1);
    assert!(
        out.starts_with("held lock=monitor holder=worker-a "),
        "{out}"
    );
    assert!(!out.contains("orphaned") && !out.contains("stale"), "{out}");

    // Only the holder releases.
    let (c, _, err) = lock(&env, &["release", "monitor", "--as", "worker-b"]);
    assert_eq!(c, 1);
    assert!(err.contains("held by worker-a"), "{err}");
    let (c, out, _) = lock(&env, &["release", "monitor", "--as", "worker-a"]);
    assert_eq!((c, out.as_str()), (0, "released lock=monitor"));
    let (c, _, err) = lock(&env, &["release", "monitor", "--as", "worker-a"]);
    assert_eq!(c, 1);
    assert!(err.contains("not held"), "{err}");
    assert!(!env.path("locks/monitor.lock").exists());

    let log = locks_log(&env);
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(log[0].contains(" event=take lock=monitor holder=worker-a id="));
    assert!(log[0].ends_with(" for=\"GUI test\""), "{}", log[0]);
    assert!(log[1].contains(" event=release lock=monitor holder=worker-a id="));
    let id = h["id"].as_str().unwrap();
    assert!(
        log.iter().all(|l| l.contains(&format!(" id={id}"))),
        "{log:?}"
    );
}

/// Separate processes that all take one lock at once: exactly one exits 0,
/// every other one exits 1 and names the winner.
#[test]
fn of_many_concurrent_takes_exactly_one_wins() {
    const TAKERS: usize = 16;
    for round in 0..3 {
        let env = Env::new();
        let children: Vec<(String, Child)> = (0..TAKERS)
            .map(|i| {
                let name = format!("worker-{i}");
                let child = env
                    .cmd(&["lock", "take", "race", "--as", &name, "--for", "race"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                (name, child)
            })
            .collect();
        let results: Vec<(String, i32, String)> = children
            .into_iter()
            .map(|(name, c)| {
                let out = c.wait_with_output().unwrap();
                (name, code(&out), stdout(&out))
            })
            .collect();
        let winners: Vec<&String> = results
            .iter()
            .filter(|(_, c, _)| *c == 0)
            .map(|(n, _, _)| n)
            .collect();
        assert_eq!(winners.len(), 1, "round {round}: {results:?}");
        let winner = winners[0];
        assert_eq!(hold(&env, "race")["holder"], winner.as_str());
        for (name, c, out) in &results {
            if name != winner {
                assert_eq!(*c, 1, "round {round}: {name}: {out}");
                assert!(
                    out.contains(&format!(" holder={winner} ")),
                    "round {round}: {name}: {out}"
                );
            }
        }
        assert_eq!(locks_log(&env).len(), 1, "round {round}");
    }
}

#[test]
fn bad_names_owner_and_pause_are_refused() {
    let env = Env::new();
    for args in [
        ["take", "../desk", "--as", "worker-a", "--for", "x"],
        ["take", "Monitor", "--as", "worker-a", "--for", "x"],
        ["take", "monitor", "--as", "Worker", "--for", "x"],
        ["take", "monitor", "--as", "owner", "--for", "x"],
    ] {
        let (c, _, err) = lock(&env, &args);
        assert_eq!(c, 5, "{args:?}: {err}");
    }
    let (c, _, _) = lock(&env, &["status", "a/b"]);
    assert_eq!(c, 5);
    // Missing flags are usage errors.
    let (c, _, _) = lock(&env, &["take", "monitor", "--as", "worker-a"]);
    assert_eq!(c, 64);
    let (c, _, _) = lock(&env, &["wait", "monitor", "--max", "0"]);
    assert_eq!(c, 64);

    fs::write(env.path("PAUSE"), "").unwrap();
    let (c, _, err) = take(&env, "monitor", "worker-a");
    assert_eq!(c, 5);
    assert!(err.contains("PAUSE"), "{err}");
    assert!(!env.path("locks/monitor.lock").exists());
    // Reads still work while paused.
    assert_eq!(lock(&env, &["status", "monitor"]).0, 0);
}

/// A session whose key is recorded for one name cannot act as another.
#[test]
fn a_session_acts_only_as_its_recorded_name() {
    let env = Env::new();
    fs::write(env.path("members/key-a"), "worker-a\n").unwrap();
    let run = |actor: &str| {
        let out = env
            .cmd(&["lock", "take", "monitor", "--as", actor, "--for", "x"])
            .env("CQ_SESSION_ID", "key-a")
            .output()
            .unwrap();
        (code(&out), stderr(&out))
    };
    let (c, err) = run("worker-b");
    assert_eq!(c, 5);
    assert!(err.contains("recorded as worker-a"), "{err}");
    assert_eq!(run("worker-a").0, 0);
    assert_eq!(hold(&env, "monitor")["session_key"], "key-a");
}

#[test]
fn the_owner_hold() {
    let env = Env::new();
    let owner_take = [
        "take",
        "monitor",
        "--as",
        "lead",
        "--owner",
        "--for",
        "user testing",
    ];
    // Only a coordinator may take it.
    assert_eq!(lock(&env, &owner_take).0, 5);
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    let (c, out, err) = lock(&env, &owner_take);
    assert_eq!(c, 0, "{err}");
    assert!(
        out.contains(" holder=owner ") && out.contains(" by=lead "),
        "{out}"
    );
    assert_eq!(hold(&env, "monitor")["by"], "lead");

    // Nobody else takes or releases it, the coordinator included.
    assert_eq!(take(&env, "monitor", "worker-a").0, 1);
    assert_eq!(take(&env, "monitor", "lead").0, 1);
    let (c, _, err) = lock(&env, &["release", "monitor", "--as", "lead"]);
    assert_eq!(c, 1);
    assert!(err.contains("--owner"), "{err}");
    assert_eq!(
        lock(&env, &["release", "monitor", "--as", "worker-a", "--owner"]).0,
        5
    );
    // Never orphaned, however old.
    age_hold(&env, "monitor", 24 * 3600);
    let (c, out, _) = lock(&env, &["status", "monitor"]);
    assert_eq!(c, 1);
    assert!(!out.contains("orphaned") && !out.contains("stale"), "{out}");

    let (c, out, _) = lock(&env, &["release", "monitor", "--as", "lead", "--owner"]);
    assert_eq!((c, out.as_str()), (0, "released lock=monitor"));
    let log = locks_log(&env);
    assert!(
        log[1].contains(" event=release lock=monitor holder=owner ") && log[1].contains(" by=lead"),
        "{log:?}"
    );
}

#[test]
fn break_is_for_coordinators_and_logged() {
    let env = Env::new();
    assert_eq!(take(&env, "monitor", "worker-a").0, 0);
    let brk = |actor: &str, reason: &str| {
        lock(
            &env,
            &["break", "monitor", "--as", actor, "--reason", reason],
        )
    };
    assert_eq!(brk("worker-b", "mine now").0, 5);
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    assert_eq!(brk("lead", " ").0, 64);
    let (c, out, err) = brk("lead", "session ended");
    assert_eq!(c, 0, "{err}");
    assert_eq!(out, "broken lock=monitor holder=worker-a by=lead");
    assert!(!env.path("locks/monitor.lock").exists());
    assert_eq!(brk("lead", "again").0, 1);
    let log = locks_log(&env);
    assert!(
        log[1].contains(" event=break lock=monitor holder=worker-a id="),
        "{log:?}"
    );
    assert!(
        log[1].ends_with(" by=lead age=0m reason=\"session ended\""),
        "{log:?}"
    );
    // The old holder's release now fails; a new take works.
    assert_eq!(lock(&env, &["release", "monitor", "--as", "worker-a"]).0, 1);
    assert_eq!(take(&env, "monitor", "worker-b").0, 0);
}

#[test]
fn orphaned_and_stale_in_status_and_list() {
    let env = Env::new();
    assert_eq!(take(&env, "monitor", "worker-a").0, 0);
    assert_eq!(take(&env, "printer", "worker-b").0, 0);
    // worker-a has no watcher, and took it 10 minutes ago: orphaned.
    age_hold(&env, "monitor", 600);
    let (c, out, _) = lock(&env, &["status", "monitor"]);
    assert_eq!(c, 1);
    assert!(
        out.ends_with(" age=10m for=\"GUI test\" why=\"no pid file\" orphaned"),
        "{out}"
    );
    // worker-b's watcher runs, and it took it 3 hours ago: stale.
    seem_alive(&env, "worker-b");
    age_hold(&env, "printer", 3 * 3600);
    let (c, out, _) = lock(&env, &["status", "printer"]);
    assert_eq!(c, 1);
    assert!(out.ends_with(" age=180m for=\"GUI test\" stale"), "{out}");
    // Under a longer limit it is not.
    let out = env
        .cmd(&["lock", "status", "printer"])
        .env("CQ_LOCK_STALE_MINUTES", "240")
        .output()
        .unwrap();
    assert!(!stdout(&out).contains("stale"), "{}", stdout(&out));
    let out = env
        .cmd(&["lock", "list"])
        .env("CQ_LOCK_STALE_MINUTES", "soon")
        .output()
        .unwrap();
    assert_eq!(code(&out), 64);

    let (c, out, _) = lock(&env, &["list"]);
    assert_eq!(c, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].starts_with("held lock=monitor holder=worker-a "));
    assert!(lines[0].ends_with(" orphaned"));
    assert!(lines[1].starts_with("held lock=printer holder=worker-b "));
    assert!(lines[1].ends_with(" stale"));

    // cq status shows the locks a name holds.
    let out = env.run(&["status", "--as", "worker-b"], None);
    assert!(
        stdout(&out).trim_end().ends_with(" locks=printer"),
        "{}",
        stdout(&out)
    );
    let out = env.run(&["status", "--as", "worker-c"], None);
    assert!(!stdout(&out).contains("locks="), "{}", stdout(&out));
}

#[test]
fn wait_returns_when_free_and_at_max() {
    let env = Env::new();
    let fast = ["--poll-ms", "50"];
    let (c, out, _) = lock(&env, &["wait", "monitor", fast[0], fast[1]]);
    assert_eq!((c, out.as_str()), (0, "free lock=monitor"));

    assert_eq!(take(&env, "monitor", "worker-a").0, 0);
    let started = Instant::now();
    let (c, out, _) = lock(
        &env,
        &["wait", "monitor", fast[0], fast[1], "--max-ms", "500"],
    );
    assert!(started.elapsed() >= Duration::from_millis(500));
    assert_eq!(c, 2);
    assert!(
        out.starts_with("held lock=monitor holder=worker-a "),
        "{out}"
    );

    // A waiter returns once the holder releases.
    let mut waiter = Watcher::spawn({
        let mut c = env.cmd(&["lock", "wait", "monitor", fast[0], fast[1]]);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    });
    assert_eq!(waiter.line(Duration::from_millis(500)), None);
    assert_eq!(lock(&env, &["release", "monitor", "--as", "worker-a"]).0, 0);
    assert_eq!(
        waiter.line(Duration::from_secs(10)).as_deref(),
        Some("free lock=monitor")
    );
    assert_eq!(waiter.child.wait().unwrap().code(), Some(0));
    // It sent nothing.
    assert!(env.ids("inbox/worker-a").is_empty());
}

/// A coordinator's watcher reports an orphaned and a stale lock once each,
/// logs them, and does not report them again after a restart.
#[test]
fn a_coordinators_watcher_reports_each_take_once() {
    let env = Env::new();
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    assert_eq!(take(&env, "monitor", "worker-a").0, 0);
    assert_eq!(take(&env, "printer", "worker-b").0, 0);
    age_hold(&env, "monitor", 600);
    seem_alive(&env, "worker-b");
    age_hold(&env, "printer", 180);
    let start = || {
        let mut c = env.cmd(&["watch", "--as", "lead", "--max-seconds", "4"]);
        c.env("CQ_LOCK_STALE_MINUTES", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Watcher::spawn(c)
    };
    let w = start();
    let mut lines = lock_lines(&w, Duration::from_secs(6));
    lines.sort();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with("CQ-LOCK-ORPHAN ")
            && lines[0].contains(" lock=monitor holder=worker-a since=")
            && lines[0].ends_with(" why=\"no pid file\" orphaned"),
        "{lines:?}"
    );
    assert!(
        lines[1].starts_with("CQ-LOCK-STALE ")
            && lines[1].contains(" lock=printer holder=worker-b since=")
            && lines[1].ends_with(" age=3m for=\"GUI test\" stale"),
        "{lines:?}"
    );
    drop(w);
    let alerts = fs::read_to_string(env.path("heartbeat/lead.lock-alerts")).unwrap();
    assert_eq!(alerts.lines().count(), 2, "{alerts}");
    let log = locks_log(&env);
    assert!(
        log.iter()
            .any(|l| l.contains(" event=orphan lock=monitor ") && l.ends_with(" seen_by=lead")),
        "{log:?}"
    );
    assert!(
        log.iter().any(|l| l.contains(" event=stale lock=printer ")),
        "{log:?}"
    );

    // Restarted: nothing again. The holder's watcher keeps beating.
    seem_alive(&env, "worker-b");
    let w = start();
    assert_eq!(lock_lines(&w, Duration::from_secs(3)), Vec::<String>::new());
    drop(w);

    // A new take of the same lock is a new report.
    assert_eq!(
        lock(
            &env,
            &["break", "monitor", "--as", "lead", "--reason", "gone"]
        )
        .0,
        0
    );
    assert_eq!(take(&env, "monitor", "worker-c").0, 0);
    age_hold(&env, "monitor", 600);
    seem_alive(&env, "worker-b");
    let w = start();
    let lines = lock_lines(&w, Duration::from_secs(3));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(" lock=monitor holder=worker-c "),
        "{lines:?}"
    );
}

/// A coordinator's own old lock is stale to its own watcher: the pid file
/// that names the watcher's process is that watcher, not a reused PID.
#[test]
fn a_coordinators_watcher_reports_its_own_stale_lock() {
    let env = Env::new();
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    assert_eq!(take(&env, "monitor", "lead").0, 0);
    age_hold(&env, "monitor", 180);
    let mut c = env.cmd(&["watch", "--as", "lead", "--max-seconds", "3"]);
    c.env("CQ_LOCK_STALE_MINUTES", "2")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let w = Watcher::spawn(c);
    let lines = lock_lines(&w, Duration::from_secs(5));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("CQ-LOCK-STALE ")
            && lines[0].contains(" lock=monitor holder=lead since=")
            && lines[0].ends_with(" stale"),
        "{lines:?}"
    );
}

/// A worker's watcher does not report locks.
#[test]
fn a_workers_watcher_reports_no_locks() {
    let env = Env::new();
    assert_eq!(take(&env, "monitor", "worker-a").0, 0);
    age_hold(&env, "monitor", 600);
    let w = env.watch("worker-b", &["--max-seconds", "2"]);
    assert_eq!(lock_lines(&w, Duration::from_secs(3)), Vec::<String>::new());
}

/// The forms sessions use from Git Bash.
#[test]
fn git_bash_take_wait_and_release() {
    let Some(bash) = git_bash() else { return };
    let env = Env::new();
    let cq = cq_for_bash();
    let script = format!(
        "\"{cq}\" lock take monitor --as worker-a --for 'GUI test' || exit 9\n\
         \"{cq}\" lock wait monitor --poll-ms 50 --max-ms 200; echo \"wait=$?\"\n\
         \"{cq}\" lock release monitor --as worker-a; echo \"release=$?\"\n\
         \"{cq}\" lock status monitor; echo \"status=$?\"\n"
    );
    let out = std::process::Command::new(&bash)
        .args(["-c", &script])
        .env("CQ_ROOT", env.root())
        .env_remove("CQ_SESSION_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CODE_HOST_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(text.contains("wait=2\n"), "{text}{}", stderr(&out));
    assert!(text.contains("release=0\n"), "{text}");
    assert!(text.contains("free lock=monitor\nstatus=0\n"), "{text}");
}
