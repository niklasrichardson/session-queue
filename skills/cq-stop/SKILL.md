---
name: cq-stop
description: Put this session's cq queue watcher to sleep - save a handoff, set the sleep flag, stop the watcher, and clear the conversation so an idle session costs nothing. Use when the user says stop cq, sleep, pause this session's queue, or when the watcher prints CQ-SLEEP. A coordinator can pass another session's queue name to put that session to sleep.
argument-hint: "[queue name of another session - coordinators only]"
---

Read `docs/SESSION-RULES.md` in the cq repo first if it is not already in context. Run `cq` with the Bash tool (Git Bash).

## A. Another session (an argument was given)

Only a coordinator listed in the queue root's `coordinators` file may do this.

1. Run `cq sleep --as <your name> <their name> --reason "<short reason>"`. Exit 5 means you are not a coordinator: tell the user and stop.
2. Their watcher exits with `CQ-SLEEP` and their session runs this skill (section B) by itself. Do not message them.
3. Tell the user in one line which session is now asleep.

## B. This session (no argument)

Do the steps in order. Do not start new work.

1. **Find your queue name** (`cq members`, or section 1 of `SESSION-RULES.md`).
2. **Check it is safe to sleep.** If you have claimed a message that is not done, or work in progress that is not saved, finish or save it first. If you cannot, tell the user and stop.
3. **Save the handoff.** Write `~/.claude/cq-handoff/<name>.md` (create the folder if needed; replace an older file). Keep it under 400 words. Include:
   - the goal and current state, with file paths, branches, issue and PR links;
   - decisions already made, so the next session does not re-open them;
   - open actions for the user, open questions and blockers;
   - background tasks that will stop (watchers, Monitors, dev servers) and anything uncommitted (`git status`).
   Save durable facts to memory or project files as the project normally does. Do not copy whole files: point to them.
4. **Set the sleep flag.** If the watcher printed `CQ-SLEEP`, the flag is already set: skip this step. Otherwise run `cq sleep --as <name> <name> --reason "<short reason>"`.
5. **Stop the watcher.** Stop your watcher Monitor with TaskStop if one still runs. Do not start another one, and do not re-arm when it ends.
6. **Clear the conversation**, so the session costs nothing while it sleeps and wakes cheaply:
   - If the session serves Remote Control (its title starts with `RC:` or the app says so), you cannot clear it. End your reply with the line: `Ready: type /clear`.
   - Otherwise call the app's clear-session tool with `"self"` (in the Claude desktop app: `clear_session`). The clear happens when this turn ends, so write your reply first. If the tool is missing or refused, end with `Ready: type /clear`.
7. **Reply** in one short block: queue name, where the handoff is, anything the user must do before the clear, and how to wake the session (`/cq-start`, or a coordinator's wake message).

After the clear, the SessionStart hook runs `cq session-start`. It sees the sleep flag and does not start a watcher. Messages to this session wait in its inbox, and the coordinator wakes it when one arrives.
