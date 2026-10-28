//! Per-session sleep (issue #7, stage 1): `cq sleep`, `cq wake`, the
//! watcher's `CQ-SLEEP` exit and a coordinator's `CQ-WAKE` lines, and how
//! `send`, `members`, `list` and `session-start` show a sleeping name.

mod common;

use std::fs;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, SystemTime};

use common::*;
use serde_json::Value;

const HOST: &str = "CLAUDE_CODE_HOST_SESSION_ID";
/// Longer than the watcher's 2-minute wait before `CQ-WAKE`.
const WAITED: Duration = Duration::from_secs(180);

fn sleep(env: &Env, actor: &str, name: &str, reason: Option<&str>) -> Output {
    let mut args = vec!["sleep", "--as", actor, name];
    if let Some(r) = reason {
        args.extend(["--reason", r]);
    }
    env.run(&args, None)
}

fn wake(env: &Env, actor: &str, name: &str) -> Output {
    env.run(&["wake", "--as", actor, name], None)
}

fn ready(env: &Env, name: &str) {
    assert!(
        wait_until(Duration::from_secs(10), || env
            .path(&format!("heartbeat/{name}"))
            .is_file()),
        "watcher {name} did not start"
    );
}

/// Make a file look as if it arrived `age` ago.
fn set_age(path: &Path, age: Duration) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

fn inbox_file(env: &Env, to: &str, id: &str) -> std::path::PathBuf {
    env.path(&format!("inbox/{to}/{id}.json"))
}

/// The `CQ-WAKE` lines among `lines`.
fn wakes(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| l.starts_with("CQ-WAKE ")).collect()
}

#[test]
fn a_name_sleeps_and_wakes_itself() {
    let env = Env::new();
    assert!(env.path("sleep").is_dir(), "cq init creates sleep/");

    let out = sleep(&env, "b", "b", Some("idle overnight"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "asleep: b (by b)\n");
    let rec = env.read_json("sleep/b");
    assert_eq!(rec["by"], "b");
    assert_eq!(rec["reason"], "idle overnight");
    let time = rec["time"].as_str().unwrap();
    assert!(time.ends_with('Z') && time.len() == 20, "{time:?}");

    // No reason: null.
    assert_eq!(code(&sleep(&env, "b", "b", None)), 0);
    assert_eq!(env.read_json("sleep/b")["reason"], Value::Null);

    let out = wake(&env, "b", "b");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "awake: b (by b)\n");
    assert!(!env.path("sleep/b").exists());

    let out = wake(&env, "b", "b");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "b was not asleep\n");
}

#[test]
fn only_a_coordinator_sleeps_another_name_and_anyone_wakes() {
    let env = Env::new();

    // No coordinators file: nobody may sleep another name.
    let out = sleep(&env, "a", "b", None);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cq: a may not sleep b"),
        "{}",
        stderr(&out)
    );
    assert!(!env.path("sleep/b").exists());

    fs::write(
        env.path("coordinators"),
        "# owner-edited\n\n queue-admin \n",
    )
    .unwrap();
    let out = sleep(&env, "a", "b", None);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    let out = sleep(&env, "queue-admin", "b", Some("idle"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(env.read_json("sleep/b")["by"], "queue-admin");

    // Any valid name may wake any name.
    let out = wake(&env, "a", "b");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!env.path("sleep/b").exists());

    // Names are checked like every other name.
    for (actor, name) in [("A", "b"), ("a", "../x"), ("a", "")] {
        assert_eq!(code(&sleep(&env, actor, name, None)), 5, "{actor} {name}");
        assert_eq!(code(&wake(&env, actor, name)), 5, "{actor} {name}");
    }
}

#[test]
fn a_root_without_sleep_dir_still_works() {
    let env = Env::new();
    fs::remove_dir(env.path("sleep")).unwrap();
    assert_eq!(code(&env.run(&["members"], None)), 0);
    assert_eq!(code(&env.run(&["list", "--as", "b"], None)), 0);
    env.send("a", "b", "info", "hi", "x");
    let out = env.run(&["watch", "--as", "b", "--max-seconds", "1"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(code(&wake(&env, "b", "b")), 0);
    assert_eq!(code(&sleep(&env, "b", "b", None)), 0);
    assert!(env.path("sleep/b").is_file());
}

#[test]
fn watcher_asleep_at_start_exits_7() {
    let env = Env::new();
    env.send("a", "b", "task", "waiting", "x");
    assert_eq!(code(&sleep(&env, "b", "b", Some("night"))), 0);

    let out = env.run(&["watch", "--as", "b"], None);
    assert_eq!(code(&out), 7, "{}", stderr(&out));
    assert_eq!(stdout(&out), "CQ-SLEEP by=b reason=\"night\"\n");
    assert_eq!(stderr(&out), "cq watch: exiting: asleep\n");
    assert!(!env.path("heartbeat/b.pid").exists());

    // An unreadable record still means asleep.
    fs::write(env.path("sleep/b"), "").unwrap();
    let out = env.run(&["watch", "--as", "b"], None);
    assert_eq!(code(&out), 7, "{}", stderr(&out));
    assert_eq!(stdout(&out), "CQ-SLEEP by=? reason=\"\"\n");
}

/// The flag appears while the watcher runs: it exits 7, frees its lock and
/// pid file, and after a wake a new watcher runs.
#[test]
fn watcher_exits_7_when_put_to_sleep_and_runs_again_after_wake() {
    let env = Env::new();
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    let mut w = env.watch("b", &["--rescan-seconds", "1"]);
    ready(&env, "b");
    assert_eq!(env.pid_of_watcher("b"), Some(w.child.id()));

    assert_eq!(code(&sleep(&env, "lead", "b", Some("idle"))), 0);
    let line = w.line(Duration::from_secs(5)).expect("a CQ-SLEEP line");
    assert_eq!(line, "CQ-SLEEP by=lead reason=\"idle\"");
    assert!(w.closed_within(Duration::from_secs(5)), "watcher exited");
    assert_eq!(w.child.wait().unwrap().code(), Some(7));
    assert!(w.stderr_text().contains("cq watch: exiting: asleep"));
    assert!(!env.path("heartbeat/b.pid").exists(), "pid file removed");

    assert_eq!(code(&wake(&env, "b", "b")), 0);
    let id = env.send("a", "b", "task", "after wake", "x");
    let again = env.watch("b", &["--rescan-seconds", "1"]);
    let line = again.line(Duration::from_secs(5)).expect("a CQ-NEW line");
    assert_eq!(event_id(&line), id, "{line}");
    assert_eq!(env.pid_of_watcher("b"), Some(again.child.id()));
}

/// `CQ-WAKE` goes to a coordinator's watcher only, once per message, for a
/// message that has waited 2 minutes for a name that is still asleep. It
/// moves nothing.
#[test]
fn coordinator_gets_cq_wake_after_two_minutes() {
    let env = Env::new();
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    let old = env.send("a", "b", "task", "old one", "x");
    let fresh = env.send("a", "b", "info", "fresh one", "x");
    set_age(&inbox_file(&env, "b", &old), WAITED);
    let awake_old = env.send("a", "c", "task", "c is awake", "x");
    set_age(&inbox_file(&env, "c", &awake_old), WAITED);
    assert_eq!(code(&sleep(&env, "b", "b", None)), 0);

    let other = env.watch("x", &["--rescan-seconds", "1"]);
    let lead = env.watch("lead", &["--rescan-seconds", "1"]);
    let lines = lead.lines_for(Duration::from_secs(3));
    let w = wakes(&lines);
    assert_eq!(
        w.len(),
        1,
        "only the message that waited 2 minutes: {lines:?}"
    );
    assert!(w[0].contains(" to=b from=a type=task "), "{}", w[0]);
    assert!(w[0].contains(" UK) "), "{}", w[0]);
    assert!(w[0].ends_with(" subject=\"old one\""), "{}", w[0]);
    assert_eq!(event_id(w[0]), old);

    // The fresh message passes 2 minutes: reported once.
    set_age(&inbox_file(&env, "b", &fresh), WAITED);
    let lines = lead.lines_for(Duration::from_secs(3));
    let w = wakes(&lines);
    assert_eq!(w.len(), 1, "{lines:?}");
    assert_eq!(event_id(w[0]), fresh);

    // Once b is awake, its waiting messages are not reported.
    assert_eq!(code(&wake(&env, "lead", "b")), 0);
    let late = env.send("a", "b", "task", "late", "x");
    set_age(&inbox_file(&env, "b", &late), WAITED);
    assert!(wakes(&lead.lines_for(Duration::from_secs(2))).is_empty());

    assert!(wakes(&other.lines_for(Duration::from_millis(200))).is_empty());
    let mut all = vec![old, fresh, late];
    all.sort();
    assert_eq!(env.ids("inbox/b"), all, "nothing moved");
    assert!(env.ids("inbox/b/.processing").is_empty());
    assert_eq!(env.ids("inbox/c"), vec![awake_old]);
}

#[test]
fn send_to_a_sleeping_name_delivers_with_a_warning() {
    let env = Env::new();
    assert_eq!(code(&sleep(&env, "b", "b", None)), 0);
    let out = env.send_raw(&["--from", "a", "--to", "b", "--type", "task"], "s", "x");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stderr(&out), "cq: b is asleep; wake it with /cq-start b\n");
    assert_eq!(env.ids("inbox/b"), vec![stdout(&out).trim().to_owned()]);

    let out = env.send_raw(&["--from", "a", "--to", "c", "--type", "task"], "s", "x");
    assert_eq!(code(&out), 0);
    assert_eq!(stderr(&out), "");
}

#[test]
fn members_and_list_show_sleeping_names() {
    let env = Env::new();
    let mut c = env.cmd(&["watch", "--as", "b", "--max-seconds", "1"]);
    c.env(HOST, "local_aaa").stdin(Stdio::null());
    assert!(c.output().unwrap().status.success());
    assert_eq!(code(&sleep(&env, "b", "b", Some("nap"))), 0);
    assert_eq!(code(&sleep(&env, "c", "c", None)), 0);

    let out = env.run(&["members"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "local_aaa -> b (asleep)\n- -> c (asleep)\n");

    let id = env.send("a", "b", "task", "waiting", "x");
    let out = env.run(&["list", "--as", "b"], None);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "ASLEEP by=b reason=\"nap\"", "{text}");
    assert!(lines[1].starts_with("UNCLAIMED ") && lines[1].contains(&id));
    assert_eq!(lines[2], "TOTAL unclaimed=1 stale=0 dlq=0");

    assert_eq!(code(&wake(&env, "b", "b")), 0);
    let out = env.run(&["list", "--as", "b"], None);
    assert!(!stdout(&out).contains("ASLEEP"));
}

#[test]
fn session_start_tells_a_sleeping_session_not_to_watch() {
    let env = Env::new();
    let mut c = env.cmd(&["watch", "--as", "worker-a", "--max-seconds", "1"]);
    c.env(HOST, "local_aaa").stdin(Stdio::null());
    assert!(c.output().unwrap().status.success());
    assert_eq!(code(&sleep(&env, "worker-a", "worker-a", Some("idle"))), 0);

    let mut c = env.cmd(&["session-start"]);
    c.env(HOST, "local_aaa").stdin(Stdio::null());
    let out = c.output().unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(stderr(&out), "");
    assert_eq!(
        stdout(&out),
        "You are in the cq session queue as worker-a, and worker-a is asleep (by=worker-a reason=\"idle\"). Do not start a watcher. Messages wait in your inbox. To wake, run /cq-start when the user or a coordinator's message asks you to.\n"
    );
}
