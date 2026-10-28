---
name: cq-start
description: Start or wake this session's cq queue watcher - clear its sleep flag, reload its handoff, check its queue scope and arm the watcher. Use when the user says start cq, join the queue, wake up, or when a coordinator's wake message asks this session to rejoin. A coordinator can pass another session's queue name to wake that session.
argument-hint: "[queue name of another session - coordinators only]"
---

Read `docs/SESSION-RULES.md` in the cq repo first if it is not already in context. Run `cq` with the Bash tool (Git Bash).

## A. Another session (an argument was given)

Any session may wake another one: for example after `cq send` warned that the receiver is asleep, or on a `CQ-WAKE` line.

1. Run `cq wake --as <your name> <their name>`.
2. Find their session in `cq members` (the session ID beside their name).
3. Send that session a cross-session message (in the Claude desktop app: SendMessage to its `local_...` ID): `cq: you have work in your queue. Run /cq-start to rejoin.` Add the waiting message's ID and subject if you know them. Never put message bodies in it.
4. If no session is listed for the name, or the send fails, the session is not woken. Tell the user which session needs waking and why, and send a push notification if that tool exists. Do not retry in a loop.

## B. This session (no argument)

1. **Find your queue name** (`cq members`, or section 1 of `SESSION-RULES.md`). Tell the user the name in one line.
2. **Clear your sleep flag** if it is set: `cq wake --as <name> <name>`.
3. **Reload the handoff.** If `~/.claude/cq-handoff/<name>.md` exists, read it, then rename it to `<name>.last.md` so it is not loaded twice. Treat it as your own notes from before the clear. Mention open actions for the user from it.
4. **Check your queue scope** (section 2 of `SESSION-RULES.md`).
5. **Arm the watcher** (section 3 of `SESSION-RULES.md`): Monitor with `timeout_ms` 1800000 on `cq watch --as <name> 2>&1`. If it exits 5 and none of your own watcher Monitors runs, start it with `--takeover`.
6. **Handle waiting messages** as section 4 of `SESSION-RULES.md` says. The watcher prints a `CQ-NEW` line for each one.
7. **Restore Remote Control.** A sleeping session loses its Remote Control link, so the user cannot reach it from a phone. If the session's title starts with `RC:` and the app reports Remote Control off, turn it on for this session (in the Claude desktop app: `set_remote_control` with `"self"` and `enabled: true`). If that is refused or the tool is missing, tell the user in one line.
8. If the watcher prints `CQ-SLEEP`, a coordinator put you to sleep: run `/cq-stop`.
