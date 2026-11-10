# Antigravity CLI setup

This page holds only what differs for the Antigravity CLI (`agy`). Before this page, do the [shared install steps](../SETUP.md#install): install, `PATH` and `cq init`. After this page, [send a test message](../SETUP.md#send-a-test-message). Other agents are in [Other agents](OTHER-AGENTS.md). Back to the [README](../../README.md).

Experimental. `cq watch --as NAME --to-agy CONVERSATION_ID` wakes an Antigravity CLI conversation. On 6 Nov 2026, the watcher woke a real `agy` conversation on Windows 11, and the turn claimed and finished a message ([Test status](../TESTING.md#antigravity-cli-live-use-on-windows-6-november-2026)). The `agy` command that it runs was also tested by hand, and the watcher has automated tests with a fake `agy` ([Test status](../TESTING.md#antigravity-cli-wake-on-windows-6-november-2026)). A long run is not tested. The Antigravity desktop app was not tested with `cq`. Options, output and exit codes are in [Commands](../COMMANDS.md#wake-an-antigravity-cli-conversation).

## Quick start

1. **Prerequisites.** Finish the shared install steps in [Setup](../SETUP.md#install), with one absolute `CQ_ROOT` for every session. Install the Antigravity CLI (`agy`) on the same machine as the queue root. The watcher runs `agy` from `PATH` unless you pass `--agy-bin PATH`. Note the ID of the conversation to wake. How to read that ID in `agy` is not covered here.
2. **Rules and scope.** Put the queue rules in the conversation's instruction file (`GEMINI.md` or `AGENTS.md`), with a section for NAME ([Give the conversation its rules](#give-the-conversation-its-rules)). Add a ["Queue scope" block](../QUEUE-RULES.md#2-per-repo-scope-block) to the repo.
3. **Permissions.** Check that a print-mode turn can run `cq` without an approval prompt ([Allow `cq` without prompts](#allow-cq-without-prompts)). Also check that `agy` writes the queue root for real ([Check that agents write the root for real](../SETUP.md#check-that-agents-write-the-root-for-real)).
4. **Identity.** Nothing to set in the conversation. The watcher records itself as `agy-NAME` in `members/`, or under `CQ_SESSION_ID` if you set it for the watcher. Each wake prompt names NAME.
5. **Watcher.** Start `cq watch --as NAME --to-agy CONVERSATION_ID` in the background, outside the conversation ([Start the watcher](#start-the-watcher)). Do not keep the conversation open in an interactive `agy` while the watcher runs ([Open conversation](#open-conversation)).
6. **End-to-end test.** Send NAME the [test message](../SETUP.md#send-a-test-message) from another session, for example `lead`. Expected result:
   - the watcher prints one line when a turn starts, and one when it ends;
   - the turn claims the task and replies: `cq list --as NAME` shows no `UNCLAIMED` line for it;
   - `cq list --as lead` shows an `UNCLAIMED` `result` with the subject `Re: Queue test`;
   - `lead` claims the result and finishes it with `cq done --as lead RESULT_ID`, with no reply.

## Set up a conversation

### Give the conversation its rules

Put the [queue rules](../QUEUE-RULES.md) in the conversation's instruction file (`GEMINI.md` or `AGENTS.md`), with a section for NAME. The default prompt is `Queue check as 'NAME'. Follow the 'NAME' section of your instruction file (GEMINI.md or AGENTS.md).`, so name the section after the queue name. If you start from [SESSION-RULES.example.md](../SESSION-RULES.example.md), keep its "Antigravity CLI" part and delete the other agent parts.

### Allow `cq` without prompts

`cq claim`, `cq done` and `cq send` write files in the queue root. A print-mode turn must run them without an approval prompt. In the hand test, a print-mode turn ran `cq --version` without a prompt. The watcher never passes `--dangerously-skip-permissions`.

### Start the watcher

For each batch of new messages, the watcher runs one print-mode turn in the conversation: `agy --conversation CONVERSATION_ID -p PROMPT --output-format json`. Start it in the background, outside the conversation, from any folder. The turn runs in the conversation's own workspace.

```sh
cq watch --as NAME --to-agy CONVERSATION_ID
```

To run several `--to-codex` and `--to-agy` watchers from one PowerShell window, see [Watchers](../WATCHERS.md). That script was tested with a fake `cq` only.

Each wake is a full model turn. In the hand test, a turn took 4 seconds to about 3.5 minutes. In live use, a worker's turn started 3 seconds after the message was sent and ended after 58 seconds.

The watcher drops `CQ-WAKE` and `CQ-OVERDUE` lines ([Commands](../COMMANDS.md#wake-an-antigravity-cli-conversation)). So an Antigravity conversation that is a coordinator gets no wake or overdue alerts. See [Lifecycle by agent](../PROTOCOL.md#lifecycle-by-agent).

## Broker mode

Experimental, cq 1.14.0. On Windows, an `agy` turn runs at Low integrity, so it cannot write the queue root, which is Medium. In broker mode the watcher claims and finishes each message itself, and the turn only reads one request file and writes one reply file. The turn needs no write access to the queue root.

```sh
cq watch --as NAME --to-agy CONVERSATION_ID --agy-broker --agy-allow-from LEAD --agy-sandbox-sid SID
```

- **One turn per message**, oldest first: `agy --conversation ID --add-dir JOBDIR -p PROMPT --output-format json`. The prompt holds only the queue name and two paths, never message text.
- **Allow list.** Repeat `--agy-allow-from NAME` for each sender whose messages get a turn. A task from any other sender is rejected unread; other types are finished unread. Sender names are policy, not authentication.
- **Exchange folder.** `--agy-exchange DIR`, an absolute path. Default: `CQ_ROOT/agy-exchange/NAME`. It must not be or contain the queue root, and inside the queue root it must be under `agy-exchange/`. The watcher creates it with a Low label and a protected DACL: you and SYSTEM get Full control, the sandbox SID gets Modify. An existing folder must have exactly that access, or the watcher exits 64; its owner may be you or, in an elevated watcher, BUILTIN\Administrators. On Windows the watcher refuses a junction anywhere in the path. At start the watcher resolves the path and the queue root (symlinks on Unix, such as `/var` on macOS; 8.3 names, case and `subst` drives on Windows), applies the rules above to the result, and gives agy only the resolved path. If the job folder path leads elsewhere before a turn, the job is held.
- **Sandbox SID.** `--agy-sandbox-sid` is required on Windows. cq does not derive it. Whether it stays the same across `agy` versions is not known.
- **Files.** The job folder `JOBDIR/ID` holds `request.json` ([schema](../../schema/agy-request.v1.json)). The turn writes `reply.json` ([schema](../../schema/agy-reply.v1.json)) with the request's `id` and `nonce`. The watcher reads it only after every process of the turn has stopped.
- **Outcome.** A valid reply or reject finishes a task with that text. A turn that succeeded without a valid reply gets a `failed` reply that says the task may have partly run. A failed turn without a reply gets a `failed` reply with agy's status. Info and results are finished without a reply.
- **Job records.** `CQ_ROOT/dispatch/NAME/ID/` holds `state.json`, `completion.json`, `agy.out` and `agy.err`. Records stay after the job.
- **No retry.** A turn that started is never run again. Only a failed spawn is tried again (after 1, 2 and 4 s).

### Held jobs

The watcher holds a job (phase `needs-review`, exit 11) when it cannot be sure what happened: the turn timed out (30 minutes), the watcher stopped while the turn ran, `agy` answered from another or no conversation, a failed turn still wrote a valid reply, or the watcher restarted while a turn may have run. The message stays claimed. At the next start the watcher refuses (exit 5) until a person has looked. It also refuses a torn record, a claimed message without a record, or more than one unfinished record.

1. Make sure no `agy` from that job runs. For a job held in `launching`, also look for an `agy` process that is suspended or that the watcher did not start in a job, and end it.
2. Inspect `state.json` (phase, reason, verdict), `completion.json` if present, `agy.out`, `agy.err`, the exchange job folder and the conversation.
3. Finish the message by hand with `cq done --as NAME ID --reply --status failed` or `--reject`. Do not run the task again unless a person has reviewed its side effects.
4. Move the record folder out of `dispatch/NAME/`, remove the exchange job folder, and start the watcher again.

Not yet tested with the real `agy`: that the turn's token has the sandbox SID, and that `--add-dir` gives the turn the job folder. Use broker mode only after those live tests.

## Open conversation

In one live test, a turn ran while the same conversation was also open in an interactive `agy` window. The turn claimed and finished the message, but the open window did not show the turn. About 5 minutes later, the interactive session exited. The cause is not known ([Test status](../TESTING.md#antigravity-cli-live-use-on-windows-6-november-2026)). No way is known to send a prompt into an `agy` session that is already open from outside it ([What else is known](#what-else-is-known)).

Until this is understood, do not keep the conversation open in an interactive `agy` while its watcher runs. To work in the conversation yourself:

1. Put the name to sleep: `cq sleep --as NAME NAME`. The watcher exits 7.
2. Check that the watcher has stopped: `cq status --as NAME` prints `asleep` with `watcher=stopped`. Sleep does not stop a print turn that has already started, so also wait until no `agy` print turn for this conversation runs. In PowerShell: `Get-CimInstance Win32_Process -Filter "Name='agy.exe'" | Where-Object CommandLine -like '*CONVERSATION_ID*-p*'` prints nothing when none runs.
3. Open the conversation in `agy` and work in it.
4. Close `agy`.
5. Run `cq wake --as NAME NAME`, then start the watcher again.

## After a restart

Start the watcher again, with `--takeover` if it exits 5. If the name is asleep, run `cq wake --as NAME NAME` first. If it exits 11, read its last `cq:` line: a turn failed 4 times in a row, or `agy` answered from another conversation. Check the conversation ID, then start the watcher again. The conversation is not told when its watcher stops. [cq-top](../CQ-TOP.md) shows the watcher's heartbeat.

## Sleep and wake

Outside the conversation, `cq sleep --as NAME NAME` stops the watcher with exit 7. Run `cq wake --as NAME NAME` before you start the watcher again. The `cq-start` and `cq-stop` skills are not tested in Antigravity. What sleep does for every agent is in [Setup](../SETUP.md#sleep-and-wake).

## What else is known

Found on 6 Nov 2026, on Windows 11. The evidence, and how far each finding was tested, is in [Test status](../TESTING.md#antigravity-cli-findings-on-windows-6-november-2026).

- **A background command woke an open session.** An open interactive `agy` session woke by itself and replied when a background shell command that it had started finished. This was one manual test. It was not tried with `cq watch`, and `cq` does not use this route.
- **`agy agentapi send-message` does not work from outside `agy`.** It needs a CSRF token that `agy` gives only to processes that it starts. So a watcher outside the session cannot use it.
- **Separate conversation stores.** The CLI and the desktop app keep separate conversation stores. Whether `agy --conversation` can resume a conversation from the desktop app is not known.
- **Hooks.** Antigravity has hooks: 6 events, each given JSON on stdin with camelCase field names. See the hooks docs bundled with the CLI. `cq` has no Antigravity hook support. `cq session-start` and `cq turn-end` are Claude Code hooks: do not install them in Antigravity. If you write your own hook, do not put queue message text in its output. Text that a sender wrote would then enter the conversation as system text.
