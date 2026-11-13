# Test status

What has been tested live, and what has not. Open test work is tracked in the [GitHub issues](https://github.com/niklasrichardson/session-queue/issues?q=is%3Aissue+is%3Aopen+label%3Atesting). Back to the [README](../README.md).

## Not verified

These were not tested by `cargo test`, or not yet tested live:

- Most live use on macOS. See [macOS, 29 October 2026](#macos-29-october-2026) for what was checked.
- The exact cause of the 4 early watcher exits seen on 24 Oct 2026. They were not reproduced (40 runs of the Monitor's command form, 10 at a time, and 4 runs under the Monitor tool). Removing the parent check removes the only silent exit path that fits; a watcher that still ends early now prints its reason to stderr.
- That an orphaned watcher (one whose Monitor was killed) never delays work. It keeps the lock until `--max-seconds`; the new Monitor's `cq watch` exits 5 and the session re-arms with `--takeover`. In a test on 24 Oct 2026, stopping a Monitor with TaskStop killed the whole process tree, `cq.exe` included.
- Remote Control.
- The Claude Code CLI beyond one bounded run ([Claude Code CLI with nobody typing](#claude-code-cli-with-nobody-typing-on-windows-29-october-2026)). Other live tests used the desktop app's Code tab.
- The rejoin hook after a compaction or a resume. It was tested live after `/clear` only.

Tested live in the desktop app on Windows: re-arming after each 30-minute watcher exit (hundreds of re-arms across 10 sessions), the rejoin hook after `/clear`, replies, rejections and multi-hop forwarding.

## Antigravity session mode on Windows, 13 November 2026

`cq watch --to-agy-session` (cq 1.17.0, [issue #104](https://github.com/niklasrichardson/session-queue/issues/104)). Checked on Windows 11 with `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --workspace`. The watcher itself was not run live.

- `src/broker/session/tests.rs` (20 tests, fake session, fake link and fake clock): restart from each phase, never NEW-WORK once `started` exists and never RESUME twice; a valid reply; an info read; an invalid reply that stays the same for 60 seconds, and one that keeps changing; exit 8 after 3 wakes with no `started`, and a new round on the next start; exit 8 on process exit, on 3 failed connects and on 4 failed agentapi calls (1, 2 and 4 s apart); an address that changed owner; nothing claimed and no deadline while asleep; a sender outside `allow_from`; a print-mode record refused; the exact wake texts.
- `src/session/tests.rs` (18 tests, fake process tree and real folders): a hostile environment (every variable but the address and the cross-checks points at a decoy root) changes nothing; no build root; a Low-labelled root; an inheritable Modify entry on the root for a capability SID that the policy does not list (the live S1 finding); a config writable by the sandbox SID; a junction; no policy table; the strict policy; an agentapi variable that names another program; no address, and an error that never shows the address; an address owned by another process; parents that started after their child; the nearest launcher; the conversation ID from `ANTIGRAVITY_CONVERSATION_ID`, the table's optional `conversation` as fallback, and exit 64 when they differ, when both are missing or when the variable is not one line; the real process and listener tables.
- `src/exchange.rs`: the trust check refuses a write grant (direct, inheritable or inherit-only) to any `S-1-15-...` SID, the Low SID, RESTRICTED, WRITE RESTRICTED and a listed SID, and passes the owner, users, Authenticated Users, administrators and SYSTEM.
- `src/main.rs`: `--to-agy-session` takes only `--as` and `--takeover`; the stop line for each exit.

Live tests by the owner on 13 November 2026, CLI and App, with `agentapi` but not with this watcher: see [Open gates](agents/ANTIGRAVITY.md#open-gates). S1 environment, A5 and A12 passed; A7 found `ANTIGRAVITY_CONVERSATION_ID`; A8 found that a wake is not an interrupt; S1 writes failed on a root with a capability-SID Modify entry, which the start check now refuses, and the CLI's file tool wrote in the queue root with no prompt (likely `allowNonWorkspaceAccess: true`, not re-tested; now a documented setup rule).

Not verified: an end-to-end run of the watcher with a real session; S1 writes on a fixed root; the Unix build (no Unix target here; CI builds it).

## cq-top context column on Windows, 12 November 2026

cq 1.16.0 / cq-top 0.5.0: the Overview Watchers table and the `--html` dashboard show each watcher's session context ([issue #102](https://github.com/niklasrichardson/session-queue/issues/102)). Checked on Windows 11 with `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` (cq-top: 92 tests, 0 failed). CI passed on Windows, macOS and Linux. Not yet checked live in a terminal.

## Review fixes from issue #83 on Windows, 11 November 2026

cq 1.15.1: findings M1, M2, L2 and L5 of [issue #83](https://github.com/niklasrichardson/session-queue/issues/83). Checked on Windows 11 with `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` (449 tests, 0 failed, 1 ignored). Nothing was run live.

- `tests/dispatch.rs`: unlisted tasks whose rejection reply the pair rate limit refuses are recorded `completed`, an allowed task in the same run still runs, and a second run starts and handles a new task. A Codex stderr cut inside a UTF-8 character at 4096 bytes still gives the detail.
- `tests/config.rs` (12 tests): a `tz_label` with a newline and a control character forges no line and is cut to 16 characters; `rules` with a newline is ignored with a warning and the reminder does not name it; a UTF-8 byte order mark is skipped; UTF-16 text with or without a byte order mark is ignored with a warning.
- `tests/rejected.rs`: a crafted `time` in a reason file prints as `rejected=?`.
- Unit tests in `src/timefmt.rs`, `src/record.rs` and `src/exitlog.rs`: the label filter, and a crafted version reads and logs as unknown.
- Each new test failed without its change, except the byte order mark test: the TOML parser already skipped the mark.

## Antigravity CLI broker mode shell on Windows, 11 November 2026

`cq watch --to-agy --agy-broker` (cq 1.15.0). Live test with the real `agy` on Windows 11, 2 turns: the turn could use the exchange folder through the sandbox SID, and `--add-dir` gave it the job folder. In both turns agy's sandboxed shell did not start: `exebox: creating sandbox: CreateProcessSecurityEnvironment: The process cannot access the file because it is being used by another process`. agy wrote `reply.json` with its file tool instead.

- **Cause, found with print turns on scratch folders.** With no handle open, the shell started, and afterwards the `--add-dir` folder and each folder above it (not the volume root) had a new entry that gives the sandbox SID traverse and synchronize. The process's current folder on the `--add-dir` folder (traverse access) did not stop the shell. A handle with the watcher's job folder access did, also with full sharing, and then no entry was added. The same handle without delete access did not. So agy's sandbox opens these folders without delete sharing, and the delete access of the watcher's held handles caused the failure. A direct test that changes a folder's DACL by name or through a `READ_CONTROL | WRITE_DAC` handle did not conflict with any held handle, because that access is not share-checked.
- **Fix.** The held exchange and job folder handles have no delete access. They still deny delete sharing. A job folder is deleted through a new handle after its identity is checked.
- **After the fix.** `live_agy_shell_starts_in_a_pinned_job_folder` (ignored by default; runs one real print turn) held the exchange and job folders through cq's exchange code, as the watcher does, with the job folder as agy's current folder and `--add-dir`. The shell started and wrote a file. agy added no entry to the exchange or job folder, where the sandbox SID already has Modify; the folders above them got the traverse entry. The exchange folder was accepted again afterwards.
- `src/exchange.rs`: a test opens the job folder, the exchange folder and a folder above them without delete sharing while they are held, adds the traverse entry, and checks that the folders still cannot be renamed, that the job folder is deleted, and that the exchange folder is accepted again. Without the fix, the open fails. The set comparison accepts the traverse entry for the sandbox SID and refuses it for another SID, with inherit flags, or with more access.
- The turn prompt now says that the turn may write `reply.json` with its shell or its file tool.
- Checked on Windows 11 with Rust 1.98.1: `cargo build`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` (440 tests, 0 failed, 1 ignored), after a rebase onto cq 1.14.0 (`cq lock`).
- **Full watcher turn after the fix.** A `cq watch --agy-broker` turn with the real `agy` ran a task that told it to use the shell: the shell started, ran the command and wrote `reply.json`, and the watcher finished the message with that reply. The exchange folder's access did not change.
- **Not verified:** whether agy adds the traverse entry to an exchange folder when the configured sandbox SID is not the shell's SID.

## Named locks on Windows, 11 November 2026

[Issue #99](https://github.com/niklasrichardson/session-queue/issues/99), `cq lock`. Checked on Windows 11 with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` (398 tests, 0 failed). Nothing was run live in a session, or on macOS or Linux.

- `src/lock.rs` unit tests: take, already yours and held; bad names, `--for` and `PAUSE`; 24 threads taking one lock at once, 5 rounds, exactly one wins; only the holder releases; the owner hold; break; an unreadable lock file; the orphan rule at 89 and 90 seconds; alerts once per take; a new unreadable file under the same name reported again with no free scan between; a coordinator's watcher finds its own old lock stale, while other readers keep the reused-PID rule; a take whose flush fails, with a break and a new take started before its cleanup, does not remove the new hold (the injected failure test fails without the guard); wait free, at its limit and on release.
- `tests/lock.rs`: the same through the `cq` binary, with 16 processes taking one lock at once, 3 rounds; a session refused under another name; orphaned and stale in `status` and `list`, and `CQ_LOCK_STALE_MINUTES`; `cq status` with `locks=`; a coordinator's watcher prints `CQ-LOCK-ORPHAN` and `CQ-LOCK-STALE` once, not again after a restart, and again for a new take; a coordinator's watcher prints `CQ-LOCK-STALE` for its own old lock; a worker's watcher prints none; take, wait, release and status from Git Bash.
- Not verified: the forms under a Claude Code Monitor or `--to-inbox` in a live session, and the lock under real GUI test use.

## Antigravity CLI broker mode on Windows, 10 November 2026

`cq watch --to-agy --agy-broker` (cq 1.15.0). Checked on Windows 11 with Rust 1.98.1: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` (412 tests, 0 failed). No test ran the real `agy`.

- `src/exchange.rs` (8 Windows tests and 5 Unix tests, on a temp folder): a new exchange folder gets the Low label and the protected DACL, and its children inherit them; a file renamed in keeps its Medium label. An existing folder is compared as a set: reordered entries pass, with the token's default owner (BUILTIN\Administrators when elevated); a missing inherit flag, an extra entry, an unprotected DACL or no label is refused, as is a missing or invalid SID. Junctions at an ancestor (with missing folders below it: none is made at its target), a job folder or `reply.json` are refused, and a junction inside a job folder is deleted, not followed. Pinned folders cannot be renamed. A read is capped, and refuses a file open for writing, a second hard link and a folder. Resolving gives the real long name for another case and makes nothing; a junction is refused. On Unix, a symlink above the exchange folder is resolved, the walk itself follows no link, and missing folders are made 0700 at the target; an exchange folder that is itself a link is refused; a job folder path that leads to another folder is caught, and a link back to the same folder is not. The Unix trust rule: a folder above the exchange folder that group or others can write is refused before anything is made in it; a sticky shared folder with a folder of this user (or root) in it is accepted, and with another user's folder in it is refused; a folder of another user is refused; and a folder above the job folder (or the job folder itself) opened to group or others after start fails the check before the turn, also after it has moved.
- `src/broker.rs` (27 tests: 2 Windows only, 3 Unix only; fake turn runner and clock): the verdict table; `reply.json` checks (UTF-8 BOM, UTF-16, NUL, wrong id or nonce, unknown field, field rules per outcome, size); the prompt and log never hold message text; the allow list; spawn retries after 1, 2 and 4 s and exit 11 after 4; timeout and a stop during a turn hold the job; a record write that fails after the spawn stops the turn once, and the next start holds the job without a second turn; PAUSE; start-up refusals (torn, unknown, missing record, claimed message without a record, two unfinished records, unexpected exchange entry) a start through a junction makes no folder at its target; an alias into the queue root (an 8.3 name on Windows, a symlink on Unix) is refused, and through an alias agy gets the resolved path; on Unix, an exchange path that moves before the turn holds the job with no turn, and one that group or others can write exits 64 at start and holds the job with no turn when it becomes so before the turn; and recovery from `claimed` with a leftover job folder (with and without `request.json`), `prepared`, `launching`, `running`, `ended` and `completing`.
- `tests/agy_broker.rs` (6 Windows tests and 1 test elsewhere, fake `agy` binary): elsewhere, `--agy-broker` exits 64 (Windows only) and makes no exchange folder. On Windows: reply, reject, read, no reply and wrong nonce end to end; another sender's task is rejected without a turn; a held record stops the next start with exit 5; bad flags exit 64.
- `examples/start-watchers.ps1` with a fake `cq`: the broker fields become the right flags, and broker fields without `broker` are refused.
- **Release gates, live:** that a real turn's token has the sandbox SID and can write the exchange folder, and that `--add-dir` gives the turn the job folder, were tested on 11 November 2026 (see the section above). Not verified: Unix code paths of the exchange folder were not compiled or run here. Broker mode is Windows only; on Unix the trust rule reads no ACLs, which is why `--agy-broker` is refused there.

## Reply and hop limits, and CI, on Windows, 10 November 2026

Three `cq send` and `cq done` changes from a code review. Checked on Windows 11 with Rust 1.98.1: `cargo build`, `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` (371 tests, 0 failed). All passed, with the Codex change below included.

- `tests/queue.rs`: `cq send --reply-to` a result or an info exits 5 and writes nothing, and a reply to a task is sent. A task with 4 hops gets its reply, its rejection and a `send --reply-to` result with 5 hops, and nothing can be sent about that answer. `cq done --reply` and `cq done --reject` on a claimed file that is not readable JSON move it and exit 4; plain `cq done` exits 0. Each of the three tests failed before the change.
- `tests/rejected.rs`: a reject notice to a message with 5 hops is refused, and the reject still succeeds.
- First CI run (`.github/workflows/ci.yml`, GitHub runners): Windows and Linux (`ubuntu-latest`) passed all four checks. This is the first Linux build and test run. macOS built and passed clippy, and failed one test: `stopping_an_agent_that_is_exiting_is_successful` in `tests/dispatch.rs`, with `stop agent process group: Operation not permitted (os error 1)`.
- After that run, `stop_agent` in `src/platform.rs` treats `EPERM` as "already gone" when the child has exited. Not verified: this macOS fix, until CI runs again. Nothing was run live on macOS or Linux.

## Codex wake without waiting on Windows, 10 November 2026

`cq watch --to-codex` no longer waits for `codex queue` in the watcher loop. Checked on Windows 11 with `cargo build`, `cargo fmt --all --check`, `cargo clippy -p cq --all-targets -- -D warnings` and `cargo test` (368 tests, 0 failed). All passed.

- Unit tests in `src/codex.rs` use a fake runner whose call can hang, and the fake clock. They check: no tick waits; a hung call is killed after 60 seconds and tried again with the lines that arrived meanwhile; retries start 1, 2 and 4 seconds later, not 1 ms before; a success resets the failure count; sleep kills a running call at once and reports the lines as not delivered; a normal stop waits for a running call, keeps the gap and the retries, and kills a hung call after 60 seconds.
- `tests/to_codex.rs`: with a fake `codex queue` that hangs and starts a helper, `cq sleep` stops the watcher with exit 7 within 5 seconds, and neither the call nor its helper runs on. Before the change, this test failed: the watcher did not exit.
- Not verified: the heartbeat during a hung call (the loop that writes it is the same one that noticed sleep, but no test waits 30 seconds for it); a real `codex queue`; macOS and Linux at run time.

## cq-top Antigravity desktop app rows on Windows, 7 November 2026

The Sessions screen reads the Antigravity desktop app's conversations (issue #71, see [cq-top](CQ-TOP.md#codex-and-antigravity-rows)). Checked on Windows 11 with `cargo test --workspace` (336 tests, 0 failed; 68 in cq-top), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`. All passed.

- The desktop app's `~/.gemini/antigravity/conversation_summaries.db` was opened read-only from a copy. Its `conversation_summaries` table has the same 21 columns as the CLI's, and `search_schema_version` is 3 in both. Only counts, times, `source` and `status` values were read, never titles, previews or summaries.
- The app's database also listed every CLI conversation, with the same ID, step count and modified time, no title, and `source` `CORTEX_TRAJECTORY_SOURCE_CLI`. Its own conversation had an empty `source`, as the CLI's rows do in the CLI's database.
- The app's `conversations/` folder holds one SQLite file per conversation, with protobuf steps. It is not read.
- Unit tests in `tools/cq-top/src/sessions/agents.rs` and `sessions.rs`: both databases, either one alone, CLI conversations listed by the app shown once as `gemini` rows, and an app database without a `source` column, without the table, or not SQLite, which gives no app rows and leaves the CLI's rows.

Not verified: the rows in a real terminal while the desktop app runs; macOS and Linux paths.

## Sandbox writes to the default root on Windows, 6 November 2026

A finding from live use on Windows 11, not a test of a cq change. See [Check that agents write the root for real](SETUP.md#check-that-agents-write-the-root-for-real).

- Claude Code's Bash tool ran in a sandbox that redirected writes under `%LOCALAPPDATA%` (`C:\Users\<user>\AppData\Local`) into a private overlay.
- Later Bash commands in that session saw the files. PowerShell and other programs did not. Whether other sandboxed sessions shared that copy was not checked.
- Writes under the project's working folders were real.
- A queue root at the default `%LOCALAPPDATA%\cq` therefore split into separate copies. Agents that ran `cq` through such a sandbox saw one queue. Real programs (watchers started elsewhere, Codex, `cq-top`) saw another.

Not checked:

- Whether this happens on every Claude Code install, or only with some sandbox settings.
- Whether two sandboxed sessions share one private copy.
- macOS and Linux.

## Root config.toml and the watcher script on Windows, 6 November 2026

`config.toml` in the queue root (see [Configuration](SETUP.md#configuration)) and `examples/start-watchers.ps1` (see [Watchers](WATCHERS.md)). Checked on Windows 11 with `cargo test --workspace` (330 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`. All passed.

- Unit tests in `src/config.rs` (8 tests): every key is read; a relative `rules` path is joined to the root and an absolute one is kept; empty text and empty values give no settings; an unknown key is named in the warning and the other keys still apply; a wrong type or an unknown zone drops only that key; bad TOML ignores the whole file and gives the line; the order is environment variable, file, build default.
- `tests/config.rs` (8 tests then) runs the real `cq` against temporary queue roots. `tz` and `tz_label` from the file set the local time unless `CQ_TZ` and `CQ_TZ_LABEL` are set; `--help` shows the file's zone; the `session-start` reminder names the file's `rules` path unless `CQ_RULES` is set; with an unknown key, `list`, `watch`, `status`, `init` and `send` run as usual, each prints one warning line that names the file and the key, and the other keys still apply; a wrong type or an unknown zone gives a warning, the other key still applies and `CQ_TZ` still wins; bad TOML and a duplicate key ignore the whole file with a warning that gives the line; `session-start` prints the warning only with `--debug`; a missing or empty file changes nothing and prints nothing.
- The build-time defaults (`CQ_RULES_PATH`, `CQ_DEFAULT_TZ`, `CQ_DEFAULT_TZ_LABEL`) are set at compile time, so only the unit test checks their place in the order.
- The script was parsed with PowerShell 7.6 (0 errors). Its config checks were run against bad configs, and its output against a fake `cq` that echoes its arguments and exits 3. Six watchers were run against a fake `cq` that prints a start line, waits 5 seconds, then prints a done line: all six started within 0.1 seconds. Before the fix, the sixth started only after the first five ended, because `Start-ThreadJob` runs at most 5 jobs by default. It has not been run with real watchers, and closing the window or pressing Ctrl+C was not tested.

## Antigravity CLI findings on Windows, 6 November 2026

Found on Windows 11 with a real `agy`. Each finding starts with its scope.

- **Background command, one manual test.** An open interactive `agy` session started a shell command in the background and ended its turn. When the command finished, the session woke by itself and replied. Not yet tried: a long-running `cq watch` as the background command, and the Antigravity desktop app.
- **`agy agentapi send-message`, tried from outside `agy`.** The command needs the address of a running language server (`ANTIGRAVITY_LS_ADDRESS`) and a CSRF token. `agy` gives that token only to processes that it starts. Called from outside `agy`, it failed. So a watcher outside the session cannot use it.
- **Conversation stores.** The CLI and the desktop app keep separate conversation stores under `~/.gemini`. The desktop app's summary database also lists CLI conversations. Not known: whether `agy --conversation` can resume a conversation from the desktop app.
- **Hooks, read in the docs only.** The hooks docs bundled with the CLI describe 6 events, each given JSON on stdin with camelCase field names. No hook was tested with `cq`. What this means for `cq` is in [Antigravity CLI setup](agents/ANTIGRAVITY.md#what-else-is-known).

## Antigravity CLI live use on Windows, 6 November 2026

Live use of `cq watch --to-agy` with a real `agy` on Windows 11. The automated tests, with a fake `agy`, and the hand tests of the `agy` command are in [Antigravity CLI wake](#antigravity-cli-wake-on-windows-6-november-2026).

- A test name's watcher woke a throwaway conversation. The turn ended with status `SUCCESS` after 22 seconds. That conversation had no queue rules, so it did not claim the message.
- A worker conversation with the queue rules in its context: one turn started 3 seconds after the message was sent and ended after 58 seconds. The message was claimed and done.
- A second message was sent while the same conversation was also open in an interactive `agy` window. The print-mode turn claimed and finished it. The open window did not show that turn. About 5 minutes later, the interactive session exited with `Read loop ... context canceled`. After it was reopened, the conversation history included the print-mode turn. The cause of the exit is not known.
- The `duration_seconds` value that `agy` reported did not match the turn's wall time: for example, 3600 for a 22-second turn. `cq` only logs this value.
- No way is known to send a prompt into an `agy` session that is already open. The CLI help was checked: `remote-control` and `acp` do not offer it. `agy agentapi send-message` was tried later: see [Antigravity CLI findings](#antigravity-cli-findings-on-windows-6-november-2026).

Not tested: the Antigravity desktop app with `cq`; a long run; macOS and Linux.

## Codex CLI with the native watcher on Windows, 3 and 6 November 2026

Live use with real agents on Windows 11: interactive Codex CLI chats, each woken by its own `cq watch --to-codex` watcher.

- 3 Nov 2026: two interactive Codex CLI chats each had their own native watcher. Each chat woke by itself on a queued message and handled it: one in 38 seconds, one in 2 minutes 23 seconds.
- 3 Nov 2026, after about 50 minutes idle: both chats marked a message done within 19 to 25 seconds. Neither watcher sent a "has not taken" warning. An idle time of several hours was not tested.
- 6 Nov 2026: a PC restart closed the CLI chats. The watchers were started again. They queued checks that nothing took, and each sent the `NAME: Codex chat has not taken a queued check` warning, as designed. Then `codex resume THREAD_ID` was run for each chat, and the chats took the waiting messages.

Not tested: macOS and Linux.

## Antigravity CLI wake on Windows, 6 November 2026

`cq watch --to-agy` (experimental, see [Commands](COMMANDS.md#wake-an-antigravity-cli-conversation)). Checked on Windows 11 with `cargo test --workspace` (315 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check`. All passed.

The `agy` command that the watcher runs was tested by hand on Windows on 6 November 2026, before the watcher was written:

- `agy --conversation CONVERSATION_ID -p PROMPT --output-format json` prints one JSON object with `conversation_id`, `status` (`SUCCESS`), `response`, `duration_seconds`, `num_turns` and `usage`. A line `root agent idle; waiting for 1 background task(s) to finish` can come before it.
- `--conversation` resumes the conversation from any folder. The turn's workspace stays the conversation's own.
- A print-mode turn ran `cq --version` without a permission prompt.
- Turns took 4 to 80 seconds, and up to about 3.5 minutes.
- Two print-mode turns on one conversation at the same time both succeeded.

Tests, all with a fake `agy`; no test runs the real one:

- Unit tests in `src/agy.rs` (16 tests) use a fake runner and the inbox's fake clock. They cover: one turn per 2-second batch, with the fixed prompt; lines during a turn give one turn after it; `CQ-WAKE` and `CQ-SLEEP` start nothing; held lines are dropped when asleep, and a normal stop starts a last turn, but not while a turn runs; retries 1, 2 and 4 seconds apart, then exit 11; a success resets the failure count; a failed start is retried; another conversation is not retried, whatever the exit code or status; a turn past the time limit is stopped and retried; usage errors for an empty or multi-line conversation ID and an empty prompt; the JSON is found after other lines; a non-zero exit, no JSON or a status other than `SUCCESS` fails; the summary and the failure reasons never hold the `response` or other stdout text, also with a cut-off JSON result; each turn gets new output files; output files over 30 minutes old are deleted, and other files are kept.
- `tests/to_agy.rs` (13 tests) runs the real `cq` against a temporary queue root and a fake `agy` (`tests/support/fake_agy.rs`) that records its arguments. The tests check: two waiting messages give one turn, and a later message a second; the arguments are exactly `--conversation ID -p PROMPT --output-format json`, and no subject, body, sender name or ID reaches them; the `response` text is never printed; the temporary output files are removed; old `cq-agy-*` output files in the temp folder are deleted at start and after each turn, and other files are kept; `cq sleep` stops the watcher with exit 7; a name asleep at start exits 7 with no turn; `--agy-prompt` replaces the prompt; a non-zero exit, a status other than `SUCCESS` and no JSON each give 4 turns, then exit 11; another `conversation_id` gives 1 turn, then exit 11; a slow turn does not block the watcher, messages during it give one more turn, and `cq sleep` stops the watcher within 3 seconds while a turn runs; bad flag combinations exit 64, `--codex-check-seconds` with `--to-agy` among them; the record is `members/agy-NAME` with mode `agy`, `CQ_SESSION_ID` sets the key, and `cq status` shows `mode=agy`; `cq session-start` for an `agy` key says not to start a watcher, both while the watcher runs and after it stops; `agy` gets `CQ_ROOT` but none of the session variables. Two tests run the turn runner directly with a fake `agy` that starts a descendant: the time-limit kill ends the descendant too, and a turn whose watcher stops runs on with its descendant. The fake `agy` fails if `CLAUDE_CODE_MESSAGING_TOKEN` reaches it.
- cq-top: `--to-agy` in a process's arguments and an `agy` record show the mode `agy` and the agent `gemini`; the conversation ID is read from `--to-agy ID` and `--to-agy=ID`; an `agy -p` turn is not counted as a conversation's process.

Not verified by these tests: `cq watch --to-agy` with a real `agy`; a turn while the same conversation is open in an interactive `agy --conversation CONVERSATION_ID`. Both were later tried in live use: see [Antigravity CLI live use](#antigravity-cli-live-use-on-windows-6-november-2026). Also not verified: a long run; macOS and Linux at run time.

## Monitor tasks and the idle alert on Windows, 6 November 2026

`cq turn-end` no longer counts a task of `type` `monitor` as background work (issue #50). In live use, a session's `background_tasks` held Monitor tasks with no command that Claude Code had armed by itself, so no idle alert was sent for it. Checked on Windows 11 with `cargo test --workspace` (289 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. All passed.

- Unit tests in `src/idle.rs`: a running Monitor task does not count, with a `cq watch` description, with another description, or with none; a running subagent, or a shell task that is not the watcher, next to a Monitor still counts.
- `tests/idle.rs`: a Monitor next to the watcher records the turn end; a Monitor next to a subagent records nothing.
- Not tested live: a session with a Monitor task that gets the alert.

## Watcher status and exit log on Windows, 4 November 2026

`cq status` and `CQ_ROOT/watch-exits.log` (see [Watcher status and exit log](COMMANDS.md#watcher-status-and-exit-log)). Checked on Windows 11 with `cargo test --workspace` (286 tests, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`. All passed.

- `tests/status.rs` (7 tests) runs the real `cq` against temporary queue roots, with no session key for `cq status`. It covers: `stopped` (exit 10) with no pid file; `running` (exit 0) with the watcher's process ID and the mode from its `members/` record; `asleep` (exit 7) after `cq sleep`, then `stopped` after `cq wake`; a pid file with no heartbeat and with a 300-second-old heartbeat; a bad name (exit 5).
- The same file covers the log: one line on `--max-seconds` (exit 0) and on sleep (exit 7); no line for a start refused with exit 5 or asleep at start; after `--takeover`, a line for the old watcher written by the new one; after a kill, a `previous watcher ended without logging` line with the old watcher's last heartbeat; a log over 1 MiB renamed to `.old`.
- Unit tests in `src/exitlog.rs` cover the line format and quoting, and the rename. `concurrent_appends_rotate_once_and_keep_every_line` starts 64 writers at once on a full log, 10 times: each time the log is renamed once and every line is kept whole. Without the lock it failed in the first round. `an_exit_that_cannot_lock_writes_nothing_and_keeps_its_pid_file` holds the lock past the wait and rotates the log meanwhile: the waiting exit writes nothing to either file and keeps its pid file.
- `watch::tests::a_logged_console_or_panic_exit_is_not_logged_again_on_restart` writes the exit line from a console event and from a panic, then runs the start-up check: no `previous watcher ended without logging` line follows.

Not verified: the Windows console events (Ctrl+C, Ctrl+Break, close, logoff, shutdown) were not sent to a watcher; the signal lines on macOS and Linux; the Unix code was not compiled, because the bundled SQLite needs a C cross-compiler.

## Claude Code CLI with nobody typing on Windows, 29 October 2026

Live test L3 from the [spec](SPEC.md) ([#4](https://github.com/niklasrichardson/session-queue/issues/4)): send 25 tasks over the queue with no human typing.

- 29 Oct 2026, 11:22 to 12:00 UK: one Claude Code CLI session (2.1.303, Windows, auto mode) in inbox mode answered 25 of 25 tasks. Nobody typed after the folder trust prompt at start.
- A script sent the tasks from two sender names, about 95 seconds apart. Send to reply took 7 to 15 seconds (average 9.5).
- Tasks 11 to 25 went past the limit of 10 in a row on Claude Code's own cross-session messages, with no change. None was rejected, and none went to the dead letter queue.

Not checked: the receiving session's screen for a cap notice. A cap would have stopped the replies, and none stopped.

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

Not checked on macOS: two Claude Code sessions handing tasks back and forth live; the Monitor fallback under the real Monitor tool; the `cq done` stdin check beyond the tests; the SessionStart hook with a bare `cq` (see [After `/clear` or a restart](agents/CLAUDE-CODE.md#after-clear-or-a-restart)); Codex chats and `cq dispatch` with a real Codex; the cq-top screens in a real terminal ([#17](https://github.com/niklasrichardson/session-queue/issues/17)).

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

Live on Windows 11: on 28 Oct 2026, `cq watch --to-codex` woke two idle Codex Desktop chats, which each claimed a queued message within about 6 seconds; on 29 Oct 2026, a review task round trip (send, wake, claim, reply) took 52 seconds. When this entry was written, the native watcher was not tested with an interactive Codex CLI chat (`codex queue` alone woke one, see [issue #12](https://github.com/niklasrichardson/session-queue/issues/12)). It was on 3 Nov 2026: see [Codex CLI with the native watcher](#codex-cli-with-the-native-watcher-on-windows-3-and-6-november-2026). Not verified: a message queued while the Codex app is closed ([issue #25](https://github.com/niklasrichardson/session-queue/issues/25)); a long run; macOS and Linux at run time.

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
- They cover the hook input: `cq watch` and `cq.exe` watchers at any path and in quotes, with redirections such as `2>&1`, are exempt; a watcher with other work after it (`&&`, `||`, `|`, `;`, `&`, `$(`, `<(`, `>(`, a backtick, a line break) counts; `grep "cq watch"`, `echo cq watch`, other `cq` commands, and tasks with no command (a subagent described "review cq watch races", a Monitor; changed on 6 November 2026: a Monitor no longer counts) count as background work; a finished task; the field absent.
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

Empty scheduled-run model-turn/usage cost is unverified: the only scheduled exchange had a pending task, and official scheduled-task guidance did not settle empty-check accounting. No controlled empty-run usage measurement was made. Use a longer interval than one minute for unattended operation until usage is checked. Unix runtime and long-running reliability also remain unverified. See [Waking Codex](agents/CODEX.md#waking-codex) for the route comparison.
