# Test status

What has been tested live, and what has not. Open test work is tracked in the [GitHub issues](https://github.com/niklasrichardson/session-queue/issues?q=is%3Aissue+is%3Aopen+label%3Atesting). Back to the [README](../README.md).

## Not verified

These were not tested by `cargo test`, or not yet tested live:

- Most live use on macOS. See [macOS, 29 October 2026](#macos-29-october-2026) for what was checked.
- The exact cause of the 4 early watcher exits seen on 24 Oct 2026. They were not reproduced (40 runs of the Monitor's command form, 10 at a time, and 4 runs under the Monitor tool). Removing the parent check removes the only silent exit path that fits; a watcher that still ends early now prints its reason to stderr.
- That an orphaned watcher (one whose Monitor was killed) never delays work. It keeps the lock until `--max-seconds`; the new Monitor's `cq watch` exits 5 and the session re-arms with `--takeover`. In a test on 24 Oct 2026, stopping a Monitor with TaskStop killed the whole process tree, `cq.exe` included.
- Remote Control. The Claude Code CLI was tested in one bounded run only: on 29 Oct 2026, one CLI session on Windows in inbox mode answered 25 of 25 tasks with nobody typing (test L3 in the [spec](SPEC.md), [#4](https://github.com/niklasrichardson/session-queue/issues/4)). Nobody checked that session's screen for a cap notice. Other live tests used the desktop app's Code tab.
- The rejoin hook after a compaction or a resume. It was tested live after `/clear` only.

Tested live in the desktop app on Windows: re-arming after each 30-minute watcher exit (hundreds of re-arms across 10 sessions), the rejoin hook after `/clear`, replies, rejections and multi-hop forwarding.

## macOS, 29 October 2026

Reported in [#1](https://github.com/niklasrichardson/session-queue/issues/1). Machine: macOS 26.7.1, Apple silicon, Rust 1.99.1, at commit 8565c92. Setup followed [Setup](SETUP.md).

Passed:

- `cargo build`, `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo test --workspace`: every suite except `tests/dispatch.rs`, including the watcher, inbox, sleep and `to_codex` suites and cq-top (51 tests).
- `cargo install` for `cq` and cq-top, and `cq init` with the default root, which has a space in its path.
- A manual smoke test of `send`, `watch`, `claim`, `done --reply` and `list` in a temporary root.
- `cq watch --takeover` stopped the old watcher for the same name.
- Inbox mode in the Claude desktop app's Code tab: the watcher posted the `CQ-NEW` line into the session, and posted the stopped notice after SIGTERM.
- The local time label in `CQ-NEW` lines.

Failed or intermittent:

- `tests/dispatch.rs`: 2 to 5 tests fail on each run, a different set each time, all with `Codex timed out after 1s`. With `--test-threads=1`, one run had 4 failures and the next passed 13 of 13. The likely cause, not yet confirmed, is the macOS first-run check of the freshly built fake Codex binary.

Not checked on macOS: two Claude Code sessions handing tasks back and forth live; the Monitor fallback under the real Monitor tool; the `cq done` stdin check beyond the tests; the SessionStart hook with a bare `cq` (see [After `/clear` or a restart](CLAUDE-CODE.md#after-clear-or-a-restart)); Codex chats and `cq dispatch` with a real Codex; the cq-top screens in a real terminal ([#17](https://github.com/niklasrichardson/session-queue/issues/17)).

## Rejection notices, list and requeue, 29 October 2026

`cq done --reject` notices for info and result messages, `cq list --rejected` and `cq requeue` ([issue #20](https://github.com/niklasrichardson/session-queue/issues/20), see [Rejections](COMMANDS.md#rejections)). Checked on Windows 11 with `cargo test --workspace` (197 tests after merging main, 0 failed; 51 in cq-top), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- Integration tests in `tests/rejected.rs`: the notice for an info and for a result (ID, subject, reason, `reply_to`, hops, no original body); a task still gets only its `rejected` result; rejecting a notice or a `rejected` result sends nothing; `PAUSE` and the hop limit block the notice but not the reject (exit 0, stderr warning); a 20 KB reason is cut to the body limit; `cq list --rejected` in both directions, with no bodies, and exit 64 with `--stale-minutes`, `--overdue` or the overdue limits; `cq requeue` keeps the ID, removes the reason file, and refuses messages outside the session's own `rejected/` (exit 6) or a path as an ID (exit 5); a running watcher announces a requeued message again, also when the claim, reject and requeue all happen between two scans; `cq requeue` waits for the recipient's claim lock; on Windows, a requeue that cannot set the message's time exits 70 and leaves it in `rejected/`. The three regressions (requeue between scans, claim lock, requeue time) failed before their fixes.

Not verified: live use by queue sessions; macOS and Linux.

## Overdue task warnings, 29 October 2026

`cq list --overdue`, the coordinator's `CQ-OVERDUE` line and the cq-top overdue marks ([issue #21](https://github.com/niklasrichardson/session-queue/issues/21), see [Commands](COMMANDS.md#overdue-tasks)). Checked on Windows 11 with `cargo test --workspace` (184 tests after merging main, 0 failed; 51 in cq-top), `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- Unit tests in `src/overdue.rs` (12 tests): a scan that meets a claim between its rename and its time update waits for the claim lock and does not report the task; on Windows, a claim lock that cannot be opened skips the claimed task; with no claim lock, the claimed time is trusted and no lock is created; the defaults; the limits at 29:59 and 30:00 unclaimed, and 119:59 and 120:00 claimed; only `task` messages count; the variables, empty values, bad values and flags; the line format; a scan that skips done, dead-lettered, `info` and young messages; a scan under `PAUSE`; one task going from unclaimed to claimed to finished.
- `tests/overdue.rs` (10 tests) runs the real `cq` against a temporary queue root. It checks: overdue tasks in two inboxes, unclaimed and claimed, with no bodies and nothing moved; `cq list --overdue` under `PAUSE`; flags win over variables; bad values and flag combinations exit 64; a coordinator's watcher prints one `CQ-OVERDUE` line per task, does not repeat them after a restart, reports a new one, and forgets a finished one; other watchers print none; no `CQ-OVERDUE` under `PAUSE` until it is removed; a bad limit stops a coordinator's watcher with exit 64 before it takes the name; a failed `--to-inbox` post (exit 8) leaves the tasks unmarked, and the next watcher reports them; a claim restarts the clock; on Windows, a claim that cannot set its time puts the message back and exits 70.
- cq-top (3 tests): a snapshot marks overdue tasks from the files' modified times, including under `PAUSE` and with tighter limits; in every theme the header count, the In flight mark and the Flow worker column use the theme's warning colour; the dashboard note and chip.

Not verified: a coordinator session acting on `CQ-OVERDUE` lines live; `CQ-OVERDUE` through `--to-inbox` in a real session; macOS and Linux at run time.

## cq-top screen order and Flow screen, 29 October 2026

Screens reordered to Overview, Sessions, Flow, Rejected, and the Flow screen restyled (see [cq-top](CQ-TOP.md#flow-screen)). Checked on Windows 11 with `cargo test --workspace` (154 tests, 0 failed; 44 in cq-top), `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- Unit tests in `tools/cq-top/src/main.rs`: the screen order on the tab bar, for the number keys and for next and previous; saved view files with screen names still load; pairs sort by count, then newest; the Workers table joins watchers and reply times. Render tests use ratatui's `TestBackend` with fake data: the Flow screen in every theme at 100 columns (figures, a Braille graph, header rows, names in separate columns, no message text), smaller sizes down to 1x1 without a panic, the columns kept at 60 columns, and the empty state.
- `cq-top --html` and `--html-screens` ran read-only against the live queue root, writing only to a temporary folder. The Flow export showed separate name columns and no message text.

Not verified: the keys pressed in a real terminal; the screens at run time on macOS and Linux.

## Codex wake on Windows, 28 October 2026

`cq watch --to-codex` (experimental, see [Commands](COMMANDS.md#wake-a-codex-chat)). Checked on Windows 11 with `cargo test --workspace` (149 tests, 0 failed), `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. All passed. `cargo clippy -p cq --all-targets -- -D warnings` also passed for `aarch64-apple-darwin` and `x86_64-linux-android` (a compile check of the Unix code; nothing ran).

- Unit tests in `src/codex.rs` (24 tests) use a fake runner and the inbox's fake clock. They cover: one call per 2-second batch, with the fixed prompt; `CQ-WAKE` and `CQ-SLEEP` give no call; the 5-second gap; held lines are dropped when asleep and queued on a normal stop; retries 1 and 2 seconds apart; exit 9 after 4 tries; usage errors for an empty or multi-line thread and an empty prompt. For the look-up in a fake Codex queue database: the item ID from the `Queued message` line; a pending item, also one still in the WAL, gives one "has not taken" line after the wait and never again for that item; that warning goes once as an `info` message to each coordinator, with the thread, the wait and the unclaimed count, never to the watcher's own name, once to a name listed twice, not at all without coordinators; a check queued only for a warning sends none when it stalls, so two unloaded Codex coordinators do not warn each other in a loop, but a batch with a real message still does; and a send refused by `PAUSE` leaves the line and the watcher running; a consumed item, a missing file, another schema or a file that is not a database gives no line; the missing file is not created; a zero wait turns the look-up off; the SQLite home comes from the root `sqlite_home` (also as a quoted key, not one in a table), then `CODEX_SQLITE_HOME`, then the Codex home; a `sqlite_home` line or a table-looking line inside a multi-line string is not read; a `config.toml` that is not TOML, or a `sqlite_home` that is not a string, falls back to the next source; a writer holding an exclusive lock makes the look-up fail within the 1-second busy timeout, with no line and no error; a writer that waits up to 5 seconds for a lock, as Codex does, commits all 50 inserts while look-ups run, and the test prints how many inserts had to wait.
- `tests/to_codex.rs` (10 tests) runs the real `cq` against a temporary queue root and a fake `codex` (`tests/support/fake_codex_queue.rs`) that records its arguments. The tests check: two waiting messages give one call, and a later message a second; the arguments are exactly `queue --thread THREAD --message PROMPT`, and no subject, body, sender name or ID reaches them; one stdout line per call; `cq sleep` stops the watcher with exit 7 and no call; a name asleep at start exits 7 with no call; `--codex-prompt` replaces the prompt; a non-zero exit, and exit 0 without `Queued message`, each give 4 calls then exit 9; bad flag combinations exit 64; the record is `members/codex-NAME` with mode `codex`, and a Claude Code session's own record is kept; `CQ_SESSION_ID` sets the key; `cq session-start` for a `codex` key says not to start a watcher, both while the watcher runs and after it stops; with `--codex-check-seconds 1`, an item still in a fake `queue_1.sqlite` in `CODEX_HOME` gives one "has not taken" line and a consumed one none; a missing database gives no line, the watcher keeps going, and `--debug` says why once; the database is found in a SQLite home apart from `CODEX_HOME`, through `sqlite_home` in `config.toml` (over a decoy `CODEX_SQLITE_HOME`), through `CODEX_SQLITE_HOME`, and through `--codex-queue-db`. Every test sets `CODEX_HOME` to a temporary folder, so no test reads the real Codex database.
- cq-top: a `codex` record and `--to-codex` in a process's arguments show the mode `codex`.

Live on Windows 11: on 28 Oct 2026, `cq watch --to-codex` woke two idle Codex Desktop chats, which each claimed a queued message within about 6 seconds; on 29 Oct 2026, a review task round trip (send, wake, claim, reply) took 52 seconds. Not tested with the native watcher: an interactive Codex CLI chat (`codex queue` alone woke one, see [issue #12](https://github.com/niklasrichardson/session-queue/issues/12)). Not verified: a message queued while the Codex app is closed ([issue #25](https://github.com/niklasrichardson/session-queue/issues/25)); a long run; macOS and Linux at run time.

## Inbox mode on Windows, 28 October 2026

`cq watch --to-inbox` (experimental, see [Commands](COMMANDS.md#watch-into-the-inbox)). Checked on Windows 11 with `cargo test --workspace` (127 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. All passed. `cargo clippy --all-targets -- -D warnings` also passed for `aarch64-apple-darwin` and `x86_64-linux-android` (a compile check of the Unix code; nothing ran).

- Unit tests in `src/inbox.rs` (10 tests) use a fake transport and a fake clock, so nothing sleeps. They cover: the auth line first and the message line's shape and key order; escaping of quotes, newlines, backslashes and Unicode in the content; missing or empty variables (exit 64); a 2-second batch; the 5-second gap; a batch over 50 lines split in two; the stopped message for `--max-seconds`, asleep and an error, with held lines in the same message; retries 1 and 2 seconds apart; and exit 8 after 4 tries, with no token in the error.
- `tests/inbox.rs` (10 tests) runs the real `cq` against a temporary queue root and a fake inbox: a named pipe made with `CreateNamedPipeW` in the test. The tests check: a waiting message in the first post; a later message in a second post at least 5 seconds after the first; `cq sleep` gives one message with `CQ-SLEEP` and the asleep line, then exit 7; `--max-seconds 1` gives a stopped message and exit 0; a watcher that finds the name taken posts the exit 5 error; an address where nothing listens gives exit 8 after about 9 seconds; and missing variables give exit 64. For `cq session-start` after `/clear`, they check: a running inbox watcher is kept; a stopped one is restarted in the background; a Monitor watch replaces the inbox mark, and an older record gets the Monitor reminder; and liveness comes from the pid file and a heartbeat under 90 seconds (a half-written heartbeat uses its modified time), with no lock file made; a coordinator's `CQ-OVERDUE` line that goes out only in the stopped message (an early `--max-seconds 1` stop) is saved to the marker, and a restarted watcher does not repeat it. No test prints the token, and none writes to stdout.
- `Env::cmd` in the tests now removes `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN`, so a test run inside Claude Code can never post into the session that runs it.

Not verified: a real Claude Code inbox (the owner's earlier live test used a separate script); a long run; macOS and Linux at run time (the Unix socket test compiles but has not run); a stopped message after SIGTERM from `--takeover`; how the desktop app treats a long-running background Bash command.

## Idle alerts from a Stop hook on Windows, 30 October 2026

[Issue #50](https://github.com/niklasrichardson/session-queue/issues/50): `cq turn-end` and the idle check in `cq watch`. Checked on Windows 11 with `cargo test --workspace` (263 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- Unit tests in `src/idle.rs` cover the decision: wait before `--idle-after`; alert after it; a final write inside the 30-second settle time still alerts; a write after it cancels; a missing transcript cancels; a watcher that starts late decides from the modified time alone.
- They cover the new-turn check: a `user` entry added 10 seconds after the turn end, inside the settle time, cancels; only `assistant` and `system` entries added still alert; a line with no newline is skipped; a transcript that shrank, cannot be read or grew by more than 1 MB cancels.
- They cover the hook input: `cq watch` and `cq.exe` watchers at any path and in quotes, with redirections such as `2>&1`, are exempt; a watcher with other work after it (`&&`, `||`, `|`, `;`, `&`, `$(`, a backtick, a line break) counts; `grep "cq watch"`, `echo cq watch`, other `cq` commands, and tasks with no command (a subagent described "review cq watch races", a Monitor) count as background work; a finished task; the field absent.
- They cover the locks and outcomes: `cq turn-end` writes and removes nothing while the marker lock is held; a check skips while the lock is held, and decides again on a marker replaced meanwhile; a failed removal is tried again with no send; a line delivered after the turn end cancels it, and one delivered before does not; in `src/watch.rs`, a fake sink that ends a new turn while it delivers, before it returns, leaves that turn end in place, for a line delivered at once and for one held until a tick; a recent alert; a failed send tried 3 times; the marker's 4 fields.
- `tests/idle.rs` runs `cq turn-end` with Stop hook JSON on stdin, and `cq watch` with planted turn ends: the marker holds only the time, generation, transcript path and length; input whose first byte comes 300 ms late is still read; background work, and a watcher task with no command, record nothing; a restarted watcher alerts once and prints nothing; a busy transcript and a recent turn end send nothing; a `CQ-NEW` line cancels the turn end; `--idle-after 30s` exits 64.
- Turn-end times are planted in the past, not waited for.

Not verified: the Stop hook in a live Claude Code session, its `background_tasks` field in practice (including whether a Monitor watcher has a command), the transcript entry types Claude Code writes after the hook, and how long it writes to the transcript after the hook.

## Idle alerts on Windows, 29 October 2026

[Issue #50](https://github.com/niklasrichardson/session-queue/issues/50): `cq idle-alert`. Checked on Windows 11 with `cargo test --workspace` (214 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- `tests/idle.rs` (10 tests) runs the command with hook JSON on stdin and temporary queue roots. It covers: a listed worker sends one `info` message to its lead with the fixed subject and body and nothing from the hook input, and records the time; an unlisted name and a missing `idle-alerts`; no member record and no session key; once per hour, and `--every`; a sleeping name; `PAUSE` refuses the send and nothing is recorded; malformed config lines; `--debug`, and exit 64 for a bad `--every`; a held lock sends nothing; 8 hooks started at once send one alert. That last test makes each hook wait 500 ms between its check and its send (hidden `--hold-ms`); with the lock removed, it failed (4 alerts).
- The interval is tested by writing an older time to `idle/<name>`, not by waiting.

Not verified: the `Notification` hook with matcher `idle_prompt` in a live Claude Code session, and the documented 60-second timing of `idle_prompt`; macOS and Linux.

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
