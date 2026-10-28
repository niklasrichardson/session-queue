//! `cq watch --to-inbox` (experimental), end to end: each test serves a fake
//! session inbox (a named pipe on Windows, a Unix socket elsewhere) and
//! points the watcher at it. No test touches a real Claude Code inbox:
//! `Env::cmd` removes the inherited variables, and these tests set them to
//! the fake.

mod common;

use std::ffi::OsString;
use std::io::Read;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use common::*;

const TOKEN: &str = "test-token-7f3a";
const SOCKET_VAR: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const TOKEN_VAR: &str = "CLAUDE_CODE_MESSAGING_TOKEN";

/// A fake session inbox. Each connection's full text arrives on `posts`.
struct FakeInbox {
    addr: OsString,
    posts: Receiver<String>,
    #[cfg(unix)]
    _dir: tempfile::TempDir,
}

impl FakeInbox {
    #[cfg(windows)]
    fn start() -> Self {
        use std::fs::File;
        use std::os::windows::io::FromRawHandle;
        use std::ptr::{null, null_mut};
        use std::sync::atomic::{AtomicU32, Ordering};

        use windows_sys::Win32::Foundation::{
            ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
        use windows_sys::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        };

        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            r"\\.\pipe\cq-test-inbox-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let create = move || -> usize {
            // SAFETY: `wide` is a NUL-terminated UTF-16 name; no security
            // attributes.
            let h = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    0,
                    65536,
                    0,
                    null(),
                )
            };
            assert!(h != INVALID_HANDLE_VALUE, "CreateNamedPipeW failed");
            h as usize
        };
        // The first instance exists before the watcher starts.
        let mut h = create();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            loop {
                // SAFETY: `h` is a live pipe instance owned by this thread.
                let ok = unsafe { ConnectNamedPipe(h as HANDLE, null_mut()) } != 0
                    || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
                // The next instance is ready while this one is read.
                let next = create();
                // SAFETY: the handle is owned here and closed once, by File.
                let mut f = unsafe { File::from_raw_handle(h as HANDLE) };
                if ok {
                    let mut s = String::new();
                    let _ = f.read_to_string(&mut s);
                    if tx.send(s).is_err() {
                        break;
                    }
                }
                drop(f);
                h = next;
            }
        });
        Self {
            addr: name.into(),
            posts: rx,
        }
    }

    #[cfg(unix)]
    fn start() -> Self {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("inbox.sock");
        let listener = UnixListener::bind(&path).expect("bind the fake inbox");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { break };
                let mut s = String::new();
                let _ = conn.read_to_string(&mut s);
                if tx.send(s).is_err() {
                    break;
                }
            }
        });
        Self {
            addr: path.into_os_string(),
            posts: rx,
            _dir: dir,
        }
    }

    /// The content of the next post, after checking its framing.
    fn post(&self, timeout: Duration) -> Option<String> {
        let text = self.posts.recv_timeout(timeout).ok()?;
        Some(content(&text))
    }
}

/// An inbox address where nothing listens.
fn nowhere() -> OsString {
    if cfg!(windows) {
        format!(r"\\.\pipe\cq-test-nowhere-{}", std::process::id()).into()
    } else {
        std::env::temp_dir()
            .join(format!("cq-test-nowhere-{}.sock", std::process::id()))
            .into_os_string()
    }
}

/// Check one connection's framing and return the message content: the
/// auth line first, then one user message from `cq`, one line each.
fn content(text: &str) -> String {
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    assert_eq!(lines.len(), 2, "two lines per post: {text:?}");
    assert!(text.ends_with('\n'), "{text:?}");
    assert_eq!(lines[0], format!(r#"{{"type":"auth","token":"{TOKEN}"}}"#));
    let msg: Value = serde_json::from_str(lines[1]).expect("JSON message line");
    assert_eq!(msg["type"], "user");
    assert_eq!(msg["from"], "cq");
    assert_eq!(msg["message"]["role"], "user");
    let c = msg["message"]["content"].as_str().expect("text content");
    assert!(!c.is_empty());
    c.to_owned()
}

/// Start `cq watch --as <name> --to-inbox` posting to `addr`.
fn watch_to(env: &Env, name: &str, addr: &OsString, flags: &[&str]) -> Watcher {
    let mut args = vec!["watch", "--as", name, "--to-inbox"];
    args.extend_from_slice(flags);
    let mut c = env.cmd(&args);
    c.env(SOCKET_VAR, addr)
        .env(TOKEN_VAR, TOKEN)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    Watcher::spawn(c)
}

/// The exit code once the process ends, within `timeout`.
fn exit_code(w: &mut Watcher, timeout: Duration) -> i32 {
    let mut code = None;
    assert!(
        wait_until(timeout, || {
            code = w.child.try_wait().unwrap().map(|s| s.code().unwrap_or(-1));
            code.is_some()
        }),
        "the watcher did not exit"
    );
    code.unwrap()
}

#[test]
fn posts_batches_then_a_stopped_message_when_asleep() {
    let env = Env::new();
    let inbox = FakeInbox::start();
    let id1 = env.send("a", "b", "task", "say \"hi\" \u{e9}", "x");
    let mut w = watch_to(&env, "b", &inbox.addr, &[]);

    let first = inbox
        .post(Duration::from_secs(10))
        .expect("the start batch");
    let t1 = Instant::now();
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines[0], "cq watch (b): queue events");
    assert_eq!(lines.len(), 2, "{first}");
    assert!(lines[1].starts_with("CQ-NEW "), "{first}");
    assert_eq!(event_id(lines[1]), id1);
    assert!(
        lines[1].contains("subject=\"say \\\"hi\\\" \u{e9}\""),
        "{first}"
    );

    let id2 = env.send("a", "b", "info", "second", "x");
    let second = inbox.post(Duration::from_secs(15)).expect("a later batch");
    assert!(
        t1.elapsed() >= Duration::from_millis(4500),
        "posts closer than the 5 s gap"
    );
    assert!(second.starts_with("cq watch (b): queue events\nCQ-NEW "));
    assert!(second.contains(&format!(" id={id2} ")), "{second}");

    let out = env.run(&["sleep", "--as", "b", "b"], None);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let last = inbox
        .post(Duration::from_secs(15))
        .expect("the stopped message");
    assert!(last.contains("\nCQ-SLEEP by=b"), "{last}");
    assert!(
        last.ends_with("cq watch (b) stopped: b is asleep. Do not re-arm; run /cq-start only when asked to wake."),
        "{last}"
    );
    assert_eq!(exit_code(&mut w, Duration::from_secs(10)), 7);
    assert!(w.lines_for(Duration::ZERO).is_empty(), "printed to stdout");
    let err = w.stderr_text();
    assert!(!err.contains(TOKEN), "{err}");
    assert!(env.pid_of_watcher("b").is_none());
}

#[test]
fn max_seconds_posts_a_stopped_message() {
    let env = Env::new();
    let inbox = FakeInbox::start();
    let mut w = watch_to(&env, "b", &inbox.addr, &["--max-seconds", "1"]);
    let last = inbox
        .post(Duration::from_secs(10))
        .expect("the stopped message");
    assert_eq!(
        last,
        "cq watch (b) stopped: --max-seconds 1 reached. Run /cq-start to re-arm."
    );
    assert_eq!(exit_code(&mut w, Duration::from_secs(10)), 0);
    assert!(!w.stderr_text().contains(TOKEN));
}

#[test]
fn an_error_at_start_posts_a_stopped_message() {
    let env = Env::new();
    let inbox = FakeInbox::start();
    let _holder = env.watch("b", &[]);
    assert!(wait_until(Duration::from_secs(10), || env
        .path("heartbeat/b")
        .is_file()));
    let mut w = watch_to(&env, "b", &inbox.addr, &[]);
    let last = inbox
        .post(Duration::from_secs(10))
        .expect("the stopped message");
    assert!(
        last.starts_with(
            "cq watch (b) stopped: error: a watcher or dispatcher for b is already running"
        ),
        "{last}"
    );
    assert!(last.ends_with(". Run /cq-start to re-arm."), "{last}");
    assert_eq!(exit_code(&mut w, Duration::from_secs(10)), 5);
}

#[test]
fn an_unreachable_inbox_exits_8() {
    let env = Env::new();
    env.send("a", "b", "task", "waiting", "x");
    let mut w = watch_to(&env, "b", &nowhere(), &[]);
    // 2 s batch window, then 4 tries 1, 2 and 4 s apart.
    assert_eq!(exit_code(&mut w, Duration::from_secs(30)), 8);
    let err = w.stderr_text();
    assert!(
        err.contains("cq: the session's inbox could not be reached after 4 tries"),
        "{err}"
    );
    assert!(!err.contains(TOKEN), "{err}");
    assert!(env.pid_of_watcher("b").is_none());
}

#[test]
fn missing_variables_exit_64() {
    let env = Env::new();
    let out = env.run(&["watch", "--as", "b", "--to-inbox"], None);
    assert_eq!(code(&out), 64);
    assert!(stderr(&out).starts_with("cq: --to-inbox needs CLAUDE_CODE_MESSAGING_SOCKET"));

    let mut c = env.cmd(&["watch", "--as", "b", "--to-inbox"]);
    c.env(SOCKET_VAR, nowhere());
    let out = c.output().unwrap();
    assert_eq!(code(&out), 64);
    let err = stderr(&out);
    assert!(err.contains(TOKEN_VAR), "{err}");
    assert!(!env.path("heartbeat/b").exists(), "the watcher started");
}
