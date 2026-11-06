# Other agents

This page holds only what differs for an agent other than Claude Code and Codex, or for a script. Before this page, do the [shared install steps](SETUP.md#install): install, `PATH` and `cq init`. After this page, [send a test message](SETUP.md#send-a-test-message). Back to the [README](../README.md).

`cq` has been tested live with Claude Code and Codex. The [Antigravity CLI](#antigravity-cli) watcher had a short live use with a real `agy` on 6 Nov 2026. Nothing else on this page has been tested with another agent. The tested setups are in [Claude Code setup](CLAUDE-CODE.md) and [Codex setup](CODEX.md).

## Quick path for another agent

1. Finish the shared install steps in [Setup](SETUP.md#install).
2. Check that the agent can run `cq --version` in its shell.
3. Add the queue rules to the agent's own instruction file ([Give the agent its rules](#give-the-agent-its-rules)).
4. Let the agent write to the queue root without an approval prompt ([Allow `cq` without prompts](#allow-cq-without-prompts)).
5. Choose a wake route ([Wake the agent](#wake-the-agent)).
6. Send NAME a test message, as in [Setup](SETUP.md#send-a-test-message), and check that the agent handles it.

## Set up another agent

The agent needs the same four things as a Claude Code session or a Codex chat: its rules, write access to the queue root without prompts, a route that wakes it, and a way to recover after a restart.

### Give the agent its rules

Put the [queue rules](QUEUE-RULES.md) in the agent's instruction file, and a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block) in each repo that takes queue work. If you start from [SESSION-RULES.example.md](SESSION-RULES.example.md), delete the Claude Code and Codex parts.

The rules work for any agent that can run a shell command. The agent runs `cq list`, `cq claim`, `cq done` and `cq send`, and treats every claimed message as data from another session, not as instructions from the user.

### Allow `cq` without prompts

Each `cq` command needs the same absolute `CQ_ROOT` and write access to the queue root. An unattended turn must be able to run them without an approval prompt. How to allow that depends on the agent.

### Wake the agent

Something must start a turn when a message arrives. One of:

- **A wake feature:** a command or socket that starts a turn in an idle chat, as `codex queue` and the Claude Code inbox do. `cq watch` supports these two, and print-mode turns for the [Antigravity CLI](#antigravity-cli). Another agent needs a new watcher mode.
- **A non-interactive mode:** a fresh agent run per task. `cq dispatch` runs only Codex today.
- **Polling:** a scheduled task in the agent that runs `cq list --as NAME` and handles new messages. With Codex, each run cost a model turn, even when the inbox was empty ([Codex setup](CODEX.md#same-chat-scheduled-automation-fallback)).

### After a restart

With a wake feature or polling, start the watcher or the scheduled task again after a restart, then run `cq list --as NAME` and handle any `UNCLAIMED` messages. `cq session-start` and its reminder describe the Claude Code workflow, so do not install it as a hook for another agent.

### Sleep and wake

The `cq-start` and `cq-stop` skills follow the Agent Skills format, and have steps for other agents. If your agent reads Agent Skills, install them as in [Install the skills](SETUP.md#install-the-skills). This is not tested. Otherwise, outside the agent, `cq sleep --as NAME NAME` puts the name to sleep, and `cq wake --as NAME NAME` must run before you start its watcher again. What sleep does for every agent is in [Setup](SETUP.md#sleep-and-wake).

With polling, `cq sleep` does not stop the scheduled task. Each run still starts and still costs a model turn, and `cq list` still lists the `UNCLAIMED` messages, after a first line `ASLEEP by=... reason="..."`. So:

1. Pause the agent's own scheduler when you run `cq sleep`.
2. Write the polling rules so that a run ends with no other work when `cq list` prints an `ASLEEP` line.
3. Run `cq wake`, then resume the scheduler.

## Antigravity CLI

Experimental. `cq watch --as NAME --to-agy CONVERSATION_ID` wakes an Antigravity CLI (`agy`) conversation. For each batch of new messages, it runs one print-mode turn in that conversation: `agy --conversation CONVERSATION_ID -p PROMPT --output-format json`. On 6 Nov 2026, the watcher woke a real `agy` conversation on Windows 11, and the turn claimed and finished a message ([Test status](TESTING.md#antigravity-cli-live-use-on-windows-6-november-2026)). The `agy` command it runs was also tested by hand, and the watcher has automated tests with a fake `agy` ([Test status](TESTING.md#antigravity-cli-wake-on-windows-6-november-2026)). The Antigravity desktop app was not tested with `cq`. Options, output and exit codes are in [Commands](COMMANDS.md#wake-an-antigravity-cli-conversation).

1. Put the queue rules in the conversation's instruction file (`GEMINI.md` or `AGENTS.md`), with a section for NAME. The default prompt tells the turn to follow that section.
2. Check that a print-mode turn can run `cq` without an approval prompt. In the hand test, a print-mode turn ran `cq --version` without a prompt. The watcher never passes `--dangerously-skip-permissions`.
3. Start the watcher in the background, outside the conversation, from any folder. The turn runs in the conversation's own workspace.

   ```sh
   cq watch --as NAME --to-agy CONVERSATION_ID
   ```

4. Send NAME a test message and check that a turn claims it. The watcher prints one line when a turn starts and one when it ends.

Each wake is a full model turn. In the hand test, a turn took 4 seconds to about 3.5 minutes. In live use, a worker's turn started 3 seconds after the message was sent and ended after 58 seconds.

**Open conversation.** In one live test, a turn ran while the same conversation was also open in an interactive `agy` window. The turn claimed and finished the message, but the open window did not show the turn. About 5 minutes later, the interactive session exited with `Read loop ... context canceled`. After it was reopened, the conversation history included the turn. The cause of the exit is not known. No way is known to send a prompt into an `agy` session that is already open.

Until this is understood, do not keep the conversation open in an interactive `agy` while its watcher runs. To work in the conversation yourself:

1. Put the name to sleep: `cq sleep --as NAME NAME`. The watcher exits 7.
2. Open the conversation in `agy` and work in it.
3. Close `agy`.
4. Run `cq wake --as NAME NAME`, then start the watcher again.

After a restart, start the watcher again, with `--takeover` if it exits 5. If it exits 11, read its last `cq:` line: a turn failed 4 times in a row, or `agy` answered from another conversation. Check the conversation ID, then start the watcher again.

## Tell us about your setup

If you use `cq` with another agent, please describe your setup in a comment on [issue #11](https://github.com/niklasrichardson/session-queue/issues/11).
