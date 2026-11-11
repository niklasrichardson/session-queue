//! A fake `agy` for `cq watch --to-agy` tests. Appends its arguments to the
//! file in `CQ_FAKE_CALLS`, one call per line, waits `CQ_FAKE_SECONDS` if
//! set, then answers as `CQ_FAKE_MODE` says: `fail` exits 1, `error` prints
//! a result with status `ERROR`, `nojson` prints no result, `other` prints a
//! result from another conversation, anything else succeeds. A result is
//! pretty-printed after a status line, as `agy` prints it.
//!
//! If `CQ_FAKE_ENV` is set, writes there which of `CQ_ROOT` and the session
//! variables it got. A prompt `tree MARKER` starts a descendant that writes
//! `MARKER` 3 s later, writes `MARKER.ready`, then waits 8 s and succeeds:
//! a turn whose helpers must end with it.
//!
//! Broker modes (`cq watch --agy-broker`) read `request.json` in the
//! `--add-dir` folder: `broker` writes a valid `reply.json` (`read` when
//! that is the only outcome, else a reply with status ok and text
//! `BROKER-DONE`), `broker-reject` rejects, `broker-none` writes nothing,
//! `broker-bad` writes a reply with another nonce. Each then succeeds.
use std::{
    env, fs,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

const SEEN: [&str; 6] = [
    "CQ_ROOT",
    "CQ_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CLAUDE_CODE_HOST_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
];

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--descendant") {
        thread::sleep(Duration::from_secs(3));
        fs::write(&args[1], "descendant survived").unwrap();
        return;
    }
    if let Some(path) = env::var_os("CQ_FAKE_CALLS") {
        let mut calls = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        // One write of the whole line, so a reader never sees half of it
        // (writeln! writes the parts one by one).
        calls.write_all(format!("{args:?}\n").as_bytes()).unwrap();
    }
    if let Some(path) = env::var_os("CQ_FAKE_ENV") {
        let seen: Vec<&str> = SEEN
            .into_iter()
            .filter(|v| env::var_os(v).is_some())
            .collect();
        fs::write(path, seen.join(" ")).unwrap();
    }
    assert!(env::var_os("CLAUDE_CODE_MESSAGING_TOKEN").is_none());
    if let Some(marker) = args.get(3).and_then(|p| p.strip_prefix("tree ")) {
        Command::new(env::current_exe().unwrap())
            .args(["--descendant", marker])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        fs::write(format!("{marker}.ready"), "descendant started").unwrap();
        thread::sleep(Duration::from_secs(8));
    }
    if let Some(secs) = env::var("CQ_FAKE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        thread::sleep(Duration::from_secs(secs));
    }
    let result = |conversation: &str, status: &str| {
        println!("root agent idle; waiting for 1 background task(s) to finish");
        println!(
            "{{\n  \"conversation_id\": \"{conversation}\",\n  \"status\": \"{status}\",\n  \"response\": \"RESPONSE-MARK\",\n  \"duration_seconds\": 4.2,\n  \"num_turns\": 2,\n  \"usage\": {{}}\n}}"
        );
    };
    let mode = env::var("CQ_FAKE_MODE").unwrap_or_default();
    if let Some(kind) = mode.strip_prefix("broker") {
        broker(&args, kind);
        result(&args[1], "SUCCESS");
        return;
    }
    match mode.as_str() {
        "fail" => {
            eprintln!("Error: conversation not found (synthetic)");
            std::process::exit(1);
        }
        "error" => {
            eprintln!("model quota exceeded (synthetic)");
            result(&args[1], "ERROR");
        }
        "nojson" => println!("root agent idle; waiting for 1 background task(s) to finish"),
        "other" => result("another-conversation", "SUCCESS"),
        _ => result(&args[1], "SUCCESS"),
    }
}

/// The first string value of `key` in `json` (enough for request.json, whose
/// top-level keys sort before `message`).
fn string_field(json: &str, key: &str) -> String {
    let start = json.find(&format!("\"{key}\": \"")).unwrap() + key.len() + 5;
    let end = start + json[start..].find('"').unwrap();
    json[start..end].to_owned()
}

fn broker(args: &[String], kind: &str) {
    let at = args.iter().position(|a| a == "--add-dir").unwrap();
    let dir = std::path::Path::new(&args[at + 1]);
    let request = fs::read_to_string(dir.join("request.json")).unwrap();
    let id = string_field(&request, "id");
    let mut nonce = string_field(&request, "nonce");
    let read_only = !request.contains("\"reply\"");
    let tail = match kind {
        "-none" => return,
        "-bad" => {
            nonce = "0".repeat(32);
            r#""outcome": "reply", "status": "ok", "text": "BROKER-DONE""#
        }
        "-reject" => r#""outcome": "reject", "text": "BROKER-NO""#,
        _ if read_only => r#""outcome": "read""#,
        _ => r#""outcome": "reply", "status": "ok", "text": "BROKER-DONE""#,
    };
    fs::write(
        dir.join("reply.json"),
        format!("{{\"id\": \"{id}\", \"nonce\": \"{nonce}\", {tail}}}"),
    )
    .unwrap();
}
