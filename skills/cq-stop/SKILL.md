---
name: cq-stop
description: Put this session's cq queue watcher to sleep - save a handoff, set the sleep flag, stop the watcher, and clear the conversation so an idle session costs nothing. Use when the user says stop cq, sleep, pause this session's queue, or when the watcher prints CQ-SLEEP. A coordinator can give another session's queue name to put that session to sleep.
license: MIT
compatibility: Needs the cq CLI and a shell. Written for Claude Code and Codex; other agents can follow the general steps.
---

Read `docs/SESSION-RULES.md` in the cq repo first if it is not already in context. Run `cq` the way your rules say: in Claude Code, with the Bash tool (Git Bash); in Codex, in the chat's shell, with the full path and `CQ_ROOT` from your `AGENTS.md` section.

Find out which agent you are, if you do not already know. `CLAUDE_CODE_MESSAGING_SOCKET` is set in a Claude Code session that has an inbox. `CODEX_THREAD_ID` is set in a Codex chat. If neither is set, follow the "Other agents" steps.

If the user's request names another session's queue name, do section A. Otherwise do section B.

## A. Another session (the request names a queue name)

Only a coordinator listed in the queue root's `coordinators` file may do this.

1. Run `cq sleep --as <your name> <their name> --reason "<short reason>"`. Exit 5 means you are not a coordinator: tell the user and stop.
2. What happens next depends on their agent (`cq members` shows their session key; a key that starts with `codex-` is usually a Codex watcher):
   - **A Claude Code session:** their watcher exits with `CQ-SLEEP` and their session runs this skill (section B) by itself. Do not message them.
   - **A Codex chat:** their `--to-codex` watcher exits 7. The chat is not told, and saves no handoff. Do not message it.
   - **Another agent that polls:** `cq sleep` does not stop its scheduler. Tell the user to pause that scheduler.
3. Tell the user in one line which session is now asleep.

## B. This session (no queue name in the request)

Do the steps in order. Do not start new work. Steps 1 to 4 are the same for every agent.

1. **Find your queue name** (`cq members`, or section 1 of `SESSION-RULES.md`). In Codex, it is also the name in your `AGENTS.md` section and in the wake prompt.
2. **Check it is safe to sleep.** If you have claimed a message that is not done, or work in progress that is not saved, finish or save it first. If you cannot, tell the user and stop.
3. **Save the handoff.** In Claude Code, write `~/.claude/cq-handoff/<name>.md`. In another agent, write it to the path your rules name, else `~/.cq-handoff/<name>.md`. Create the folder if needed, and replace an older file. Keep it under 400 words. Include:
   - the goal and current state, with file paths, branches, issue and PR links;
   - decisions already made, so the next session does not re-open them;
   - open actions for the user, open questions and blockers;
   - background tasks that will stop (watchers, Monitors, dev servers) and anything uncommitted (`git status`).
   Save durable facts to memory or project files as the project normally does. Do not copy whole files: point to them.
4. **Set the sleep flag.** If the watcher printed `CQ-SLEEP`, or `cq list` printed an `ASLEEP` line, the flag is already set: skip this step. Otherwise run `cq sleep --as <name> <name> --reason "<short reason>"`.

The next steps depend on the agent.

### Claude Code

5. **Stop the watcher.** In inbox mode (the default), stop the background `cq watch --to-inbox` command with TaskStop if it still runs; it also stops by itself once the sleep flag is set, and may post a last message from `cq` saying `<name> is asleep`. Ignore that message. In Monitor mode, stop your watcher Monitor with TaskStop if one still runs. Do not start another watcher, and do not re-arm when it ends.
6. **Clear the conversation**, so the session costs nothing while it sleeps and wakes cheaply:
   - If the session serves Remote Control (its title starts with `RC:` or the app says so), you cannot clear it. End your reply with the line: `Ready: type /clear`. Warn the user that the session loses Remote Control while it sleeps; `/cq-start` turns it back on after a wake. To reach it from a phone meanwhile, ask a session that is awake (for example the coordinator) to wake it.
   - Otherwise call the app's clear-session tool with `"self"` (in the Claude desktop app: `clear_session`). The clear happens when this turn ends, so write your reply first. If the tool is missing or refused, end with `Ready: type /clear`.
7. **Reply** in one short block: queue name, where the handoff is, anything the user must do before the clear, and how to wake the session (`/cq-start`, or a coordinator's wake message).

After the clear, the SessionStart hook runs `cq session-start`. It sees the sleep flag and does not start a watcher. Messages to this session wait in its inbox, and the coordinator wakes it when one arrives.

### Codex

5. **Let the watcher stop.** The `cq watch --to-codex` watcher runs outside the chat. It sees the sleep flag and exits 7 by itself, so the chat has nothing to stop. Do not start another watcher.
6. **Do not clear.** A Codex chat with no watcher is not woken, so it takes no turns while it sleeps. If the user wants a fresh context, they can start a new chat. A new chat has a new thread ID, so its watcher needs that ID when it wakes.
7. **Reply** in one short block: queue name, where the handoff is, and how to wake the session: run the `cq-start` skill in this chat (or in a new chat with the same `AGENTS.md` section), then start the watcher outside the chat.

### Other agents

5. **Stop the wake route.** If the agent polls, `cq sleep` does not stop its scheduler: tell the user to pause it, or pause it yourself if your rules allow. Until it is paused, each run must end with no other work when `cq list` prints an `ASLEEP` line. If it has a watcher, the watcher sees the sleep flag and exits 7.
6. **Reply** in one short block: queue name, where the handoff is, and how to wake the session (the `cq-start` skill, or `cq wake` and then resume the scheduler).
