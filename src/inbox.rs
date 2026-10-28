//! `cq watch --to-inbox` (experimental): post the watcher's event lines into
//! the session's own Claude Code inbox instead of printing them.
//!
//! Claude Code exports the inbox address and a per-session token to Bash
//! commands and hooks as `CLAUDE_CODE_MESSAGING_SOCKET` (a Unix socket, or
//! a named pipe on Windows) and `CLAUDE_CODE_MESSAGING_TOKEN`. A post opens
//! a connection, writes an auth line and one message line, and closes it. A
//! message to an idle session starts a new turn, so the watcher needs no
//! Monitor and no re-arm every 30 minutes. See
//! <https://code.claude.com/docs/en/cross-session-messaging.md#the-sessions-inbox-socket>.
//!
//! Claude Code refuses rapid bursts to one inbox, so lines are batched:
//! lines within `BATCH_WINDOW` of the first go in one message, and posts
//! are at least `MIN_GAP` apart. The token is never printed.

use std::ffi::OsString;
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{CqError, Result, exit};
use crate::platform;
use crate::watch::{Ended, Sink};

/// The inbox address Claude Code exports to Bash commands.
pub const SOCKET_VAR: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
/// The per-session token Claude Code exports to Bash commands.
pub const TOKEN_VAR: &str = "CLAUDE_CODE_MESSAGING_TOKEN";
/// The `from` name on every post.
pub const FROM: &str = "cq";
/// Lines that arrive within this time of the first held line go in one
/// message.
pub const BATCH_WINDOW: Duration = Duration::from_secs(2);
/// The shortest time between two posts.
pub const MIN_GAP: Duration = Duration::from_secs(5);
/// At most this many event lines in one message; the rest follow after
/// `MIN_GAP`.
pub const MAX_LINES: usize = 50;
/// Tries per post. Between tries the wait doubles from `RETRY_FIRST`
/// (1, 2 and 4 seconds), so an unreachable inbox fails a post after about
/// 7 seconds.
pub const POST_TRIES: u32 = 4;
pub const RETRY_FIRST: Duration = Duration::from_secs(1);

/// Opens one connection to the inbox per post. Tests use a fake.
pub trait Transport {
    type Conn: Write;
    fn connect(&mut self) -> io::Result<Self::Conn>;
}

/// The real inbox: the named pipe or Unix socket in `SOCKET_VAR`.
pub struct Socket(OsString);

impl Transport for Socket {
    type Conn = platform::InboxConn;
    fn connect(&mut self) -> io::Result<Self::Conn> {
        platform::open_inbox(&self.0)
    }
}

/// The time source for batching and retries. Tests use a fake.
pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, d: Duration) {
        thread::sleep(d);
    }
}

/// A `Sink` that posts into a session's inbox. Holds the token, so it has
/// no `Debug`.
pub struct Inbox<T: Transport, C: Clock> {
    name: String,
    token: String,
    transport: T,
    clock: C,
    /// Event lines not posted yet, oldest first.
    held: Vec<String>,
    /// When the batch in `held` started.
    batch_start: Option<Instant>,
    last_post: Option<Instant>,
}

impl Inbox<Socket, SystemClock> {
    /// The inbox of the session that started this process, from its
    /// environment. Exit 64 when either variable is missing or empty.
    pub fn from_env(name: &str) -> Result<Self> {
        Self::from_vars(
            name,
            std::env::var_os(SOCKET_VAR),
            std::env::var_os(TOKEN_VAR),
        )
    }

    /// `from_env` with the variables passed in.
    pub fn from_vars(
        name: &str,
        socket: Option<OsString>,
        token: Option<OsString>,
    ) -> Result<Self> {
        let missing = |var: &str| {
            CqError::usage(format!(
                "--to-inbox needs {var}, which Claude Code sets for Bash commands and hooks; it is not set or empty"
            ))
        };
        let socket = socket
            .filter(|s| !s.is_empty())
            .ok_or_else(|| missing(SOCKET_VAR))?;
        let token = token
            .filter(|t| !t.is_empty())
            .ok_or_else(|| missing(TOKEN_VAR))?;
        let token = token
            .into_string()
            .map_err(|_| CqError::usage(format!("--to-inbox: {TOKEN_VAR} is not valid UTF-8")))?;
        Ok(Self::new(name, token, Socket(socket), SystemClock))
    }
}

impl<T: Transport, C: Clock> Inbox<T, C> {
    pub fn new(name: &str, token: String, transport: T, clock: C) -> Self {
        Self {
            name: name.to_owned(),
            token,
            transport,
            clock,
            held: Vec::new(),
            batch_start: None,
            last_post: None,
        }
    }

    /// The first line of every message of event lines.
    fn header(&self) -> String {
        format!("cq watch ({}): queue events", self.name)
    }

    /// The last line the session gets from this watcher.
    fn stopped_line(&self, reason: &str, ended: Ended) -> String {
        let name = &self.name;
        match ended {
            Ended::Asleep => format!(
                "cq watch ({name}) stopped: {name} is asleep. Do not re-arm; run /cq-start only when asked to wake."
            ),
            Ended::Normal => {
                format!("cq watch ({name}) stopped: {reason}. Run /cq-start to re-arm.")
            }
        }
    }

    /// True when the gap since the last post has passed.
    fn gap_passed(&self, now: Instant) -> bool {
        self.last_post.is_none_or(|t| now >= t + MIN_GAP)
    }

    /// Take up to `MAX_LINES` held lines as a message, after the header.
    fn take_batch(&mut self) -> Vec<String> {
        let n = self.held.len().min(MAX_LINES);
        let mut lines = vec![self.header()];
        lines.extend(self.held.drain(..n));
        // Lines left over start a new batch now.
        self.batch_start = (!self.held.is_empty()).then(|| self.clock.now());
        lines
    }

    /// Post one message: wait for `MIN_GAP`, then try up to `POST_TRIES`
    /// times with a doubling wait between tries.
    fn post(&mut self, content: &str) -> io::Result<()> {
        let frame = frame(&self.token, content);
        let mut wait = RETRY_FIRST;
        let mut tries = 0;
        loop {
            let now = self.clock.now();
            if let Some(t) = self.last_post
                && now < t + MIN_GAP
            {
                self.clock.sleep(t + MIN_GAP - now);
            }
            tries += 1;
            match self.send(&frame) {
                Ok(()) => {
                    self.last_post = Some(self.clock.now());
                    return Ok(());
                }
                Err(e) if tries >= POST_TRIES => return Err(e),
                Err(_) => {
                    self.clock.sleep(wait);
                    wait *= 2;
                }
            }
        }
    }

    /// One connection: the auth and message lines, then close. Connect
    /// only now, because Claude Code closes a connection that sends no
    /// complete line within 30 seconds.
    fn send(&mut self, frame: &str) -> io::Result<()> {
        let mut conn = self.transport.connect()?;
        conn.write_all(frame.as_bytes())?;
        conn.flush()
    }
}

impl<T: Transport, C: Clock> Sink for Inbox<T, C> {
    fn line(&mut self, line: &str) -> io::Result<()> {
        if self.held.is_empty() {
            self.batch_start = Some(self.clock.now());
        }
        self.held.push(line.to_owned());
        Ok(())
    }

    /// Post the held lines once the batch window has closed and the gap
    /// since the last post has passed. Never waits.
    fn tick(&mut self) -> io::Result<()> {
        let now = self.clock.now();
        let due = self
            .batch_start
            .is_some_and(|t| now >= t + BATCH_WINDOW && self.gap_passed(now));
        if due {
            let content = self.take_batch().join("\n");
            self.post(&content)?;
        }
        Ok(())
    }

    /// Post the held lines and the stopped line, together when they fit
    /// in one message, so the session gets one turn, not two.
    fn stopped(&mut self, reason: &str, ended: Ended) {
        let last = self.stopped_line(reason, ended);
        let mut result = Ok(());
        while result.is_ok() && self.held.len() > MAX_LINES {
            let content = self.take_batch().join("\n");
            result = self.post(&content);
        }
        if result.is_ok() {
            let mut lines = if self.held.is_empty() {
                Vec::new()
            } else {
                self.take_batch()
            };
            lines.push(last);
            result = self.post(&lines.join("\n"));
        }
        if let Err(e) = result {
            eprintln!("cq watch: could not post the stopped message to the session's inbox ({e})");
        }
    }

    fn failed(&mut self, e: io::Error) -> Result<String> {
        Err(CqError::new(
            exit::INBOX_GONE,
            format!(
                "the session's inbox could not be reached after {POST_TRIES} tries ({e}); the session has probably ended"
            ),
        ))
    }
}

/// The bytes of one post: the auth line, then one user message, each
/// ending in a newline, with the keys in the documented order. `content`
/// must not be empty.
pub fn frame(token: &str, content: &str) -> String {
    debug_assert!(!content.is_empty());
    let text = |s: &str| Value::String(s.to_owned()).to_string();
    let auth = format!(r#"{{"type":"auth","token":{}}}"#, text(token));
    let msg = format!(
        r#"{{"type":"user","from":{},"message":{{"role":"user","content":{}}}}}"#,
        text(FROM),
        text(content)
    );
    format!("{auth}\n{msg}\n")
}

/// A fake `Clock` for unit tests, shared with `codex`.
#[cfg(test)]
pub(crate) mod fake_clock {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::Clock;

    /// A clock that moves only when told to, or when slept on.
    #[derive(Clone)]
    pub struct FakeClock {
        base: Instant,
        offset: Rc<Cell<Duration>>,
        pub slept: Rc<RefCell<Vec<Duration>>>,
    }

    impl FakeClock {
        pub fn new() -> Self {
            Self {
                base: Instant::now(),
                offset: Rc::new(Cell::new(Duration::ZERO)),
                slept: Rc::default(),
            }
        }
        pub fn advance(&self, d: Duration) {
            self.offset.set(self.offset.get() + d);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.base + self.offset.get()
        }
        fn sleep(&self, d: Duration) {
            self.slept.borrow_mut().push(d);
            self.advance(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use serde_json::json;

    use super::fake_clock::FakeClock;
    use super::*;

    const TOKEN: &str = "secret-token-1234";

    /// Records each connection's bytes, with the fake time it closed.
    /// Fails the next `fail` connects.
    #[derive(Clone, Default)]
    struct FakeTransport {
        posts: Rc<RefCell<Vec<Vec<u8>>>>,
        fail: Rc<Cell<u32>>,
    }

    struct Conn {
        buf: Vec<u8>,
        posts: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl Write for Conn {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.buf.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Conn {
        fn drop(&mut self) {
            self.posts.borrow_mut().push(std::mem::take(&mut self.buf));
        }
    }

    impl Transport for FakeTransport {
        type Conn = Conn;
        fn connect(&mut self) -> io::Result<Conn> {
            if self.fail.get() > 0 {
                self.fail.set(self.fail.get() - 1);
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such pipe"));
            }
            Ok(Conn {
                buf: Vec::new(),
                posts: self.posts.clone(),
            })
        }
    }

    fn inbox() -> (Inbox<FakeTransport, FakeClock>, FakeTransport, FakeClock) {
        let t = FakeTransport::default();
        let c = FakeClock::new();
        (
            Inbox::new("w", TOKEN.to_owned(), t.clone(), c.clone()),
            t,
            c,
        )
    }

    /// The `content` of each post, after checking the framing.
    fn contents(t: &FakeTransport) -> Vec<String> {
        t.posts
            .borrow()
            .iter()
            .map(|p| {
                let text = std::str::from_utf8(p).unwrap();
                let lines: Vec<&str> = text.split_terminator('\n').collect();
                assert_eq!(lines.len(), 2, "{text}");
                assert!(text.ends_with('\n'));
                let auth: Value = serde_json::from_str(lines[0]).unwrap();
                assert_eq!(auth, json!({ "type": "auth", "token": TOKEN }));
                let msg: Value = serde_json::from_str(lines[1]).unwrap();
                assert_eq!(msg["type"], "user");
                assert_eq!(msg["from"], "cq");
                assert_eq!(msg["message"]["role"], "user");
                msg["message"]["content"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    #[test]
    fn frame_escapes_the_content_and_puts_auth_first() {
        let content = "say \"hi\"\nback\\slash \u{e9}\u{1F600}\ttab";
        let text = frame(TOKEN, content);
        let lines: Vec<&str> = text.split_terminator('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], format!(r#"{{"type":"auth","token":"{TOKEN}"}}"#));
        assert!(
            lines[1]
                .starts_with(r#"{"type":"user","from":"cq","message":{"role":"user","content":""#),
            "{}",
            lines[1]
        );
        let msg: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(msg["message"]["content"], content);
        assert!(lines[1].contains(r#"\"hi\""#) && lines[1].contains(r"\n"));
    }

    #[test]
    fn missing_or_empty_variables_are_usage_errors() {
        let some = |s: &str| Some(OsString::from(s));
        for (socket, token, var) in [
            (None, some("t"), SOCKET_VAR),
            (some(""), some("t"), SOCKET_VAR),
            (some("/s"), None, TOKEN_VAR),
            (some("/s"), some(""), TOKEN_VAR),
        ] {
            let Err(e) = Inbox::from_vars("w", socket, token) else {
                panic!("accepted a missing {var}");
            };
            assert_eq!(e.code, exit::USAGE);
            assert!(e.message.contains(var), "{}", e.message);
        }
        assert!(Inbox::from_vars("w", some("/s"), some("t")).is_ok());
    }

    #[test]
    fn lines_within_the_window_go_in_one_message() {
        let (mut ib, t, c) = inbox();
        ib.line("CQ-NEW a").unwrap();
        c.advance(Duration::from_millis(1500));
        ib.line("CQ-NEW b").unwrap();
        ib.tick().unwrap();
        assert!(
            t.posts.borrow().is_empty(),
            "posted before the window closed"
        );
        c.advance(Duration::from_millis(500));
        ib.tick().unwrap();
        assert_eq!(
            contents(&t),
            ["cq watch (w): queue events\nCQ-NEW a\nCQ-NEW b"]
        );
        ib.tick().unwrap();
        assert_eq!(t.posts.borrow().len(), 1, "posted the same lines twice");
    }

    #[test]
    fn posts_keep_the_minimum_gap() {
        let (mut ib, t, c) = inbox();
        ib.line("CQ-NEW a").unwrap();
        c.advance(BATCH_WINDOW);
        ib.tick().unwrap();
        ib.line("CQ-NEW b").unwrap();
        c.advance(BATCH_WINDOW);
        ib.tick().unwrap();
        assert_eq!(t.posts.borrow().len(), 1, "posted inside the gap");
        c.advance(MIN_GAP - BATCH_WINDOW);
        ib.tick().unwrap();
        assert_eq!(contents(&t)[1], "cq watch (w): queue events\nCQ-NEW b");
        assert!(c.slept.borrow().is_empty(), "tick waited");
    }

    #[test]
    fn a_large_batch_is_split() {
        let (mut ib, t, c) = inbox();
        for i in 0..MAX_LINES + 3 {
            ib.line(&format!("CQ-NEW {i}")).unwrap();
        }
        c.advance(BATCH_WINDOW);
        ib.tick().unwrap();
        c.advance(MIN_GAP);
        ib.tick().unwrap();
        let posts = contents(&t);
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].lines().count(), MAX_LINES + 1);
        assert_eq!(posts[1].lines().count(), 4);
    }

    #[test]
    fn stopped_posts_held_lines_and_the_reason_together() {
        let (mut ib, t, _c) = inbox();
        ib.line("CQ-NEW a").unwrap();
        ib.stopped("--max-seconds 60 reached", Ended::Normal);
        assert_eq!(
            contents(&t),
            [
                "cq watch (w): queue events\nCQ-NEW a\ncq watch (w) stopped: --max-seconds 60 reached. Run /cq-start to re-arm."
            ]
        );
    }

    #[test]
    fn stopped_asleep_says_not_to_re_arm() {
        let (mut ib, t, _c) = inbox();
        ib.line("CQ-SLEEP by=lead reason=null").unwrap();
        ib.stopped("asleep", Ended::Asleep);
        let posts = contents(&t);
        assert_eq!(posts.len(), 1);
        assert!(posts[0].contains("\nCQ-SLEEP by=lead"), "{}", posts[0]);
        assert!(
            posts[0].ends_with("cq watch (w) stopped: w is asleep. Do not re-arm; run /cq-start only when asked to wake."),
            "{}",
            posts[0]
        );
    }

    #[test]
    fn stopped_with_nothing_held_posts_one_line_after_the_gap() {
        let (mut ib, t, c) = inbox();
        ib.line("CQ-NEW a").unwrap();
        c.advance(BATCH_WINDOW);
        ib.tick().unwrap();
        c.advance(Duration::from_secs(1));
        ib.stopped("error: something broke", Ended::Normal);
        let posts = contents(&t);
        assert_eq!(
            posts[1],
            "cq watch (w) stopped: error: something broke. Run /cq-start to re-arm."
        );
        assert_eq!(*c.slept.borrow(), [MIN_GAP - Duration::from_secs(1)]);
    }

    #[test]
    fn a_failed_post_is_retried_with_backoff() {
        let (mut ib, t, c) = inbox();
        t.fail.set(2);
        ib.line("CQ-NEW a").unwrap();
        c.advance(BATCH_WINDOW);
        ib.tick().unwrap();
        assert_eq!(contents(&t).len(), 1);
        assert_eq!(
            *c.slept.borrow(),
            [Duration::from_secs(1), Duration::from_secs(2)]
        );
    }

    #[test]
    fn an_unreachable_inbox_fails_after_the_last_try() {
        let (mut ib, t, c) = inbox();
        t.fail.set(u32::MAX);
        ib.line("CQ-NEW a").unwrap();
        c.advance(BATCH_WINDOW);
        let e = ib.tick().unwrap_err();
        assert_eq!(
            *c.slept.borrow(),
            [1, 2, 4].map(Duration::from_secs),
            "{POST_TRIES} tries"
        );
        let err = ib.failed(e).unwrap_err();
        assert_eq!(err.code, exit::INBOX_GONE);
        assert!(!err.message.contains(TOKEN));
        // The stopped message fails too, and does not panic or loop.
        ib.stopped("x", Ended::Normal);
        assert!(t.posts.borrow().is_empty());
    }
}
