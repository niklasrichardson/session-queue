//! Overdue tasks (issue #21): `cq list --overdue` across all inboxes, and a
//! coordinator watcher's `CQ-OVERDUE` lines, once per task across restarts.
//! Both read only.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::*;

const MIN: Duration = Duration::from_secs(60);

/// Make a file look as if it arrived, or was claimed, `age` ago.
fn set_age(path: &Path, age: Duration) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

fn inbox_file(env: &Env, to: &str, id: &str) -> PathBuf {
    env.path(&format!("inbox/{to}/{id}.json"))
}

fn claimed_file(env: &Env, to: &str, id: &str) -> PathBuf {
    env.path(&format!("inbox/{to}/.processing/{id}.json"))
}

fn list_overdue(env: &Env, flags: &[&str]) -> (i32, Vec<String>) {
    let mut args = vec!["list", "--overdue"];
    args.extend_from_slice(flags);
    let out = env.run(&args, None);
    (
        code(&out),
        stdout(&out).lines().map(str::to_owned).collect(),
    )
}

/// The `CQ-OVERDUE` lines among `lines`.
fn overdues(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.starts_with("CQ-OVERDUE "))
        .collect()
}

/// Two names, each with an overdue task; a young task, an old result, a
/// claimed task inside its limit and a finished task, none of them overdue.
struct Queue {
    env: Env,
    unclaimed: String,
    claimed: String,
}

fn queue() -> Queue {
    let env = Env::new();
    let unclaimed = env.send("lead", "worker-a", "task", "Run the tests", "BODY-1");
    set_age(&inbox_file(&env, "worker-a", &unclaimed), 45 * MIN);
    let young = env.send("lead", "worker-a", "task", "young", "BODY-2");
    set_age(&inbox_file(&env, "worker-a", &young), 29 * MIN);
    let result = env.send("lead", "worker-a", "info", "an old note", "BODY-3");
    set_age(&inbox_file(&env, "worker-a", &result), 600 * MIN);

    let claimed = env.send("lead", "design-a", "task", "Draw the page", "BODY-4");
    assert_eq!(code(&env.claim("design-a", &claimed)), 0);
    set_age(&claimed_file(&env, "design-a", &claimed), 150 * MIN);
    let recent_claim = env.send("lead", "design-a", "task", "recent claim", "BODY-5");
    assert_eq!(code(&env.claim("design-a", &recent_claim)), 0);
    set_age(&claimed_file(&env, "design-a", &recent_claim), 60 * MIN);
    let finished = env.send("lead", "design-a", "task", "finished", "BODY-6");
    assert_eq!(code(&env.claim("design-a", &finished)), 0);
    assert_eq!(
        code(&env.run(&["done", "--as", "design-a", &finished], None)),
        0
    );
    set_age(
        &env.path(&format!("done/design-a/{finished}.json")),
        600 * MIN,
    );
    Queue {
        env,
        unclaimed,
        claimed,
    }
}

#[test]
fn list_overdue_shows_overdue_tasks_in_every_inbox_and_no_bodies() {
    let q = queue();
    let (status, lines) = list_overdue(&q.env, &[]);
    assert_eq!(status, 0, "{lines:?}");
    assert_eq!(lines.len(), 3, "{lines:?}");
    // The tasks were sent in the same second, so either may come first.
    let line = |id: &str| lines.iter().find(|l| l.contains(id)).unwrap().as_str();
    let unclaimed = line(&q.unclaimed);
    assert!(unclaimed.starts_with("OVERDUE 20"), "{unclaimed}");
    assert!(
        unclaimed.contains(" UK) state=unclaimed age=45m from=lead to=worker-a id="),
        "{unclaimed}"
    );
    assert!(
        unclaimed.ends_with(" subject=\"Run the tests\""),
        "{unclaimed}"
    );
    let claimed = line(&q.claimed);
    assert!(
        claimed.contains(" state=claimed age=150m from=lead to=design-a id="),
        "{claimed}"
    );
    assert!(claimed.ends_with(" subject=\"Draw the page\""), "{claimed}");
    assert_eq!(
        lines[2],
        "TOTAL overdue=2 unclaimed_after=30m claimed_after=120m"
    );
    assert!(!lines.iter().any(|l| l.contains("BODY")), "{lines:?}");

    // Read only: every message is where it was.
    assert_eq!(q.env.ids("inbox/worker-a").len(), 3);
    assert_eq!(q.env.ids("inbox/design-a/.processing").len(), 2);
    assert_eq!(q.env.ids("done/design-a").len(), 1);
}

#[test]
fn list_overdue_works_while_paused() {
    let q = queue();
    fs::write(q.env.path("PAUSE"), "").unwrap();
    let (status, lines) = list_overdue(&q.env, &[]);
    assert_eq!(status, 0);
    assert_eq!(
        lines.last().unwrap(),
        "TOTAL overdue=2 unclaimed_after=30m claimed_after=120m"
    );
}

#[test]
fn limits_come_from_flags_then_the_environment() {
    let q = queue();
    // The young task (29 minutes) and the recent claim (60 minutes) join.
    let (status, lines) = list_overdue(
        &q.env,
        &["--unclaimed-minutes", "20", "--claimed-minutes", "60"],
    );
    assert_eq!(status, 0);
    assert_eq!(
        lines.last().unwrap(),
        "TOTAL overdue=4 unclaimed_after=20m claimed_after=60m"
    );

    let run_env = |vars: &[(&str, &str)], flags: &[&str]| {
        let mut args = vec!["list", "--overdue"];
        args.extend_from_slice(flags);
        let mut c = q.env.cmd(&args);
        for (k, v) in vars {
            c.env(k, v);
        }
        c.output().unwrap()
    };
    let out = run_env(&[("CQ_OVERDUE_UNCLAIMED_MINUTES", "20")], &[]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).ends_with("TOTAL overdue=3 unclaimed_after=20m claimed_after=120m\n"),
        "{}",
        stdout(&out)
    );
    // A flag wins over the variable.
    let out = run_env(
        &[("CQ_OVERDUE_CLAIMED_MINUTES", "60")],
        &["--claimed-minutes", "200"],
    );
    assert!(
        stdout(&out).ends_with("TOTAL overdue=1 unclaimed_after=30m claimed_after=200m\n"),
        "{}",
        stdout(&out)
    );
    // A bad value is a usage error.
    let out = run_env(&[("CQ_OVERDUE_CLAIMED_MINUTES", "two hours")], &[]);
    assert_eq!(code(&out), 64);
    assert!(
        stderr(&out).contains("CQ_OVERDUE_CLAIMED_MINUTES"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn list_overdue_flag_combinations() {
    let env = Env::new();
    let (status, lines) = list_overdue(&env, &[]);
    assert_eq!(status, 0);
    assert_eq!(
        lines,
        ["TOTAL overdue=0 unclaimed_after=30m claimed_after=120m"]
    );
    for args in [
        vec!["list"],
        vec!["list", "--overdue", "--as", "lead"],
        vec!["list", "--overdue", "--stale-minutes", "5"],
        vec!["list", "--as", "lead", "--unclaimed-minutes", "5"],
        vec!["list", "--overdue", "--claimed-minutes", "0"],
    ] {
        assert_eq!(code(&env.run(&args, None)), 64, "{args:?}");
    }
    // Plain `cq list --as` is unchanged.
    assert_eq!(code(&env.run(&["list", "--as", "lead"], None)), 0);
}

/// A coordinator's watcher prints one `CQ-OVERDUE` line per overdue task,
/// once, even across restarts. Other watchers print none, and nothing moves.
#[test]
fn coordinator_gets_cq_overdue_once_per_task() {
    let q = queue();
    let env = &q.env;
    fs::write(env.path("coordinators"), "lead\n").unwrap();

    let other = env.watch("worker-a", &["--max-seconds", "2"]);
    let lead = env.watch("lead", &["--max-seconds", "2"]);
    let lines = lead.lines_for(Duration::from_secs(4));
    let o = overdues(&lines);
    assert_eq!(o.len(), 2, "{lines:?}");
    let event = |id: &str| *o.iter().find(|l| event_id(l) == id).unwrap();
    let unclaimed = event(&q.unclaimed);
    assert!(unclaimed.starts_with("CQ-OVERDUE 20"), "{unclaimed}");
    assert!(
        unclaimed.contains(" UK) state=unclaimed age=45m from=lead to=worker-a "),
        "{unclaimed}"
    );
    let claimed = event(&q.claimed);
    assert!(
        claimed.contains(" state=claimed age=150m from=lead to=design-a "),
        "{claimed}"
    );
    assert!(!lines.iter().any(|l| l.contains("BODY")));
    assert!(overdues(&other.lines_for(Duration::from_secs(3))).is_empty());
    let mut marker: Vec<String> = fs::read_to_string(env.path("heartbeat/lead.overdue"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let mut want = vec![q.unclaimed.clone(), q.claimed.clone()];
    want.sort();
    marker.sort();
    assert_eq!(marker, want);

    // A restarted watcher does not repeat them, but reports a new one.
    let late = env.send("lead", "worker-a", "task", "late", "x");
    set_age(&inbox_file(env, "worker-a", &late), 31 * MIN);
    let lead = env.watch("lead", &["--max-seconds", "2"]);
    let lines = lead.lines_for(Duration::from_secs(4));
    let o = overdues(&lines);
    assert_eq!(o.len(), 1, "{lines:?}");
    assert_eq!(event_id(o[0]), late);

    // Nothing moved.
    assert_eq!(env.ids("inbox/worker-a").len(), 4);
    assert_eq!(env.ids("inbox/design-a/.processing").len(), 2);

    // A finished task is forgotten.
    assert_eq!(code(&env.claim("worker-a", &late)), 0);
    assert_eq!(
        code(&env.run(&["done", "--as", "worker-a", &late], None)),
        0
    );
    let lead = env.watch("lead", &["--max-seconds", "1"]);
    assert!(overdues(&lead.lines_for(Duration::from_secs(3))).is_empty());
    let marker = fs::read_to_string(env.path("heartbeat/lead.overdue")).unwrap();
    assert!(!marker.contains(&late), "{marker}");
    assert!(marker.contains(&q.unclaimed), "{marker}");
}

/// Under `PAUSE` a coordinator's watcher prints no `CQ-OVERDUE`, like
/// `CQ-NEW`; it reports the task once `PAUSE` is removed.
#[test]
fn cq_overdue_waits_for_pause_to_clear() {
    let q = queue();
    let env = &q.env;
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    fs::write(env.path("PAUSE"), "").unwrap();
    let lead = env.watch("lead", &["--max-seconds", "4"]);
    assert!(overdues(&lead.lines_for(Duration::from_secs(1))).is_empty());
    assert!(!env.path("heartbeat/lead.overdue").exists());
    fs::remove_file(env.path("PAUSE")).unwrap();
    let lines = lead.lines_for(Duration::from_secs(5));
    assert_eq!(overdues(&lines).len(), 2, "{lines:?}");
}

/// A bad limit stops a coordinator's watcher at start (exit 64), before it
/// takes the name. Other watchers do not read the limits.
#[test]
fn a_bad_limit_stops_a_coordinator_watcher() {
    let env = Env::new();
    fs::write(env.path("coordinators"), "lead\n").unwrap();
    let run = |name: &str| {
        env.cmd(&["watch", "--as", name, "--max-seconds", "1"])
            .env("CQ_OVERDUE_UNCLAIMED_MINUTES", "0")
            .output()
            .unwrap()
    };
    let out = run("lead");
    assert_eq!(code(&out), 64, "{}", stderr(&out));
    assert!(!env.path("heartbeat/lead.pid").exists());
    assert_eq!(code(&run("worker-a")), 0);
}
