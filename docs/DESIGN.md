# Design decisions

Implementation choices that the specification left open, and what `cq` does. The full requirements and architecture are in [`SPEC.md`](SPEC.md). Back to the [README](../README.md).

## CLI and exit codes

- Exit codes are as in the table in [Commands](COMMANDS.md#exit-codes). No general error uses exit 1, because a session treats `claim` exit 1 as "already handled". Clap's own usage errors (normally exit 2) are mapped to 64, because 2 means a rename failed.
- `cq init` creates the layout. Every other command refuses to run (exit 70) until it has. Per-name directories are created on demand.
- `CQ_ROOT` must be absolute. A relative path is refused, so that sessions in different worktrees cannot end up with different queues.
- `send` reads stdin only when it is not a terminal and `--input FILE` is not given. A leading UTF-8 BOM in the input is removed. Unknown input fields are refused (exit 64).
- `done` reads input only with `--reply` or `--reject`. With neither, it never reads stdin, so an open stdin pipe (as in Claude Code's Bash tool) cannot block it. It does check stdin without blocking, and refuses (exit 64, nothing moved) when input is waiting, so a reply given without `--reply` is never lost silently (seen live on 24 Oct 2026). Input is waiting when stdin is a pipe with bytes in it, or a non-empty file; Git Bash passes a small heredoc as a pipe and a large one as a temporary file. A console, `NUL`, a closed pipe, or an open pipe that stays empty for 100 ms counts as no input. On macOS and Linux, a non-empty file counts, and a pipe or socket counts when bytes arrive within 100 ms (Node.js, which runs Claude Code, uses socket pairs for child-process pipes). `--input` without one of the flags is refused (exit 64).
- `--reply` needs a non-empty `"reply"`; `--reject` needs a non-empty `"reason"` and allows neither `"reply"` nor `--status`. `--status` needs `--reply`. `--reply` and `--reject` cannot be combined.
- `--reply` on a `result` or `info` is refused (exit 5) before the move, so the message stays claimed. If the claimed file cannot be read, its type is unknown: `done` moves it and sends nothing.
- `cq watch` has two hidden flags for tests: `--rescan-seconds` (default 60) and `--no-fs-events`.

## Messages

- Hop limit: a message that would have 5 hops is refused, so the highest stored value is 4. With both `--parent` and `--reply-to`, hops is 1 + the higher of the two.
- Replies sent by `cq done`, and DLQ notices sent by `cq claim`, take their hop count from the message they answer, without a lookup.
- The pair, recipient and sender limits count files by name (the time stamp and sender in the ID), across the inbox, `.processing/`, `done/`, `rejected/` and `dlq/` of the recipient (pair, recipient) or of every recipient (sender). The recipient is the directory name. Messages pruned after 14 days are long outside the 1-hour window.
- The checks run in this order: arguments, body size, `PAUSE`, hops, pair, recipient, sender. The first one that fails is reported.
- The recipient limit stops many senders flooding one session; the sender limit stops one session fanning out to many. Both were added on 24 Oct 2026.
- `PAUSE` refuses sends as well as silencing watchers, so a storm stops at the source. A `done --reply`, or a `--reject` of a task, under `PAUSE` still moves the message and exits 4 (reply not sent). A `--reject` of an info or result exits 0 and warns on stderr that the sender was not told. A `claim` that dead-letters a message still exits 3; its stderr line says the sender could not be told.
- The 20 KB limit is 20480 bytes of UTF-8 in `body` (and in a `done` reply or reason). The subject has no limit in the file; the event line shows at most 80 characters.
- Validation goes beyond the schema: `id` must match the file name, `to` must match the inbox, `created` must be RFC 3339 ending in `Z`, `reply_to` and `parent` must be IDs, a `result` needs a status, a `task` or `info` must have status null, and a file starting with a BOM is invalid.
- Schema: `reply_to`, `parent`, `status`, `from_session_id` and `from_host_session_id` are optional (null when absent). `cq` always writes every field.
- A DLQ notice (a `failed` result, subject `Not processed: <id>`) goes to the sender named in `from`, or, when the JSON is unreadable, to the sender named in the file's ID. It is sent for any message type, following the brief; the hop count and rate limit stop loops.
- Reply subjects: `Re: <subject>`, and `Rejected: <subject>` for a rejection. The body of a task's `rejected` result is the reason.
- Rejecting an info or result sends the sender an `info` notice ([issue #20](https://github.com/niklasrichardson/session-queue/issues/20)): `reply_to` is the rejected ID, and the body names the ID, the subject and the reason. It is an `info`, not a result, because only tasks get results. The reject is the main action, so a notice that a limit blocks only gives a stderr warning. Rejecting a `rejected` result or a notice sends nothing, so two sessions cannot reject back and forth; the hop limit is a second stop.
- `cq requeue` moves a message only from the session's own `rejected/` folder to its inbox, by rename, so the ID and contents stay the same. It sets the file's modified time to now, so the watcher announces it again and a coordinator's `CQ-WAKE` counts from the requeue. It holds the recipient's claim lock (`heartbeat/<name>.claim.lock`) until the reason file is removed, so a claim cannot start before then.
- `done` checks its input first, and for `--reply` the message type, and moves nothing if either is wrong. It then moves the file and sends the reply. If the reply cannot be sent, it exits 4 with the message already in `done/` or `rejected/`.
- Reason files are `<id>.reason.json` (`id`, `inbox`, `reason`, `time`), next to the message in `dlq/` or `rejected/`.
- Moves never replace an existing file. If a send's ID is already taken, `cq` picks a new random suffix (up to 5 tries).

## Watcher

- Any file-system event in the inbox triggers a full inbox scan. The scan decides what is new, so events never need parsing. If `notify` cannot start or stops, the watcher rescans every 5 seconds and says so on stderr.
- A message is announced once per watcher run. A restarted watcher announces every unclaimed message again, once; `cq claim` exit 1 makes repeats harmless. The watcher knows a message by its ID and modified time, so a message that `cq requeue` puts back (with a new modified time) is announced again, even when the claim, reject and requeue all happen between two scans.
- Only files whose name is a valid ID are announced or listed. A file with any other name cannot be claimed.
- The event time is when the watcher saw the message, not the `created` time.
- Unreadable `from` or `type` fields show as `?` in event and list lines. Such a message still gets announced, so that `cq claim` can send it to the DLQ.
- While `PAUSE` exists, the watcher announces nothing and remembers nothing. When `PAUSE` goes, it scans at once.
- The loop ticks every 250 ms (clock, `PAUSE`). Heartbeat at start and every 30 seconds; the file holds the time. Prune at start and every hour, by file modified time.
- `claim` sets the file's modified time to the claim time, so `list --stale-minutes` measures from the claim, and the prune measures from the claim too (a move keeps the modified time).
- Lock: `File::try_lock` on `heartbeat/<name>.lock`. The OS releases it however the process exits.
- `--takeover` reads the pid file, checks that the process is a `cq` executable (same file name as the running binary), terminates it, and waits up to 5 seconds for the lock. On macOS and Linux it sends SIGTERM, then SIGKILL if the process is still running after 5 seconds. It refuses to stop any other program.
- No parent check (removed 24 Oct 2026). Under the Monitor, the parent of `cq.exe` is a forked Git Bash (the Monitor runs `bash -c "... eval 'cq watch ...' < /dev/null && pwd -P ..."`), whose lifetime Cygwin controls, not the Monitor. The check was the only path that could end a watcher with exit 0 and no output while its Monitor still ran, which was seen 4 times. `--max-seconds 1790`, the single-instance lock and `--takeover` bound an orphan instead: it lives at most 30 minutes, and `--takeover` replaces it at once.
- T14 follows from this: after only the parent is killed, the orphaned watcher keeps running and a plain second `cq watch` exits 5; `--takeover` stops the orphan, and exactly one watcher runs. A separate test runs the watcher through three nested `bash -c ... eval` forms and checks that each runs to `--max-seconds`.
- The watcher exits 0 at `--max-seconds` or when stdout can no longer be written, and prints one line to stderr saying why (`cq watch: exiting: --max-seconds 1790 reached`). Every error exit prints `cq: <reason>`. It removes the pid file if the file still holds its own ID.
- The watcher exits 7 with `CQ-SLEEP` while its name is asleep (see [Sleep](#sleep)). Exit 7 is not an error, so it prints no `cq:` line.

## Inbox mode

`cq watch --to-inbox` posts the watcher's lines into the session's own Claude Code inbox instead of stdout. The commands are in [Commands](COMMANDS.md#watch-into-the-inbox) and the session steps in [Session protocol](PROTOCOL.md#inbox-mode).

- **Decision (28 Oct 2026).** Inbox mode is the default way for Claude Code sessions to receive queue events. Monitor mode is the fallback, for a session where `--to-inbox` exits 64 (no inbox). Inbox mode was added as experimental in [#24](https://github.com/niklasrichardson/session-queue/pull/24). On 28 Oct 2026 the owner moved all live sessions to it, to test it in real use. A planned 24-hour soak test was stopped after about 1 hour, so there are no long-run results yet.

- **Why.** Under the Monitor, the watcher must be re-armed at least every 30 minutes, because the Monitor expires. Each re-arm is a model turn that reads the whole context again. An idle session pays for 2 re-arms an hour for nothing. A watcher that posts into the inbox can run for hours, and the session pays only for turns that have queue events.
- **The documented path.** Claude Code gives each session an inbox and exports its address and a token to the session's Bash commands and hooks ([The session's inbox socket](https://code.claude.com/docs/en/cross-session-messaging.md#the-sessions-inbox-socket)). It delivers a message from the session's own child process, and a message to an idle session starts a new turn. On Windows the token in the auth line is the only proof that the poster is a child of the session, so `cq` always sends it. The owner tested this live on 28 Oct 2026: a background child posted after 180 seconds and woke an idle session.
- **Batching.** Claude Code refuses rapid bursts to one inbox and drops identical repeats, and each message to an idle session costs a turn. So `cq` holds lines for 2 seconds and keeps posts 5 seconds apart. The burst limit is not documented, so 5 seconds is a guess. A post holds at most 50 lines.
- **One last message.** A watcher that stops posts why, with any held lines, in one message, so the session knows to re-arm (or, when asleep, not to). One message costs one turn, not two. Exit 7 and `CQ-SLEEP` keep their meaning.
- **Exit 8.** A post that still fails after 4 tries over about 7 seconds means the inbox is gone, which means the session has ended. The watcher stops, so it never runs on without a reader. It is a new code because sessions act on exit codes, and none of the others fits.
- **No default `--max-seconds`.** The time limit only exists because the Monitor expires.
- **Signals.** The watcher catches SIGTERM, so on macOS and Linux a watcher replaced by `--takeover` posts a stopped message. If it is still posting 5 seconds later, `--takeover` sends SIGKILL and the message can be lost. On Windows, `--takeover` uses `TerminateProcess`, which cannot be caught, so the old watcher posts nothing.
- **Transport.** A named pipe opened with `std::fs::OpenOptions` on Windows, and `std::os::unix::net::UnixStream` elsewhere, in `src/platform.rs`. No new dependency. The tests' fake inbox needs the `Win32_System_IO` feature of windows-sys, as a dev-dependency only.
- **Unknowns.** Whether the desktop app stops the processes of an idle session, and how long the inbox stays reachable. How Claude Code's Bash tool reports the watcher's exit next to the stopped message. A watcher whose session ended without stopping it finds out only at its next post; until then it holds the name and writes its heartbeat. Long runs (many hours) have not been tested.

### Known risks

- **A watcher that dies silently.** An inbox watcher posts a stopped message when its name is put to sleep, when it reaches `--max-seconds`, on an error, or (on macOS and Linux) on a signal. A watcher that is killed without warning posts nothing: for example, if the desktop app stops the session's processes. The session is not told, so it does not restart the watcher, and messages wait in its inbox. `cq send` does not warn the sender.
- **Monitor mode heals itself.** The Monitor expires at least every 30 minutes, and the session re-arms it. So a dead Monitor-mode watcher is replaced within 30 minutes. Inbox mode has no such timer.
- **How to spot it.** The watcher's heartbeat stops. [cq-top](CQ-TOP.md) shows it as `STALE` after 90 seconds. After a `/clear`, `cq session-start` also tells the session that its inbox watcher has stopped. To restart it, run `/cq-start` in that session.

## Codex wake

`cq watch --to-codex THREAD` wakes an existing Codex Desktop chat with `codex queue`. The commands are in [Commands](COMMANDS.md#wake-a-codex-chat) and the setup in [Codex setup](CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex).

- **Why.** The fallback, a same-chat automation that runs every 10 minutes, costs a model turn per run even when the inbox is empty (about 1% of the 5-hour limit per run, [issue #12](https://github.com/niklasrichardson/session-queue/issues/12)). `codex queue` starts a turn only when there is work. Other routes were rejected in issue #12: the Desktop app's app-server is private, `codex exec resume` on a live chat can split its history, and hooks cannot start a turn.
- **A sink, like the inbox.** It reuses the watcher and its `Sink` trait, so the lock, heartbeat, `PAUSE`, sleep and `--takeover` behave as in every mode. It uses the inbox values for batching (2-second window, 5-second gap) and retries (4 tries, 1, 2 and 4 seconds apart). One Codex turn handles every `UNCLAIMED` message, so one call per batch is enough.
- **Fixed prompt.** Codex reads the queued message as a user instruction. Queue text is written by other sessions, so none of it goes into the message: not the subject, body, sender or ID. The chat reads its messages with `cq claim`, under its `AGENTS.md` rules.
- **No shell.** `cq` starts the Codex executable directly, with each argument separate, so no character in the thread name or prompt is interpreted.
- **Exit 9.** A call fails on a non-zero exit, on no `Queued message` line, or after 60 seconds. After 4 failed tries the watcher stops with a new code, because nothing wakes the chat any more and none of the other codes fits.
- **Its own member key.** The watcher usually runs in another process tree than the Codex chat, often started by a Claude Code session. Recording it under that session's key would replace the session's own name, so it uses `CQ_SESSION_ID` or `codex-NAME`.
- **Known risks.** `codex queue` is experimental. It confirms that the message is stored, not that a turn started. A message queued while the app is closed is untested ([issue #25](https://github.com/niklasrichardson/session-queue/issues/25)). The chat is not told when its watcher stops; cq-top shows the heartbeat.

## Sleep

Stage 1 of [issue #7](https://github.com/niklasrichardson/session-queue/issues/7): manual sleep and wake for one session. The protocol is in [Session protocol](PROTOCOL.md#sleep-and-wake).

- **Why a flag file.** `sleep/<name>` works like `PAUSE`, but for one name. A file needs no running process, survives restarts, can be checked by any `cq` command and by `cq-top`, and can be removed by hand. The watcher checks it on every 250 ms tick, so a running watcher stops within a second. A message to the session would not work: a stopped watcher cannot read it.
- **Its own exit code.** Exit 7 tells the session "do not re-arm". Exit 0 means "re-arm", and exit 5 means "retry with `--takeover`".
- **At start, no lock.** A watcher that finds its name asleep at start exits before it takes the lock or writes a heartbeat, pid file or members record. So it cannot replace a running watcher's record.
- **`CQ-SLEEP` under `PAUSE`.** The watcher prints `CQ-SLEEP` even while `PAUSE` exists. Otherwise a paused, sleeping watcher would run to `--max-seconds`, exit 0 and be re-armed for nothing.
- **Who may sleep whom.** Sleeping another session stops its work, so only a coordinator listed in `coordinators` may do it. Waking costs only tokens, so any name may wake any name (owner decision, 28 Oct 2026).
- **Waking comes from outside `cq`.** A sleeping session runs no `cq` process. The sender wakes it through Claude Code (the `/cq-start` skill), not through the queue. `cq send` still delivers, so nothing is lost, and warns, so the sender knows to wake the receiver.
- **`CQ-WAKE` is a backup.** A coordinator's watcher reports a message only after it has waited 2 minutes (`WAKE_AFTER`), so the sender has time to wake the receiver first. The age is the file's modified time, as for the prune. The coordinator scans every second, not on file events, and never claims or moves a message. It reports each message once per run, so a restarted coordinator reports old ones again, once. It is silent under `PAUSE`.
- **When sleep pays off.** Waking a session writes its whole context to the prompt cache again, which costs about 2 times the context in input tokens. Staying awake costs about 0.4 times the context per hour in re-arms and cache reads. So sleep pays off only for an idle period of about 5 hours or more, unless the context is small. These figures are for Monitor mode. In inbox mode, an idle session takes no re-arm turns, so staying awake costs less, and sleep pays off only after a longer idle period.
- **Not in stage 1:** automatic sleep after an idle time. `cq dispatch` (Codex) does not check the flag.

## Rejoin in the Claude Code CLI

[Issue #5](https://github.com/niklasrichardson/session-queue/issues/5). The protocol is in [Session protocol](PROTOCOL.md#rejoin-after-clear).

- **Why a process match.** The CLI sets no host session ID, and its `CLAUDE_CODE_SESSION_ID` changes on `/clear`. The `claude` process stays, and the SessionStart hook and the watcher both run under it. So a CLI record holds that process, and `cq session-start` uses it to find the record after a key miss.
- **Why the start time.** A process ID can be reused after its process exits. The record holds the start time too, and both must match. While walking up the parents, a parent that started after its child also stops the walk: its ID was reused after the real parent exited.
- **Why only the CLI key.** In the desktop app, the host ID already outlives `/clear`. A desktop or `CQ_SESSION_ID` record holds no process, and a host or `CQ_SESSION_ID` key never falls back to one. So the desktop app and Codex behave as before.
- **Why only `/clear`.** A compaction keeps the session ID. `/resume` can open an unrelated conversation in the same process, and a fresh `claude` process cannot match. Moving a record only for `"source": "clear"`, and only when the hook's `session_id` is the session's key, keeps a name from going to the wrong conversation.
- **Why exactly one record.** One process normally holds one session. If two records name the same process, `cq` cannot tell which one this session owned, so it moves neither. It never matches on the working folder: several sessions can share one folder.
- **Limits.** `claude --resume` starts a new process, so it needs a fixed `CQ_SESSION_ID` or `/cq-start`. The match looks for a process, `argv[0]` or executable named `claude` (`claude.exe` on Windows), as the native installer starts it. A CLI that runs as `node` may not match, and the session then gets no reminder, as before. On Unix systems other than macOS and Linux, no process is recorded.

## Dependencies

- clap, serde, serde_json, jiff (time zones; the bundled database on Windows), notify, windows-sys (Windows), libc (macOS and Linux). Tests: jsonschema (without default features, to avoid a network client build) and tempfile.

## Codex dispatcher

- `dispatch` is a serial polling worker, not a Monitor replacement. One-second inbox scans find startup and later messages; it shares the watcher lock. A separate Codex process handles each task, with no prior conversation history.
- An explicit sender allowlist and a trusted local scope file are required. Sender names remain a soft filter. Queue bodies cannot select executables, flags or the scope file. Codex reads the target repository's AGENTS.md.
- The dispatcher invokes Codex directly, without a shell. Prompt data goes through a file on stdin. Structured final output carries status and reply. Event logs and stderr are files, so a full output pipe cannot deadlock a worker.
- Defaults are read-only sandboxing, no interactive approvals, ephemeral runs and invocation-local disabling of hooks and user configuration. A worker can explicitly select workspace-write. Existing saved authentication is reused.
- `dispatch/NAME/ID` retains prompts, output and completion records. The intended completion is written before `done`; incomplete records block restart. Records are not pruned automatically.
- Failed agent exits and invalid or oversized answers yield bounded failed results. Timeouts and interrupted active tasks stop the agent process tree, leave the task claimed and stop the dispatcher. Windows uses a non-inheritable kill-on-close Job Object, assigned before the suspended agent resumes. Forced dispatcher death closes the job and stops the tree. Unix SIGHUP/SIGINT/SIGTERM stop the agent process group; SIGKILL on Unix can leave a child running. An operator must review before recovery. Tasks are never automatically replayed.
- Completion exit 4 stops with the task already moved. No reply retry occurs automatically, because a crash near completion can make delivery uncertain. Recovery must inspect the saved completion and sender queue. Exactly-once side effects are not promised.
- The session membership key is the first non-empty CQ_SESSION_ID, CLAUDE_CODE_HOST_SESSION_ID or CLAUDE_CODE_SESSION_ID, with the existing sanitisation and 64-character cap. Codex variables are not read implicitly. The dispatcher does not register its parent chat, and clears inherited session IDs for each fresh child.

- The dispatcher reserves each job directory before claiming; a collision refuses the job without claiming the inbox message. It writes its PID while holding the watcher lock, removes its own PID on normal exit, and shares a single 30-second heartbeat schedule across idle scans, agent execution and PAUSE waits.
- When PAUSE appears during a task, the dispatcher retains its answer and waits before calling done. Interruption leaves the task claimed. PAUSE can still race with completion after the final check; a resulting exit 4 remains an operator recovery case.

- Claims take a separate OS lock at heartbeat/NAME.claim.lock for the move and validation transaction. Windows can otherwise report two successful renames when both callers opened the same source before it moved. The second claimant checks the inbox after the first transaction and exits 1. The lock file is retained; deleting it while held could let a new file bypass the lock.
