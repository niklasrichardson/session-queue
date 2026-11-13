//! How a watcher wakes the session it serves with one fixed text: a post
//! into a Claude Code inbox (`inbox::Inbox`, for `--to-inbox`), or an
//! `agentapi send-message` into an Antigravity conversation (`AgentApi`,
//! for `--to-agy-session`). The text is always cq's own, never message text.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::platform;

/// Sends one wake text. Tests use a fake.
pub trait Wake {
    /// Send `text` once. `Ok` only when it was delivered as far as cq can
    /// tell.
    fn post(&mut self, text: &str) -> io::Result<()>;
}

/// What tells a `--to-agy-session` watcher that its session still exists.
/// Tests use a fake.
pub trait Link {
    /// The launching session's process still runs.
    fn alive(&mut self) -> bool;
    /// A TCP connect to the language server address succeeds. No data is
    /// sent.
    fn reachable(&mut self) -> bool;
    /// The address's listening socket still belongs to the launching
    /// process. Checked before each wake.
    fn owned(&mut self) -> bool;
}

/// The variables the agentapi child gets; every other one is removed, so
/// nothing the session set reaches it except these. Live test A12 (13 Nov
/// 2026, CLI and App): `send-message` delivers with exactly this list.
pub const CHILD_ENV: [&str; 5] = [
    ADDRESS_VAR,
    "ANTIGRAVITY_CSRF_TOKEN",
    "SystemRoot",
    "TEMP",
    "TMP",
];

/// The language server address Antigravity gives its shell. Never printed,
/// logged or recorded.
pub const ADDRESS_VAR: &str = "ANTIGRAVITY_LS_ADDRESS";
/// The agentapi binary Antigravity names in the App's shell. A cross-check
/// only: the binary run is the launching session's own image.
pub const AGENTAPI_VAR: &str = "ANTIGRAVITY_AGENTAPI_EXE";
/// The conversation ID Antigravity sets in the session's shell (live test
/// A7, both surfaces). The source of the ID; `session::conversation_of`
/// says how the policy's optional `conversation` falls back and checks it.
pub const CONVERSATION_VAR: &str = "ANTIGRAVITY_CONVERSATION_ID";

/// The longest one agentapi call may take.
pub const AGENTAPI_TIMEOUT: Duration = Duration::from_secs(60);

/// `<exe> agentapi send-message <conversation> <text>`: no shell, the
/// arguments passed one by one, and only `CHILD_ENV` in its environment.
pub struct AgentApi {
    pub exe: PathBuf,
    pub conversation: String,
    /// The values of `CHILD_ENV` that were set at start. Holds the token,
    /// so `AgentApi` has no `Debug`.
    env: Vec<(&'static str, OsString)>,
}

impl AgentApi {
    /// `env` gives each variable's value at start.
    pub fn new(exe: PathBuf, conversation: String, env: impl Fn(&str) -> Option<OsString>) -> Self {
        let env = CHILD_ENV
            .iter()
            .filter_map(|&k| env(k).map(|v| (k, v)))
            .collect();
        Self {
            exe,
            conversation,
            env,
        }
    }

    /// The arguments after the executable.
    pub fn args<'a>(&'a self, text: &'a str) -> [&'a str; 4] {
        ["agentapi", "send-message", &self.conversation, text]
    }
}

impl Wake for AgentApi {
    fn post(&mut self, text: &str) -> io::Result<()> {
        let mut command = Command::new(&self.exe);
        command
            .args(self.args(text))
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut agent = platform::spawn_agent(&mut command)?;
        let deadline = Instant::now() + AGENTAPI_TIMEOUT;
        let status = loop {
            match agent.child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() >= deadline => {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("agentapi ran past {} s", AGENTAPI_TIMEOUT.as_secs()),
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(e) => break Err(e),
            }
        };
        if platform::stop_agent(&mut agent).is_err() {
            let _ = agent.child.kill();
            let _ = agent.child.wait();
        }
        match status? {
            s if s.success() => Ok(()),
            s => Err(io::Error::other(match s.code() {
                Some(c) => format!("agentapi exited {c}"),
                None => "agentapi was stopped".to_owned(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_child_gets_only_the_allowed_variables_and_the_text_as_one_argument() {
        let api = AgentApi::new("agy.exe".into(), "conv-1".into(), |k| match k {
            "TEMP" => Some("t".into()),
            "PATH" | "CQ_ROOT" => Some("bad".into()),
            _ => None,
        });
        let names: Vec<_> = api.env.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, ["TEMP"]);
        assert_eq!(
            api.args("two words; and more"),
            ["agentapi", "send-message", "conv-1", "two words; and more"]
        );
        assert!(CHILD_ENV.contains(&ADDRESS_VAR));
    }
}
