//! A fake `agy` for `cq watch --to-agy` tests. Appends its arguments to the
//! file in `CQ_FAKE_CALLS`, one call per line, waits `CQ_FAKE_SECONDS` if
//! set, then answers as `CQ_FAKE_MODE` says: `fail` exits 1, `error` prints
//! a result with status `ERROR`, `nojson` prints no result, `other` prints a
//! result from another conversation, anything else succeeds. A result is
//! pretty-printed after a status line, as `agy` prints it.
use std::{env, fs, io::Write, thread, time::Duration};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut calls = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env::var("CQ_FAKE_CALLS").unwrap())
        .unwrap();
    writeln!(calls, "{args:?}").unwrap();
    drop(calls);
    assert!(env::var_os("CLAUDE_CODE_MESSAGING_TOKEN").is_none());
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
    match env::var("CQ_FAKE_MODE").unwrap_or_default().as_str() {
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
