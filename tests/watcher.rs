//! `cq watch`: spec tests T5, T6, T9, T10 and T14, plus the start scan,
//! rescan, deduplication, `--max-seconds` and nested Git Bash tests.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use common::*;

const DAY: Duration = Duration::from_secs(24 * 3600);

/// Wait until the watcher has written its first heartbeat, which happens
/// after the lock, the pid file and the file-system events are set up.
fn ready(env: &Env, name: &str) {
    assert!(
        wait_until(Duration::from_secs(10), || env
            .path(&format!("heartbeat/{name}"))
            .is_file()),
        "watcher {name} did not start"
    );
}

fn age(path: &Path) -> Duration {
    let m = fs::metadata(path).unwrap().modified().unwrap();
    SystemTime::now().duration_since(m).unwrap_or_default()
}

fn set_age(path: &Path, age: Duration) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

#[test]
fn start_scan_finds_a_waiting_message() {
    let env = Env::new();
    let id = env.send("a", "b", "task", "waiting", "x");
    let w = env.watch("b", &[]);
    let line = w.line(Duration::from_secs(5)).expect("a CQ-NEW line");
    assert!(line.starts_with("CQ-NEW "), "{line}");
    assert_eq!(event_id(&line), id);
    assert!(line.contains(" to=b from=a type=task "), "{line}");
    assert!(line.contains(" UK) "), "{line}");
}

#[test]
fn file_system_events_find_a_new_message() {
    let env = Env::new();
    let w = env.watch("b", &[]); // rescan every 60 s
    ready(&env, "b");
    let sent = Instant::now();
    let id = env.send("a", "b", "info", "new", "x");
    let line = w.line(Duration::from_secs(5)).expect("a CQ-NEW line");
    assert_eq!(event_id(&line), id);
    assert!(sent.elapsed() < Duration::from_secs(5));
}

/// With no file-system events, the rescan still finds the message.
#[test]
fn rescan_finds_a_missed_message() {
    let env = Env::new();
    let w = env.watch("b", &["--no-fs-events", "--rescan-seconds", "2"]);
    ready(&env, "b");
    let id = env.send("a", "b", "task", "missed", "x");
    let line = w.line(Duration::from_secs(6)).expect("found by the rescan");
    assert_eq!(event_id(&line), id);
}

/// Events and rescans together report each message once.
#[test]
fn each_message_is_reported_once() {
    let env = Env::new();
    let w = env.watch("b", &["--rescan-seconds", "1"]);
    ready(&env, "b");
    let a = env.send("a", "b", "task", "one", "x");
    let b = env.send("c", "b", "task", "two", "x");
    let lines = w.lines_for(Duration::from_secs(4));
    let mut ids: Vec<&str> = lines.iter().map(|l| event_id(l)).collect();
    ids.sort();
    let mut want = vec![a.as_str(), b.as_str()];
    want.sort();
    assert_eq!(ids, want, "{lines:?}");
}

#[test]
fn max_seconds_exits_on_time() {
    let env = Env::new();
    let started = Instant::now();
    let mut w = env.watch("b", &["--max-seconds", "2"]);
    let status = w.child.wait().unwrap();
    let took = started.elapsed();
    assert!(status.success(), "{status:?}");
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_secs(5),
        "took {took:?}"
    );
    assert!(env.pid_of_watcher("b").is_none(), "pid file removed");
    let err = w.stderr_text();
    assert!(
        err.contains("cq watch: exiting: --max-seconds 2 reached"),
        "says why it exited: {err:?}"
    );
}

/// The watcher runs to `--max-seconds` when started through the nested
/// Git Bash form the Monitor uses (`bash -c "... eval '...' < /dev/null &&
/// ..."`), in which the parent of `cq.exe` is a forked bash. It never exits
/// early, and its exit is never silent.
#[cfg(windows)]
#[test]
fn nested_git_bash_watcher_runs_to_max_seconds() {
    use std::process::{Command, Stdio};

    let Some(bash) = git_bash() else { return };
    let env = Env::new();
    let cq = cq_for_bash();
    let forms = [
        format!(
            "true && eval '\"{cq}\" watch --as b --max-seconds 4 2>&1' < /dev/null && pwd -P > /dev/null"
        ),
        format!("eval '\"{cq}\" watch --as b --max-seconds 4'"),
        format!("bash -c \"eval '\\\"{cq}\\\" watch --as b --max-seconds 4 2>&1'\""),
    ];
    for form in &forms {
        let started = Instant::now();
        let out = Command::new(&bash)
            .args(["-c", form])
            .env("CQ_ROOT", env.root())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let took = started.elapsed();
        let text = format!("{}{}", stdout(&out), stderr(&out));
        assert!(out.status.success(), "{form}: {out:?}");
        assert!(
            took >= Duration::from_secs(4),
            "{form}: exited after {took:?}: {text}"
        );
        assert!(
            text.contains("exiting: --max-seconds 4 reached"),
            "{form}: {text:?}"
        );
    }
}

/// T5: nothing while `PAUSE` exists; waiting messages after it is removed.
/// `PAUSE` also refuses sends, so the message is sent before it.
#[test]
fn t5_pause() {
    let env = Env::new();
    let id = env.send("a", "b", "task", "paused", "x");
    fs::write(env.path("PAUSE"), "").unwrap();
    let w = env.watch("b", &["--rescan-seconds", "1"]);
    ready(&env, "b");
    let out = env.send_raw(&["--from", "a", "--to", "b", "--type", "task"], "s", "x");
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(
        w.lines_for(Duration::from_secs(3)).is_empty(),
        "silent while paused"
    );
    fs::remove_file(env.path("PAUSE")).unwrap();
    let lines = w.lines_for(Duration::from_secs(3));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(event_id(&lines[0]), id);
}

/// T6: after a restart, unclaimed messages are announced once and claimed
/// ones are not.
#[test]
fn t6_restart_announces_unclaimed_once() {
    let env = Env::new();
    let claimed = env.send("a", "b", "task", "claimed", "x");
    let waiting = env.send("a", "b", "task", "waiting", "x");
    assert_eq!(code(&env.claim("b", &claimed)), 0);
    for _ in 0..2 {
        let w = env.watch("b", &["--rescan-seconds", "1"]);
        let lines = w.lines_for(Duration::from_secs(3));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(event_id(&lines[0]), waiting);
        drop(w); // kills the watcher
    }
}

/// T9: the prune removes `done/` and `rejected/` files older than 14 days,
/// and never touches `dlq/`.
#[test]
fn t9_prune() {
    let env = Env::new();
    let make = |rel: &str, days: u32| {
        let p = env.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "{}").unwrap();
        set_age(&p, DAY * days);
        p
    };
    let young = make("done/b/20261010T000000Z-a-0013.json", 13);
    let old = make("done/b/20261008T000000Z-a-0015.json", 15);
    let old_rejected = make("rejected/b/20261008T000000Z-a-0015.json", 15);
    let old_reason = make("rejected/b/20261008T000000Z-a-0015.reason.json", 15);
    let old_dlq = make("dlq/b/20261008T000000Z-a-0015.json", 30);
    let other = make("done/c/20261008T000000Z-a-0015.json", 15);

    // This tests pruning, not watcher startup speed. A one-second lifetime
    // can expire during file-watcher initialisation on a busy host.
    let w = env.watch("b", &["--max-seconds", "15"]);
    assert!(
        wait_until(Duration::from_secs(10), || !old.exists()
            && !old_rejected.exists()
            && !old_reason.exists()),
        "old files were not pruned before the deadline"
    );
    assert!(young.exists(), "13 days old: kept");
    assert!(!old.exists(), "15 days old: removed");
    assert!(!old_rejected.exists() && !old_reason.exists());
    assert!(old_dlq.exists(), "dlq is never pruned");
    assert!(other.exists(), "only the watcher's own directories");
    drop(w);
}

/// T10: during 65 seconds the heartbeat is never more than 30 seconds old.
#[test]
fn t10_heartbeat() {
    let env = Env::new();
    let w = env.watch("b", &["--max-seconds", "70"]);
    ready(&env, "b");
    let hb = env.path("heartbeat/b");
    let until = Instant::now() + Duration::from_secs(65);
    let mut worst = Duration::ZERO;
    while Instant::now() < until {
        worst = worst.max(age(&hb));
        let text = fs::read_to_string(&hb).unwrap_or_default();
        if !text.is_empty() {
            assert!(text.trim().ends_with('Z'), "{text:?}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // 1 second of tolerance for file-time resolution and the 250 ms tick.
    assert!(
        worst <= Duration::from_secs(31),
        "heartbeat was {worst:?} old"
    );
    drop(w);
}

/// A second watcher for the same name refuses; `--takeover` stops the
/// first and runs alone.
#[test]
fn single_watcher_and_takeover() {
    let env = Env::new();
    let mut first = env.watch("b", &["--rescan-seconds", "1"]);
    ready(&env, "b");
    let first_pid = env.pid_of_watcher("b").unwrap();
    assert_eq!(first_pid, first.child.id());

    let refused = env.run(&["watch", "--as", "b", "--max-seconds", "5"], None);
    assert_eq!(code(&refused), 5, "{}", stderr(&refused));
    assert!(stderr(&refused).contains("already running"));
    assert_eq!(env.pid_of_watcher("b"), Some(first_pid));

    let second = env.watch("b", &["--takeover", "--rescan-seconds", "1"]);
    assert!(
        first.closed_within(Duration::from_secs(10)),
        "first watcher stopped"
    );
    assert!(!first.child.wait().unwrap().success());
    assert!(wait_until(Duration::from_secs(5), || env
        .pid_of_watcher("b")
        == Some(second.child.id())));

    let id = env.send("a", "b", "task", "after takeover", "x");
    let lines = second.lines_for(Duration::from_secs(3));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(event_id(&lines[0]), id);
}

/// An install renames the live `cq` executable and copies the new one in.
/// `--takeover` still stops a watcher that runs from the renamed file.
#[test]
fn takeover_stops_watcher_with_renamed_exe() {
    use std::process::{Command, Stdio};

    let env = Env::new();
    let bin = tempfile::tempdir().unwrap();
    let name = Path::new(CQ).file_name().unwrap().to_str().unwrap();
    let copy = bin.path().join(name);
    fs::copy(CQ, &copy).unwrap();
    let mut c = Command::new(&copy);
    c.args(["watch", "--as", "b", "--rescan-seconds", "1"])
        .args(["--max-seconds", "60"])
        .env("CQ_ROOT", env.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut first = Watcher::spawn(c);
    ready(&env, "b");
    assert_eq!(env.pid_of_watcher("b"), Some(first.child.id()));

    // The running file can be renamed, also on Windows.
    fs::rename(&copy, bin.path().join(format!("{name}.old-0156"))).unwrap();

    let second = env.watch("b", &["--takeover", "--rescan-seconds", "1"]);
    assert!(
        first.closed_within(Duration::from_secs(10)),
        "--takeover stopped the watcher with the renamed exe"
    );
    assert!(!first.child.wait().unwrap().success());
    assert!(wait_until(Duration::from_secs(5), || env
        .pid_of_watcher("b")
        == Some(second.child.id())));
}

/// T14: kill only the watcher's parent. The orphaned watcher keeps its lock
/// (there is no parent check; `--max-seconds` bounds it), so a second
/// watcher without `--takeover` refuses. With `--takeover` it stops the
/// first, runs alone and reports each message once.
#[cfg(windows)]
#[test]
fn t14_parent_killed() {
    use std::process::{Command, Stdio};

    let env = Env::new();
    let mut parent = Command::new("cmd");
    parent
        .args(["/C", CQ, "watch", "--as", "b", "--rescan-seconds", "1"])
        .args(["--max-seconds", "60"])
        .env("CQ_ROOT", env.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut first = Watcher::spawn(parent);
    ready(&env, "b");
    let cmd_pid = first.child.id();
    let watcher_pid = env.pid_of_watcher("b").unwrap();
    assert_ne!(watcher_pid, cmd_pid, "cq runs as a child of cmd");

    let m1 = env.send("a", "b", "task", "before", "x");
    let line = first
        .line(Duration::from_secs(5))
        .expect("first watcher reports m1");
    assert_eq!(event_id(&line), m1);
    assert_eq!(code(&env.claim("b", &m1)), 0);

    // Kill only cmd. The orphaned cq keeps running and holds the pipe open.
    first.child.kill().unwrap();
    let _ = first.child.wait();
    assert!(
        !first.closed_within(Duration::from_secs(2)),
        "the orphaned watcher is still running"
    );
    assert_eq!(env.pid_of_watcher("b"), Some(watcher_pid));
    let refused = env.run(&["watch", "--as", "b", "--max-seconds", "5"], None);
    assert_eq!(code(&refused), 5, "{}", stderr(&refused));

    let second = env.watch("b", &["--takeover", "--rescan-seconds", "1"]);
    assert!(
        first.closed_within(Duration::from_secs(10)),
        "--takeover stopped the orphaned watcher"
    );
    assert!(wait_until(Duration::from_secs(5), || env
        .pid_of_watcher("b")
        == Some(second.child.id())));
    let third = env.run(&["watch", "--as", "b", "--max-seconds", "5"], None);
    assert_eq!(code(&third), 5, "exactly one watcher runs");

    let m2 = env.send("a", "b", "task", "after", "x");
    let lines = second.lines_for(Duration::from_secs(4));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(event_id(&lines[0]), m2);
}
