# Test status

What has been tested live, and what has not. Open test work is tracked in the [GitHub issues](https://github.com/niklasrichardson/session-queue/issues?q=is%3Aissue+is%3Aopen+label%3Atesting). Back to the [README](../README.md).

## Not verified

These were not tested by `cargo test`, or not yet tested live:

- Anything on macOS. The code is checked by compiling it for macOS, but it has not run on a Mac.
- The exact cause of the 4 early watcher exits seen on 24 Oct 2026. They were not reproduced (40 runs of the Monitor's command form, 10 at a time, and 4 runs under the Monitor tool). Removing the parent check removes the only silent exit path that fits; a watcher that still ends early now prints its reason to stderr.
- That an orphaned watcher (one whose Monitor was killed) never delays work. It keeps the lock until `--max-seconds`; the new Monitor's `cq watch` exits 5 and the session re-arms with `--takeover`. In a test on 24 Oct 2026, stopping a Monitor with TaskStop killed the whole process tree, `cq.exe` included.
- The Claude Code CLI and Remote Control paths. Live tests used the desktop app's Code tab.
- The rejoin hook after a compaction or a resume. It was tested live after `/clear` only.
- Sending 25 tasks in a row with no person typing (test L3 in the spec).

Tested live in the desktop app on Windows: re-arming after each 30-minute watcher exit (hundreds of re-arms across 10 sessions), the rejoin hook after `/clear`, replies, rejections and multi-hop forwarding.

## cq-top screen order and Flow screen, 29 October 2026

Screens reordered to Overview, Sessions, Flow, Rejected, and the Flow screen restyled (see [cq-top](CQ-TOP.md#flow-screen)). Checked on Windows 11 with `cargo test --workspace` (154 tests, 0 failed; 44 in cq-top), `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- Unit tests in `tools/cq-top/src/main.rs`: the screen order on the tab bar, for the number keys and for next and previous; saved view files with screen names still load; pairs sort by count, then newest; the Workers table joins watchers and reply times. Render tests use ratatui's `TestBackend` with fake data: the Flow screen in every theme at 100 columns (figures, a Braille graph, header rows, names in separate columns, no message text), smaller sizes down to 1x1 without a panic, the columns kept at 60 columns, and the empty state.
- `cq-top --html` and `--html-screens` ran read-only against the live queue root, writing only to a temporary folder. The Flow export showed separate name columns and no message text.

Not verified: the keys pressed in a real terminal; the screens at run time on macOS and Linux.

## Codex wake on Windows, 28 October 2026

`cq watch --to-codex` (experimental, see [Commands](COMMANDS.md#wake-a-codex-chat)). Checked on Windows 11 with `cargo test --workspace` (149 tests, 0 failed), `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. All passed. `cargo clippy -p cq --all-targets -- -D warnings` also passed for `aarch64-apple-darwin` and `x86_64-linux-android` (a compile check of the Unix code; nothing ran).

- Unit tests in `src/codex.rs` (7 tests) use a fake runner and the inbox's fake clock. They cover: one call per 2-second batch, with the fixed prompt; `CQ-WAKE` and `CQ-SLEEP` give no call; the 5-second gap; held lines are dropped when asleep and queued on a normal stop; retries 1 and 2 seconds apart; exit 9 after 4 tries; usage errors for an empty or multi-line thread and an empty prompt.
- `tests/to_codex.rs` (7 tests) runs the real `cq` against a temporary queue root and a fake `codex` (`tests/support/fake_codex_queue.rs`) that records its arguments. The tests check: two waiting messages give one call, and a later message a second; the arguments are exactly `queue --thread THREAD --message PROMPT`, and no subject, body, sender name or ID reaches them; one stdout line per call; `cq sleep` stops the watcher with exit 7 and no call; a name asleep at start exits 7 with no call; `--codex-prompt` replaces the prompt; a non-zero exit, and exit 0 without `Queued message`, each give 4 calls then exit 9; bad flag combinations exit 64; the record is `members/codex-NAME` with mode `codex`, and a Claude Code session's own record is kept; `CQ_SESSION_ID` sets the key; `cq session-start` for a `codex` key says not to start a watcher, both while the watcher runs and after it stops.
- cq-top: a `codex` record and `--to-codex` in a process's arguments show the mode `codex`.

Not verified: `cq watch --to-codex` with the real Codex CLI and Desktop app (the live test in [issue #12](https://github.com/niklasrichardson/session-queue/issues/12) used a prototype script with the same `codex queue` call); a message queued while the Codex app is closed ([issue #25](https://github.com/niklasrichardson/session-queue/issues/25)); a long run; macOS and Linux at run time.

## Inbox mode on Windows, 28 October 2026

`cq watch --to-inbox` (experimental, see [Commands](COMMANDS.md#watch-into-the-inbox)). Checked on Windows 11 with `cargo test --workspace` (127 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. All passed. `cargo clippy --all-targets -- -D warnings` also passed for `aarch64-apple-darwin` and `x86_64-linux-android` (a compile check of the Unix code; nothing ran).

- Unit tests in `src/inbox.rs` (10 tests) use a fake transport and a fake clock, so nothing sleeps. They cover: the auth line first and the message line's shape and key order; escaping of quotes, newlines, backslashes and Unicode in the content; missing or empty variables (exit 64); a 2-second batch; the 5-second gap; a batch over 50 lines split in two; the stopped message for `--max-seconds`, asleep and an error, with held lines in the same message; retries 1 and 2 seconds apart; and exit 8 after 4 tries, with no token in the error.
- `tests/inbox.rs` (9 tests) runs the real `cq` against a temporary queue root and a fake inbox: a named pipe made with `CreateNamedPipeW` in the test. The tests check: a waiting message in the first post; a later message in a second post at least 5 seconds after the first; `cq sleep` gives one message with `CQ-SLEEP` and the asleep line, then exit 7; `--max-seconds 1` gives a stopped message and exit 0; a watcher that finds the name taken posts the exit 5 error; an address where nothing listens gives exit 8 after about 9 seconds; and missing variables give exit 64. For `cq session-start` after `/clear`, they check: a running inbox watcher is kept; a stopped one is restarted in the background; a Monitor watch replaces the inbox mark, and an older record gets the Monitor reminder; and liveness comes from the pid file and a heartbeat under 90 seconds (a half-written heartbeat uses its modified time), with no lock file made. No test prints the token, and none writes to stdout.
- `Env::cmd` in the tests now removes `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN`, so a test run inside Claude Code can never post into the session that runs it.

Not verified: a real Claude Code inbox (the owner's earlier live test used a separate script); a long run; macOS and Linux at run time (the Unix socket test compiles but has not run); a stopped message after SIGTERM from `--takeover`; how the desktop app treats a long-running background Bash command.

## Sleep and wake on Windows, 28 October 2026

Stage 1 of [issue #7](https://github.com/niklasrichardson/session-queue/issues/7): `cq sleep`, `cq wake`, exit 7, `CQ-SLEEP`, `CQ-WAKE` and the cq-top `asleep` display. Checked on Windows 11 with `cargo test --workspace` (112 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- `tests/sleep.rs` (9 tests) uses temporary queue roots. It covers: a name sleeps and wakes itself; only a coordinator sleeps another name (exit 5 otherwise) and any name wakes; a root without `sleep/`; a watcher asleep at start exits 7 and writes no pid file; a running watcher exits 7 within 5 seconds of `cq sleep` and removes its pid file, and a new watcher takes the name after `cq wake`; `CQ-WAKE` for a coordinator only, once per message, only after 2 minutes, never for an awake name, and with no message moved; the `cq send` warning; `cq members` and `cq list`; and `cq session-start` for a sleeping name.
- The 2-minute rule is tested by setting the message file's modified time, not by waiting.
- `cq-top` tests check that a sleeping name is read with its folder, and that Overview, Sessions and both HTML exports show `asleep` and no stale warning.

Not verified: macOS and Linux; the `/cq-stop` and `/cq-start` skills in a live session; waking a session by a cross-session message; cq-top's Sessions row for a sleeping name in a real terminal.

## cq-top on macOS, 27 October 2026

`cq-top` is a read-only terminal view of the queue (see [cq-top](CQ-TOP.md)). It was built and checked on macOS 27.0.1 (Apple silicon) with Rust 1.99.1. No `cq` source file was changed. The root `Cargo.toml` gained a workspace table, so `cq-top` is a workspace member and a default member.

- At the repo root, the four required checks (cargo build, cargo fmt --check, cargo clippy --all-targets -- -D warnings and cargo test) passed: 70 tests in 7 test programs, no failures. 62 are the tests of `cq`, and 8 are the tests of `cq-top`.
- The `cq-top` tests read a temporary queue root and check that a snapshot moves no file. They check that `.reason.json` files are not counted as messages, that the last-hour count skips old queued and claimed messages and counts each message once, and that a bad heartbeat read keeps the last good value. They also draw both screens in all three themes into a test buffer. Each of these checks fails when its fix is removed.
- The program ran against a throwaway queue made with `cq init` and `cq send`. The Overview screen showed the two queued messages. A missing `CQ_ROOT` and a relative `CQ_ROOT` both printed `cq`'s own message and exited with code 70.
- An earlier standalone version of `cq-top` ran for about an hour beside four Claude Code sessions in the desktop app (Code tab) on the same Mac. Each session ran `cq watch` under the Monitor tool. It showed their heartbeats, claims and replies.

Not verified: the keys `b`, `Tab`, `1`, `2` and `t` were not pressed in a real terminal, and the saved view file was tested only through its read and write functions. I did not build `cq-top` on Windows or Linux. The maintainer tested the first commit on Windows 11 (see the pull request).

## Codex on Windows, 26 October 2026

The native dispatcher was built and tested in an isolated clone, with a separate build directory and TEMP/TMP inside that clone. No live queue was used.

- All four required checks passed: cargo build, cargo fmt --check, cargo clippy --all-targets -- -D warnings, and cargo test. The suite ran 66 tests with no failures.
- Thirteen native dispatcher integration tests use a compiled local fake Codex executable. They verify serial task handling, stdin transport, Unicode replies, no duplicate execution, sender rejection, non-task handling, invalid messages, failed/empty/oversized answers, agent rejection, timeout process-tree cleanup, unresolved completion refusal, shared watcher lock, PAUSE/resume, deferred completion, later arrivals, existing-job refusal before claim, PID/takeover, 30-second heartbeat cadence, forced dispatcher death and exit races.
- The session identity test verifies CQ_SESSION_ID precedence, empty-value fallback, sanitisation, the 64-character cap and membership lookup.
- A real read-only Codex CLI 0.162.0-alpha.2 task ran through the native cq dispatch command. It returned NATIVE CODEX ACK and delivered one correlated ok result.
- Earlier compatibility probes verified Git Bash quoted heredocs, native PowerShell 7.6.5 UTF-8 JSON pipelines, AGENTS.md loading and a bounded explicit app-server toolOutput bridge. One earlier headless run hit HTTP 429 and was reported as failed.

Not verified: native dispatcher execution on macOS/Linux, Ctrl+C handling in a foreground terminal, general tool-using write tasks, persistent app-server integration, long-running service operation, automatic recovery or exactly-once task side effects. Forced dispatcher death on Windows was tested: the kill-on-close Job Object stopped the agent and descendant. Unix SIGHUP handling compile-checked successfully for aarch64-apple-darwin; its conditional runtime test was not run here; Unix SIGKILL can still leave children. The dispatcher is a fresh-process worker; it does not wake an existing desktop/interactive CLI chat. Same-chat scheduled polling was tested separately, as described below.

### Review follow-up

The example rules are committed as docs/SESSION-RULES.example.md; the owner's private docs/SESSION-RULES.md stays ignored. Local findings, scratch/build folders and the test queue are ignored. Public documentation links were checked against the staged file list.

Windows regression tests passed for Job Object cleanup after forced dispatcher termination, timeout cleanup, stopping an already exiting agent, PID/watch takeover, heartbeat timing, PAUSE appearing during a run, and an existing job directory leaving the message unclaimed. The test fixture is rustfmt-formatted; its executable lives in the queue test's owned TempDir and is removed with that test.

One initial full run timed out on the continuous-worker test's five-second output deadline. The test now uses a fifteen-second non-performance deadline and includes process/state diagnostics. A separate three-second watcher-readiness assertion also failed; dispatcher test startup waits now consistently allow fifteen seconds, and fixture compilation happens before starting the lock-holder watcher. The final full suite passed. Process startup also checks that ResumeThread actually resumed a suspended thread instead of accepting a no-op on a running thread; the timeout's exact cause was not established.

The existing prune test also failed with its one-second watcher lifetime. It now waits for the prune outcome under a longer bounded watcher instead of measuring startup speed. No prune behaviour was changed. A later run exposed a real concurrent-claim failure: both processes returned exit 0. Claims now hold a per-recipient OS lock across the move/validation transaction; the existing two-claim regression verifies one exit 0 and one exit 1.

A real read-only Codex acknowledgement also passed under the Windows Job Object. No changes were made to the live queue, global settings or permissions.

### Same-chat scheduled polling and Claude interoperability

In the isolated Windows test root, a one-minute Codex desktop automation resumed the existing chat on schedule. Claude sent its test at 15:11:31 UTC; the scheduled trigger arrived at 15:11:45.720 UTC. Codex claimed the task and sent its reply at 15:12:17 UTC. Claude confirmed that its watcher woke on the result, then claimed and finished it with Unicode, dollar signs, backticks and the path literal intact. The automation was paused after success. An earlier Claude-to-Codex watch test also delivered a message, but required a later manual chat turn to process it; stdout alone did not wake Codex.

The first scheduled claim failed with Access is denied (os error 5), because the isolated root was outside the writable workspace. Approved escalation allowed the same claim to succeed. The queue root must be writable from the sandbox before unattended use. No global settings or filesystem permissions were changed.

Empty scheduled-run model-turn/usage cost is unverified: the only scheduled exchange had a pending task, and official scheduled-task guidance did not settle empty-check accounting. No controlled empty-run usage measurement was made. Use a longer interval than one minute for unattended operation until usage is checked. Unix runtime and long-running reliability also remain unverified. See [Waking Codex](CODEX.md#waking-codex) for the route comparison.
