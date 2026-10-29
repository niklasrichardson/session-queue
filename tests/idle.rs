//! `cq idle-alert`, the Claude Code `Notification` (idle_prompt) hook: one
//! fixed `info` message to the lead of a listed worker, at most once per
//! interval.

mod common;

use std::fs;
use std::io::Write;
use std::process::{Output, Stdio};

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
