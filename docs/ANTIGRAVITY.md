# Antigravity CLI setup

This page holds only what differs for the Antigravity CLI (`agy`). Before this page, do the [shared install steps](SETUP.md#install): install, `PATH` and `cq init`. After this page, [send a test message](SETUP.md#send-a-test-message). Other agents are in [Other agents](OTHER-AGENTS.md). Back to the [README](../README.md).

Experimental. `cq watch --as NAME --to-agy CONVERSATION_ID` wakes an Antigravity CLI conversation. On 6 Nov 2026, the watcher woke a real `agy` conversation on Windows 11, and the turn claimed and finished a message ([Test status](TESTING.md#antigravity-cli-live-use-on-windows-6-november-2026)). The `agy` command that it runs was also tested by hand, and the watcher has automated tests with a fake `agy` ([Test status](TESTING.md#antigravity-cli-wake-on-windows-6-november-2026)). A long run is not tested. The Antigravity desktop app was not tested with `cq`. Options, output and exit codes are in [Commands](COMMANDS.md#wake-an-antigravity-cli-conversation).

## Quick start

1. **Prerequisites.** Finish the shared install steps in [Setup](SETUP.md#install), with one absolute `CQ_ROOT` for every session. Install the Antigravity CLI (`agy`) on the same machine as the queue root. The watcher runs `agy` from `PATH` unless you pass `--agy-bin PATH`. Note the ID of the conversation to wake. How to read that ID in `agy` is not covered here.
2. **Rules and scope.** Put the queue rules in the conversation's instruction file (`GEMINI.md` or `AGENTS.md`), with a section for NAME ([Give the conversation its rules](#give-the-conversation-its-rules)). Add a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block) to the repo.
3. **Permissions.** Check that a print-mode turn can run `cq` without an approval prompt ([Allow `cq` without prompts](#allow-cq-without-prompts)). Also check that `agy` writes the queue root for real ([Check that agents write the root for real](SETUP.md#check-that-agents-write-the-root-for-real)).
4. **Identity.** Nothing to set in the conversation. The watcher records itself as `agy-NAME` in `members/`, or under `CQ_SESSION_ID` if you set it for the watcher. Each wake prompt names NAME.
5. **Watcher.** Start `cq watch --as NAME --to-agy CONVERSATION_ID` in the background, outside the conversation ([Start the watcher](#start-the-watcher)). Do not keep the conversation open in an interactive `agy` while the watcher runs ([Open conversation](#open-conversation)).
6. **End-to-end test.** Send NAME the [test message](SETUP.md#send-a-test-message) from another session, for example `lead`. Expected result:
   - the watcher prints one line when a turn starts, and one when it ends;
   - the turn claims the task and replies: `cq list --as NAME` shows no `UNCLAIMED` line for it;
   - `cq list --as lead` shows an `UNCLAIMED` `result` with the subject `Re: Queue test`;
   - `lead` claims the result and finishes it with `cq done --as lead RESULT_ID`, with no reply.

## Set up a conversation

### Give the conversation its rules

Put the [queue rules](QUEUE-RULES.md) in the conversation's instruction file (`GEMINI.md` or `AGENTS.md`), with a section for NAME. The default prompt is `Queue check as 'NAME'. Follow the 'NAME' section of your instruction file (GEMINI.md or AGENTS.md).`, so name the section after the queue name. If you start from [SESSION-RULES.example.md](SESSION-RULES.example.md), keep its "Antigravity CLI" part and delete the other agent parts.

### Allow `cq` without prompts

`cq claim`, `cq done` and `cq send` write files in the queue root. A print-mode turn must run them without an approval prompt. In the hand test, a print-mode turn ran `cq --version` without a prompt. The watcher never passes `--dangerously-skip-permissions`.

### Start the watcher

For each batch of new messages, the watcher runs one print-mode turn in the conversation: `agy --conversation CONVERSATION_ID -p PROMPT --output-format json`. Start it in the background, outside the conversation, from any folder. The turn runs in the conversation's own workspace.

```sh
cq watch --as NAME --to-agy CONVERSATION_ID
```

To run several `--to-codex` and `--to-agy` watchers from one PowerShell window, see [Watchers](WATCHERS.md). That script was tested with a fake `cq` only.

Each wake is a full model turn. In the hand test, a turn took 4 seconds to about 3.5 minutes. In live use, a worker's turn started 3 seconds after the message was sent and ended after 58 seconds.

The watcher drops `CQ-WAKE` and `CQ-OVERDUE` lines ([Commands](COMMANDS.md#wake-an-antigravity-cli-conversation)). So an Antigravity conversation that is a coordinator gets no wake or overdue alerts. See [Lifecycle by agent](PROTOCOL.md#lifecycle-by-agent).

## Open conversation

In one live test, a turn ran while the same conversation was also open in an interactive `agy` window. The turn claimed and finished the message, but the open window did not show the turn. About 5 minutes later, the interactive session exited. The cause is not known ([Test status](TESTING.md#antigravity-cli-live-use-on-windows-6-november-2026)). No way is known to send a prompt into an `agy` session that is already open from outside it ([What else is known](#what-else-is-known)).

Until this is understood, do not keep the conversation open in an interactive `agy` while its watcher runs. To work in the conversation yourself:

1. Put the name to sleep: `cq sleep --as NAME NAME`. The watcher exits 7.
2. Check that the watcher has stopped: `cq status --as NAME` prints `asleep` with `watcher=stopped`. Sleep does not stop a print turn that has already started, so also wait until no `agy` print turn for this conversation runs. In PowerShell: `Get-CimInstance Win32_Process -Filter "Name='agy.exe'" | Where-Object CommandLine -like '*CONVERSATION_ID*-p*'` prints nothing when none runs.
3. Open the conversation in `agy` and work in it.
4. Close `agy`.
5. Run `cq wake --as NAME NAME`, then start the watcher again.

## After a restart

Start the watcher again, with `--takeover` if it exits 5. If the name is asleep, run `cq wake --as NAME NAME` first. If it exits 11, read its last `cq:` line: a turn failed 4 times in a row, or `agy` answered from another conversation. Check the conversation ID, then start the watcher again. The conversation is not told when its watcher stops. [cq-top](CQ-TOP.md) shows the watcher's heartbeat.

## Sleep and wake

Outside the conversation, `cq sleep --as NAME NAME` stops the watcher with exit 7. Run `cq wake --as NAME NAME` before you start the watcher again. The `cq-start` and `cq-stop` skills are not tested in Antigravity. What sleep does for every agent is in [Setup](SETUP.md#sleep-and-wake).

## What else is known

Found on 6 Nov 2026, on Windows 11. The evidence, and how far each finding was tested, is in [Test status](TESTING.md#antigravity-cli-findings-on-windows-6-november-2026).

- **A background command woke an open session.** An open interactive `agy` session woke by itself and replied when a background shell command that it had started finished. This was one manual test. It was not tried with `cq watch`, and `cq` does not use this route.
- **`agy agentapi send-message` does not work from outside `agy`.** It needs a CSRF token that `agy` gives only to processes that it starts. So a watcher outside the session cannot use it.
- **Separate conversation stores.** The CLI and the desktop app keep separate conversation stores. Whether `agy --conversation` can resume a conversation from the desktop app is not known.
- **Hooks.** Antigravity has hooks: 6 events, each given JSON on stdin with camelCase field names. See the hooks docs bundled with the CLI. `cq` has no Antigravity hook support. `cq session-start` and `cq turn-end` are Claude Code hooks: do not install them in Antigravity. If you write your own hook, do not put queue message text in its output. Text that a sender wrote would then enter the conversation as system text.
