use std::{
    env, fs,
    io::{self, Read, Write},
    process::{Command, Stdio},
    thread,
    time::Duration,
};
fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some("--exit") {
        return;
    }
    if args.get(1).map(String::as_str) == Some("--descendant") {
        thread::sleep(Duration::from_secs(3));
        fs::write(env::var("CQ_FAKE_MARKER").unwrap(), "orphan survived").unwrap();
        return;
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt).unwrap();
    let calls = env::var("CQ_FAKE_CALLS").unwrap();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&calls)
        .unwrap();
    writeln!(file, "CALL {:?}", args).unwrap();
    fs::write(format!("{calls}.prompt"), prompt).unwrap();
    assert!(env::var_os("CQ_SESSION_ID").is_none());
    assert!(env::var_os("CODEX_THREAD_ID").is_none());
    let answer = &args[args.iter().position(|a| a == "-o").unwrap() + 1];
    let mode = env::var("CQ_FAKE_MODE").unwrap_or_default();
    match mode.as_str() {
        "fail" => {
            println!(r#"{{"type":"error","message":"429 Too Many Requests (synthetic)"}}"#);
            std::process::exit(1);
        }
        "stderr" => {
            eprintln!("synthetic authentication failure");
            std::process::exit(1);
        }
        "stderr-cut" => {
            // 33 ASCII bytes, then 2-byte characters: byte 4096 splits one.
            eprintln!("synthetic authentication failure {}", "é".repeat(3000));
            std::process::exit(1);
        }
        "timeout" => {
            Command::new(env::current_exe().unwrap())
                .arg("--descendant")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            fs::write(format!("{calls}.ready"), "descendant started").unwrap();
            thread::sleep(Duration::from_secs(60));
        }
        "oversize" => fs::write(
            answer,
            format!(r#"{{"status":"ok","reply":"{}"}}"#, "é".repeat(11000)),
        )
        .unwrap(),
        "reject" => fs::write(
            answer,
            r#"{"status":"rejected","reply":"Outside local scope"}"#,
        )
        .unwrap(),
        "empty" => fs::write(answer, "").unwrap(),
        "pause" => {
            fs::write(
                std::path::Path::new(&env::var("CQ_ROOT").unwrap()).join("PAUSE"),
                "test",
            )
            .unwrap();
            fs::write(
                answer,
                r#"{"status":"ok","reply":"completed before pause"}"#,
            )
            .unwrap();
        }
        _ => fs::write(
            answer,
            r#"{"status":"ok","reply":"£ café 😀 literal $HOME `ticks`"}"#,
        )
        .unwrap(),
    }
}
