# cq commands

Every command, its exit codes and the files it uses. Back to the [README](../README.md).

## Commands

Every command has `--help`. Message text never goes on the command line: `send` reads JSON from stdin (use a quoted heredoc, `<<'EOF'`) or from `--input FILE`. `done` reads JSON the same way, but only with `--reply` or `--reject`; without them it refuses input on stdin (exit 64, nothing moved).

| Command | What it does |
| --- | --- |
| `cq init` | Create the layout under `CQ_ROOT`. |
| `cq send --from A --to B --type task\|result\|info [--reply-to ID] [--parent ID] [--status ok\|failed\|rejected] [--input FILE]` | Read `{"subject": "...", "body": "..."}`, write the message, print its ID. |
| `cq watch --as B [--takeover] [--max-seconds 1790] [--to-inbox \| --to-codex THREAD [--codex-bin PATH] [--codex-prompt TEXT]]` | Print one `CQ-NEW` line per new message. One watcher per name. While `B` is asleep, print `CQ-SLEEP` and exit 7. For a coordinator, also print `CQ-WAKE` lines (see [Sleep and wake](#sleep-and-wake)) and `CQ-OVERDUE` lines (see [Overdue tasks](#overdue-tasks)). `--to-inbox` posts the lines into the Claude Code session's own inbox instead, the default for Claude Code (see [Watch into the inbox](#watch-into-the-inbox)). `--to-codex` runs `codex queue` to wake a Codex chat instead, the default for Codex (see [Wake a Codex chat](#wake-a-codex-chat)). |
| `cq dispatch --as B --codex FILE --workspace DIR --instructions FILE --allow-from NAME [--allow-from NAME ...] [--sandbox read-only\|workspace-write] [--timeout-seconds 600] [--once]` | Run one fresh Codex process per task, serially. See [Codex setup](CODEX.md). |
| `cq claim --as B ID` | Move the message to `.processing/` and print its JSON. |
| `cq done --as B ID [--reply [--status ok\|failed] \| --reject] [--input FILE]` | Finish a claimed message. With no flag, sends no reply, and refuses (exit 64, nothing moved) if input is waiting on stdin. `--reply` reads `{"reply": "..."}` (tasks only); `--reject` reads `{"reason": "..."}` and tells the sender (see [Rejections](#rejections)). |
| `cq list --as B [--stale-minutes 60]` | List unclaimed, stale claimed and dead-lettered messages, oldest first. No bodies. Starts with an `ASLEEP by=... reason="..."` line while `B` is asleep. |
| `cq list --as B --rejected` | List the messages `B` rejected and the messages from `B` that others rejected, oldest first, with the reasons. No bodies. Cannot be combined with `--stale-minutes` or `--overdue`. See [Rejections](#rejections). |
| `cq requeue --as B ID` | Move a message from `rejected/B/` back to `inbox/B/`, with its original ID. See [Rejections](#rejections). |
| `cq list --overdue [--unclaimed-minutes 30] [--claimed-minutes 120]` | List the overdue tasks in every inbox, oldest first, then a `TOTAL` line. No bodies. Read only, and works under `PAUSE`. See [Overdue tasks](#overdue-tasks). |
| `cq leave --as B` | Forget which session owns `B` (remove its `members/` record). Exit 0 when there is none. |
| `cq members` | Print one `<session key> -> <name>` line per record, with ` (asleep)` after a sleeping name. A sleeping name with no record prints as `- -> <name> (asleep)`. |
| `cq sleep --as A B [--reason TEXT]` | Put `B` to sleep: write `sleep/B`. `A` may be `B` itself or a coordinator. |
| `cq wake --as A B` | Wake `B`: remove `sleep/B`. Any name may wake any name. Exit 0 when `B` was not asleep. |
| `cq session-start [--debug]` | For the Claude Code SessionStart hook. Prints the rejoin reminder when this session owns a name, a "you are asleep" note when that name is asleep, else nothing. The text ends with a note that a subagent ignores it, because subagents share the host session ID. Always exits 0. |
| `cq idle-alert [--every 1h] [--debug]` | For the Claude Code Notification hook (matcher `idle_prompt`). When this session owns a name listed in `CQ_ROOT/idle-alerts`, send its lead one fixed `info` message, at most once per `--every`. Prints nothing and exits 0. A bad `--every` value is a configuration error and exits 64. See [Idle alerts](#idle-alerts). |

Example:

```bash
cq send --from lead --to worker-a --type task <<'EOF'
{"subject": "Run the tests", "body": "Run `cargo test` in D:/Code/x and report failures."}
EOF
```

Reply to a task, or reject a message:

```bash
cq done --as worker-a 20261024T000615Z-lead-ab12 --reply <<'EOF'
{"reply": "cargo test passed."}
EOF

cq done --as worker-a 20261024T000615Z-lead-ab12 --reject <<'EOF'
{"reason": "Deploys are out of scope for this repo."}
EOF
```

Event line:

```text
CQ-NEW 2026-10-24T00:06:16Z (00:06 GMT) to=worker-a from=lead type=task id=20261024T000615Z-lead-ab12 subject="Run the tests"
```

The time is when the watcher saw the message: UTC first, then local time with its label, computed by `cq` (see `CQ_TZ` and `CQ_TZ_LABEL` in [Setup](SETUP.md#configuration)).

## Rejections

[Issue #20](https://github.com/niklasrichardson/session-queue/issues/20). `cq done --reject` moves the message to `rejected/B/`, writes its reason file, and tells the sender.

- **Task.** The sender gets a `rejected` result: subject `Rejected: <subject>`, body the reason, `reply_to` the task's ID.
- **Info or result.** The sender gets an `info` notice: subject `Rejected: <subject>`, `reply_to` the rejected message's ID, and this body (the subject cut to 80 characters, the body cut to 20 KB):

  ```text
  worker-a rejected your info 20261024T000615Z-lead-ab12 (subject "Status").
  Reason: Sender not in scope.
  ```

- **No loops.** Rejecting a rejection sends nothing: not for a `rejected` result, and not for a notice (an `info` with a `Rejected: ` subject whose `reply_to` is a message from its recipient).
- **Send limits.** The notice goes through the same limits as `cq send`. If a limit or `PAUSE` blocks it, the reject still succeeds (exit 0, `reply=none`) and stderr says `cq: message <id> moved to rejected/B/, but <sender> was not told: <why>`. A task's `rejected` result is a reply, so a blocked one still exits 4.
- `done` prints the notice's ID as `reply=<id>`.

`cq list --as B --rejected` prints one line per rejected message, oldest first (by the time in the ID), then a total. `REJECTED-IN` is a message that `B` rejected (in `rejected/B/`). `REJECTED-OUT` is a message from `B` (by the sender in the file name) that another session rejected. `rejected=` is the time in the reason file, or `?` without one. It never prints bodies. It does not take `--stale-minutes` (exit 64).

```text
REJECTED-IN 2026-10-26T09:00:00Z (09:00 GMT) rejected=2026-10-26T09:01:12Z by=worker-a from=lead type=info id=20261026T090000Z-lead-ab12 subject="hello" reason="Sender not in scope."
REJECTED-OUT 2026-10-26T10:00:00Z (10:00 GMT) rejected=2026-10-26T10:00:40Z by=worker-b from=worker-a type=task id=20261026T100000Z-worker-a-cd34 subject="Deploy" reason="Deploys are out of scope."
TOTAL rejected_in=1 rejected_out=1
```

`cq requeue --as B ID` moves `rejected/B/ID.json` back to `inbox/B/`, for example after `B`'s allowed senders are fixed. The message keeps its ID and contents, so the watcher announces it again and `B` claims it as usual. Its modified time is set to now before the move, so overdue scans and `CQ-WAKE` count the wait from the requeue; if that fails, it exits 70 and moves nothing. The reason file is removed. It moves only from `B`'s own `rejected/` folder: for an ID anywhere else (another session's `rejected/`, the inbox, `.processing/`, `done/`), it exits 6 and moves nothing. It sends nothing to the sender, and `PAUSE` does not stop it. It prints `requeued id=<id> to=inbox/B/`.

## Sleep and wake

Stage 1 of [issue #7](https://github.com/niklasrichardson/session-queue/issues/7): manual commands only. Nothing puts a session to sleep by itself. The protocol is in [Session protocol](PROTOCOL.md#sleep-and-wake).

```bash
cq sleep --as worker-a worker-a --reason "idle overnight"
cq wake --as lead worker-a
```

- `cq sleep` writes `sleep/<name>`: a small JSON file with `by`, `time` (UTC) and `reason` (null without `--reason`). It replaces an earlier one.
- A name may sleep itself. Only a coordinator may sleep another name; anyone else gets exit 5. Coordinators are listed one name per line in `CQ_ROOT/coordinators`, which the owner edits by hand. Blank lines and lines starting with `#` are skipped. Without the file, there are no coordinators.
- Any name may wake any name. Waking only costs tokens.
- While `sleep/<name>` exists, `cq watch --as <name>` prints one line and exits 7, at start or within 250 ms of the file appearing. It prints the line even under `PAUSE`, so the session does not re-arm. It releases its lock and removes its pid file as on a normal exit:

  ```text
  CQ-SLEEP by=lead reason="idle overnight"
  ```

  stderr: `cq watch: exiting: asleep`.
- `cq send` to a sleeping name still delivers the message (exit 0) and prints `cq: <name> is asleep; wake it with /cq-start <name>` on stderr.
- A coordinator's watcher prints one `CQ-WAKE` line for each message that has been in a sleeping name's inbox for at least 2 minutes (`WAKE_AFTER` in `src/watch.rs`, by the file's modified time). It is a backup for when the sender did not wake the name. It prints each message at most once per watcher run, reports messages that have already waited 2 minutes when it starts, and checks every second. It never claims or moves the message. Under `PAUSE` it prints nothing, like `CQ-NEW`.

  ```text
  CQ-WAKE 2026-10-28T09:12:03Z (09:12 GMT) to=worker-a from=lead type=task id=20261028T091000Z-lead-ab12 subject="Run the tests"
  ```

## Idle alerts

[Issue #50](https://github.com/niklasrichardson/session-queue/issues/50). The lead hears when a chosen worker session waits for input, without a timed check. Claude Code only: the hook setup is in [Claude Code setup](CLAUDE-CODE.md#tell-a-lead-when-a-worker-is-idle).

`CQ_ROOT/idle-alerts` lists the workers, one `worker lead` pair per line. The owner edits it by hand:

```text
# worker lead
worker-a lead
```

- Blank lines, lines starting with `#`, and lines that are not exactly two valid names are skipped. When a worker is listed more than once, the first valid line wins.
- `cq idle-alert` finds this session's name from its `members/` record, as `cq session-start` does. With no record, no listed name, or a sleeping name, it does nothing.
- Otherwise it sends the lead the message that `cq send --from worker-a --to lead --type info` would send, with the subject `worker-a is idle` and the body `Idle since 14:05 UK.` (the local time, as in `CQ-NEW` lines). The text is fixed: nothing from the session's work or the hook's input goes into it.
- After a send it writes the UTC time to `idle/worker-a`. It skips the alert while that time is less than `--every` ago (default `1h`; for example `--every 30m`). A time in the future does not count.
- Claude Code runs matching hooks in parallel. The check, the send and the record run under an exclusive lock on `idle/worker-a.lock`. A second `cq idle-alert` that finds the lock taken sends nothing and exits 0.
- Every `cq send` limit applies, and the alert counts towards them. When the send is refused (a limit, `PAUSE`) or fails, nothing is recorded, so the next idle tries again.
- It writes nothing to stderr without `--debug`. With `--debug` it prints the session key, the name and the result on one stderr line.

## Overdue tasks

A task is overdue when it has been unclaimed for 30 minutes, or claimed for 120 minutes without `cq done`. Only `task` messages can be overdue; `result` and `info` never are. The time is taken from the message file's modified time: when it arrived in the inbox, or when `cq claim` moved it to `.processing/`.

- **Limits.** Set them with `CQ_OVERDUE_UNCLAIMED_MINUTES` and `CQ_OVERDUE_CLAIMED_MINUTES` (whole minutes, 1 or more). Unset or empty means the default. Any other value is a usage error (exit 64). For `cq list --overdue`, the flags `--unclaimed-minutes N` and `--claimed-minutes M` override the variables. A task is overdue from the minute it reaches the limit: at 30 minutes, not after it.
- **Claim time.** `cq claim` moves the file into `.processing/` and then sets its modified time, both under the claim lock (`heartbeat/<name>.claim.lock`). Before it reports a claimed task, an overdue check waits for that lock and reads the time again, so a task is never reported in the moment between the two steps. If the lock file exists but cannot be opened or locked, the check skips claimed tasks for that name until a later check succeeds. The check never creates the lock file. If `cq claim` cannot set the time, it moves the message back to the inbox and exits 70.
- **`cq list --overdue`.** Prints one line per overdue task in every inbox, oldest first, then one `TOTAL` line. It shows the ID, sender, recipient, subject, state and age, never the body. It needs no `--as`, and it cannot be used with `--as` or `--stale-minutes` (exit 64):

  ```text
  OVERDUE 2026-10-29T09:00:00Z (09:00 GMT) state=unclaimed age=45m from=lead to=worker-a id=20261029T090000Z-lead-ab12 subject="Run the tests"
  TOTAL overdue=1 unclaimed_after=30m claimed_after=120m
  ```

  The time after `OVERDUE` is when the message was created.
- **`CQ-OVERDUE` lines.** A coordinator's watcher (a name in `CQ_ROOT/coordinators`) prints one line per task when it becomes overdue. It checks at start and then every 30 seconds. It reports each task once: an ID is written to `heartbeat/<name>.overdue` after its line has been delivered (written to stdout, or posted with `--to-inbox`, including in the stopped message), so a restarted watcher does not repeat it. A line that was never delivered, for example because the post failed (exit 8) or the watcher stopped first, is reported again by the next watcher. An ID is removed from that file when its task is finished, rejected or dead-lettered. Other watchers print no `CQ-OVERDUE` lines and do not read the limits. A coordinator's watcher with a bad limit stops at start with exit 64.

  ```text
  CQ-OVERDUE 2026-10-29T09:45:02Z (09:45 GMT) state=unclaimed age=45m from=lead to=worker-a id=20261029T090000Z-lead-ab12 subject="Run the tests"
  ```

  The time after `CQ-OVERDUE` is when the watcher saw that the task was overdue.
- **Nothing moves.** `cq` never resends, reclaims or moves an overdue task. The coordinator decides what to do (see [Protocol](PROTOCOL.md#overdue-tasks)).
- **`PAUSE`.** `cq list --overdue` and `cq-top` still report overdue tasks under `PAUSE`, because they only read. A coordinator's watcher prints no `CQ-OVERDUE` lines under `PAUSE`, like `CQ-NEW`; it reports the tasks once `PAUSE` is removed.

## Watch into the inbox

`cq watch --as B --to-inbox` posts its lines into the Claude Code session that started it, instead of printing them. A message to an idle session starts a new turn, so the session needs no Monitor and no re-arm every 30 minutes. It is the default way for Claude Code sessions to receive queue events. How a session runs it is in [Session protocol](PROTOCOL.md#inbox-mode). Claude Code documents the inbox in [The session's inbox socket](https://code.claude.com/docs/en/cross-session-messaging.md#the-sessions-inbox-socket).

- **Inbox address.** Claude Code exports `CLAUDE_CODE_MESSAGING_SOCKET` (a Unix socket, or a named pipe such as `\\.\pipe\LOCAL\cc-msg-<hex>` on Windows) and `CLAUDE_CODE_MESSAGING_TOKEN` to Bash commands and hooks. If either is missing or empty, `cq watch` exits 64. It never prints the token or the address.
- **One post.** Connect, write `{"type":"auth","token":"..."}`, then `{"type":"user","from":"cq","message":{"role":"user","content":"..."}}`, each followed by a newline, then close. `cq` connects only when the message is ready, because Claude Code closes a connection that sends no complete line within 30 seconds.
- **Content.** The header line `cq watch (B): queue events`, then the event lines unchanged (`CQ-NEW`, `CQ-WAKE`, `CQ-OVERDUE`, `CQ-SLEEP`), one per line.
- **Batching.** Lines that arrive within 2 seconds of the first held line go in one message. Posts are at least 5 seconds apart. One message holds at most 50 event lines; the rest follow in the next post. Messages already waiting at start go in the first post. The values are `BATCH_WINDOW`, `MIN_GAP` and `MAX_LINES` in `src/inbox.rs`.
- **No time limit.** With `--to-inbox`, `--max-seconds` has no default. The flag still works when given.
- **Stopped message.** When the watcher stops, it posts one last message. Lines still held go in the same message, before the stopped line:

  ```text
  cq watch (B) stopped: <reason>. Run /cq-start to re-arm.
  cq watch (B) stopped: B is asleep. Do not re-arm; run /cq-start only when asked to wake.
  ```

  The reason is `--max-seconds N reached`, `stopped by a signal`, or `error: <the cq: message>` (for example exit 5, another watcher is running). The lock, pid file, heartbeat, `PAUSE` and `--takeover` work as without `--to-inbox`.
- **Signals.** In this mode the watcher catches SIGINT, SIGTERM and SIGHUP (macOS and Linux) and Ctrl+C (Windows console), and posts the stopped message. So on macOS and Linux, a watcher replaced by `--takeover` (SIGTERM) says so. On Windows, `--takeover` ends the old watcher at once, and it posts nothing.
- **Unreachable inbox.** Each post is tried 4 times, 1, 2 and 4 seconds apart. If every try fails, the session has probably ended: the watcher exits 8 with a `cq:` line on stderr, and posts nothing more.

## Wake a Codex chat

`cq watch --as B --to-codex THREAD` wakes an idle Codex chat when `B` has new messages. It runs `codex queue --thread THREAD --message PROMPT`. Codex stores the message, and the Codex Desktop app starts a turn in that thread when the thread is idle. A message queued during a turn runs after that turn. `THREAD` is the thread ID or its exact name. How to set it up is in [Codex setup](CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex).

- **Fixed prompt.** The message is always the same text, never anything from a queue message. By default it is `Queue check as 'B'. Follow the 'B' section of AGENTS.md. If there are no UNCLAIMED messages, end with no other work and no update.` `--codex-prompt TEXT` replaces it. Do not put text from a queue message in it: subjects, bodies and sender names are written by other sessions.
- **The executable.** `--codex-bin PATH` names the Codex executable. The default is `codex` on `PATH` (on Windows, `codex.exe`). `cq` runs it directly, without a shell, with each argument passed separately. If it cannot be found at start, the watcher exits 64.
- **One call per batch.** `CQ-NEW` lines are batched as in [Watch into the inbox](#watch-into-the-inbox): lines within 2 seconds of the first go in one batch, and calls are at least 5 seconds apart. Each batch runs `codex queue` once, however many messages it holds, because one Codex turn handles every `UNCLAIMED` message. Messages already waiting at start give one call.
- **Other lines.** `CQ-WAKE` and `CQ-OVERDUE` lines are dropped. When `B` is asleep, the watcher stops with exit 7 and queues nothing; messages wait until `B` wakes.
- **Output.** After each call, one line on stdout: `cq watch (B): <UTC time> queued a check for N new message(s): <the line codex printed>`.
- **Success.** A call succeeds when `codex queue` exits 0 and prints a line with `Queued message`. A call that takes over 60 seconds is stopped and counts as failed.
- **Failure.** A failed call is tried 4 times, 1, 2 and 4 seconds apart, with a `cq watch:` line on stderr for each retry. If every try fails, the watcher exits 9 with a `cq:` line on stderr. A retry after a non-zero exit can queue a second check, if Codex stored the first one; the second turn then finds nothing to do.
- **No time limit.** As with `--to-inbox`, `--max-seconds` has no default. When the watcher stops for another reason (`--max-seconds`, a signal), it first queues any batch it holds. It tells the Codex chat nothing about stopping: watch its heartbeat in [cq-top](CQ-TOP.md).
- **Session record.** The watcher records `codex` as the mode in `members/<key>`. The key is `CQ_SESSION_ID` if set, else `codex-B`. It never uses the Claude Code session IDs, because the watcher usually runs outside the Codex chat, and the session that started it must keep its own record.
- `--to-codex` and `--to-inbox` cannot be used together (exit 64). `--codex-bin` and `--codex-prompt` need `--to-codex`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | OK |
| 1 | `claim`: the message is no longer in the inbox (another claim won, or a duplicate event). Ignore it. |
| 2 | A rename still failed after 3 retries over 1 second. |
| 3 | `claim`: the message is invalid and went to `dlq/`. The sender was told, when known. |
| 4 | `done` or `dispatch` completion: the message is finished, but the reply was not sent. |
| 5 | Refused: bad name, body over 20 KB, hop limit, a pair, recipient or sender limit, `PAUSE`, missing parent, a watcher already running, `done --reply` on a `result` or `info`, or `sleep` of another name by a name that is not a coordinator. |
| 6 | `done`: the message is not claimed by this session. `requeue`: the message is not in this session's `rejected/` folder. |
| 7 | `watch`: the name is asleep (`sleep/<name>` exists). Do not re-arm. |
| 8 | `watch --to-inbox`: the session's inbox could not be reached after 4 tries. The session has probably ended. |
| 9 | `watch --to-codex`: `codex queue` failed 4 times in a row. Nothing wakes the Codex chat until the watcher is started again. |
| 64 | Bad arguments or bad JSON input, `done` input without `--reply` or `--reject`, `watch --to-inbox` without `CLAUDE_CODE_MESSAGING_SOCKET` or `CLAUDE_CODE_MESSAGING_TOKEN`, or `watch --to-codex` with an empty thread, an empty prompt or a Codex executable that cannot be found. A bad `CQ_OVERDUE_UNCLAIMED_MINUTES` or `CQ_OVERDUE_CLAIMED_MINUTES` value for `list --overdue`, a coordinator's `watch` or `cq-top`. |
| 70 | Dispatch timeout/interruption (task stays claimed), or any other error (I/O, `CQ_ROOT` missing or not initialised). |


## Layout

```text
CQ_ROOT/
  tmp/                       temp files, renamed into inboxes
  inbox/<name>/              new messages
  inbox/<name>/.processing/  claimed messages
  done/<name>/               finished messages (pruned after 14 days)
  rejected/<name>/           declined messages and reason files (pruned after 14 days)
  dlq/<name>/                invalid messages and reason files (never pruned)
  heartbeat/<name>           time of the watcher/dispatcher heartbeat (every 30 s)
  heartbeat/<name>.lock      locked by the running watcher or dispatcher
  heartbeat/<name>.claim.lock serialises claims for this recipient
  heartbeat/<name>.pid       the running watcher/dispatcher process ID
  heartbeat/<name>.overdue   overdue task IDs a coordinator's watcher has reported, one per line
  dispatch/<name>/<id>/       Codex logs and completion state (not auto-pruned)
  members/<key>              the name owned by a session (a Claude Code session, or `codex-NAME` for a `--to-codex` watcher), the UTC time it was recorded, the watcher's working folder, and its mode (monitor, inbox or codex)
  sleep/<name>               while it exists, the name is asleep (by, time, reason)
  coordinators               names that may sleep other names, one per line (edited by the owner)
  idle-alerts                `worker lead` pairs for cq idle-alert, one per line (edited by the owner)
  idle/<name>                the UTC time of the last idle alert for the name
  idle/<name>.lock           locked by cq idle-alert while it checks, sends and records
  PAUSE                      while it exists, watchers print nothing and cq send refuses
```

Message format: [`schema/message.v1.json`](../schema/message.v1.json). One file per message, `<id>.json`, UTF-8 without BOM. IDs are `<UTC yyyymmddThhmmssZ>-<from>-<4 hex>`. Readers ignore unknown fields.

To stop all queue traffic at once, create a file named `PAUSE` in the queue root (for example `touch "$CQ_ROOT/PAUSE"`). While it exists, watchers announce nothing and every `cq send` is refused (exit 5). Remove the file to resume; waiting messages are then announced. `cq list`, `cq list --overdue` and `cq-top` still work under `PAUSE`, because they only read.

## Codex dispatch

`dispatch` requires an explicit executable, workspace, trusted scope file and sender allowlist. Paths must be absolute. It holds the same lock as `watch`; it has no takeover option. It scans every second, observes PAUSE, and handles the current inbox once with --once. Each task starts `codex exec` with stdin data, structured output, no interactive approvals and an explicit read-only (default) or workspace-write sandbox. Hooks and user configuration are disabled for that invocation. Info/result messages finish without starting Codex. Disallowed senders are rejected.

Agent failures become failed results; a successfully delivered failed result leaves dispatch running (exit 0 with --once). Timeout/interruption leaves work claimed and exits 70. Completion failures preserve their existing cq exit code. Claimed tasks and unresolved dispatch records refuse restart with exit 5. See [recovery](CODEX.md#failure-handling-and-recovery) before restarting. No automatic retry occurs.

A dispatcher waits for PAUSE to clear before completion if it appeared during a run. Its PID file allows an explicitly requested watch --takeover to stop it. Heartbeats occur at startup and every 30 seconds. A pre-existing job directory refuses the message before claim.
