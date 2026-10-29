---
name: cq-stop
description: Put this session's cq queue watcher to sleep - save a handoff, set the sleep flag, stop the watcher, and clear the conversation so an idle session costs nothing. Use when the user says stop cq, sleep, pause this session's queue, or when the watcher prints CQ-SLEEP. A coordinator can give another session's queue name to put that session to sleep.
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

Only a coordinator listed in the queue root's `coordinators` file may do this.

1. Run `cq sleep --as <your name> <their name> --reason "<short reason>"`. Exit 5 means you are not a coordinator: tell the user and stop.
2. What happens next depends on their wake route. `cq members` lists a key only after `cq watch` ran for the name. Line 4 of the member record (`CQ_ROOT/members/<key>`) is the watcher's mode: `inbox` or `monitor` for Claude Code, `codex` for a `--to-codex` watcher. A name with no key is usually woken by polling.
   - **A Claude Code session:** their watcher exits with `CQ-SLEEP` and their session runs this skill (section B) by itself. Do not message them.
   - **A Codex chat with a watcher:** their `--to-codex` watcher exits 7. The chat is not told, and saves no handoff. Do not message it.
   - **A session that polls** (a Codex scheduled automation, or another agent's scheduler): `cq sleep` does not stop its scheduler. Each run sees an `ASLEEP` line in `cq list` and should end with no other work. Tell the user to pause that scheduler.
3. Tell the user in one line which session is now asleep.

## B. This session (no queue name in the request)

Do the steps in order. Do not start new work. Steps 1 to 4 are the same for every agent.

1. **Find your queue name** (`cq members`, or the "Name and scope" section of your rules). In Codex, it is also the name in your `AGENTS.md` section and in the wake prompt.
2. **Check it is safe to sleep.** If you have claimed a message that is not done, or work in progress that is not saved, finish or save it first. If you cannot, tell the user and stop.
3. **Save the handoff.** In Claude Code, write `~/.claude/cq-handoff/<name>.md`. In another agent, write it to the path your rules name, else `~/.cq-handoff/<name>.md`. In Codex, `~/.cq-handoff` is outside the chat's writable workspace, so expect an approval prompt unless your rules name a path inside the workspace. Create the folder if needed, and replace an older file. Keep it under 400 words. Include:
   - the goal and current state, with file paths, branches, issue and PR links;
   - decisions already made, so the next session does not re-open them;
   - open actions for the user, open questions and blockers;
   - background tasks that will stop (watchers, Monitors, dev servers) and anything uncommitted (`git status`).
   Save durable facts to memory or project files as the project normally does. Do not copy whole files: point to them.
4. **Set the sleep flag.** If the watcher printed `CQ-SLEEP`, or `cq list` printed an `ASLEEP` line, the flag is already set: skip this step. Otherwise run `cq sleep --as <name> <name> --reason "<short reason>"`.

The next steps depend on the agent and its wake route.

### Claude Code

5. **Stop the watcher.** In inbox mode (the default), stop the background `cq watch --to-inbox` command with TaskStop if it still runs; it also stops by itself once the sleep flag is set, and may post a last message from `cq` saying `<name> is asleep`. Ignore that message. In Monitor mode, stop your watcher Monitor with TaskStop if one still runs. Do not start another watcher, and do not re-arm when it ends.
6. **Clear the conversation**, so the session costs nothing while it sleeps and wakes cheaply:
   - If the session serves Remote Control (its title starts with `RC:` or the app says so), you cannot clear it. End your reply with the line: `Ready: type /clear`. Warn the user that the session loses Remote Control while it sleeps; `/cq-start` turns it back on after a wake. To reach it from a phone meanwhile, ask a session that is awake (for example the coordinator) to wake it.
   - Otherwise call the app's clear-session tool with `"self"` (in the Claude desktop app: `clear_session`). The clear happens when this turn ends, so write your reply first. If the tool is missing or refused, end with `Ready: type /clear`.
7. **Reply** in one short block: queue name, where the handoff is, anything the user must do before the clear, and how to wake the session (`/cq-start`, or a coordinator's wake message).

After the clear, the SessionStart hook runs `cq session-start`. It sees the sleep flag and does not start a watcher. Messages to this session wait in its inbox, and the coordinator wakes it when one arrives.

### Codex with a watcher

5. **Let the watcher stop.** The `cq watch --to-codex` watcher runs outside the chat. It sees the sleep flag and exits 7 by itself, so the chat has nothing to stop. Do not start another watcher.
6. **Do not clear.** With its watcher stopped, the chat is not woken, so it takes no turns while it sleeps. If the user wants a fresh context, they can start a new chat. A new chat has a new thread ID, so its watcher needs that ID when it wakes.
7. **Reply** in one short block: queue name, where the handoff is, and how to wake the session: run the `cq-start` skill in this chat (or in a new chat with the same `AGENTS.md` section), then start the watcher outside the chat.

### Polling

This covers a Codex scheduled automation and another agent that polls.

5. **Pause the scheduler.** `cq sleep` does not stop it, so its runs still take turns. Tell the user to pause it, or pause it yourself if your rules allow. Until it is paused, each run must end with no other work when `cq list` prints an `ASLEEP` line.
6. **Do not clear** the conversation, unless your agent's rules say how.
7. **Reply** in one short block: queue name, where the handoff is, whether the scheduler is paused, and how to wake the session (the `cq-start` skill, or `cq wake` and then resume the scheduler).

### Other wake routes

5. **Stop the wake route.** A watcher sees the sleep flag and exits 7. Otherwise stop the route the way the agent's setup says, or tell the user.
6. **Reply** in one short block: queue name, where the handoff is, and how to wake the session (the `cq-start` skill, or `cq wake` and then start the wake route again).
