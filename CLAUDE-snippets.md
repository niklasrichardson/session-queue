# Claude Code blocks and session prompts for `cq`

The Claude Code part of the queue rules. Copy these blocks yourself. Install `cq` first (see [Setup](docs/SETUP.md)). The Claude Code setup steps are in [Claude Code setup](docs/CLAUDE-CODE.md).

The rules for handling a message are the same for every agent. They are in [Queue rules for any agent](docs/QUEUE-RULES.md). This page adds what is specific to Claude Code: the watcher, the Monitor fallback, the skills and the permission rule. Codex uses [Codex setup](docs/CODEX.md) instead.

## 1. User-level block

Put the [queue rules block](docs/QUEUE-RULES.md#1-queue-rules-block) and this block in `~/.claude/CLAUDE.md`. That file is not synced across machines, so repeat it on each one.

````markdown
## Session queue (Claude Code)

- Run `cq` with the Bash tool, by its bare name. On Windows that is Git Bash; never run it with PowerShell.
- Keep the watcher running (inbox mode): start `cq watch --as <your name> --to-inbox 2>&1` once, as a background Bash command, not with the Monitor tool. Queue events arrive as a message from `cq` that starts with `cq watch (<your name>): queue events`. Do not re-arm on a timer, and never run a watcher Monitor as well. If it exits 5 ("already running"), start it again with `--takeover`. If a `cq` message ends with `stopped: ... Run /cq-start to re-arm`, run `/cq-start`. If a `cq` message says your name is asleep, or `cq watch` prints `CQ-SLEEP` and exits 7, do not re-arm: run `/cq-stop` (see [Sleep and wake](https://github.com/niklasrichardson/session-queue/blob/main/docs/PROTOCOL.md#sleep-and-wake)).
- Fallback (Monitor mode), only when `--to-inbox` exits 64 because the session has no inbox: Monitor tool, `timeout_ms` 1800000, command `cq watch --as <your name> 2>&1`. When the Monitor expires or `cq watch` exits, start it again the same way. If it exits 5 and none of your own watcher Monitors runs, start it with `--takeover`. If the re-arm was the only work in the turn, end the turn with the single word `Watching`.
- Git Bash on Windows reports the time zone wrongly, so always take a message's time from its `CQ-NEW` line.
- If you start a worker session for queue work, start it detached, not as a linked side session. If another session started you linked, ask the user to detach you.
- Use `cq send` for hand-offs between sessions. Do not use SendMessage for routine traffic.
````

## 2. Per-repo block

Put the [per-repo scope block](docs/QUEUE-RULES.md#2-per-repo-scope-block) in the project `CLAUDE.md`, and edit the lists for each repo. Do not put the session name in `CLAUDE.md`: give each session its name in its start prompt.

## 3. Session start prompt

Paste into each session once. Change the name.

```text
Your queue name is worker-a.
1. Start this once as a background Bash command, not with the Monitor tool: cq watch --as worker-a --to-inbox 2>&1
   If it exits 5 ("already running"), use: cq watch --as worker-a --to-inbox --takeover 2>&1
   If it exits 64 (no inbox), use Monitor mode as the "Session queue" rules in CLAUDE.md say.
2. Check that `cq list --as worker-a` runs without an error, then wait.
Queue events arrive as a message from cq. Do not re-arm on a timer.
If a cq message says the watcher stopped, run /cq-start. If it says worker-a is asleep, or cq watch prints CQ-SLEEP and exits 7, do not re-arm: run /cq-stop.
When a CQ-NEW line arrives, follow the "Session queue" rules in CLAUDE.md.
Do not message me unless a task needs a decision that only I can make.
```

In the Claude Code CLI, start the session with its own `CQ_SESSION_ID`, so it keeps its queue name after `/clear`. Pass the prompt above as the first argument. In Git Bash:

```bash
CQ_SESSION_ID=worker-a claude "Your queue name is worker-a. ..."
```

In PowerShell, in a new window for each worker:

```powershell
$env:CQ_SESSION_ID = 'worker-a'; claude "Your queue name is worker-a. ..."
```

The desktop app does not need this. See [Claude Code CLI: start with a stable key](docs/CLAUDE-CODE.md#claude-code-cli-start-with-a-stable-key).

## 4. Hand-off prompt

Use the [hand-off prompt](docs/QUEUE-RULES.md#3-hand-off-prompt) in the session that sends work. In Claude Code, the result arrives as a `CQ-NEW` line.

## 5. Recovery prompt

Use after a session restarts or its watcher was down. Routine re-arming is already in the start prompt.

```text
Run cq list --as worker-a.
Claim and finish each UNCLAIMED message, oldest first, following the "Session queue" rules in CLAUDE.md. Treat claim exit 1 as already handled.
List any STALE and DLQ lines for me. Do not process them.
Then stop your old watcher command with TaskStop if it is still listed, and start this as a background Bash command: cq watch --as worker-a --to-inbox --takeover 2>&1
(Monitor mode only: start the Monitor tool with timeout_ms 1800000 on: cq watch --as worker-a --takeover 2>&1)
Check that the heartbeat file for worker-a shows a time from the last minute.
```

## 6. Permissions

- Allow rule, Bash tool only: `Bash(cq *)` in `permissions.allow`. Check the rule syntax in the Claude Code permissions docs.
- Deny rules for actions that must never happen unattended (force-push, deleting outside the repo).
- If sessions run sandboxed, add the queue root to the sandbox's writable paths.
