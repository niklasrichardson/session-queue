---
name: cq-start
description: Start or wake this session's cq queue watcher - clear its sleep flag, reload its handoff, check its queue scope and arm the watcher. Use when the user says start cq, join the queue, wake up, or when a coordinator's wake message asks this session to rejoin. A coordinator can give another session's queue name to wake that session.
license: MIT
compatibility: Needs the cq CLI and a shell. Written for Claude Code and Codex; other agents can follow the general steps.
---

Read `docs/SESSION-RULES.md` in the cq repo first if it is not already in context. Run `cq` the way your rules say: in Claude Code, with the Bash tool (Git Bash); in Codex, in the chat's shell, with the full path and `CQ_ROOT` from your `AGENTS.md` section.

Find out which agent you are, if you do not already know. `CLAUDE_CODE_MESSAGING_SOCKET` is set in a Claude Code session that has an inbox. `CODEX_THREAD_ID` is set in a Codex chat. If neither is set, follow the "Other agents" steps.

If the user's request names another session's queue name, do section A. Otherwise do section B.

## A. Another session (the request names a queue name)

Any session may wake another one: for example after `cq send` warned that the receiver is asleep, or on a `CQ-WAKE` line.

1. Run `cq wake --as <your name> <their name>`.
2. Find their session in `cq members` (the session key beside their name). A key that starts with `codex-` is usually a Codex watcher.
3. Wake the session the way its agent needs:
   - **A Claude Code session, and you are in Claude Code:** send that session a cross-session message (in the Claude desktop app: SendMessage to its `local_...` ID): `cq: you have work in your queue. Run /cq-start to rejoin.` Add the waiting message's ID and subject if you know them. Never put message bodies in it.
   - **A Claude Code session, and you are not in Claude Code:** you cannot send that message. Do step 4.
   - **A Codex chat:** its `cq watch --to-codex` watcher stopped when the name went to sleep, and it runs outside the chat. Tell the user that the watcher for `<their name>` must be started again, outside the chat (see "Wake a Codex chat" in the cq Codex setup page). The chat can also run this skill itself, when the user asks it to.
   - **Another agent that polls:** tell the user to resume that agent's scheduler.
4. If no session is listed for the name, or the wake cannot be delivered, the session is not woken. Tell the user which session needs waking and why, and send a push notification if that tool exists. Do not retry in a loop.

## B. This session (no queue name in the request)

Steps 1 to 4 are the same for every agent.

1. **Find your queue name** (`cq members`, or section 1 of `SESSION-RULES.md`). In Codex, it is also the name in your `AGENTS.md` section and in the wake prompt. Tell the user the name in one line.
2. **Clear your sleep flag** if it is set: `cq wake --as <name> <name>`.
3. **Reload the handoff.** In Claude Code, the handoff is `~/.claude/cq-handoff/<name>.md`. In another agent, it is where the `cq-stop` skill wrote it: the path your rules name, else `~/.cq-handoff/<name>.md`. If it exists, read it, then rename it to `<name>.last.md` so it is not loaded twice. Treat it as your own notes from before the sleep. Mention open actions for the user from it.
4. **Check your queue scope** (section 2 of `SESSION-RULES.md`).

The next steps depend on the agent.

### Claude Code

5. **Arm the watcher** (section 3 of `SESSION-RULES.md`). Inbox mode is the default.
   - Inbox mode: start `cq watch --as <name> --to-inbox 2>&1` once, as a background Bash command (not the Monitor tool). If it exits 5, start it again with `--takeover`. Do not re-arm on a timer. Never run a watcher Monitor as well.
   - Monitor mode, only if `--to-inbox` exits 64 (this session has no inbox): Monitor with `timeout_ms` 1800000 on `cq watch --as <name> 2>&1`. If it exits 5 and none of your own watcher Monitors runs, start it with `--takeover`.
6. **Handle waiting messages** as section 4 of `SESSION-RULES.md` says. The watcher prints a `CQ-NEW` line for each one.
7. **Restore Remote Control.** A sleeping session loses its Remote Control link, so the user cannot reach it from a phone. If the session's title starts with `RC:` and the app reports Remote Control off, turn it on for this session (in the Claude desktop app: `set_remote_control` with `"self"` and `enabled: true`). If that is refused or the tool is missing, tell the user in one line.
8. If the watcher prints `CQ-SLEEP`, or a message from `cq` says `<name> is asleep`, a coordinator put you to sleep: run `/cq-stop`.

### Codex

5. **Get the watcher started.** The `cq watch --to-codex` watcher runs outside the chat, so do not start it from this chat. Read your thread ID (`$env:CODEX_THREAD_ID` in PowerShell, `echo "$CODEX_THREAD_ID"` in Git Bash). Give the user the watcher command with that ID, your queue name, the same `CQ_ROOT` and the full `cq` path, to run in a terminal outside the chat:
   `cq watch --as <name> --to-codex <thread ID> --codex-bin <full path to codex>`
   If an old watcher may still hold the name, add `--takeover`. If your setup gives you a trusted way to start it outside the chat, such as a coordinator session, use that instead.
6. **Handle waiting messages** now: run `cq list --as <name>`, and handle each `UNCLAIMED` message, oldest first, as your `AGENTS.md` section says.

### Other agents

5. **Resume the wake route.** If the agent polls, tell the user to resume its scheduler, or resume it yourself if your rules allow. If it has a watcher, start the watcher the way the agent's setup says.
6. **Handle waiting messages:** run `cq list --as <name>`, and handle each `UNCLAIMED` message, oldest first. On a later run, if the first line of `cq list` starts with `ASLEEP`, the name is asleep again: end the run with no other work.
