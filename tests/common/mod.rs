//! Shared helpers for the integration tests. Each test gets its own
//! temporary `CQ_ROOT` and runs the real `cq` binary.

#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

pub const CQ: &str = env!("CARGO_BIN_EXE_cq");

pub struct Env {
    dir: TempDir,
}

/// The rules path `cq session-start` names in tests (`CQ_RULES`).
pub const RULES: &str = "/rules/SESSION-RULES.md";

impl Env {
    /// A fresh queue root, initialised with `cq init`.
    pub fn new() -> Self {
        let env = Self {
            dir: tempfile::tempdir().expect("temp dir"),
        };
        let out = env.run(&["init"], None);
        assert!(out.status.success(), "cq init failed: {}", stderr(&out));
        env
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }

    /// A `cq` command with this root, no session IDs, no Claude Code inbox
    /// (so no test can post into the session that runs the tests), and
    /// fixed local time and rules settings, so the output does not depend
    /// on the build's `.cargo/config.toml`.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(CQ);
        c.args(args)
            .env("CQ_ROOT", self.root())
            .env("CQ_TZ", "Europe/London")
            .env("CQ_TZ_LABEL", "UK")
            .env("CQ_RULES", RULES)
            .env_remove("CQ_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_CODE_HOST_SESSION_ID")
            .env_remove("CLAUDE_CODE_MESSAGING_SOCKET")
            .env_remove("CLAUDE_CODE_MESSAGING_TOKEN");
        c
    }

    /// Run `cq` to completion, with `stdin` as its input (or no input).
    pub fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut c = self.cmd(args);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().expect("spawn cq");
        let mut pipe = child.stdin.take().unwrap();
        if let Some(text) = stdin {
            pipe.write_all(text.as_bytes()).unwrap();
        }
        drop(pipe);
        child.wait_with_output().unwrap()
    }

    /// `cq send`; returns the new ID. Panics if the send fails.
    pub fn send(&self, from: &str, to: &str, kind: &str, subject: &str, body: &str) -> String {
        let out = self.send_raw(&["--from", from, "--to", to, "--type", kind], subject, body);
        assert!(out.status.success(), "cq send failed: {}", stderr(&out));
        stdout(&out).trim().to_owned()
    }

    pub fn send_raw(&self, flags: &[&str], subject: &str, body: &str) -> Output {
        let input = json!({ "subject": subject, "body": body }).to_string();
        let mut args = vec!["send"];
        args.extend_from_slice(flags);
        self.run(&args, Some(&input))
    }

    pub fn claim(&self, name: &str, id: &str) -> Output {
        self.run(&["claim", "--as", name, id], None)
    }

    /// Message files (stems) in a directory under the root.
    pub fn ids(&self, rel: &str) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.path(rel))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.ends_with(".json") && !n.ends_with(".reason.json"))
                    .map(|n| n.trim_end_matches(".json").to_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    pub fn read_json(&self, rel: &str) -> Value {
        let bytes = fs::read(self.path(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Write a message file straight into `inbox/<to>/`, bypassing `cq send`.
    pub fn plant(&self, to: &str, id: &str, bytes: &[u8]) {
        let dir = self.path(&format!("inbox/{to}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{id}.json")), bytes).unwrap();
    }

    /// Start `cq watch --as <name>` with extra flags.
    pub fn watch(&self, name: &str, flags: &[&str]) -> Watcher {
        let mut args = vec!["watch", "--as", name];
        args.extend_from_slice(flags);
        let mut c = self.cmd(&args);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Watcher::spawn(c)
    }

    pub fn pid_of_watcher(&self, name: &str) -> Option<u32> {
        fs::read_to_string(self.path(&format!("heartbeat/{name}.pid")))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
}

/// A running process whose stdout lines arrive on a channel.
pub struct Watcher {
    pub child: Child,
    lines: Receiver<String>,
}

impl Watcher {
    pub fn spawn(mut c: Command) -> Self {
        let mut child = c.spawn().expect("spawn watcher");
        let out = child.stdout.take().unwrap();
        Self {
            child,
            lines: read_lines(out),
        }
    }

    /// The next line, or `None` on timeout or end of output.
    pub fn line(&self, timeout: Duration) -> Option<String> {
        self.lines.recv_timeout(timeout).ok()
    }

    /// All lines that arrive within `d`.
    pub fn lines_for(&self, d: Duration) -> Vec<String> {
        let until = Instant::now() + d;
        let mut v = Vec::new();
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(l) => v.push(l),
                Err(_) => break,
            }
        }
        v
    }

    /// True when stdout reaches end of file within `timeout`: every process
    /// that held the pipe has exited.
    pub fn closed_within(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(_) => continue,
                Err(RecvTimeoutError::Disconnected) => return true,
                Err(RecvTimeoutError::Timeout) => return false,
            }
        }
    }

    pub fn stderr_text(&mut self) -> String {
        let _ = self.child.wait();
        let mut s = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut e, &mut s);
        }
        s
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_lines(out: ChildStdout) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// Poll `cond` every 50 ms until it holds or `timeout` passes.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Git for Windows' bash, for tests of the forms sessions use. `None` when
/// it is not installed, and the test then passes without running.
pub fn git_bash() -> Option<PathBuf> {
    let p = PathBuf::from(r"C:\Program Files\Git\bin\bash.exe");
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped: Git Bash not found at {}", p.display());
        None
    }
}

/// `CQ` as a path Git Bash accepts.
pub fn cq_for_bash() -> String {
    CQ.replace('\\', "/")
}

/// The `id=` field of a `CQ-NEW` line.
pub fn event_id(line: &str) -> &str {
    line.split(' ')
        .find_map(|f| f.strip_prefix("id="))
        .expect("an id= field")
}

/// A valid v1 message as a JSON value, for planting.
pub fn v1(id: &str, from: &str, to: &str, kind: &str, hops: u32) -> Value {
    json!({
        "schema_version": 1,
        "id": id,
        "from": from,
        "to": to,
        "type": kind,
        "subject": "planted",
        "body": "planted body",
        "reply_to": null,
        "parent": null,
        "status": if kind == "result" { json!("ok") } else { Value::Null },
        "hops": hops,
        "created": "2026-10-24T00:06:15Z",
        "from_session_id": null,
        "from_host_session_id": null
    })
}

/// An ID from `from`, with a fixed stamp and the given suffix.
pub fn fake_id(from: &str, hex: &str) -> String {
    format!("20261024T000615Z-{from}-{hex}")
}
