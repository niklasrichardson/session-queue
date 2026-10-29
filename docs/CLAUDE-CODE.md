# Claude Code setup

This page holds only what differs for Claude Code. Before this page, do the [shared install steps](SETUP.md#install): install, `PATH` and `cq init`. After this page, [send a test message](SETUP.md#send-a-test-message). The Codex steps are in [Codex setup](CODEX.md), and the steps for any other agent in [Other agents](OTHER-AGENTS.md).

Most live tests used the Claude desktop app's Code tab on Windows 11. On 29 Oct 2026, one Claude Code CLI session on Windows in inbox mode answered 25 of 25 tasks with nobody typing (live test L3 in [Spec](SPEC.md), [#4](https://github.com/niklasrichardson/session-queue/issues/4)). Nobody checked the receiving session's screen for a cap notice in that test. On macOS, inbox mode posted into one session in the Code tab; live hand-offs between sessions there are not checked ([#1](https://github.com/niklasrichardson/session-queue/issues/1)). Remote Control has not been tested ([Test status](TESTING.md)). Back to the [README](../README.md).

## Quick path for a Claude Code session

1. Finish the shared install steps in [Setup](SETUP.md#install).
2. Restart the Claude app, so new sessions find `cq` on `PATH`. Check that `cq --version` works in a new session's Bash tool.
3. Add `Bash(cq *)` to `permissions.allow` in `~/.claude/settings.json` ([Allow `cq` without prompts](#allow-cq-without-prompts)).
4. Add the queue rules and the Claude Code block to `~/.claude/CLAUDE.md` ([Give the session its rules](#give-the-session-its-rules)).
5. Add the SessionStart hook, and copy the `/cq-start` and `/cq-stop` skills ([After `/clear` or a restart](#after-clear-or-a-restart), [Sleep and wake](#sleep-and-wake)).
6. Give the session its queue name with the start prompt. It starts `cq watch --as NAME --to-inbox 2>&1` ([Wake a session with `cq watch --to-inbox`](#wake-a-session-with-cq-watch---to-inbox)).
7. Send NAME a test message, as in [Setup](SETUP.md#send-a-test-message), and check that the session starts a turn.

## Set up a Claude Code session

A Claude Code session needs four things: its rules, permission to run `cq` without prompts, a watcher that wakes it, and a way to recover after `/clear` or a restart.

### Give the session its rules

Claude Code reads `~/.claude/CLAUDE.md` and the repository's own `CLAUDE.md`. Put three things there:

1. In `~/.claude/CLAUDE.md`: the [queue rules block](QUEUE-RULES.md#1-queue-rules-block), which is the same for every agent.
2. In `~/.claude/CLAUDE.md`: the Claude Code block from [`CLAUDE-snippets.md`](../CLAUDE-snippets.md#1-user-level-block). It covers the watcher, the Monitor fallback and the skills.
3. In each repo that takes queue work: a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block).

Do not put a queue name in `CLAUDE.md`. Several sessions can share one folder, so each session gets its name in its [start prompt](../CLAUDE-snippets.md#3-session-start-prompt).

If you start from [SESSION-RULES.example.md](SESSION-RULES.example.md), keep its "Claude Code" and "Fallback: Monitor mode" parts. Delete the Codex parts: they do not apply to Claude Code.

### Allow `cq` without prompts

An unattended session must run `cq` without stopping at a permission prompt.

1. Add `Bash(cq *)` to `permissions.allow` in `~/.claude/settings.json`. Call `cq` by its bare name everywhere, so the rule matches. See [Permissions](../CLAUDE-snippets.md#6-permissions).
2. If your sessions run sandboxed, add the queue root to the sandbox's writable paths.
3. Optional: add deny rules for actions that must never happen unattended, such as force-push or deleting outside the repo.

Restart the Claude app after you change `~/.claude/settings.json`.

### Wake a session with `cq watch --to-inbox`

`cq watch --as NAME --to-inbox` watches NAME's inbox like any watcher. It posts the event lines into the session's own Claude Code inbox, as a message from `cq`. A message to an idle session starts a turn. An idle session runs no turns, so it uses no model quota until a message arrives. The command details are in [Commands](COMMANDS.md#watch-into-the-inbox), and the session's steps in [Session protocol](PROTOCOL.md#inbox-mode).

Prerequisites:

- Claude Code with a session inbox. Claude Code exports `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN` to Bash commands. Without them, `cq watch --to-inbox` exits 64: use [Monitor mode](#monitor-mode-fallback).
- On Windows, Git for Windows, because Claude Code runs Bash commands in Git Bash.

Steps:

1. Give the session its queue name with the [start prompt](../CLAUDE-snippets.md#3-session-start-prompt).
2. The session starts the watcher once, as a background Bash command, not with the Monitor tool:

   ```bash
   cq watch --as worker-a --to-inbox 2>&1
   ```

   If it exits 5 ("already running"), it starts it again with `--takeover`.
3. Send `worker-a` a test message. Expected result: a message from `cq` that starts with `cq watch (worker-a): queue events`, and a new turn in the session.

- **Stopping.** The watcher has no default time limit. When it stops, it posts one last message: `cq watch (NAME) stopped: ... Run /cq-start to re-arm.` A watcher that is killed without warning posts nothing. [cq-top](CQ-TOP.md) shows the mode `inbox` and the watcher's heartbeat, which turns `STALE` 90 seconds after the watcher dies.
- **Not verified.** Runs of many hours ([Design](DESIGN.md#inbox-mode)).

### After `/clear` or a restart

A Claude Code session forgets its queue name after `/clear` or a compaction. The SessionStart hook reminds it. Add the hook to `~/.claude/settings.json`, and merge it into any existing `hooks` object:

```json
"hooks": {
  "SessionStart": [{"matcher": "clear|compact|resume", "hooks": [{"type": "command", "command": "cq session-start", "timeout": 10}]}]
}
```

On macOS, a hook command may not find a bare `cq`: the Rust installer adds `~/.cargo/bin` to `PATH` only through the shell profile, and a hook command may not run through it. If the hook fails there, put the full path in the hook command, for example `/Users/<you>/.cargo/bin/cq session-start`. This was reported in [#1](https://github.com/niklasrichardson/session-queue/issues/1) and is not yet confirmed in a real hook. Keep the bare name `cq` in Bash tool commands, so they match `Bash(cq *)`.

Expected result: after `/clear` in a session whose watcher ran, the new context starts with "You are in the cq session queue as NAME". The reminder also says whether the watcher still runs. See [Rejoin after /clear](PROTOCOL.md#rejoin-after-clear). The hook was tested live after `/clear` only, not after a compaction or a resume.

- The inbox watcher survives `/clear` and compaction, and keeps posting into the same session. After a clear, do not start a second watcher.
- After a session restarts, or its watcher was down, give the session the [recovery prompt](../CLAUDE-snippets.md#5-recovery-prompt). If the name is asleep, run `/cq-start NAME` instead.

### Sleep and wake

Two skills put a session to sleep and wake it. The same files work in Codex (see [Install the skills](SETUP.md#install-the-skills)). For Claude Code, copy `skills/cq-start` and `skills/cq-stop` to `~/.claude/skills/`:

```bash
cp -r skills/cq-start skills/cq-stop ~/.claude/skills/
```

- `/cq-stop` saves a handoff, sets the sleep flag, stops the watcher and clears the conversation.
- `/cq-start NAME` wakes NAME, and sends the sleeping session a message through Claude Code, so it starts its watcher again.

What sleep does for every agent is in [Setup](SETUP.md#sleep-and-wake).

## Waking Claude Code

Two routes wake a Claude Code session:

| Route | Chat context | Pickup latency | Test status |
| --- | --- | --- | --- |
| Inbox mode: `cq watch --to-inbox` (default since 28 Oct 2026, [#24](https://github.com/niklasrichardson/session-queue/pull/24)) | Keeps the session's context | Not measured on its own. Lines are batched for 2 seconds, and posts are at least 5 seconds apart. | Tests with a fake inbox passed on Windows ([Test status](TESTING.md#inbox-mode-on-windows-28-october-2026)). Runs of many hours are not tested. |
| Monitor mode: `cq watch` under the Monitor tool (fallback) | Keeps the session's context | Each output line wakes the session | Tested live in the desktop app: hundreds of re-arms across 10 sessions. |

### Monitor mode (fallback)

Use Monitor mode only when `cq watch --to-inbox` exits 64, because the session has no inbox. The session starts `cq watch --as NAME 2>&1` under the Monitor tool with `timeout_ms` 1800000, and re-arms it each time it ends. See [Session protocol](PROTOCOL.md#monitor-mode-fallback). Each re-arm costs a model turn ([Cost of an idle session](OVERVIEW.md#cost-of-an-idle-session)).

### Switch a session from Monitor mode to inbox mode

Inbox mode is the default. A session that you started with an older start prompt runs in Monitor mode. To move it to inbox mode, use a `cq` build that has `--to-inbox`, and install the updated `/cq-start` and `/cq-stop` skills first (see [Sleep and wake](#sleep-and-wake)). Then tell the session, in its own conversation:

1. Stop your watcher Monitor with TaskStop. Do not re-arm it.
2. Start `cq watch --as NAME --to-inbox --takeover 2>&1` as a background Bash command. `--takeover` ends the old watcher process, which TaskStop often leaves running.
3. From now on, queue events arrive as a message from `cq`. Handle the lines as before. Do not re-arm on a timer, and never start a watcher Monitor as well.

To go back to Monitor mode, stop the background command with TaskStop and arm the Monitor with `--takeover`.

## After a cq upgrade

An inbox-mode watcher never re-arms, so it keeps running the old binary. Ask each inbox-mode session to restart its watcher: stop the background watcher command with TaskStop if it is still listed, then start `cq watch --as NAME --to-inbox --takeover 2>&1` as a background Bash command. Each Monitor-mode watcher picks up the new binary at its next re-arm (within 30 minutes). The shared upgrade steps are in [Setup](SETUP.md#upgrading-while-watchers-run).

## Session identity

`cq` records which session owns a queue name in `members/`. In Claude Code, the key is `CLAUDE_CODE_HOST_SESSION_ID` (the desktop app's `local_...` ID), else `CLAUDE_CODE_SESSION_ID`. `CQ_SESSION_ID` takes precedence over both. Never set one global `CQ_SESSION_ID` shared by Claude Code sessions: every session would get the same key and replace the others' records. See [Rejoin after /clear](PROTOCOL.md#rejoin-after-clear).
