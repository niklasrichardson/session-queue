//! `cq idle-alert`, the Claude Code `Notification` (idle_prompt) hook: one
//! fixed `info` message to the lead of a listed worker, at most once per
//! interval.

mod common;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime};

use common::*;

const HOST: &str = "CLAUDE_CODE_HOST_SESSION_ID";
const KEY: &str = "local_aaa";

/// A queue root where session `KEY` owns `name` and `idle-alerts` holds
/// `config`.
fn setup(name: &str, config: &str) -> Env {
    let env = Env::new();
    fs::write(
        env.path(&format!("members/{KEY}")),
        format!("{name}\n2026-10-29T09:00:00Z\n/work\ninbox\n"),
    )
    .unwrap();
    fs::write(env.path("idle-alerts"), config).unwrap();
    env
}

/// `cq idle-alert` for session `KEY`, with the hook's JSON on stdin.
fn idle_alert(env: &Env, flags: &[&str]) -> Output {
    let mut args = vec!["idle-alert"];
    args.extend_from_slice(flags);
    let mut c = env.cmd(&args);
    c.env(HOST, KEY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().expect("spawn cq idle-alert");
    let mut pipe = child.stdin.take().unwrap();
    // What the hook sends, with text from the session's work in it.
    let _ = pipe.write_all(
        br#"{"session_id":"s-1","transcript_path":"/t/secret-plan.jsonl","hook_event_name":"Notification","notification_type":"idle_prompt","message":"Claude is waiting for your input: SECRET WORK"}"#,
    );
    drop(pipe);
    child.wait_with_output().unwrap()
}

/// Run the hook and check it is silent and exits 0.
fn quiet(env: &Env) {
    let out = idle_alert(env, &[]);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
}

fn record(env: &Env, name: &str) -> Option<String> {
    fs::read_to_string(env.path(&format!("idle/{name}"))).ok()
}

/// A UTC time `minutes` ago, as `cq` writes it.
fn minutes_ago(minutes: i64) -> String {
    let t = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(minutes);
    t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[test]
fn a_listed_worker_alerts_its_lead_with_fixed_text() {
    let env = setup("worker-a", "worker-a lead\n");
    quiet(&env);

    let ids = env.ids("inbox/lead");
    assert_eq!(ids.len(), 1, "{ids:?}");
    let msg = env.read_json(&format!("inbox/lead/{}.json", ids[0]));
    assert_eq!(msg["from"], "worker-a");
    assert_eq!(msg["to"], "lead");
    assert_eq!(msg["type"], "info");
    assert_eq!(msg["subject"], "worker-a is idle");
    let body = msg["body"].as_str().unwrap();
    let hhmm = body
        .strip_prefix("Idle since ")
        .and_then(|b| b.strip_suffix(" UK."))
        .unwrap_or_else(|| panic!("{body:?}"));
    assert!(hhmm.len() == 5 && hhmm.as_bytes()[2] == b':', "{body:?}");
    let text = serde_json::to_string(&msg).unwrap();
    assert!(!text.contains("SECRET") && !text.contains("secret-plan"));

    let rec = record(&env, "worker-a").expect("the alert time is recorded");
    assert!(
        rec.trim().ends_with('Z') && rec.trim().len() == 20,
        "{rec:?}"
    );
}

#[test]
fn an_unlisted_name_sends_nothing() {
    let env = setup("worker-b", "worker-a lead\n");
    quiet(&env);
    assert!(env.ids("inbox/lead").is_empty());
    assert!(record(&env, "worker-b").is_none());

    // No config file at all.
    fs::remove_file(env.path("idle-alerts")).unwrap();
    quiet(&env);
    assert!(env.ids("inbox/lead").is_empty());
}

#[test]
fn no_member_record_sends_nothing() {
    let env = setup("worker-a", "worker-a lead\n");
    fs::remove_file(env.path(&format!("members/{KEY}"))).unwrap();
    quiet(&env);

    // No session key either.
    let out = env.run(&["idle-alert"], Some("{}"));
    assert_eq!(code(&out), 0);
    assert_eq!(stderr(&out), "");
    assert!(env.ids("inbox/lead").is_empty());
    assert!(record(&env, "worker-a").is_none());
}

#[test]
fn alerts_at_most_once_per_interval() {
    let env = setup("worker-a", "worker-a lead\n");
    quiet(&env);
    quiet(&env);
    assert_eq!(env.ids("inbox/lead").len(), 1, "the second idle is skipped");

    // 59 minutes ago: still within the default hour.
    fs::write(env.path("idle/worker-a"), minutes_ago(59)).unwrap();
    quiet(&env);
    assert_eq!(env.ids("inbox/lead").len(), 1);

    // 61 minutes ago: alert again.
    fs::write(env.path("idle/worker-a"), minutes_ago(61)).unwrap();
    quiet(&env);
    assert_eq!(env.ids("inbox/lead").len(), 2);

    // --every changes the interval.
    fs::write(env.path("idle/worker-a"), minutes_ago(10)).unwrap();
    let out = idle_alert(&env, &["--every", "15m"]);
    assert_eq!(code(&out), 0);
    assert_eq!(env.ids("inbox/lead").len(), 2);
    let out = idle_alert(&env, &["--every", "5m"]);
    assert_eq!(code(&out), 0);
    assert_eq!(env.ids("inbox/lead").len(), 3);
}

#[test]
fn an_asleep_name_sends_nothing() {
    let env = setup("worker-a", "worker-a lead\n");
    let out = env.run(&["sleep", "--as", "worker-a", "worker-a"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    quiet(&env);
    assert!(env.ids("inbox/lead").is_empty());
    assert!(record(&env, "worker-a").is_none());
}

#[test]
fn a_refused_send_records_nothing() {
    let env = setup("worker-a", "worker-a lead\n");
    fs::write(env.path("PAUSE"), "").unwrap();
    quiet(&env);
    assert!(env.ids("inbox/lead").is_empty());
    assert!(
        record(&env, "worker-a").is_none(),
        "no record after a refusal"
    );

    // So the alert goes once PAUSE is gone.
    fs::remove_file(env.path("PAUSE")).unwrap();
    quiet(&env);
    assert_eq!(env.ids("inbox/lead").len(), 1);
}

#[test]
fn malformed_config_lines_are_ignored() {
    let config = "\
# worker lead
\r
worker-a
Worker-A boss
worker-a boss extra
worker-a ../x
#worker-a boss
  worker-a   lead  \r
worker-a second
";
    let env = setup("worker-a", config);
    quiet(&env);
    assert_eq!(env.ids("inbox/lead").len(), 1, "the first valid line wins");
    assert!(env.ids("inbox/boss").is_empty());
    assert!(env.ids("inbox/second").is_empty());
}

#[test]
fn debug_reports_the_result() {
    let env = setup("worker-a", "worker-a lead\n");
    let out = idle_alert(&env, &["--debug"]);
    assert_eq!(code(&out), 0);
    let err = stderr(&out);
    assert!(err.starts_with("cq idle-alert: "), "{err}");
    assert!(
        err.contains("name=Some(\"worker-a\")") && err.contains("Sent"),
        "{err}"
    );

    let out = idle_alert(&env, &["--every", "0s"]);
    assert_eq!(code(&out), 64, "a bad --every is a usage error");
}

#[test]
fn a_held_lock_sends_nothing() {
    let env = setup(
        "worker-a",
        "worker-a lead
",
    );
    fs::create_dir_all(env.path("idle")).unwrap();
    let lock = File::create(env.path("idle/worker-a.lock")).unwrap();
    lock.lock().unwrap();
    quiet(&env);
    assert!(env.ids("inbox/lead").is_empty());
    assert!(record(&env, "worker-a").is_none());

    drop(lock);
    quiet(&env);
    assert_eq!(env.ids("inbox/lead").len(), 1);
}

#[test]
fn parallel_hooks_send_one_alert() {
    // Claude Code runs matching hooks in parallel. Each hook waits 500 ms
    // between its check and its send, so without the lock all of them pass
    // the check and send.
    let env = Arc::new(setup(
        "worker-a",
        "worker-a lead
",
    ));
    let start = Arc::new(Barrier::new(8));
    let runs: Vec<_> = (0..8)
        .map(|_| {
            let (env, start) = (Arc::clone(&env), Arc::clone(&start));
            thread::spawn(move || {
                start.wait();
                idle_alert(&env, &["--hold-ms", "500"])
            })
        })
        .collect();
    for run in runs {
        let out = run.join().unwrap();
        assert_eq!(code(&out), 0);
        assert_eq!(stderr(&out), "");
    }
    assert_eq!(env.ids("inbox/lead").len(), 1);
}

/// `cq turn-end` for session `KEY`, with `input` as the Stop hook's JSON.
fn turn_end(env: &Env, input: &str) -> Output {
    turn_end_after(env, input, Duration::ZERO)
}

/// `cq turn-end`, with the hook's JSON written after `delay`.
fn turn_end_after(env: &Env, input: &str, delay: Duration) -> Output {
    let mut c = env.cmd(&["turn-end"]);
    c.env(HOST, KEY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().expect("spawn cq turn-end");
    let mut pipe = child.stdin.take().unwrap();
    std::thread::sleep(delay);
    let _ = pipe.write_all(input.as_bytes());
    drop(pipe);
    child.wait_with_output().unwrap()
}

/// The Stop hook's JSON for a transcript at `path`, with `tasks` as its
/// `background_tasks` (left out when `None`).
fn stop_input(path: &Path, tasks: Option<serde_json::Value>) -> String {
    let mut v = serde_json::json!({
        "session_id": "s-1",
        "transcript_path": path,
        "cwd": "/work",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "SECRET WORK",
        "session_crons": [{"id": "c-1", "schedule": "0 9 * * *", "recurring": true, "prompt": "SECRET"}],
    });
    if let Some(tasks) = tasks {
        v["background_tasks"] = tasks;
    }
    v.to_string()
}

fn marker(env: &Env, name: &str) -> Option<serde_json::Value> {
    let text = fs::read_to_string(env.path(&format!("idle/{name}.turn-end"))).ok()?;
    Some(serde_json::from_str(&text).unwrap())
}

/// A transcript for the hook's input, with one entry.
fn transcript(env: &Env) -> PathBuf {
    let tx = env.path("transcript.jsonl");
    fs::write(&tx, "{\"type\":\"user\"}\n").unwrap();
    tx
}

/// Run the hook and check it is silent and exits 0.
fn quiet_turn_end(env: &Env, input: &str) {
    let out = turn_end(env, input);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
}

#[test]
fn turn_end_records_the_time_generation_and_transcript() {
    let env = setup("worker-a", "worker-a lead\n");
    let tx = transcript(&env);
    quiet_turn_end(&env, &stop_input(&tx, Some(serde_json::json!([]))));

    let m = marker(&env, "worker-a").expect("a turn-end marker");
    assert_eq!(m["transcript_path"], serde_json::json!(tx));
    assert_eq!(m["transcript_len"], fs::metadata(&tx).unwrap().len());
    let at = m["turn_end"].as_str().unwrap();
    assert!(at.ends_with('Z') && at.len() >= 20, "{at:?}");
    assert_eq!(m.as_object().unwrap().len(), 4, "{m}");
    let text = m.to_string();
    assert!(!text.contains("SECRET"), "{text}");
    assert!(env.ids("inbox/lead").is_empty(), "the hook sends nothing");

    // A second turn end writes a new generation.
    let first = m["generation"].clone();
    quiet_turn_end(&env, &stop_input(&tx, None));
    assert_ne!(marker(&env, "worker-a").unwrap()["generation"], first);
}

#[test]
fn turn_end_waits_for_input_that_starts_late() {
    let env = setup("worker-a", "worker-a lead\n");
    let tx = transcript(&env);
    let out = turn_end_after(&env, &stop_input(&tx, None), Duration::from_millis(300));
    assert_eq!(code(&out), 0);
    assert!(marker(&env, "worker-a").is_some());
}

#[test]
fn turn_end_with_background_work_records_nothing() {
    let env = setup("worker-a", "worker-a lead\n");
    let tx = transcript(&env);
    let watcher = serde_json::json!({"id": "t-1", "type": "shell", "status": "running",
        "description": "queue watcher", "command": "cq watch --as worker-a --to-inbox"});
    let agent = serde_json::json!({"id": "t-2", "type": "subagent", "status": "running",
        "description": "SECRET review", "agent_type": "coder"});
    // A watcher through a tool that reports no command: it counts.
    let monitor = serde_json::json!({"id": "t-3", "type": "monitor", "status": "running",
        "description": "cq watch --as worker-a"});

    // Only the cq watcher: recorded.
    quiet_turn_end(&env, &stop_input(&tx, Some(serde_json::json!([watcher]))));
    assert!(marker(&env, "worker-a").is_some());

    // A running subagent as well: nothing, and the older marker goes.
    quiet_turn_end(
        &env,
        &stop_input(&tx, Some(serde_json::json!([watcher, agent]))),
    );
    assert!(marker(&env, "worker-a").is_none());
    quiet_turn_end(&env, &stop_input(&tx, None));
    quiet_turn_end(&env, &stop_input(&tx, Some(serde_json::json!([monitor]))));
    assert!(marker(&env, "worker-a").is_none());

    // No background_tasks field: unknown, so recorded.
    quiet_turn_end(&env, &stop_input(&tx, None));
    assert!(marker(&env, "worker-a").is_some());
}

#[test]
fn turn_end_records_nothing_when_it_should_not() {
    let tx = PathBuf::from("/t/transcript.jsonl");

    // Not listed.
    let env = setup("worker-b", "worker-a lead\n");
    quiet_turn_end(&env, &stop_input(&tx, None));
    assert!(marker(&env, "worker-b").is_none());

    // Asleep.
    let env = setup("worker-a", "worker-a lead\n");
    let out = env.run(&["sleep", "--as", "worker-a", "worker-a"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    quiet_turn_end(&env, &stop_input(&tx, None));
    assert!(marker(&env, "worker-a").is_none());

    // No transcript path, bad JSON, or no input at all.
    let env = setup("worker-a", "worker-a lead\n");
    quiet_turn_end(&env, r#"{"session_id":"s-1","hook_event_name":"Stop"}"#);
    quiet_turn_end(&env, "not json");
    let out = env.run(&["turn-end"], None);
    assert_eq!(code(&out), 0);
    assert_eq!(stderr(&out), "");
    assert!(marker(&env, "worker-a").is_none());

    // No member record.
    fs::remove_file(env.path(&format!("members/{KEY}"))).unwrap();
    quiet_turn_end(&env, &stop_input(&tx, None));
    assert!(marker(&env, "worker-a").is_none());
}

/// Plant a marker for `worker-a` whose turn end was `minutes` ago, with a
/// transcript last written then.
fn plant_turn_end(env: &Env, minutes: i64) -> PathBuf {
    let tx = env.path("transcript.jsonl");
    fs::write(&tx, "{}\n").unwrap();
    let f = fs::OpenOptions::new().write(true).open(&tx).unwrap();
    let ago = Duration::from_secs(minutes as u64 * 60);
    f.set_modified(SystemTime::now() - ago).unwrap();
    fs::create_dir_all(env.path("idle")).unwrap();
    let text = serde_json::json!({"generation": format!("g-{minutes}"),
        "turn_end": minutes_ago(minutes), "transcript_path": tx, "transcript_len": 3});
    fs::write(env.path("idle/worker-a.turn-end"), text.to_string()).unwrap();
    tx
}

/// Run a watcher for `worker-a` for 2 seconds.
fn watch_briefly(env: &Env, flags: &[&str]) -> Output {
    let mut args = vec!["watch", "--as", "worker-a", "--max-seconds", "2"];
    args.extend_from_slice(flags);
    let out = env.run(&args, None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    out
}

#[test]
fn a_restarted_watcher_alerts_after_an_idle_turn_end() {
    let env = setup("worker-a", "worker-a lead\n");
    plant_turn_end(&env, 10);
    let out = watch_briefly(&env, &[]);
    assert_eq!(stdout(&out), "", "no line for the alert");

    let ids = env.ids("inbox/lead");
    assert_eq!(ids.len(), 1, "{ids:?}");
    let msg = env.read_json(&format!("inbox/lead/{}.json", ids[0]));
    assert_eq!(msg["subject"], "worker-a is idle");
    assert!(marker(&env, "worker-a").is_none(), "the marker is removed");
}

#[test]
fn the_watcher_sends_nothing_after_a_busy_or_recent_turn_end() {
    // The transcript changed after the settle time.
    let env = setup("worker-a", "worker-a lead\n");
    let tx = plant_turn_end(&env, 10);
    fs::write(&tx, "{}\n{}\n").unwrap();
    watch_briefly(&env, &[]);
    assert!(env.ids("inbox/lead").is_empty());
    assert!(marker(&env, "worker-a").is_none(), "the marker is removed");

    // Within --idle-after: the marker waits.
    plant_turn_end(&env, 2);
    watch_briefly(&env, &[]);
    assert!(env.ids("inbox/lead").is_empty());
    assert!(marker(&env, "worker-a").is_some(), "the marker stays");

    // A shorter --idle-after alerts; one of 30 s or less is refused.
    watch_briefly(&env, &["--idle-after", "90s"]);
    assert_eq!(env.ids("inbox/lead").len(), 1);
    let out = env.run(&["watch", "--as", "worker-a", "--idle-after", "30s"], None);
    assert_eq!(code(&out), 64);
}

#[test]
fn a_line_from_the_watcher_cancels_the_turn_end() {
    let env = setup("worker-a", "worker-a lead\n");
    plant_turn_end(&env, 10);
    let id = env.send("lead", "worker-a", "task", "wake up", "x");
    let out = watch_briefly(&env, &[]);
    assert!(stdout(&out).contains(&id), "{}", stdout(&out));
    assert!(env.ids("inbox/lead").is_empty(), "no alert");
    assert!(marker(&env, "worker-a").is_none(), "the marker is removed");
}
