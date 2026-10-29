# Setting up cq

How to install `cq`, configure it and upgrade it. Back to the [README](../README.md).

`cq` supports two agents: Claude Code and Codex. The install steps below are for Claude Code. A Codex chat needs only steps 1 and 3, then [Codex setup](CODEX.md). [Configuration](#configuration) applies to every agent. For any other agent, see [Other agents](#other-agents).

## Requirements

- One of the two supported agents:
  - **Claude Code.** Tested in the Claude desktop app's Code tab. A session receives queue events in its own Claude Code inbox ([inbox mode](PROTOCOL.md#inbox-mode)). A session with no inbox uses the Monitor tool instead ([Monitor mode](PROTOCOL.md#monitor-mode-fallback)).
  - **Codex.** The Codex Desktop app or the interactive Codex CLI, and a Codex CLI with `codex queue`. Tested on Windows only. See [Codex setup](CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex).
- Stable Rust (built and tested with 1.98.1).
- Windows 10 or 11, or macOS. For Claude Code on Windows, install Git for Windows, because Claude Code runs Bash commands in Git Bash. Codex on Windows ran its commands in PowerShell in the tested setup.

macOS support is new. The code builds for macOS, but it has not yet been tested on a Mac. Please open an issue if something does not work.

## Install

1. Build and install the binary:

   ```bash
   cargo install --path .
   ```

   This puts `cq` (`cq.exe` on Windows) in `~/.cargo/bin`, which the Rust installer adds to your `PATH`. If you prefer `cargo build --release`, add `target/release` to your `PATH` instead.

2. Restart the Claude app, because sessions read `PATH` only when they start.

   Expected result: `cq --version` works in a new session's Bash tool.

   **Codex:** the tested Codex setup did not rely on `PATH`. It called `cq` by its full path, for example `C:/full/path/to/cq.exe`, as in [Codex setup](CODEX.md#give-the-chat-its-rules). If you use the bare name, check that `cq --version` works in a new Codex chat. `cq watch --to-codex` runs `codex` from `PATH` unless you pass `--codex-bin PATH`.

3. Create the queue layout. Safe to run more than once:

   ```bash
   cq init
   ```

   The queue lives in a per-user folder by default (see [Configuration](#configuration)). Every session must use the same queue root.

4. Allow `cq` without permission prompts, so unattended sessions never stop to ask. Add `Bash(cq *)` to `permissions.allow` in `~/.claude/settings.json`. Call `cq` by its bare name everywhere so the rule matches.

5. If your sessions run sandboxed, add the queue root to the sandbox's writable paths.

6. Add the rules to `~/.claude/CLAUDE.md`: the [queue rules block](QUEUE-RULES.md#1-queue-rules-block), which is the same for every agent, and the Claude Code block from [`CLAUDE-snippets.md`](../CLAUDE-snippets.md#1-user-level-block). Add a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block) to each repo that takes queue work.

7. Optional: add deny rules for actions that must never happen unattended, such as force-push or deleting outside the repo.

8. Add the SessionStart hook to `~/.claude/settings.json`, so a session that joined the queue rejoins after `/clear`, a compaction or a resume. Merge it into any existing `hooks` object:

   ```json
   "hooks": {
     "SessionStart": [{"matcher": "clear|compact|resume", "hooks": [{"type": "command", "command": "cq session-start", "timeout": 10}]}]
   }
   ```

   Expected result: after `/clear` in a session whose watcher ran, the new context starts with "You are in the cq session queue as NAME".

9. Start each session with the [start prompt](../CLAUDE-snippets.md#3-session-start-prompt), giving each one its own queue name.

## Configuration

All settings are environment variables. Set queue-wide values before you start the Claude app (on Windows: Settings > System > About > Advanced system settings > Environment Variables > User variables).

| Variable | What it sets | Default |
| --- | --- | --- |
| `CQ_SESSION_ID` | Per-command or dedicated session-process membership key, before either Claude identity variable. Never set one global value shared by Claude sessions. Empty values fall back. | None |
| `CQ_ROOT` | The queue root. Must be an absolute path. | Windows: `%LOCALAPPDATA%\cq`. macOS: `~/Library/Application Support/cq`. Linux: `$XDG_DATA_HOME/cq` or `~/.local/share/cq`. |
| `CQ_RULES` | The rules file that the rejoin reminder tells a session to re-read. | None: the reminder names the rules in general terms. |
| `CQ_TZ` | The time zone of the local time in `CQ-NEW` lines (an IANA name such as `Europe/London`). | The system time zone. |
| `CQ_TZ_LABEL` | The label after that time, such as `UK`. | The zone's abbreviation, such as `GMT` or `CET`. |
| `CQ_OVERDUE_UNCLAIMED_MINUTES` | Minutes before an unclaimed task counts as overdue (see [Overdue tasks](COMMANDS.md#overdue-tasks)). Whole number, 1 or more. | 30 |
| `CQ_OVERDUE_CLAIMED_MINUTES` | Minutes before a claimed task with no `cq done` counts as overdue. Whole number, 1 or more. | 120 |

The macOS default path contains a space, so quote it in shell commands: `ls "$HOME/Library/Application Support/cq"`.

To bake your own defaults into your build instead, create `.cargo/config.toml` in the repo (it is gitignored). A variable set at run time still wins. Cargo does not apply a value from `[env]` if the same variable is already set in the shell that runs the build.

```toml
[env]
CQ_DEFAULT_ROOT = 'D:\queues\cq'
CQ_RULES_PATH = 'D:/queues/SESSION-RULES.md'
CQ_DEFAULT_TZ = 'Europe/London'
CQ_DEFAULT_TZ_LABEL = 'UK'
```

## Skills

Two Claude Code skills put a session to sleep and wake it (see [Sleep and wake](PROTOCOL.md#sleep-and-wake)). Copy `skills/cq-start` and `skills/cq-stop` to `~/.claude/skills/`:

```bash
cp -r skills/cq-start skills/cq-stop ~/.claude/skills/
```

To let a session sleep other sessions, add its name on its own line in `CQ_ROOT/coordinators`.

Codex has no skills. Outside the Codex chat, `cq sleep --as NAME NAME` stops its watcher, and `cq wake --as NAME NAME` must run before you start the watcher again. See [Codex setup](CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex).

## Other agents

`cq` has been tested with Claude Code and Codex only. Another agent can use the queue if it has:

1. **Shell access.** It runs `cq send`, `cq claim`, `cq done` and `cq list`. Each command needs the same absolute `CQ_ROOT` and write access to the queue root, without an approval prompt.
2. **Its rules.** Put the [queue rules](QUEUE-RULES.md) in the agent's instruction file.
3. **A wake route.** Something must start a turn when a message arrives. One of:
   - **A wake feature:** a command or socket that starts a turn in an idle chat, as `codex queue` and the Claude Code inbox do. `cq watch` supports only these two today, so a new agent needs a new watcher mode.
   - **A non-interactive mode:** a fresh agent run per task. `cq dispatch` runs only Codex today.
   - **Polling:** a scheduled task in the agent that runs `cq list --as NAME` and handles new messages. With Codex, each run cost a model turn, even when the inbox was empty ([Codex setup](CODEX.md#same-chat-scheduled-automation-fallback)).

If you use `cq` with another agent, please describe your setup in a comment on [issue #11](https://github.com/niklasrichardson/session-queue/issues/11).

## Upgrading while watchers run

On Windows, a running `cq.exe` is locked, but it can be renamed. To upgrade without stopping the sessions:

1. Build into a separate target folder, so the build does not touch the running binary: `CARGO_TARGET_DIR=../cq-build cargo build --release`.
2. Rename the live binary, for example `cq.exe` to `cq.old.exe` or `cq.exe.old-0156`. Running watchers keep using the old file. Name it `cq.exe.old-*` or `cq.old-*` (`cq.old.exe` also works): `--takeover` stops an old watcher only when its file is `cq.exe` or has one of these names.
3. Copy the new binary into its place.
4. Inbox-mode watchers never re-arm, so they keep running the old binary. Ask each inbox-mode session to restart its watcher: stop the background watcher command with TaskStop if it is still listed, then start `cq watch --as NAME --to-inbox --takeover 2>&1` as a background Bash command. Each Monitor-mode watcher picks up the new binary at its next re-arm (within 30 minutes).
5. Delete the old file once no process uses it.

On macOS, `cargo install --path .` replaces the binary while watchers run. Inbox-mode watchers need step 4. Each Monitor-mode watcher picks up the new binary at its next re-arm.

### Switch a session from Monitor mode to inbox mode

Inbox mode is the default. A session that you started with an older start prompt runs in Monitor mode. To move it to inbox mode, use a `cq` build that has `--to-inbox`, and install the updated `/cq-start` and `/cq-stop` skills first (see [Skills](#skills)). Then tell the session, in its own conversation:

1. Stop your watcher Monitor with TaskStop. Do not re-arm it.
2. Start `cq watch --as NAME --to-inbox --takeover 2>&1` as a background Bash command. `--takeover` ends the old watcher process, which TaskStop often leaves running.
3. From now on, queue events arrive as a message from `cq`. Handle the lines as before. Do not re-arm on a timer, and never start a watcher Monitor as well.

To go back to Monitor mode, stop the background command with TaskStop and arm the Monitor with `--takeover`.

Never run `cargo clean` on the folder your sessions run `cq` from: it deletes the live binary.
