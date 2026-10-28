# cq commands

Every command, its exit codes and the files it uses. Back to the [README](../README.md).

## Commands

Every command has `--help`. Message text never goes on the command line: `send` reads JSON from stdin (use a quoted heredoc, `<<'EOF'`) or from `--input FILE`. `done` reads JSON the same way, but only with `--reply` or `--reject`; without them it refuses input on stdin (exit 64, nothing moved).

| Command | What it does |
| --- | --- |
| `cq init` | Create the layout under `CQ_ROOT`. |
| `cq send --from A --to B --type task\|result\|info [--reply-to ID] [--parent ID] [--status ok\|failed\|rejected] [--input FILE]` | Read `{"subject": "...", "body": "..."}`, write the message, print its ID. |
| `cq watch --as B [--takeover] [--max-seconds 1790]` | Print one `CQ-NEW` line per new message. One watcher per name. While `B` is asleep, print `CQ-SLEEP` and exit 7. For a coordinator, also print `CQ-WAKE` lines (see [Sleep and wake](#sleep-and-wake)). |
| `cq dispatch --as B --codex FILE --workspace DIR --instructions FILE --allow-from NAME [--allow-from NAME ...] [--sandbox read-only\|workspace-write] [--timeout-seconds 600] [--once]` | Run one fresh Codex process per task, serially. See [Codex setup](CODEX.md). |
| `cq claim --as B ID` | Move the message to `.processing/` and print its JSON. |
| `cq done --as B ID [--reply [--status ok\|failed] \| --reject] [--input FILE]` | Finish a claimed message. With no flag, sends no reply, and refuses (exit 64, nothing moved) if input is waiting on stdin. `--reply` reads `{"reply": "..."}` (tasks only); `--reject` reads `{"reason": "..."}`. |
| `cq list --as B [--stale-minutes 60]` | List unclaimed, stale claimed and dead-lettered messages, oldest first. No bodies. Starts with an `ASLEEP by=... reason="..."` line while `B` is asleep. |
| `cq leave --as B` | Forget which session owns `B` (remove its `members/` record). Exit 0 when there is none. |
| `cq members` | Print one `<session key> -> <name>` line per record, with ` (asleep)` after a sleeping name. A sleeping name with no record prints as `- -> <name> (asleep)`. |
| `cq sleep --as A B [--reason TEXT]` | Put `B` to sleep: write `sleep/B`. `A` may be `B` itself or a coordinator. |
| `cq wake --as A B` | Wake `B`: remove `sleep/B`. Any name may wake any name. Exit 0 when `B` was not asleep. |
| `cq session-start [--debug]` | For the SessionStart hook. Prints the rejoin reminder when this session owns a name, a "you are asleep" note when that name is asleep, else nothing. Always exits 0. |

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

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | OK |
| 1 | `claim`: the message is no longer in the inbox (another claim won, or a duplicate event). Ignore it. |
| 2 | A rename still failed after 3 retries over 1 second. |
| 3 | `claim`: the message is invalid and went to `dlq/`. The sender was told, when known. |
| 4 | `done` or `dispatch` completion: the message is finished, but the reply was not sent. |
| 5 | Refused: bad name, body over 20 KB, hop limit, a pair, recipient or sender limit, `PAUSE`, missing parent, a watcher already running, `done --reply` on a `result` or `info`, or `sleep` of another name by a name that is not a coordinator. |
| 6 | `done`: the message is not claimed by this session. |
| 7 | `watch`: the name is asleep (`sleep/<name>` exists). Do not re-arm. |
| 64 | Bad arguments or bad JSON input, or `done` input without `--reply` or `--reject`. |
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
  dispatch/<name>/<id>/       Codex logs and completion state (not auto-pruned)
  members/<key>              the name owned by a Claude Code session, the UTC time it was recorded, and the watcher's working folder
  sleep/<name>               while it exists, the name is asleep (by, time, reason)
  coordinators               names that may sleep other names, one per line (edited by the owner)
  PAUSE                      while it exists, watchers print nothing and cq send refuses
```

Message format: [`schema/message.v1.json`](../schema/message.v1.json). One file per message, `<id>.json`, UTF-8 without BOM. IDs are `<UTC yyyymmddThhmmssZ>-<from>-<4 hex>`. Readers ignore unknown fields.

To stop all queue traffic at once, create a file named `PAUSE` in the queue root (for example `touch "$CQ_ROOT/PAUSE"`). While it exists, watchers announce nothing and every `cq send` is refused (exit 5). Remove the file to resume; waiting messages are then announced.

## Codex dispatch

`dispatch` requires an explicit executable, workspace, trusted scope file and sender allowlist. Paths must be absolute. It holds the same lock as `watch`; it has no takeover option. It scans every second, observes PAUSE, and handles the current inbox once with --once. Each task starts `codex exec` with stdin data, structured output, no interactive approvals and an explicit read-only (default) or workspace-write sandbox. Hooks and user configuration are disabled for that invocation. Info/result messages finish without starting Codex. Disallowed senders are rejected.

Agent failures become failed results; a successfully delivered failed result leaves dispatch running (exit 0 with --once). Timeout/interruption leaves work claimed and exits 70. Completion failures preserve their existing cq exit code. Claimed tasks and unresolved dispatch records refuse restart with exit 5. See [recovery](CODEX.md#failure-handling-and-recovery) before restarting. No automatic retry occurs.

A dispatcher waits for PAUSE to clear before completion if it appeared during a run. Its PID file allows an explicitly requested watch --takeover to stop it. Heartbeats occur at startup and every 30 seconds. A pre-existing job directory refuses the message before claim.
