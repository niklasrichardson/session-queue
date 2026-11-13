# Antigravity CLI setup

This page holds only what differs for the Antigravity CLI (`agy`). Before this page, do the [shared install steps](../SETUP.md#install): install, `PATH` and `cq init`. After this page, [send a test message](../SETUP.md#send-a-test-message). Other agents are in [Other agents](OTHER-AGENTS.md). Back to the [README](../../README.md).

Experimental. `cq watch --as NAME --to-agy CONVERSATION_ID` wakes an Antigravity CLI conversation. On 6 Nov 2026, the watcher woke a real `agy` conversation on Windows 11, and the turn claimed and finished a message ([Test status](../TESTING.md#antigravity-cli-live-use-on-windows-6-november-2026)). The `agy` command that it runs was also tested by hand, and the watcher has automated tests with a fake `agy` ([Test status](../TESTING.md#antigravity-cli-wake-on-windows-6-november-2026)). A long run is not tested. The Antigravity desktop app was not tested with `cq`. Options, output and exit codes are in [Commands](../COMMANDS.md#wake-an-antigravity-cli-conversation).

**Which mode.** Print mode without broker (the quick start below) is for the sandbox off, or for macOS and Linux only: on Windows a sandboxed turn cannot write the queue root. On Windows with the sandbox on, use [broker mode](#broker-mode) (a watcher outside the conversation) or [session mode](#session-mode) (a watcher that the open session starts itself, CLI or App).

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

Experimental, cq 1.15.0, Windows only. On macOS and Linux `--agy-broker` exits 64: the problem it fixes (Low integrity) is Windows only, and the Unix exchange folder would need trust checks that read ACLs (macOS extended ACLs, POSIX ACLs), which cq does not have yet. On Windows, an `agy` turn runs at Low integrity, so it cannot write the queue root, which is Medium. In broker mode the watcher claims and finishes each message itself, and the turn only reads one request file and writes one reply file. The turn needs no write access to the queue root.

```sh
cq watch --as NAME --to-agy CONVERSATION_ID --agy-broker --agy-allow-from LEAD --agy-sandbox-sid SID
```

- **One turn per message**, oldest first: `agy --conversation ID --add-dir JOBDIR -p PROMPT --output-format json`. The prompt holds only the queue name and two paths, never message text.
- **Allow list.** Repeat `--agy-allow-from NAME` for each sender whose messages get a turn. A task from any other sender is rejected unread; other types are finished unread. Sender names are policy, not authentication.
- **Exchange folder.** `--agy-exchange DIR`, an absolute path. Default: `CQ_ROOT/agy-exchange/NAME`. It must not be or contain the queue root, and inside the queue root it must be under `agy-exchange/`. The watcher creates it with a Low label and a protected DACL: you and SYSTEM get Full control, the sandbox SID gets Modify. An existing folder must have exactly that access, or the watcher exits 64; its owner may be you or, in an elevated watcher, BUILTIN\Administrators. On Windows the watcher refuses a junction anywhere in the path. At start the watcher resolves the path and the queue root (symlinks on Unix, such as `/var` on macOS; 8.3 names, case and `subst` drives on Windows), applies the rules above to the result, and gives agy only the resolved path. If the job folder path leads elsewhere before a turn, the job is held.
- **Unix (not supported yet).** The Unix exchange code keeps this rule, for a later release: every folder from `/` down to the job folder must be owned by root or you, and not writable by group or others; a sticky folder such as `/tmp` may be, when the folder in it is yours or root's. Otherwise another user could swap a folder of the path before agy opens the request. The rule reads only owners and mode bits, so an ACL that lets another user add or delete entries in a folder of the path is not caught; that is why broker mode is refused on Unix. Windows does not need the rule, because the watcher's open handles stop any folder of the path being renamed.
- **Sandbox SID.** `--agy-sandbox-sid` is required on Windows. cq does not derive it. Whether it stays the same across `agy` versions is not known.
- **Files.** The job folder `JOBDIR/ID` holds `request.json` ([schema](../../schema/agy-request.v1.json)). The turn writes `reply.json` ([schema](../../schema/agy-reply.v1.json)) as UTF-8, with its shell or its file tool, with the request's `id` and `nonce`, and writes no other file in that folder. The watcher reads it only after every process of the turn has stopped.
- **Shell.** When agy's sandboxed shell starts, it adds an entry to the job folder and to each folder above it, so that the sandbox SID can traverse them. In the live test it added nothing to the exchange and job folders, where the sandbox SID already has Modify. It opens each folder without delete sharing, so the watcher's held handles have no delete access. They still deny delete sharing, so no folder of the path can be renamed during a turn. The entry stays on the folders above the exchange folder, as it does for any `--add-dir` path. An existing exchange folder with that entry (traverse and synchronize, not inherited, for the sandbox SID) is still accepted.
- **Outcome.** A valid reply or reject finishes a task with that text. A turn that succeeded without a valid reply gets a `failed` reply that says the task may have partly run. A failed turn without a reply gets a `failed` reply with agy's status. Info and results are finished without a reply.
- **Job records.** `CQ_ROOT/dispatch/NAME/ID/` holds `state.json`, `completion.json`, `agy.out` and `agy.err`. Records stay after the job.
- **No retry.** A turn that started is never run again. Only a failed spawn is tried again (after 1, 2 and 4 s).

### Held jobs

The watcher holds a job (phase `needs-review`, exit 11) when it cannot be sure what happened: the turn timed out (30 minutes), the watcher stopped while the turn ran, `agy` answered from another or no conversation, a failed turn still wrote a valid reply, or the watcher restarted while a turn may have run. The message stays claimed. At the next start the watcher refuses (exit 5) until a person has looked. It also refuses a torn record, a claimed message without a record, or more than one unfinished record.

1. Make sure no `agy` from that job runs. For a job held in `launching`, also look for an `agy` process that is suspended or that the watcher did not start in a job, and end it.
2. Inspect `state.json` (phase, reason, verdict), `completion.json` if present, `agy.out`, `agy.err`, the exchange job folder and the conversation.
3. Finish the message by hand with `cq done --as NAME ID --reply --status failed` or `--reject`. Do not run the task again unless a person has reviewed its side effects.
4. Move the record folder out of `dispatch/NAME/`, remove the exchange job folder, and start the watcher again.

Live test, 11 Nov 2026, Windows 11, real `agy`, 2 turns: the turn could use the exchange folder through the sandbox SID, and `--add-dir` gave the turn the job folder. In both turns agy's shell did not start (`exebox: creating sandbox: CreateProcessSecurityEnvironment: The process cannot access the file because it is being used by another process`), and agy wrote `reply.json` with its file tool. The cause was the delete access of the watcher's handles; cq 1.15.0 removes it ([Shell](#broker-mode)). After the fix, the shell started in a print turn on a job folder that cq's exchange code held open as the watcher does. Then a full `cq watch --agy-broker` turn ran a task that used the shell: the shell started and wrote `reply.json`, and the watcher finished the message ([test record](../TESTING.md)).

## Session mode

Experimental, cq 1.17.0, Windows only (exit 64 elsewhere). The open Antigravity session (CLI or App) starts the watcher itself, once, as a background command that bypasses the sandbox. For each message the watcher wakes that same session with `agentapi send-message`, through the exchange folder of [broker mode](#broker-mode). A wake from inside the session works; the agentapi call needs the token that Antigravity gives only to processes that it starts. The full watcher is not yet tested live (see [open gates](#open-gates)).

```text
C:\<cq folder>\cq.exe watch --as NAME --to-agy-session
```

### Setup

1. **Build.** Build `cq` with `CQ_DEFAULT_ROOT` set to the queue root. Session mode uses only that root and exits 64 in a build without it. It ignores `CQ_ROOT` and every other setting from the environment.
2. **Install.** Put `cq.exe` in a folder whose path has no spaces, for example `C:\<cq folder>\cq.exe`. No sandbox may write it: no Low label and no write grant to a sandbox principal on the file or any folder above it. **No agent workspace (Antigravity, Codex or Claude) may contain the queue root or the `cq.exe` folder.** The watcher checks the ACLs at start (exit 64), but it cannot check two things, so they are setup rules:
   - **Workspaces.** The agent's file tool runs outside the OS sandbox, so the ACL checks do not bind it. In a live test the CLI's file tool wrote into a `cq.exe` folder inside its workspace with no prompt, although the folder's ACL refused the sandboxed shell. It also wrote files in the queue root, outside its workspace, with no prompt; the likely cause is `"allowNonWorkspaceAccess": true` in the CLI's `settings.json` (not yet confirmed by a re-test). So:
     - set `allowNonWorkspaceAccess` to `false` in any CLI that hosts a session-mode watcher;
     - keep the queue root and `cq.exe` outside that session's workspace, and outside every folder listed in its trusted workspaces.

     cq does not read Antigravity's settings and cannot tell which folders are a workspace.
   - **Named sandbox groups.** Some sandboxes run as an ordinary local group (for example Codex's sandbox users group) that had Modify on a repo folder. Such a group looks like any other group, so cq does not refuse it. Give no such group write access to `cq.exe`, the queue root or any folder above them.
3. **Policy.** Add a table for NAME to `config.toml` in the queue root ([table below](#the-agy-session-table)). The queue root, `config.toml` and every folder above them must pass the same checks, with no junction or symbolic link.
4. **Rule.** In Antigravity, save a `command(...)` rule with the exact line, with the fixed absolute path to `cq.exe`, in the scope "Shared with Antigravity". It carries the sandbox bypass with no prompt, in every new conversation. Wildcard and prefix rules do not match on Windows.
5. **Instruction file.** Put this in the NAME section of `GEMINI.md` or `AGENTS.md`, with the real path:

   > Start the queue watcher once, as a background command, with `BypassSandbox: true`, using exactly this line and nothing else: `C:\<cq folder>\cq.exe watch --as NAME --to-agy-session`. Never add arguments, variables or other commands to it. Run every other command sandboxed.

   Add the "Antigravity session mode" rules from [SESSION-RULES.example.md](../SESSION-RULES.example.md#antigravity-session-mode).

### The `agy-session` table

```toml
[agy-session.NAME]
allow_from = ["lead"]
sandbox_sids = ["S-1-15-3-..."]
reply_minutes = 60
```

| Key | Required | Meaning |
| --- | --- | --- |
| `conversation` | no | A fallback conversation ID. Each wake goes to `ANTIGRAVITY_CONVERSATION_ID`, which Antigravity sets in the session's shell. If both are set and differ: exit 64. If the variable is missing, this value is used; if both are missing: exit 64. |
| `allow_from` | yes | The senders whose messages wake the session. A task from any other sender is rejected unread; other types are finished unread. |
| `sandbox_sids` | yes | Exactly one SID in this version: the sandbox SID that gets Modify on the exchange folder. The root checks also refuse a write grant to it. CLI and App may differ. |
| `ack_minutes` | no, 15 | No `started` this long after the first wake: exit 8. |
| `reply_minutes` | no, 60 | No reply this long after `started`: one RESUME wake. |
| `resume_minutes` | no, 10 | No reply this long after the RESUME wake: the job needs review, exit 11. |

The table is strict: an unknown key or a bad value is exit 64. Minutes are 1 to 1440.

### What the watcher checks at start

All before any queue write; each failure is exit 64.

- The root is the build's `CQ_DEFAULT_ROOT`. The root, `config.toml` and `cq.exe`: no junction or link anywhere in the path, no Low label, and no write grant, direct or inheritable, on the object or any folder above it, to any app-container or capability SID (`S-1-15-2-...`, `S-1-15-3-...`, listed in `sandbox_sids` or not), the Low label SID, RESTRICTED (`S-1-5-12`), WRITE RESTRICTED (`S-1-5-33`) or a `sandbox_sids` entry. The owner, users, Authenticated Users, administrators and SYSTEM may write: that is ordinary user trust. The root folders stay held open for the watcher's life.
- **If the root check fails.** In a live test the sandboxed shell could create files in the queue root and its folders, because the root's DACL had an inheritable Modify entry for a capability SID (`S-1-15-3-1024-...`). The start check refuses such a root. Remove the entry (and any like it on the folders above) before you use session mode; cq never changes the root's ACL itself.
- The conversation: `ANTIGRAVITY_CONVERSATION_ID`, which Antigravity sets and the session cannot change (each run-command call is a fresh process with no environment parameter). With `conversation` in the table as well, the two must match. Without the variable, the table's value is used. Neither, or a value that is not one line: exit 64.
- The launching session: the nearest `agy.exe` (CLI) or `language_server.exe` (App) above the watcher in the process tree, where each parent started before its child.
- The agentapi binary: that process's own image, run as `IMAGE agentapi send-message ID TEXT`. `PATH` is not searched. If `ANTIGRAVITY_AGENTAPI_EXE` names another file: exit 64. The image and every folder above it get the same checks as `cq.exe`. The watcher holds the image and its folders open for its life, so while it runs, the image cannot be changed, renamed, replaced or deleted, and its folders cannot be renamed. Before each wake it checks that the path still opens the same file (volume serial and file index); if not, it sends nothing and stops with exit 8, and the next start checks the program again.
- The address: `ANTIGRAVITY_LS_ADDRESS` must be a loopback host and port, and every listening socket a connect could reach must belong to the launching process. Checked again before each wake.
- The agentapi child gets only `ANTIGRAVITY_LS_ADDRESS`, `ANTIGRAVITY_CSRF_TOKEN`, `SystemRoot`, `TEMP` and `TMP`. cq never prints, logs or records the token or the address.

### Per message

1. The watcher claims the message, writes `request.json` in `CQ_ROOT/agy-exchange/NAME/ID/`, and records the wake in `CQ_ROOT/dispatch/NAME/ID/state.json` (schema `cq-agy-job/2`, `kind: "session"`) before it sends it.
2. It sends the NEW-WORK wake. The text holds only NAME and paths, never message text: "cq: queue job for 'NAME'. ... First create .../started as a new file; if it already exists, stop and end the turn. Then read .../request.json ... Write .../reply.json as the request says, in one write."
3. The session creates `started` as a new file. If it exists, the job was already started, and the session ends the turn. No `started`: the same wake again at 2 and 6 minutes; after `ack_minutes`, exit 8 and the job stays `waking`.
4. The session writes `reply.json` in one write: one JSON object, UTF-8, at most 64 KiB, with the request's `id` and `nonce` and no other fields ([schema](../../schema/agy-reply.v1.json)):
   - task: `{"id": ID, "nonce": NONCE, "outcome": "reply", "status": "ok", "text": "..."}` (status `ok` or `failed`), or `{"id": ID, "nonce": NONCE, "outcome": "reject", "text": "why"}`;
   - info or result: `{"id": ID, "nonce": NONCE, "outcome": "read"}`.
5. The watcher reads the folder every second. A valid reply finishes the message. A reply that is not valid and stays the same for 60 seconds gives the sender a `failed` reply. No reply after `reply_minutes`: one RESUME wake ("... was already started. Do not start it again. ..."); none after `resume_minutes` more: the job needs review, exit 11.

A restart reads the job record and the folder before any wake: it never sends NEW-WORK once `started` exists, and never sends the RESUME wake twice. `woken` with `started` gone: needs review. A `waking` or `woken` job whose message is no longer claimed (for example, finished with `cq done` or rejected while no watcher ran): needs review, exit 11, and no wake. Held jobs are handled as in [broker mode](#held-jobs).

### While it runs

- **Session end.** The watcher stops with exit 8 when the launching process exits, after 3 failed TCP connects to the address in a row (every 30 s), or when agentapi fails 4 times in a row (1, 2 and 4 s apart). It checks the process and the address on every tick, also with no job, while asleep and under `PAUSE`, so a watcher whose session has gone frees NAME. In the App one language server serves many conversations, so closing one conversation trips none of these; the next job then ends with exit 8 after `ack_minutes`.
- **Sleep.** While NAME is asleep the watcher keeps running, claims nothing and wakes nothing. Deadlines that ran out during the sleep start again on `cq wake`.
- **PAUSE.** While `PAUSE` exists the watcher claims nothing, finishes nothing and sends no wake: no NEW-WORK retry and no RESUME. It still reads `started` and `reply.json`. Deadlines work as for sleep: one that ran out under `PAUSE` starts again when `PAUSE` is removed, and a due RESUME then goes out at once.
- **Second start.** Exit 5. The stop line says "already running for this conversation; nothing to do" when the holder is a session-mode watcher for the same conversation, else "NAME is held by another watcher; ask the user". Only a person decides on `--takeover`.
- **Stop line.** When the watcher ends, its last stdout line is `cq watch (NAME) stopped: REASON. WHAT TO DO`. "Start the watcher again with the same line" follows exit 8. "Do not restart; tell the user" follows exit 5, 11 and 64. Progress lines go to stderr.
- **A wake during a turn.** A wake does not interrupt a running turn and does not cancel a running command. It arrives inside that turn as a system message at the next tool-response boundary (live test, CLI and App). So the session may get the NEW-WORK text in the middle of other work. The `started` rule still applies: create it as a new file, and stop if it exists.
- **Cross-surface sends fail silently.** A wake to a conversation of the other surface (CLI to App, or App to CLI) is not delivered, and agentapi does not say so. The ack timeout then gives exit 8.
- **Coordinators.** As in broker mode, a session-mode name can only reply: it cannot send queue messages, and gets no `CQ-WAKE` or `CQ-OVERDUE` lines.

### Open gates

Live tests on 13 November 2026, CLI and App, with `agentapi` and the sandbox but not yet with this watcher:

| Gate | Result |
| --- | --- |
| S1 environment | Pass. Each run-command call is a fresh PowerShell process: environment, working folder, functions and aliases never carry over, and the tool has no environment or working-folder parameter. |
| S1 writes | **Fail on the test machine**: the root's DACL gave a capability SID inheritable Modify, so the sandboxed shell could write the queue. The start check now refuses that root. The CLI's file tool also wrote in the queue root with no prompt, likely because of `allowNonWorkspaceAccess` (see [Setup](#setup)); cq cannot check that. To pass: fix the ACL and the setting, then confirm that nothing the session runs, shell or file tool, can write the root, `config.toml` or `cq.exe`. |
| A5 | Pass: the `cq.exe` path checks hold for the installed path. |
| A7 | Antigravity sets `ANTIGRAVITY_CONVERSATION_ID` to the conversation ID; cq uses it, with the table's `conversation` as an optional fallback and check. |
| A8 | A wake during a turn is not an interrupt (see [While it runs](#while-it-runs)). v1 is not blocked. |
| A12 | `send-message` delivers with exactly `ANTIGRAVITY_LS_ADDRESS`, `ANTIGRAVITY_CSRF_TOKEN`, `SystemRoot`, `TEMP` and `TMP`. Without the conversation variable the message shows as from the system. |

Still open: S1 writes on a fixed root, an end-to-end run of this watcher, and the agentapi image checks and hold on a real Antigravity install. The token is not protected from the session: the sandboxed shell already sees it and the address.

## Open conversation

This section is about print mode only. In one live test, a turn ran while the same conversation was also open in an interactive `agy` window. The turn claimed and finished the message, but the open window did not show the turn. About 5 minutes later, the interactive session exited. The cause is not known ([Test status](../TESTING.md#antigravity-cli-live-use-on-windows-6-november-2026)). No way is known to send a prompt into an `agy` session that is already open from outside it ([What else is known](#what-else-is-known)).

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
