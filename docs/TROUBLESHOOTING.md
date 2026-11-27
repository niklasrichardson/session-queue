# Troubleshooting

A short index of common problems. Each entry says what to check, what to do, and where the detail is. The exit codes are in [Commands](COMMANDS.md#exit-codes). Back to the [README](../README.md).

## A watcher has stopped

Messages wait, and nothing wakes the session.

1. Run `cq status --as NAME`. Exit 0 prints `running`, exit 10 `stopped` and exit 7 `asleep`. Do not trust a process ID that you remember. [cq-top](CQ-TOP.md) also shows a stopped watcher as `STALE` 90 seconds after it dies.
2. Read the last lines of `CQ_ROOT/watch-exits.log` for NAME. Each line gives the exit code and the reason ([Watcher status and exit log](COMMANDS.md#watcher-status-and-exit-log)). A watcher that was killed outright writes no line.
3. If the name is asleep, run `cq wake --as NAME NAME` first. A watcher for a sleeping name exits 7 at once, even with `--takeover`.
4. Start the watcher again. If it exits 5, an old watcher still holds the name: start it with `--takeover`.
   - Claude Code: run `/cq-start` in the session ([Inbox mode](PROTOCOL.md#inbox-mode)).
   - Codex chat: start it outside the chat. Exit 9 means `codex queue` failed 4 times: read the `cq:` line on stderr first ([Codex setup](agents/CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex)).
   - Antigravity CLI: start it outside the conversation. Exit 11 means a turn failed 4 times, or `agy` answered from another conversation: check the conversation ID first ([After a restart](agents/ANTIGRAVITY.md#after-a-restart)).

After a `cq` upgrade, a running watcher keeps the old binary. Start it again with `--takeover` ([Upgrading while watchers run](SETUP.md#upgrading-while-watchers-run)).

If every watcher stopped at about the same time, check that the `cq` binary still exists ([Antivirus software removed `cq`](#antivirus-software-removed-cq)).

## Antivirus software removed `cq`

`cq: command not found`, or the installed `cq.exe` is missing. Watchers that ran from it stop at the same time and write no line to `watch-exits.log`.

What was seen, on one Windows 11 computer: the installed `cq.exe` disappeared twice in one day while watchers ran from it. `cq-top.exe` in the same folder, and copies of the same `cq.exe` in other folders, stayed. The antivirus product's quarantine and logs did not show a removal.

What is suspected, not verified: the product's behaviour-based protection reacted to the running watcher, not to the file itself.

1. Check the exact path that each watcher was started with. `cq --version` is not enough: it can run another copy on `PATH`.
2. Find out what removed the file: look in the antivirus product's history, quarantine and logs. Only add an exception when the product shows that it removed `cq.exe`, and the file is one you built from this repo or got from its releases. Then add a **file** exception for that `cq.exe`, for the protection that removed it. A folder exception may not cover behaviour-based protection (not verified).
3. Install the binary again ([Install](SETUP.md#install)).
4. Run `cq status --as NAME` for each name. Start only the watchers that show `stopped`, without `--takeover`. If one exits 5, check which process holds the name before you use `--takeover` ([A watcher has stopped](#a-watcher-has-stopped)).

## Codex checks are queued but not taken

The watcher printed `queued a check`, but the chat starts no turn. The watcher may also print or send `has not taken the queued check`, and a coordinator may get `CQ-OVERDUE`.

- **Codex Desktop:** the chat may be unloaded. Open the chat in the Codex app ([If a wake does not start a turn](agents/CODEX.md#if-a-wake-does-not-start-a-turn)).
- **Codex CLI:** the chat may be closed, for example after a restart of the computer. Reopen it with `codex resume THREAD_ID`, then start or keep its watcher ([After a new chat or a restart](agents/CODEX.md#after-a-new-chat-or-a-restart)).

## A turn waits for an approval

A turn that waits for an approval handles nothing until a person answers.

- **Codex:** run the [preflight check](agents/CODEX.md#check-queue-writes-without-prompts) in the chat. `Access is denied` or an approval prompt means the chat cannot yet write the queue root unattended ([Allow `cq` without prompts](agents/CODEX.md#allow-cq-without-prompts)).
- **Claude Code:** check that `Bash(cq *)` is in `permissions.allow`, and that every command calls `cq` by its bare name ([Allow `cq` without prompts](agents/CLAUDE-CODE.md#allow-cq-without-prompts)).
- **Antigravity CLI:** check that a print-mode turn runs `cq` without a prompt ([Allow `cq` without prompts](agents/ANTIGRAVITY.md#allow-cq-without-prompts)).

## An open `agy` session exited

In one live test, an interactive `agy` session exited about 5 minutes after a print-mode turn ran in the same conversation. The cause is not known. Do not keep the conversation open in an interactive `agy` while its watcher runs. To work in it, put the name to sleep first ([Open conversation](agents/ANTIGRAVITY.md#open-conversation)).

## Sessions see different queues

One session sends, and the receiver never sees the message, or `cq-top` shows a different queue. An agent sandbox may write the queue root into a private copy. Run the check in [Check that agents write the root for real](SETUP.md#check-that-agents-write-the-root-for-real) for each agent.
