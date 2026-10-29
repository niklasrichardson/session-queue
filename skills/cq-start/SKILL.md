---
name: cq-start
description: Start or wake this session's cq queue watcher - clear its sleep flag, reload its handoff, check its queue scope and arm the watcher. Use when the user says start cq, join the queue, wake up, or when a coordinator's wake message asks this session to rejoin. A coordinator can give another session's queue name to wake that session.
license: MIT
compatibility: Needs the cq CLI and a shell. Written for Claude Code and Codex; other agents can follow the general steps.
---

Read your session rules first if they are not already in context: the file your setup made from cq's `docs/SESSION-RULES.example.md` (`CQ_RULES` points at it), or the queue rules in your `CLAUDE.md` or `AGENTS.md`. Run `cq` the way your rules say: in Claude Code, with the Bash tool (Git Bash); in Codex, in the chat's shell, with the full path and `CQ_ROOT` from your `AGENTS.md` section.

Find out which agent you are and how your name is woken (its wake route), if you do not already know:

- **Claude Code:** `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID` or `CLAUDE_CODE_HOST_SESSION_ID` is set. cq keys its member records on the two session IDs. Follow the "Claude Code" steps. They cover inbox mode and Monitor mode, so they also apply when `CLAUDE_CODE_MESSAGING_SOCKET` is not set.
- **Codex:** `CODEX_THREAD_ID` is set. A Codex chat is woken by a `cq watch --to-codex` watcher or by a scheduled automation that polls. Your `AGENTS.md` section should say which. If it does not, run `cq members`: if a key is listed for your name and line 4 of its member record (`CQ_ROOT/members/<key>`) is `codex`, the route is the watcher. A key can be left from an older watcher, so if you are not sure, ask the user. Follow "Codex with a watcher" or "Polling".
- **Another agent:** follow "Polling" if a scheduler polls for it, else "Other wake routes".

If the user's request names another session's queue name, do section A. Otherwise do section B.

## A. Another session (the request names a queue name)

Any session may wake another one: for example after `cq send` warned that the receiver is asleep, or on a `CQ-WAKE` line.

1. Run `cq wake --as <your name> <their name>`.
2. Find their session in `cq members` (the session key beside their name). `cq members` lists a key only after `cq watch` ran for the name. Line 4 of the member record (`CQ_ROOT/members/<key>`) is the watcher's mode: `inbox` or `monitor` for Claude Code, `codex` for a `--to-codex` watcher. A name with no key is usually woken by polling, or has no wake route.
3. Wake the session the way its route needs:
   - **A Claude Code session, and you are in Claude Code:** send that session a cross-session message (in the Claude desktop app: SendMessage to its `local_...` ID): `cq: you have work in your queue. Run /cq-start to rejoin.` Add the waiting message's ID and subject if you know them. Never put message bodies in it.
   - **A Claude Code session, and you are not in Claude Code:** you cannot send that message. Do step 4.
   - **A Codex chat with a watcher:** its `cq watch --to-codex` watcher stopped when the name went to sleep, and it runs outside the chat. Tell the user that the watcher for `<their name>` must be started again, outside the chat (see "Wake a Codex chat" in the cq Codex setup page). The chat can also run this skill itself, when the user asks it to.
   - **A session that polls** (a Codex scheduled automation, or another agent's scheduler): `cq wake` cleared the flag, so its next run handles the messages. If its scheduler was paused, tell the user to resume it.
4. If no session is listed for the name and it does not poll, or the wake cannot be delivered, the session is not woken. Tell the user which session needs waking and why, and send a push notification if that tool exists. Do not retry in a loop.

## B. This session (no queue name in the request)

Steps 1 to 4 are the same for every agent.

1. **Find your queue name** (`cq members`, or the "Name and scope" section of your rules). In Codex, it is also the name in your `AGENTS.md` section and in the wake prompt. Tell the user the name in one line.
2. **Clear your sleep flag** if it is set: `cq wake --as <name> <name>`.
3. **Reload the handoff.** In Claude Code, the handoff is `~/.claude/cq-handoff/<name>.md`. In another agent, it is where the `cq-stop` skill wrote it: the path your rules name, else `~/.cq-handoff/<name>.md`. If it exists, read it, then rename it to `<name>.last.md` so it is not loaded twice. Treat it as your own notes from before the sleep. Mention open actions for the user from it.
4. **Check your queue scope** (the "Name and scope" section of your rules).

The next steps depend on the agent and its wake route.

### Claude Code

5. **Arm the watcher** (the "Keep the receiver running" section of your rules). Inbox mode is the default.
   - Inbox mode: start `cq watch --as <name> --to-inbox 2>&1` once, as a background Bash command (not the Monitor tool). If it exits 5, start it again with `--takeover`. Do not re-arm on a timer. Never run a watcher Monitor as well.
   - Monitor mode, only if `--to-inbox` exits 64 (this session has no inbox): Monitor with `timeout_ms` 1800000 on `cq watch --as <name> 2>&1`. If it exits 5 and none of your own watcher Monitors runs, start it with `--takeover`.
6. **Handle waiting messages** as the "Claim, decide and finish" section of your rules says. The watcher prints a `CQ-NEW` line for each one.
7. **Restore Remote Control.** A sleeping session loses its Remote Control link, so the user cannot reach it from a phone. If the session's title starts with `RC:` and the app reports Remote Control off, turn it on for this session (in the Claude desktop app: `set_remote_control` with `"self"` and `enabled: true`). If that is refused or the tool is missing, tell the user in one line.
8. If the watcher prints `CQ-SLEEP`, or a message from `cq` says `<name> is asleep`, a coordinator put you to sleep: run `/cq-stop`.

### Codex with a watcher

5. **Get the watcher started.** The `cq watch --to-codex` watcher runs outside the chat, so do not start it from this chat. Read your thread ID (`$env:CODEX_THREAD_ID` in PowerShell, `echo "$CODEX_THREAD_ID"` in Git Bash). Give the user the watcher command with that ID, your queue name, the same `CQ_ROOT` and the full `cq` path, to run in a terminal outside the chat:
   `cq watch --as <name> --to-codex <thread ID> --codex-bin <full path to codex>`
   If an old watcher may still hold the name, add `--takeover`. If your setup gives you a trusted way to start it outside the chat, such as a coordinator session, use that instead.
6. **Handle waiting messages** now: run `cq list --as <name>`, and handle each `UNCLAIMED` message, oldest first, as your `AGENTS.md` section says.

### Polling

This covers a Codex scheduled automation and another agent that polls.

5. **Resume the scheduler.** If it was paused for the sleep, tell the user to resume it, or resume it yourself if your rules allow.
6. **Handle waiting messages:** run `cq list --as <name>`, and handle each `UNCLAIMED` message, oldest first. On a later run, if the first line of `cq list` starts with `ASLEEP`, the name is asleep again: end the run with no other work.

### Other wake routes

5. **Start the wake route** the way the agent's setup says, for example its watcher.
6. **Handle waiting messages:** run `cq list --as <name>`, and handle each `UNCLAIMED` message, oldest first.
