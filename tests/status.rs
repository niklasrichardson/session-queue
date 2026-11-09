//! `cq status` and the watcher exit log (`watch-exits.log`).

mod common;

use std::fs;
use std::process::Stdio;
use std::time::Duration;

use common::*;

const V: &str = env!("CARGO_PKG_VERSION");

/// Wait until the watcher has written its first heartbeat.
fn ready(env: &Env, name: &str) {
    assert!(
        wait_until(Duration::from_secs(10), || env
            .path(&format!("heartbeat/{name}"))
            .is_file()),
        "watcher {name} did not start"
    );
}

fn status(env: &Env, name: &str) -> (i32, String) {
    let out = env.run(&["status", "--as", name], None);
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
    (code(&out), stdout(&out).trim_end().to_owned())
}

fn exit_log(env: &Env) -> Vec<String> {
    fs::read_to_string(env.path("watch-exits.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The part of an exit line after its time stamp.
fn fields(line: &str) -> &str {
    line.split_once(' ').expect("a time stamp").1
}

#[test]
fn status_running_stopped_and_asleep() {
    let env = Env::new();
    let (c, line) = status(&env, "demo-a");
    assert_eq!(c, 10, "{line}");
    assert_eq!(
        line,
        r#"stopped name=demo-a why="no pid file" mode=unknown"#
    );

    // A watcher with a session key records its mode. `cq status` itself
    // runs with no session key.
    let mut cmd = env.cmd(&["watch", "--as", "demo-a", "--max-seconds", "30"]);
    cmd.env("CQ_SESSION_ID", "key-a")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut w = Watcher::spawn(cmd);
    ready(&env, "demo-a");
    let pid = w.child.id();
    let (c, line) = status(&env, "demo-a");
    assert_eq!(c, 0, "{line}");
    assert!(
        line.starts_with(&format!("running name=demo-a pid={pid} heartbeat=")),
        "{line}"
    );
    assert!(
        line.ends_with(&format!("s mode=monitor version={V}")),
        "{line}"
    );

    let out = env.run(
        &["sleep", "--as", "demo-a", "demo-a", "--reason", "done"],
        None,
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(w.child.wait().unwrap().code() == Some(7));
    let (c, line) = status(&env, "demo-a");
    assert_eq!(c, 7, "{line}");
    assert!(
        line.starts_with(
            r#"asleep name=demo-a by=demo-a reason="done" watcher=stopped why="no pid file" heartbeat="#
        ),
        "{line}"
    );
    assert!(line.ends_with(" mode=monitor"), "{line}");

    let out = env.run(&["wake", "--as", "demo-b", "demo-a"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let (c, line) = status(&env, "demo-a");
    assert_eq!(c, 10, "{line}");
    assert!(
        line.starts_with(r#"stopped name=demo-a why="no pid file" heartbeat="#),
        "{line}"
    );
}

/// A pid file left by a killed watcher, with an old heartbeat, reads as
/// stopped; with no heartbeat at all, too.
#[test]
fn status_stale_heartbeat_and_no_heartbeat() {
    let env = Env::new();
    fs::write(env.path("heartbeat/demo-a.pid"), "4242\n").unwrap();
    let (c, line) = status(&env, "demo-a");
    assert_eq!(c, 10, "{line}");
    assert_eq!(
        line,
        r#"stopped name=demo-a why="no heartbeat" pid=4242 mode=unknown version=unknown outdated"#
    );

    let old = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(300);
    fs::write(
        env.path("heartbeat/demo-a"),
        format!("{}\n", old.strftime("%Y-%m-%dT%H:%M:%SZ")),
    )
    .unwrap();
    let (c, line) = status(&env, "demo-a");
    assert_eq!(c, 10, "{line}");
    assert!(
        line.starts_with(r#"stopped name=demo-a why="heartbeat 30"#),
        "{line}"
    );
    assert!(line.contains(" pid=4242 heartbeat=30"), "{line}");
}

/// A pid file whose process has exited reads as stopped at once, even with
/// a fresh heartbeat.
#[test]
fn status_dead_process_is_stopped() {
    let env = Env::new();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cq"))
        .arg("--version")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    fs::write(env.path("heartbeat/demo-a.pid"), format!("{pid}\n")).unwrap();
    fs::write(
        env.path("heartbeat/demo-a"),
        format!("{}\n", cq::timefmt::rfc3339(cq::timefmt::now())),
    )
    .unwrap();
    let (c, line) = status(&env, "demo-a");
    assert_eq!(c, 10, "{line}");
    assert!(
        line.starts_with(&format!(
            r#"stopped name=demo-a why="process {pid} is not running" pid={pid} heartbeat="#
        )),
        "{line}"
    );
}

/// `version=` and `outdated` come from the watcher record; `stale-target`
/// from comparing an inbox watcher's record with the caller's variables.
#[test]
fn status_version_outdated_and_stale_target() {
    let env = Env::new();
    fs::create_dir_all(env.path("members")).unwrap();
    fs::write(
        env.path("members/local_aaa"),
        "demo-a\n2026-10-01T00:00:00Z\n/somewhere\ninbox\n",
    )
    .unwrap();
    // This test process stands in for the watcher: it is alive.
    let pid = std::process::id();
    fs::write(env.path("heartbeat/demo-a.pid"), format!("{pid}\n")).unwrap();
    fs::write(
        env.path("heartbeat/demo-a"),
        format!("{}\n", cq::timefmt::rfc3339(cq::timefmt::now())),
    )
    .unwrap();
    let record = |version: &str, token: &str| {
        fs::write(
            env.path("heartbeat/demo-a.watcher"),
            format!(
                "pid={pid}\nversion={version}\nsocket=sock-a\ntoken-sha256={}\n",
                cq::record::token_hash(token.as_bytes())
            ),
        )
        .unwrap();
    };
    let status_with = |token: Option<&str>| {
        let mut c = env.cmd(&["status", "--as", "demo-a"]);
        if let Some(t) = token {
            c.env("CLAUDE_CODE_MESSAGING_SOCKET", "sock-a")
                .env("CLAUDE_CODE_MESSAGING_TOKEN", t);
        }
        let out = c.output().unwrap();
        (code(&out), stdout(&out).trim_end().to_owned())
    };

    record(V, "token-a");
    let (c, line) = status_with(Some("token-a"));
    assert_eq!(c, 0, "{line}");
    assert!(
        line.ends_with(&format!(" mode=inbox version={V}")),
        "{line}"
    );

    record("1.10.2", "token-a");
    let (c, line) = status_with(Some("token-a"));
    assert_eq!(c, 0, "{line}");
    assert!(
        line.ends_with(" mode=inbox version=1.10.2 outdated"),
        "{line}"
    );

    // A new token in the caller's environment; without the variables there
    // is nothing to compare.
    record(V, "token-a");
    let (c, line) = status_with(Some("token-b"));
    assert_eq!(c, 0, "{line}");
    assert!(
        line.ends_with(&format!(" mode=inbox version={V} stale-target")),
        "{line}"
    );
    let (_, line) = status_with(None);
    assert!(
        line.ends_with(&format!(" mode=inbox version={V}")),
        "{line}"
    );
}

#[test]
fn status_refuses_a_bad_name() {
    let env = Env::new();
    let out = env.run(&["status", "--as", "../x"], None);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
}

#[test]
fn exit_log_on_max_seconds_and_asleep() {
    let env = Env::new();
    let mut w = env.watch("demo-a", &["--max-seconds", "1"]);
    let pid = w.child.id();
    assert!(w.child.wait().unwrap().success());
    let lines = exit_log(&env);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        fields(&lines[0]),
        format!(
            r#"name=demo-a pid={pid} mode=monitor version={V} exit=0 reason="--max-seconds 1 reached""#
        )
    );

    let mut w = env.watch("demo-a", &["--max-seconds", "30"]);
    ready(&env, "demo-a");
    let pid = w.child.id();
    let out = env.run(&["sleep", "--as", "demo-a", "demo-a"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(w.child.wait().unwrap().code(), Some(7));
    let lines = exit_log(&env);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        fields(&lines[1]),
        format!(r#"name=demo-a pid={pid} mode=monitor version={V} exit=7 reason="asleep""#)
    );

    // Asleep at start: the watcher never took the lock, so no line.
    let out = env.run(&["watch", "--as", "demo-a"], None);
    assert_eq!(code(&out), 7, "{}", stderr(&out));
    assert_eq!(exit_log(&env).len(), 2);
}

/// A watcher stopped by `--takeover` is ended outright on Windows (and by
/// SIGTERM with no handler under a Monitor on Unix), so the new watcher
/// writes its line. A start refused with exit 5 writes none.
#[test]
fn exit_log_on_takeover() {
    let env = Env::new();
    let mut first = env.watch("demo-a", &["--max-seconds", "30"]);
    ready(&env, "demo-a");
    let first_pid = first.child.id();

    let refused = env.run(&["watch", "--as", "demo-a", "--max-seconds", "5"], None);
    assert_eq!(code(&refused), 5, "{}", stderr(&refused));
    assert!(exit_log(&env).is_empty(), "{:?}", exit_log(&env));

    let mut second = env.watch("demo-a", &["--takeover", "--max-seconds", "2"]);
    let second_pid = second.child.id();
    assert!(!first.child.wait().unwrap().success());
    assert!(second.child.wait().unwrap().success());
    let lines = exit_log(&env);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        fields(&lines[0]),
        format!(
            r#"name=demo-a pid={first_pid} mode=unknown version={V} reason="stopped by --takeover from PID {second_pid}""#
        )
    );
    assert_eq!(
        fields(&lines[1]),
        format!(
            r#"name=demo-a pid={second_pid} mode=monitor version={V} exit=0 reason="--max-seconds 2 reached""#
        )
    );
}

/// A watcher killed outright leaves its pid file. The next watcher writes
/// the line it could not write, with its last heartbeat.
#[test]
fn exit_log_for_a_watcher_that_vanished() {
    let env = Env::new();
    let mut first = env.watch("demo-a", &["--max-seconds", "30"]);
    ready(&env, "demo-a");
    let first_pid = first.child.id();
    first.child.kill().unwrap();
    let _ = first.child.wait();
    let beat = fs::read_to_string(env.path("heartbeat/demo-a")).unwrap();
    assert!(exit_log(&env).is_empty());

    let mut second = env.watch("demo-a", &["--max-seconds", "1"]);
    let second_pid = second.child.id();
    assert!(second.child.wait().unwrap().success());
    let lines = exit_log(&env);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        fields(&lines[0]),
        format!(
            r#"name=demo-a pid={first_pid} mode=unknown version={V} reason="previous watcher ended without logging (last heartbeat {}); found by PID {second_pid}""#,
            beat.trim()
        )
    );
    assert!(
        fields(&lines[1]).starts_with(&format!("name=demo-a pid={second_pid} ")),
        "{lines:?}"
    );
}

/// A log over 1 MiB is renamed to `.old` before the next line.
#[test]
fn exit_log_is_capped() {
    let env = Env::new();
    let big = vec![b'x'; 1024 * 1024 + 1];
    fs::write(env.path("watch-exits.log"), &big).unwrap();
    let mut w = env.watch("demo-a", &["--max-seconds", "1"]);
    assert!(w.child.wait().unwrap().success());
    let lines = exit_log(&env);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains(" reason=\"--max-seconds 1 reached\""));
    assert_eq!(
        fs::metadata(env.path("watch-exits.log.old")).unwrap().len(),
        big.len() as u64
    );
}
