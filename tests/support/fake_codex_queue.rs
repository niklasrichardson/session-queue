//! A fake `codex` for `cq watch --to-codex` tests. Appends its arguments to
//! the file in `CQ_FAKE_CALLS`, one call per line, then answers as
//! `CQ_FAKE_MODE` says: `fail` exits 1, `silent` exits 0 without the
//! `Queued message` line, anything else succeeds.
use std::{env, fs, io::Write};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut calls = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env::var("CQ_FAKE_CALLS").unwrap())
        .unwrap();
    writeln!(calls, "{args:?}").unwrap();
    assert!(env::var_os("CLAUDE_CODE_MESSAGING_TOKEN").is_none());
    match env::var("CQ_FAKE_MODE").unwrap_or_default().as_str() {
        "fail" => {
            eprintln!("Error: thread not found (synthetic)");
            std::process::exit(1);
        }
        "silent" => {}
        _ => println!("Queued message q-1 for thread {}.", args[2]),
    }
}
