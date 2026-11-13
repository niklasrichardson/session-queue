# cq commands

Every command, its exit codes and the files it uses. Back to the [README](../README.md).

## Commands

Every command has `--help`. Message text never goes on the command line: `send` reads JSON from stdin (use a quoted heredoc, `<<'EOF'`) or from `--input FILE`. `done` reads JSON the same way, but only with `--reply` or `--reject`; without them it refuses input on stdin (exit 64, nothing moved).

| Command | What it does |
| --- | --- |
| `cq init` | Create the layout under `CQ_ROOT`. |
| `cq send --from A --to B --type task\|result\|info [--reply-to ID] [--parent ID] [--status ok\|failed\|rejected] [--input FILE]` | Read `{"subject": "...", "body": "..."}`, write the message, print its ID. |
| `cq watch --as B [--takeover] [--max-seconds 1790] [--idle-after 5m] [--idle-every 1h] [--to-inbox \| --to-codex THREAD [--codex-bin PATH] [--codex-prompt TEXT] [--codex-check-seconds 60] [--codex-queue-db PATH] [--debug] \| --to-agy CONVERSATION_ID [--agy-bin PATH] [--agy-prompt TEXT] [--agy-broker --agy-allow-from NAME... [--agy-exchange DIR] [--agy-sandbox-sid SID]]]` | Print one `CQ-NEW` line per new message. One watcher per name. While `B` is asleep, print `CQ-SLEEP` and exit 7. For a coordinator, also print `CQ-WAKE` lines (see [Sleep and wake](#sleep-and-wake)), `CQ-OVERDUE` lines (see [Overdue tasks](#overdue-tasks)), and `CQ-LOCK-ORPHAN` and `CQ-LOCK-STALE` lines (see [Locks](#locks)). `--to-inbox` posts the lines into the Claude Code session's own inbox instead, the default for Claude Code (see [Watch into the inbox](#watch-into-the-inbox)). `--to-codex` runs `codex queue` to wake a Codex chat instead, the default for Codex (see [Wake a Codex chat](#wake-a-codex-chat)). `--to-agy` runs an `agy` turn to wake an Antigravity CLI conversation instead (see [Wake an Antigravity CLI conversation](#wake-an-antigravity-cli-conversation)). When `B` is listed in `CQ_ROOT/idle-alerts`, also send the idle alert after a turn end recorded by `cq turn-end`, once no activity is observed for `--idle-after` (see [Idle alerts](#idle-alerts)). |
| `cq dispatch --as B --codex FILE --workspace DIR --instructions FILE --allow-from NAME [--allow-from NAME ...] [--sandbox read-only\|workspace-write] [--timeout-seconds 600] [--once]` | Run one fresh Codex process per task, serially. See [Codex setup](agents/CODEX.md). |
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
| `cq status --as B` | Print one line saying whether `B`'s watcher runs: `running` (exit 0), `stopped` with the reason (exit 10), or `asleep` (exit 7). Reads only, and needs no session key: any session or the user may check any name. See [Watcher status and exit log](#watcher-status-and-exit-log). |
| `cq lock take LOCK --as A --for TEXT [--owner]` | Take the named lock `LOCK`. Exit 0 when taken or already `A`'s, 1 when another name holds it (the holder is printed). Refused (exit 5) under `PAUSE`. `--owner` takes the user's hold; coordinators only. See [Locks](#locks). |
| `cq lock release LOCK --as A [--owner]` | Release `LOCK`. Exit 0 when released, 1 when it is not held or not `A`'s. |
| `cq lock status LOCK` | Print whether `LOCK` is free (exit 0) or held (exit 1), with the holder, time taken, `--for` text and age, and `orphaned` or `stale`. Reads only. |
| `cq lock wait LOCK [--max MINUTES]` | Wait until `LOCK` is free (exit 0), or still held at `--max` (exit 2). Reads every 15 seconds; sends nothing, takes nothing. |
| `cq lock list` | Print one line per held lock, with its holder. Reads only. |
| `cq lock break LOCK --as A --reason TEXT` | Break `LOCK`, whoever holds it, and log why. Coordinators only (exit 5). Exit 1 when it is not held. |
| `cq session-start [--debug]` | For the Claude Code SessionStart hook. Prints the rejoin reminder when this session owns a name, a "you are asleep" note when that name is asleep, else nothing. The text ends with a note that a subagent ignores it, because subagents share the host session ID. Always exits 0. |
| `cq turn-end [--debug]` | For the Claude Code Stop hook. When this session owns a name listed in `CQ_ROOT/idle-alerts`, record the turn end in `idle/<name>.turn-end` for the name's watcher. Records nothing when the hook input lists other running background work. A Monitor task does not count. Prints nothing and always exits 0. See [Idle alerts](#idle-alerts). |
| `cq idle-alert [--every 1h] [--debug]` | For the Claude Code Notification hook (matcher `idle_prompt`), or manual use. When this session owns a name listed in `CQ_ROOT/idle-alerts`, send its lead one fixed `info` message at once, at most once per `--every`. The desktop app does not send `idle_prompt`: use `cq turn-end` there. Prints nothing and exits 0. A bad `--every` value is a configuration error and exits 64. See [Idle alerts](#idle-alerts). |

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

The time is when the watcher saw the message: UTC first, then local time with its label, computed by `cq` (see `tz` and `tz_label` in `config.toml`, or `CQ_TZ` and `CQ_TZ_LABEL`, in [Setup](SETUP.md#configuration)).

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

`cq list --as B --rejected` prints one line per rejected message, oldest first (by the time in the ID), then a total. `REJECTED-IN` is a message that `B` rejected (in `rejected/B/`). `REJECTED-OUT` is a message from `B` (by the sender in the file name) that another session rejected. `rejected=` is the time in the reason file, or `?` without one or when it is not a UTC time. It never prints bodies. It does not take `--stale-minutes` (exit 64).

```text
REJECTED-IN 2026-10-26T09:00:00Z (09:00 GMT) rejected=2026-10-26T09:01:12Z by=worker-a from=lead type=info id=20261026T090000Z-lead-ab12 subject="hello" reason="Sender not in scope."
REJECTED-OUT 2026-10-26T10:00:00Z (10:00 GMT) rejected=2026-10-26T10:00:40Z by=worker-b from=worker-a type=task id=20261026T100000Z-worker-a-cd34 subject="Deploy" reason="Deploys are out of scope."
TOTAL rejected_in=1 rejected_out=1
```

`cq requeue --as B ID` moves `rejected/B/ID.json` back to `inbox/B/`, for example after `B`'s allowed senders are fixed. The message keeps its ID and contents, so the watcher announces it again and `B` claims it as usual. Its modified time is set to now before the move, so overdue scans and `CQ-WAKE` count the wait from the requeue; if that fails, it exits 70 and moves nothing. The reason file is removed. It moves only from `B`'s own `rejected/` folder: for an ID anywhere else (another session's `rejected/`, the inbox, `.processing/`, `done/`), it exits 6 and moves nothing. It sends nothing to the sender, and `PAUSE` does not stop it. It prints `requeued id=<id> to=inbox/B/`.

## Sleep and wake

Stage 1 of [issue #7](https://github.com/niklasrichardson/session-queue/issues/7): manual commands only. Nothing puts a session to sleep by itself. The protocol is in [Protocol](PROTOCOL.md#sleep-and-wake).

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

[Issue #50](https://github.com/niklasrichardson/session-queue/issues/50). The lead hears when a chosen worker session has stopped working, without a timed check by the lead. Claude Code only: the hook setup is in [Claude Code setup](agents/CLAUDE-CODE.md#tell-a-lead-when-a-worker-is-idle).

`CQ_ROOT/idle-alerts` lists the workers, one `worker lead` pair per line. The owner edits it by hand:

```text
# worker lead
worker-a lead
```

- Blank lines, lines starting with `#`, and lines that are not exactly two valid names are skipped. When a worker is listed more than once, the first valid line wins.
Two ways send the alert. The Stop hook way works in the desktop app; `idle_prompt` does not fire there.

**Stop hook: `cq turn-end` and the watcher.**

- `cq turn-end` finds this session's name from its `members/` record, as `cq session-start` does. With no record, no listed name, a sleeping name, no `transcript_path` in the hook input, or a transcript it cannot read, it does nothing. It reads the hook input for up to 2 seconds in all, from the first byte to the end; input that is not complete by then counts as none.
- When the hook input has a `background_tasks` list with a task that is not finished, is not a Monitor and is not the `cq watch` watcher, it writes nothing and removes an older turn end: background work may wake the session later. The watcher is a task whose command runs `cq` or `cq.exe` (at any path, quotes allowed) with the first argument `watch`, and runs nothing else. A command with `&&`, `||`, `;`, `|`, a lone `&` (run in the background), `$(`, `<(` or `>(` (process substitution), a backtick or a line break is not the watcher, even when those characters are in quotes. A redirection such as `2>&1`, `> file` or `2>/dev/null` is allowed. A task with no command, such as a subagent, is never the watcher, whatever its description says. So a watcher run through a tool that reports no command blocks the alert. A task of `type` `monitor` never blocks the alert: Claude Code reports no command for it, and it can run for the whole session, either as a Monitor-mode watcher or as a Monitor that Claude Code arms by itself (for example for live updates). The cost: a session that waits on a Monitor is reported idle after `--idle-after`. When the field is absent, the background work is unknown, and it writes the turn end anyway. `session_crons` is not used: a scheduled wake that starts a new turn is detected like any new turn, from a new `user` entry in the transcript or a later transcript write.
- Otherwise it writes `idle/worker-a.turn-end`: a JSON object with a unique `generation`, the UTC `turn_end` time (with fractions of a second), the hook's `transcript_path`, and `transcript_len`, the transcript's length in bytes. Nothing else from the hook input or the transcript goes into it. It writes, or removes an older turn end, only under a lock on `idle/worker-a.turn-end.lock`. When that lock stays taken for 1 second, it changes nothing.
- The watcher for `worker-a` checks the file every 5 seconds, while the name is listed and `PAUSE` does not exist. It sends the alert as `cq idle-alert` would, once `--idle-after` (default `5m`, more than `30s`) has passed since the turn end with no activity observed. Activity is any of these:
  - a complete transcript entry with `type` `user` added after `transcript_len`: a new prompt, a tool result or a queued notification;
  - more than 1 MB added to the transcript;
  - a transcript that shrank, is missing or cannot be read;
  - a transcript modified time later than 30 seconds after the turn end.
- The watcher reads only the added bytes, at most 1 MB, and only each entry's `type`. It never keeps or prints transcript text. These checks use only the file and the transcript, so a restarted watcher decides the same way.
- Each line the watcher delivers (`CQ-NEW`, `CQ-WAKE`, `CQ-OVERDUE`, in any mode) wakes the session. The watcher records when it last delivered a line: written to stdout, or posted by `--to-inbox`, `--to-codex` or `--to-agy`. It cancels a turn end that is earlier than that time. A line that `--to-codex` or `--to-agy` drops by design also counts, which can only stop an alert. While a line waits to be posted, the watcher sends no alert.
- Each check runs under the lock on `idle/worker-a.turn-end.lock`: the watcher reads the turn end, decides, sends and removes it while it holds the lock. When `cq turn-end` holds the lock, the watcher skips that check.
- The watcher removes the turn end after the alert is sent, after `Recent` (an alert within `--idle-every`, default `1h`), when it cancels it, or when the name is no longer listed or is asleep. When `idle/worker-a.lock` is taken or the send fails (a limit, I/O), it tries again after 90 seconds and 3 more minutes, then removes the turn end. When the removal fails, it tries the removal again on each check, and never sends for that turn end.
- The watcher prints nothing for this.
- Limits: a Stop hook does not run when the user interrupts a turn, and an API error runs `StopFailure` instead, so neither records a turn end. A session that waits on work the hook input does not report can be reported idle, at most once per `--idle-every`.

**`idle_prompt`: `cq idle-alert`.**

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

## Locks

[Issue #99](https://github.com/niklasrichardson/session-queue/issues/99). A named lock says which session uses a shared resource, for example the desktop that GUI tests drive. Sessions read the lock instead of messaging its holder. The protocol is in [Protocol](PROTOCOL.md#locks).

```bash
cq lock take desktop --as worker-a --for "GUI test of the settings window"
cq lock status desktop
cq lock wait desktop --max 60
cq lock release desktop --as worker-a
```

- **Names.** A lock name follows the queue-name rule: `a-z`, `0-9` and `-`, starting with a letter or digit, at most 32 characters. A bad name exits 5. `--as` must be a valid queue name too, and not `owner`. When this session's key is recorded in `members/` for another name, the command is refused (exit 5), so a session cannot act under another session's name.
- **One holder.** `take` creates `locks/LOCK.lock` with an exclusive create, so of many takes at once exactly one wins. A take by the current holder exits 0 and changes nothing (the first `--for` text stays). Any other take exits 1 and prints the holder on stdout. `--for` is required, at most 200 characters (longer exits 5).
- **The file.** `locks/LOCK.lock` is JSON: `lock`, `id` (the take, in the message ID format), `holder`, `by` (the name that ran `take`), `taken` (UTC), `taken_local` (for a person reading the file), `for`, and `session_key` when the taker has one. A file that cannot be read as this is held by `?`: it blocks takes, cannot be released, and a coordinator can break it.
- **Release.** Only the holder releases (exit 1 for any other name, or when the lock is not held). Release and break hold `locks/LOCK.guard` while they read and remove the file, so neither removes a lock that was broken and taken again in between.
- **The user's hold.** The holder `owner` is the user's hold, for example while the user uses the desktop. A coordinator (a name in `CQ_ROOT/coordinators`) takes and releases it with `--owner`: `cq lock take desktop --as lead --owner --for "user testing"`. Any coordinator may release it. Nobody else can take the lock while it is held; `--owner` from another name exits 5, and `--as owner` exits 5. The owner hold is never orphaned or stale.
- **Break.** `cq lock break LOCK --as A --reason TEXT` removes the lock whoever holds it. Only a coordinator may break (exit 5); `--reason` must not be empty (exit 64). Exit 1 when the lock is not held.
- **Orphaned.** A held lock is orphaned when its holder's watcher is stopped (as `cq status` sees it) and the last sign of life, the heartbeat or the take itself, is at least 90 seconds old. So a lock taken by a name with no watcher is orphaned 90 seconds after the take. An unreadable lock file is orphaned once its modified time is 90 seconds old.
- **Stale.** A held lock is stale when its holder's watcher runs and it has been held 120 minutes or more. Set `CQ_LOCK_STALE_MINUTES` (whole minutes, 1 or more; unset or empty means the default; any other value exits 64 for every `cq lock` command and a coordinator's watcher).
- **Status and list.** `status` prints `free lock=LOCK` (exit 0) or one `held` line (exit 1). `list` prints one `held` line per lock, by name, and nothing when none is held:

  ```text
  held lock=desktop holder=worker-a since=2026-11-11T10:00:00Z (10:00 UK) age=12m for="GUI test of the settings window"
  held lock=desktop holder=owner since=2026-11-11T10:00:00Z (10:00 UK) age=3m by=lead for="user testing"
  held lock=desktop holder=worker-a since=2026-11-11T08:00:00Z (08:00 UK) age=130m for="GUI test" why="heartbeat 340s old" orphaned
  held lock=desktop holder=worker-b since=2026-11-11T08:00:00Z (08:00 UK) age=130m for="GUI test" stale
  ```

- **Wait.** `wait` checks every 15 seconds whether `locks/LOCK.lock` exists, and exits 0 with `free lock=LOCK` once it does not. With `--max MINUTES` (1 or more) it exits 2 with the `held` line when the lock is still held then. Without `--max` it waits until the lock is free. It sends no message, does not take the lock, and works under `PAUSE`. Run it in the background and take the lock after it returns: another session may take it first.
- **Reports.** A coordinator's watcher checks the locks at start and every 15 seconds, and prints one line per take of a lock that is orphaned or stale. It reports each take once per state: a key is written to `heartbeat/<name>.lock-alerts` after its line is delivered, as for [overdue tasks](#overdue-tasks), so a restarted watcher does not repeat it. A new take of the same lock is reported again. Under `PAUSE` it prints nothing. Other watchers print no lock lines. `--to-codex` and `--to-agy` drop these lines, as they drop `CQ-OVERDUE`.

  ```text
  CQ-LOCK-ORPHAN 2026-11-11T10:05:02Z (10:05 UK) lock=desktop holder=worker-a since=2026-11-11T10:00:00Z (10:00 UK) age=5m for="GUI test" why="no pid file" orphaned
  CQ-LOCK-STALE 2026-11-11T12:00:10Z (12:00 UK) lock=desktop holder=worker-b since=2026-11-11T10:00:00Z (10:00 UK) age=120m for="GUI test" stale
  ```

- **Nothing breaks by itself.** `cq` never breaks or releases a lock. The coordinator decides (see [Protocol](PROTOCOL.md#locks)).
- **`locks.log`.** `CQ_ROOT/locks.log` gets one line per take, release and break, and per orphan or stale report, appended and rotated as `watch-exits.log` is (`locks.log.lock`, `locks.log.old` over 1 MiB). A line that cannot be written goes to stderr; the command's result does not change.

  ```text
  2026-11-11T10:00:00Z event=take lock=desktop holder=worker-a id=20261111T100000Z-worker-a-ab12 for="GUI test"
  2026-11-11T10:05:02Z event=orphan lock=desktop holder=worker-a id=20261111T100000Z-worker-a-ab12 age=5m seen_by=lead
  2026-11-11T10:06:00Z event=break lock=desktop holder=worker-a id=20261111T100000Z-worker-a-ab12 by=lead age=6m reason="session ended"
  2026-11-11T10:07:00Z event=release lock=desktop holder=owner id=20261111T100700Z-lead-cd34 by=lead age=1m
  ```

- **`cq status`.** `cq status --as B` ends with `locks=a,b` when `B` holds locks.

## Watch into the inbox

`cq watch --as B --to-inbox` posts its lines into the Claude Code session that started it, instead of printing them. A message to an idle session starts a new turn, so the session needs no Monitor and no re-arm every 30 minutes. It is the default way for Claude Code sessions to receive queue events. How a session runs it is in [Protocol](PROTOCOL.md#inbox-mode). Claude Code documents the inbox in [The session's inbox socket](https://code.claude.com/docs/en/cross-session-messaging.md#the-sessions-inbox-socket).

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

  The reason is `--max-seconds N reached`, `stopped by a signal (<signal>)` (for example `SIGTERM` or `CTRL_C_EVENT`), or `error: <the cq: message>` (for example exit 5, another watcher is running). The lock, pid file, heartbeat, `PAUSE` and `--takeover` work as without `--to-inbox`.
- **Signals.** In this mode the watcher catches SIGINT, SIGTERM and SIGHUP (macOS and Linux) and Ctrl+C and Ctrl+Break (Windows console), and posts the stopped message. So on macOS and Linux, a watcher replaced by `--takeover` (SIGTERM) says so. On Windows, `--takeover` ends the old watcher at once, and it posts nothing; the new watcher writes its line in the [exit log](#watcher-status-and-exit-log).
- **Unreachable inbox.** Each post is tried 4 times, 1, 2 and 4 seconds apart. If every try fails, the session has probably ended: the watcher exits 8 with a `cq:` line on stderr, and posts nothing more.

## Wake a Codex chat

`cq watch --as B --to-codex THREAD` wakes an idle Codex chat when `B` has new messages. It runs `codex queue --thread THREAD --message PROMPT`. Codex stores the message, and the Codex Desktop app starts a turn in that thread when the thread is idle. A message queued during a turn runs after that turn. `THREAD` is the thread ID or its exact name. How to set it up is in [Codex setup](agents/CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex).

- **Fixed prompt.** The message is always the same text, never anything from a queue message. By default it is `Queue check as 'B'. Follow the 'B' section of AGENTS.md. If there are no UNCLAIMED messages, end with no other work and no update.` `--codex-prompt TEXT` replaces it. Do not put text from a queue message in it: subjects, bodies and sender names are written by other sessions.
- **The executable.** `--codex-bin PATH` names the Codex executable. The default is `codex` on `PATH` (on Windows, `codex.exe`). `cq` runs it directly, without a shell, with each argument passed separately. If it cannot be found at start, the watcher exits 64.
- **One call per batch.** `CQ-NEW` lines are batched as in [Watch into the inbox](#watch-into-the-inbox): lines within 2 seconds of the first go in one batch, and calls are at least 5 seconds apart. Each batch runs `codex queue` once, however many messages it holds, because one Codex turn handles every `UNCLAIMED` message. Messages already waiting at start give one call.
- **Other lines.** `CQ-WAKE` and `CQ-OVERDUE` lines are dropped. When `B` is asleep, the watcher stops with exit 7 and queues nothing; messages wait until `B` wakes.
- **Output.** After each call, one line on stdout: `cq watch (B): <UTC time> queued a check for N new message(s): <the line codex printed>`.
- **Not taken.** Codex Desktop takes a queued check only while the chat is loaded in the app ([issue #25](https://github.com/niklasrichardson/session-queue/issues/25)). `--codex-check-seconds` after each call (default 60; 0 turns it off), the watcher looks up the item ID from the `Queued message ID for thread ...` line in the `queued_items` table of Codex's `queue_1.sqlite`. The file is in Codex's SQLite home: `sqlite_home` in `CODEX_HOME/config.toml`, else `CODEX_SQLITE_HOME`, else `CODEX_HOME` (default `~/.codex`). `--codex-queue-db PATH` names the file instead. It opens the database read-only and never changes its contents; SQLite may create the `-wal` and `-shm` files and takes shared locks. It looks up each item once. A look-up waits at most 1 second for a lock, and can briefly delay a Codex write, which waits for the lock. If the item is still there, it prints one line on stdout, once per item: `cq watch (B): <UTC time> codex thread THREAD has not taken the queued check after SECS s (the chat may be unloaded): open the chat in the Codex app`. It also sends one `info` message, subject `B: Codex chat has not taken a queued check`, from `B` to each coordinator except `B`, through the same path as `cq send` (limits and `PAUSE` apply). The body gives the thread, the wait, how many unclaimed messages wait for `B`, and says to open the chat in the Codex app. A send that fails prints one line on stderr; no coordinators means no message. If every message behind a check is such a warning, a stall of that check sends none, so two Codex coordinators do not warn each other in a loop. If the database, the table or the ID cannot be read, it skips the look-up and prints nothing; `--debug` prints the reason once on stderr. The look-up never stops the watcher. What to do is in [Codex setup](agents/CODEX.md#if-a-wake-does-not-start-a-turn).
- **Success.** A call succeeds when `codex queue` exits 0 and prints a line with `Queued message`. A call that takes over 60 seconds is stopped, with every process it started, and counts as failed.
- **Never waits.** The watcher does not wait for `codex queue`. It starts the call and checks on each tick whether it has ended. While a call runs, the watcher still writes its heartbeat and notices sleep, `PAUSE` and signals within a second. One call runs at a time. Lines that arrive during a call, or while a failed call waits to be tried again, go into the next call.
- **Failure.** A failed call is tried 4 times, 1, 2 and 4 seconds apart, with a `cq watch:` line on stderr for each retry. If every try fails, the watcher exits 9 with a `cq:` line on stderr. A retry after a non-zero exit can queue a second check, if Codex stored the first one; the second turn then finds nothing to do.
- **No time limit.** As with `--to-inbox`, `--max-seconds` has no default. When the watcher stops for another reason (`--max-seconds`, a signal), it first waits for a call that still runs and queues any batch it holds, with the same gap and retries. It waits at most 60 seconds for this, then stops the call. When the name falls asleep, it stops a call that still runs at once; the messages wait until `B` wakes. It tells the Codex chat nothing about stopping: watch its heartbeat in [cq-top](CQ-TOP.md).
- **Session record.** The watcher records `codex` as the mode in `members/<key>`. The key is `CQ_SESSION_ID` if set, else `codex-B`. It never uses the Claude Code session IDs, because the watcher usually runs outside the Codex chat, and the session that started it must keep its own record.
- `--to-codex` and `--to-inbox` cannot be used together (exit 64). `--codex-bin`, `--codex-prompt`, `--codex-check-seconds`, `--codex-queue-db` and `--debug` need `--to-codex`.

## Wake an Antigravity CLI conversation

Experimental. `cq watch --as B --to-agy CONVERSATION_ID` wakes an Antigravity CLI conversation when `B` has new messages. For each batch it runs one print-mode turn: `agy --conversation CONVERSATION_ID -p PROMPT --output-format json`. The turn resumes that conversation, with its context, and `agy` exits when the turn ends. Run the watcher in the background, outside the conversation. How to set it up is in [Antigravity CLI setup](agents/ANTIGRAVITY.md).

- **Fixed prompt.** The prompt is always the same text, never anything from a queue message. By default it is `Queue check as 'B'. Follow the 'B' section of your instruction file (GEMINI.md or AGENTS.md). If there are no UNCLAIMED messages, end with no other work and no update.` `--agy-prompt TEXT` replaces it. Do not put text from a queue message in it: subjects, bodies and sender names are written by other sessions.
- **The executable.** `--agy-bin PATH` names the Antigravity CLI executable. The default is `agy` on `PATH`. `cq` runs it directly, without a shell, with each argument passed separately, and without `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN`. It also removes the session variables `CQ_SESSION_ID`, `CODEX_THREAD_ID`, `CODEX_SESSION_ID`, `CLAUDE_CODE_HOST_SESSION_ID` and `CLAUDE_CODE_SESSION_ID`, as `cq dispatch` does, so the turn never reads as the session that started the watcher. Each turn runs in its own process group (on Windows, a Job Object, with no console window). It never passes `--dangerously-skip-permissions`. If the executable cannot be found at start, the watcher exits 64.
- **One turn per batch.** `CQ-NEW` lines are batched as for `--to-codex`: lines within 2 seconds of the first go in one batch, and a turn starts at least 5 seconds after the last one ended. Each batch runs one turn, however many messages it holds. Messages already waiting at start give one turn.
- **No waiting.** A turn takes seconds to minutes, so the watcher never waits for one. It starts `agy` and checks every 250 ms whether it has exited. One turn runs at a time; lines that arrive during a turn give one more turn after it. `agy` writes to two temporary files (`cq-agy-<pid>-<n>.out` and `.err` in the system temp folder), not to pipes. The watcher reads and deletes them when the turn ends. At start and after each turn, it also deletes `cq-agy-*.out` and `cq-agy-*.err` files there that are over 30 minutes old: the output of turns that still ran when an earlier watcher stopped. It ignores a file it cannot delete; on Windows, that is a file that a turn still writes.
- **Other lines.** `CQ-WAKE` and `CQ-OVERDUE` lines are dropped. When `B` is asleep, the watcher stops with exit 7 and starts no turn; messages wait until `B` wakes.
- **Output.** When a turn starts, one line on stdout: `cq watch (B): <UTC time> started an agy turn for N new message(s)`. When it ends: `cq watch (B): <UTC time> agy turn for N new message(s) ended after S s: status=SUCCESS duration_seconds=X num_turns=N`. `duration_seconds` is the value that `agy` reports. In live use it did not match the turn's wall time (for example, 3600 for a 22-second turn), so `cq` only logs it. The watcher never prints the turn's `response`, or any other text from `agy`'s stdout. A failure quotes only the last line of `agy`'s stderr.
- **Success.** A turn succeeds when `agy` exits 0 and prints a JSON object whose `status` is `SUCCESS`. A turn that takes over 30 minutes is stopped and counts as failed. The stop ends the whole turn: `agy` and every helper or command it started.
- **Failure.** A failed turn is tried again 1, 2 and 4 seconds later, with a `cq watch:` line on stderr for each retry. After 4 failed turns in a row, the watcher exits 11 with a `cq:` line on stderr. A retry can run a second turn if the first one did some work; the second turn then finds nothing to do.
- **Another conversation.** If the JSON names another `conversation_id`, `agy` did not resume the conversation. Whatever the exit code and `status`, the watcher exits 11 at once and does not try again, so that it does not start a new conversation on every wake.
- **Open conversation.** In one live test on 6 Nov 2026, a turn ran while the same conversation was open in an interactive `agy` window. The turn claimed and finished its message, and the window did not show the turn. About 5 minutes later, the interactive session exited; the cause is not known. Until this is understood, do not keep the conversation open while its watcher runs: see [Open conversation](agents/ANTIGRAVITY.md#open-conversation). Two print-mode turns on one conversation at the same time both succeeded.
- **Stopping.** As with `--to-codex`, `--max-seconds` has no default. When the watcher stops for another reason (`--max-seconds`, a signal), it starts a last turn for any batch it holds and does not wait for it. If a turn still runs, it starts no last turn, because only one turn runs at a time: the messages wait in the queue. The running turn runs `cq list` and may take them. A turn that still runs when the watcher stops ends on its own: its process group or Job Object does not end with the watcher. Only the 30-minute limit stops a turn. The watcher tells the conversation nothing about stopping: watch its heartbeat in [cq-top](CQ-TOP.md).
- **Session record.** The watcher records `agy` as the mode in `members/<key>`. The key is `CQ_SESSION_ID` if set, else `agy-B`, for the same reason as with `--to-codex`.
- `--to-agy` cannot be used with `--to-inbox` or `--to-codex` (exit 64). `--agy-bin` and `--agy-prompt` need `--to-agy`.

### Broker mode

Experimental, cq 1.15.0, Windows only: on macOS and Linux `--agy-broker` exits 64, because the exchange folder there needs trust checks that read ACLs (macOS extended ACLs, POSIX ACLs), which cq does not have yet. `--agy-broker` changes `--to-agy`: the watcher claims and finishes each message itself, and the `agy` turn only reads `request.json` and writes `reply.json` in an exchange folder. Without `--agy-broker`, `--to-agy` works as above. Setup, the exchange folder's access and the operator steps are in [Broker mode](agents/ANTIGRAVITY.md#broker-mode).

| Flag | Meaning |
| --- | --- |
| `--agy-broker` | Broker mode, Windows only (exit 64 elsewhere). Needs `--to-agy`. |
| `--agy-allow-from NAME` | Repeat for each sender whose messages get a turn. At least one. A task from another sender is rejected unread; other types are finished unread. |
| `--agy-exchange DIR` | The exchange folder, absolute. Default: `CQ_ROOT/agy-exchange/B`. Not the queue root, not a folder that contains it, and inside it only under `agy-exchange/`. |
| `--agy-sandbox-sid SID` | The SID of `agy`'s sandbox. Required on Windows. It gets Modify access to the exchange folder. |

- **One turn per message**, oldest first: `agy --conversation ID --add-dir JOBDIR -p PROMPT --output-format json`. The prompt holds the name and two paths that `cq` made, never message text. `--agy-prompt` replaces it and must contain `{request}` and `{reply}` (and may contain `{name}`), else exit 64.
- **Job record.** `CQ_ROOT/dispatch/B/ID/state.json`, written durably at each phase: `claimed`, `prepared`, `launching`, `running`, `ended`, `completing`, `completed` or `needs-review`. `completion.json` holds the completion before `cq done` runs. `agy.out` and `agy.err` hold the turn's output. Records are not pruned.
- **Verdict.** A valid reply or reject from a successful turn finishes the task with its text. A successful turn without a valid reply gives a `failed` reply that says the task may have partly run. A failed turn without a reply gives a `failed` reply with `agy`'s status or exit code. Info and results are finished without a reply.
- **Held job.** A timeout, a stop during the turn, another or no `conversation_id`, or a failed turn that wrote a valid reply leaves the message claimed in phase `needs-review`, and the watcher exits 11.
- **Start.** The watcher checks every record before the first job. It refuses (exit 5) a torn or unknown record, a record that needs review, a claimed message without a record, and more than one unfinished record. A record left in `launching` or `running` is held (exit 11). One left in `prepared` is prepared again, `ended` is decided from its stored verdict, and `completing` is finished if its `completion.json` matches.
- **Retries.** A spawn failure is tried again after 1, 2 and 4 s. After 4 failed spawns or failed turns in a row the watcher exits 11. A started turn is never run again.
- **PAUSE.** While `PAUSE` exists, the watcher claims nothing, starts no turn and finishes nothing.
- **Output.** `cq watch (B): <UTC time> started agy job ID`, then `cq watch (B): <UTC time> agy job ID status=ok|failed|rejected|read|unread`.

### Session mode

Experimental, cq 1.17.0, Windows only (exit 64 elsewhere). `cq watch --as B --to-agy-session` is started by the open Antigravity session itself, as a background command, and wakes that session with `agentapi send-message` for each message. It uses the exchange folder and job record of broker mode, with `kind: "session"`. Setup, the checks and the operator steps are in [Session mode](agents/ANTIGRAVITY.md#session-mode).

- **Arguments.** Only `--as B` and `--takeover`. Every other flag is exit 64. All settings come from the `[agy-session.B]` table in `config.toml` in the build's `CQ_DEFAULT_ROOT`; `CQ_ROOT` and other settings from the environment are ignored. The conversation ID comes from `ANTIGRAVITY_CONVERSATION_ID`, with the table's optional `conversation` as fallback and check.
- **Start checks.** Root, `config.toml` and `cq.exe` trust, the launching `agy.exe` or `language_server.exe`, the agentapi binary and the language-server address. Each failure is exit 64, before any queue write.
- **Per message.** Claim, write `request.json`, record the wake, send the NEW-WORK wake. The session creates `started`, then writes `reply.json`. No `started` after `ack_minutes` (default 15): exit 8. No reply after `reply_minutes` (default 60): one RESUME wake; none after `resume_minutes` (default 10) more: needs review, exit 11.
- **Asleep.** The watcher does not exit 7. It keeps running, claims nothing and wakes nothing until `cq wake`.
- **Stop line.** The last stdout line is `cq watch (B) stopped: REASON. WHAT TO DO`. Progress lines go to stderr.
- **Status.** `cq status` shows `mode=agy-session`.

## Watcher status and exit log

`cq status --as B` prints one line from the files of `B`'s watcher or dispatcher. It uses the rule that `cq session-start` uses: the watcher runs when `heartbeat/B.pid` exists, the heartbeat is less than 90 seconds old, and the process in the pid file still runs. A watcher that was killed counts as stopped at once; where the process cannot be checked, it counts as running until its heartbeat is 90 seconds old. It never opens the lock file and writes nothing.

```text
running name=B pid=4242 heartbeat=12s mode=inbox version=1.11.0
running name=B pid=4242 heartbeat=12s mode=inbox version=1.11.0 stale-target
running name=B pid=4242 heartbeat=12s mode=monitor version=unknown outdated
stopped name=B why="no pid file" heartbeat=340s mode=inbox
stopped name=B why="process 4242 is not running" pid=4242 heartbeat=20s mode=inbox version=1.11.0
stopped name=B why="heartbeat 340s old" pid=4242 heartbeat=340s mode=inbox version=1.10.2 outdated
asleep name=B by=lead reason="done for today" watcher=stopped why="no pid file" heartbeat=900s mode=inbox
```

- The first word is the state; the exit code says the same: 0 `running`, 10 `stopped`, 7 `asleep`. A sleeping name is `asleep` even while its watcher still runs (it stops within 250 ms).
- `why` is `no pid file`, `no heartbeat`, `heartbeat Ns old` or `process N is not running`. `pid` is the process ID in the pid file, and `heartbeat` the age of the last heartbeat, when there is one. `mode` is the mode in the `members/` record that names `B` (`monitor`, `inbox`, `codex`, `agy` or `agy-session`), or `unknown` without one.
- With a pid file, the line ends with what the watcher or dispatcher recorded in `heartbeat/B.watcher`. `version` is its `cq` version, or `unknown` for one started by a `cq` older than 1.11.0. `outdated` follows when that is not the version of the `cq` that runs `cq status` (`unknown` counts). `stale-target` follows in `inbox` mode when the caller has `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN` and the watcher's recorded address or token hash is another, or missing: its posts may not reach the caller's session. It is only checked when the caller's session key is recorded in `members/` for B (B's own session, also after `/clear`); any other caller has its own inbox, which never matches. Restart it with `cq watch --as B --to-inbox --takeover`. These words do not change the exit code.
- A bad name exits 5; a root that is not initialised exits 70.

`cq watch` appends one line to `CQ_ROOT/watch-exits.log` when a watcher that held the lock ends. A start refused with exit 5, and a start while the name is asleep, write nothing. The line never holds message text:

```text
2026-11-04T09:15:02Z name=B pid=4242 mode=inbox version=1.11.0 exit=0 reason="--max-seconds 60 reached"
2026-11-04T09:20:11Z name=B pid=4300 mode=inbox version=1.11.0 exit=0 signal=SIGTERM reason="stopped by a signal (SIGTERM)"
2026-11-04T09:25:30Z name=B pid=4350 mode=inbox version=1.11.0 exit=7 reason="asleep"
2026-11-04T09:31:40Z name=B pid=4400 mode=monitor version=1.11.0 signal=CTRL_CLOSE_EVENT reason="console event CTRL_CLOSE_EVENT"
2026-11-04T09:40:00Z name=B pid=4500 mode=inbox version=1.11.0 reason="stopped by --takeover from PID 4600"
2026-11-04T09:52:13Z name=B pid=4600 mode=inbox version=unknown reason="previous watcher ended without logging (last heartbeat 2026-11-04T09:51:50Z); found by PID 4700"
```

- The fields are the UTC time, the name, the process ID, mode and `cq` version of the watcher that ended, the exit code when the watcher knows it, the signal or console event when there was one, and the reason. An error gives `exit=<code> reason="error: <the cq: message>"`.
- **Caught.** Every normal exit (`--max-seconds`, asleep, stdout closed, an error after the lock). With `--to-inbox`, `--to-codex` or `--to-agy`: Ctrl+C and Ctrl+Break on Windows, SIGINT, SIGTERM and SIGHUP on macOS and Linux. On Windows in every mode: a console close, logoff or shutdown, and, under a Monitor, Ctrl+C and Ctrl+Break; the line is written, then the process ends as before.
- **Not caught.** A watcher that is killed outright cannot write its line: `TerminateProcess` on Windows (Task Manager, a stopped Monitor, `--takeover`), SIGKILL, and under a Monitor on macOS and Linux any signal. Its pid file stays; every exit that writes its line removes it. The next watcher for the name finds it and writes the line for it: `stopped by --takeover from PID N` after `--takeover`, else `previous watcher ended without logging (last heartbeat T)`. Holding the lock proves that the old process is gone. The `mode` of such a line is read from the `members/` record before the new watcher writes its own, and its `version` from the old watcher's `heartbeat/<name>.watcher`; `version=unknown` when it wrote none (a `cq` older than 1.11.0).
- **Size.** When the log is over 1 MiB, the next watcher exit renames it to `watch-exits.log.old`, replacing the last one, and starts a new file. Watchers lock `watch-exits.log.lock` around the size check, the rename and the append, so exits at the same time rename once and keep every line. A watcher waits at most 0.5 seconds for the lock. After that it drops its line and writes nothing to either file; its pid file stays, so the next watcher for the name reports the exit as `previous watcher ended without logging`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | OK |
| 1 | `claim`: the message is no longer in the inbox (another claim won, or a duplicate event). Ignore it. `lock take` and `lock status`: another name holds the lock. `lock release`: the lock is not held, or not by this name. `lock break`: the lock is not held. |
| 2 | A rename still failed after 3 retries over 1 second. `lock wait`: the lock is still held at `--max`. |
| 3 | `claim`: the message is invalid and went to `dlq/`. The sender was told, when known. |
| 4 | `done` or `dispatch` completion: the message is finished, but the reply was not sent. This includes `done --reply` or `done --reject` on a claimed file that cannot be read. |
| 5 | Refused: bad name, body over 20 KB, hop limit, a pair, recipient or sender limit, `PAUSE`, missing parent, a watcher already running, `done --reply` or `send --reply-to` on a `result` or `info`, or `sleep` of another name by a name that is not a coordinator. `lock`: a bad lock name, `--as owner`, `--for` over 200 characters, `--owner` or `break` by a name that is not a coordinator, or a session acting as a name other than its recorded one. `watch --agy-broker`: a job record that is torn, unknown or needs review, a claimed message without a record, or more than one unfinished record. `watch --to-agy-session`: also a watcher that already holds the name. |
| 6 | `done`: the message is not claimed by this session. `requeue`: the message is not in this session's `rejected/` folder. |
| 7 | `watch`: the name is asleep (`sleep/<name>` exists). Do not re-arm. `status`: the name is asleep. |
| 8 | `watch --to-inbox`: the session's inbox could not be reached after 4 tries. The session has probably ended. `watch --to-agy-session`: the launching process exited, the address failed 3 connects in a row, agentapi failed 4 times in a row, or no `started` came within `ack_minutes`. Start the watcher again with the same line. |
| 9 | `watch --to-codex`: `codex queue` failed 4 times in a row. Nothing wakes the Codex chat until the watcher is started again. |
| 10 | `status`: no watcher runs for the name, and it is not asleep. |
| 11 | `watch --to-agy`: an `agy` turn failed 4 times in a row, or ran in another conversation. Nothing wakes the conversation until the watcher is started again. With `--agy-broker`: a job needs review, or `agy` could not start 4 times in a row. With `--to-agy-session`: a job needs review. |
| 64 | Bad arguments or bad JSON input, `done` input without `--reply` or `--reject`, `watch --to-inbox` without `CLAUDE_CODE_MESSAGING_SOCKET` or `CLAUDE_CODE_MESSAGING_TOKEN`, or `watch --to-codex` with an empty thread, an empty prompt or a Codex executable that cannot be found, or `watch --to-agy` with an empty conversation ID, an empty prompt or an `agy` executable that cannot be found, or `watch --agy-broker` without `--agy-allow-from`, with a bad exchange folder or path, a prompt without `{request}` and `{reply}`, or, on Windows, without a valid `--agy-sandbox-sid`. `watch --to-agy-session` with any other flag, off Windows, in a build without `CQ_DEFAULT_ROOT`, with a missing or bad `[agy-session.NAME]` table, with no conversation ID (neither `ANTIGRAVITY_CONVERSATION_ID` nor `conversation` in the table) or two that differ, or when a start check fails. A bad `CQ_OVERDUE_UNCLAIMED_MINUTES` or `CQ_OVERDUE_CLAIMED_MINUTES` value for `list --overdue`, a coordinator's `watch` or `cq-top`. A bad `CQ_LOCK_STALE_MINUTES` value for `lock` or a coordinator's `watch`. A problem in `config.toml` in the queue root is not an error: `cq` warns on stderr and ignores the bad part (see [Setup](SETUP.md#configtoml)). |
| 70 | Dispatch timeout/interruption (task stays claimed), or any other error (I/O, `CQ_ROOT` missing or not initialised). |


## Layout

```text
CQ_ROOT/
  config.toml                optional settings: rules, tz, tz_label (edited by the owner; see Setup)
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
  heartbeat/<name>.watcher   its record: pid, cq version and, for --to-inbox, the inbox address and a 16-digit SHA-256 prefix of the token (never the token)
  heartbeat/<name>.overdue   overdue task IDs a coordinator's watcher has reported, one per line
  heartbeat/<name>.lock-alerts orphaned and stale lock takes a coordinator's watcher has reported, one per line
  dispatch/<name>/<id>/       Codex logs and completion state, or agy broker job records (not auto-pruned)
  agy-exchange/<name>/        default agy broker exchange folder (Low label on Windows)
  members/<key>              the name owned by a session (a Claude Code session, `codex-NAME` for a `--to-codex` watcher, or `agy-NAME` for a `--to-agy` or `--to-agy-session` watcher), the UTC time it was recorded, the watcher's working folder, and its mode (monitor, inbox, codex, agy or agy-session)
  sleep/<name>               while it exists, the name is asleep (by, time, reason)
  coordinators               names that may sleep other names, one per line (edited by the owner)
  idle-alerts                `worker lead` pairs for cq idle-alert, one per line (edited by the owner)
  idle/<name>                the UTC time of the last idle alert for the name
  idle/<name>.lock           locked by cq idle-alert while it checks, sends and records
  idle/<name>.turn-end       the last turn end, written by cq turn-end and removed by the name's watcher
  idle/<name>.turn-end.lock  locked while the turn end is written, checked or removed
  PAUSE                      while it exists, watchers print nothing and cq send refuses
  watch-exits.log            one line per watcher exit (see Watcher status and exit log)
  watch-exits.log.lock       locked while a watcher appends its line or renames the log
  watch-exits.log.old        the previous log, once it passed 1 MiB
  locks/<lock>.lock          while it exists, the lock is held (JSON: holder, time, --for text)
  locks/<lock>.guard         locked while a lock is released or broken (never removed)
  locks.log                  one line per lock take, release, break and report (see Locks)
  locks.log.lock             locked while a line is appended or the log renamed
  locks.log.old              the previous log, once it passed 1 MiB
```

`cq` and `cq-top` read only the files and folders listed above. They ignore any other file or folder in the root, so your own files, such as `SESSION-RULES.md` and `watchers.json`, can live there.

Message format: [`schema/message.v1.json`](../schema/message.v1.json). One file per message, `<id>.json`, UTF-8 without BOM. IDs are `<UTC yyyymmddThhmmssZ>-<from>-<4 hex>`. Readers ignore unknown fields.

To stop all queue traffic at once, create a file named `PAUSE` in the queue root (for example `touch "$CQ_ROOT/PAUSE"`). While it exists, watchers announce nothing, and every `cq send` and `cq lock take` is refused (exit 5). Release, break, status, list and wait still work. Remove the file to resume; waiting messages are then announced. `cq list`, `cq list --overdue` and `cq-top` still work under `PAUSE`, because they only read.

## Codex dispatch

`dispatch` requires an explicit executable, workspace, trusted scope file and sender allowlist. Paths must be absolute. It holds the same lock as `watch`; it has no takeover option. It scans every second, observes PAUSE, and handles the current inbox once with --once. Each task starts `codex exec` with stdin data, structured output, no interactive approvals and an explicit read-only (default) or workspace-write sandbox. Hooks and user configuration are disabled for that invocation. Info/result messages finish without starting Codex. Disallowed senders are rejected. If the hop or rate limit refuses that rejection reply, the job is still recorded `completed` and the dispatcher continues.

Agent failures become failed results; a successfully delivered failed result leaves dispatch running (exit 0 with --once). Timeout/interruption leaves work claimed and exits 70. Completion failures preserve their existing cq exit code. Claimed tasks and unresolved dispatch records refuse restart with exit 5. See [recovery](agents/CODEX-DISPATCH.md#failure-handling-and-recovery) before restarting. No automatic retry occurs.

A dispatcher waits for PAUSE to clear before completion if it appeared during a run. Its PID file allows an explicitly requested watch --takeover to stop it. Heartbeats occur at startup and every 30 seconds. A pre-existing job directory refuses the message before claim.
