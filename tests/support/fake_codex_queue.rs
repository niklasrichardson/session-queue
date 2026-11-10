//! A fake `codex` for `cq watch --to-codex` tests. Appends its arguments to
//! the file in `CQ_FAKE_CALLS`, one call per line, then answers as
//! `CQ_FAKE_MODE` says: `fail` exits 1, `silent` exits 0 without the
//! `Queued message` line, `hang` starts a helper and never answers,
//! anything else succeeds.
use std::{
    env, fs,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

/// After this long, a `hang` call and its helper each leave a file that
/// says they were not stopped.
const RUN_ON: Duration = Duration::from_secs(3);

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let calls_path = env::var("CQ_FAKE_CALLS").unwrap();
    if args.first().map(String::as_str) == Some("--helper") {
        thread::sleep(RUN_ON);
        fs::write(format!("{calls_path}.helper-ran-on"), "").unwrap();
        return;
    }
    let mut calls = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&calls_path)
        .unwrap();
    writeln!(calls, "{args:?}").unwrap();
    assert!(env::var_os("CLAUDE_CODE_MESSAGING_TOKEN").is_none());
    match env::var("CQ_FAKE_MODE").unwrap_or_default().as_str() {
        "fail" => {
            eprintln!("Error: thread not found (synthetic)");
            std::process::exit(1);
        }
        "silent" => {}
        "hang" => {
            Command::new(env::current_exe().unwrap())
                .arg("--helper")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            fs::write(format!("{calls_path}.hung"), "").unwrap();
            thread::sleep(RUN_ON);
            fs::write(format!("{calls_path}.codex-ran-on"), "").unwrap();
            // Long enough for the tests; short, so a failed test does not
            // hold the test run for long.
            thread::sleep(Duration::from_secs(30));
        }
        _ => println!("Queued message q-1 for thread {}.", args[2]),
    }
}
