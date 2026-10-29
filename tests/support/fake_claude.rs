//! A fake Claude Code CLI process for the `/clear` rejoin tests. The tests
//! build it as `claude` (`claude.exe` on Windows), so `cq` finds it as the
//! nearest `claude` process above it. It runs one `cq` command (the binary
//! in `CQ_BIN`) for each line on stdin, in this one process, like the CLI
//! runs its tools and hooks across `/clear`.
//!
//! A line is `SESSION<TAB>HOOK<TAB>ARGS`. SESSION sets
//! `CLAUDE_CODE_SESSION_ID` (`-` unsets it); `SESSION,HOST` also sets
//! `CLAUDE_CODE_HOST_SESSION_ID` for that command. HOOK is the hook JSON for
//! stdin: `-` for none, text that starts with `{` as is, else the `source`
//! of a SessionStart JSON with SESSION as its `session_id`. ARGS are split
//! on spaces. The output of each command is its stdout, a `\x01ERR` line,
//! its stderr, then a `\x01END <exit code>` line. Output that does not end
//! in a newline gets one.
use std::env;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

fn main() {
    let cq = env::var("CQ_BIN").unwrap();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        let mut parts = line.splitn(3, '\t');
        let (session, hook, args) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let (session, host) = session.split_once(',').unwrap_or((session, ""));
        let mut c = Command::new(&cq);
        if !host.is_empty() {
            c.env("CLAUDE_CODE_HOST_SESSION_ID", host);
        }
        c.args(args.split(' ').filter(|a| !a.is_empty()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if session == "-" {
            c.env_remove("CLAUDE_CODE_SESSION_ID");
        } else {
            c.env("CLAUDE_CODE_SESSION_ID", session);
        }
        let json = match hook {
            "-" => None,
            h if h.starts_with('{') => Some(h.to_owned()),
            source => Some(format!(
                r#"{{"session_id":"{session}","hook_event_name":"SessionStart","source":"{source}"}}"#
            )),
        };
        c.stdin(if json.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = c.spawn().unwrap();
        if let Some(text) = json {
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(text.as_bytes()).unwrap();
        }
        let done = child.wait_with_output().unwrap();
        write_lines(&mut out, &done.stdout);
        out.write_all(b"\x01ERR\n").unwrap();
        write_lines(&mut out, &done.stderr);
        writeln!(out, "\x01END {}", done.status.code().unwrap_or(-1)).unwrap();
        out.flush().unwrap();
    }
}

/// Write `text`, with a newline at the end when it has none.
fn write_lines(out: &mut impl Write, text: &[u8]) {
    out.write_all(text).unwrap();
    if !text.is_empty() && !text.ends_with(b"\n") {
        out.write_all(b"\n").unwrap();
    }
}
