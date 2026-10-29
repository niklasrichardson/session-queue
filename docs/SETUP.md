# Setting up cq

How to install `cq`, connect an agent, configure it and upgrade it. Back to the [README](../README.md).

Set up `cq` in three parts, in this order:

1. [Install](#install), on this page. The same for every agent.
2. [Connect each session](#connect-each-session), on your agent's page: its rules file, approvals, watcher and recovery.
3. [Send a test message](#send-a-test-message), on this page. The same for every agent.

| Agent | Page | Tested |
| --- | --- | --- |
| Claude Code | [Claude Code setup](CLAUDE-CODE.md) | Live on Windows 11: in the Claude desktop app's Code tab, and one Claude Code CLI session (live test L3) |
| Codex | [Codex setup](CODEX.md) | Live, in the Codex Desktop app on Windows 11 |
| Any other agent, or a script | [Other agents](OTHER-AGENTS.md) | Not tested |

## Requirements

- Stable Rust (built and tested with 1.98.1).
- Windows 10 or 11, or macOS. macOS support is new. The code builds for macOS, but it has not yet been tested on a Mac. Please open an issue if something does not work.
- An agent that can run shell commands. What each agent needs:
  - **Claude Code:** on Windows, Git for Windows, because Claude Code runs Bash commands in Git Bash. See [Claude Code setup](CLAUDE-CODE.md#wake-a-session-with-cq-watch---to-inbox).
  - **Codex:** the Codex Desktop app or the interactive Codex CLI, and a Codex CLI with `codex queue`. Codex on Windows ran its commands in PowerShell in the tested setup. See [Codex setup](CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex).
  - **Other agents:** see [Other agents](OTHER-AGENTS.md).

## Install

1. Build and install the binary:

   ```bash
   cargo install --path .
   ```

   This puts `cq` (`cq.exe` on Windows) in `~/.cargo/bin`, which the Rust installer adds to your `PATH`. If you prefer `cargo build --release`, add `target/release` to your `PATH` instead.

2. Make sure the agent can find `cq`. An agent reads `PATH` only when it starts.

   | Agent | What to do |
   | --- | --- |
   | Claude Code | Restart the Claude app. Expected result: `cq --version` works in a new session's Bash tool. |
   | Codex | The tested setup did not rely on `PATH`. It called `cq` by its full path, for example `C:/full/path/to/cq.exe`. If you use the bare name, check that `cq --version` works in a new Codex chat. `cq watch --to-codex` runs `codex` from `PATH` unless you pass `--codex-bin PATH`. |
   | Other agents | Restart the agent, and check that `cq --version` works in its shell. |

3. Create the queue layout. Safe to run more than once:

   ```bash
   cq init
   ```

   The queue lives in a per-user folder by default (see [Configuration](#configuration)). Every session must use the same absolute queue root. If you set `CQ_ROOT`, use the same value for every `cq` command: in each session, for each watcher and for each sender.

## Connect each session

These facts are the same for every agent:

- Each session gets its own queue name, such as `lead` or `worker-a`. Never put a fixed name in a shared instruction file: several sessions can share one folder.
- The [queue rules block](QUEUE-RULES.md#1-queue-rules-block) is the same for every agent. Each repo that takes queue work also gets a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block).
- `cq claim`, `cq done` and `cq send` write files in the queue root, so an unattended session must be able to run them without an approval prompt.
- Each agent page also says how to recover after `/clear`, a new chat or a restart.

The rest differs per agent. Follow the page for your agent, then come back to [Send a test message](#send-a-test-message):

| Agent | Rules file | No prompts | Wake route | Page |
| --- | --- | --- | --- | --- |
| Claude Code | `~/.claude/CLAUDE.md`, with the Claude Code block. The queue name goes in the start prompt. | `Bash(cq *)` in `permissions.allow` | `cq watch --to-inbox`, in the session | [Claude Code setup](CLAUDE-CODE.md) |
| Codex | `~/.codex/AGENTS.md` or the repo's `AGENTS.md`, with one section per queue name | The queue root writable from the chat's sandbox. A setup where Codex never asks for approval is not tested yet. | `cq watch --to-codex`, outside the chat | [Codex setup](CODEX.md) |
| Other agents | The agent's own instruction file | Depends on the agent | A wake feature, a fresh run per task, or polling | [Other agents](OTHER-AGENTS.md) |

## Send a test message

Do this after your agent's page. From another session:

```bash
cq send --from lead --to worker-a --type task <<'EOF'
{"subject": "Run the tests", "body": "Run cargo test and report failures."}
EOF
```

Expected result: the receiving session starts a turn within seconds, claims the task and replies. A Claude Code session gets a message from `cq` with a `CQ-NEW` line. A Codex watcher prints one `queued a check` line, and the chat starts a turn. An agent that polls handles the task at its next run.

## Sleep and wake

A name that will be idle for a long time can sleep. `cq sleep --as A B` writes a sleep record for `B`, and `cq wake --as A B` removes it. Messages to a sleeping name still arrive, and `cq send` warns the sender. See [Sleep and wake](PROTOCOL.md#sleep-and-wake) and [Commands](COMMANDS.md#sleep-and-wake).

What sleep stops depends on the wake route:

- **A watcher** (`--to-inbox`, `--to-codex`, Monitor mode) sees the record, prints `CQ-SLEEP by=... reason="..."` and exits 7. After that, no watcher wakes the session until someone runs `cq wake` and starts its watcher again.
- **Polling** is not stopped by `cq sleep`. A scheduled run still starts and still costs a model turn. `cq list` still lists the `UNCLAIMED` messages, after a first line `ASLEEP by=... reason="..."`. So pause the agent's own scheduler as well, and resume it after `cq wake`. The polling rules must end the run with no other work when `cq list` prints an `ASLEEP` line.

To let a session sleep other sessions, add its name on its own line in `CQ_ROOT/coordinators`.

| Agent | How to sleep and wake |
| --- | --- |
| Claude Code | The `/cq-stop` and `/cq-start` skills. See [Sleep and wake](CLAUDE-CODE.md#sleep-and-wake). |
| Codex | No skills. Outside the chat, run `cq sleep --as NAME NAME`, and run `cq wake --as NAME NAME` before you start the watcher again. With the scheduled automation fallback, also pause the automation. See [Sleep and wake](CODEX.md#sleep-and-wake). |
| Other agents | No skills. Run `cq sleep` and `cq wake` as for Codex, and pause a polling scheduler yourself. See [Sleep and wake](OTHER-AGENTS.md#sleep-and-wake). |

## Configuration

All settings are environment variables. Set queue-wide values before you start the agent app, such as the Claude app or the Codex app (on Windows: Settings > System > About > Advanced system settings > Environment Variables > User variables).

| Variable | What it sets | Default |
| --- | --- | --- |
| `CQ_SESSION_ID` | Per-command or dedicated session-process membership key, before either Claude identity variable. Never set one global value shared by Claude sessions. Empty values fall back. | None |
| `CQ_ROOT` | The queue root. Must be an absolute path. | Windows: `%LOCALAPPDATA%\cq`. macOS: `~/Library/Application Support/cq`. Linux: `$XDG_DATA_HOME/cq` or `~/.local/share/cq`. |
| `CQ_RULES` | The rules file that the rejoin reminder tells a session to re-read. | None: the reminder names the rules in general terms. |
| `CQ_TZ` | The time zone of the local time in `CQ-NEW` lines (an IANA name such as `Europe/London`). | The system time zone. |
| `CQ_TZ_LABEL` | The label after that time, such as `UK`. | The zone's abbreviation, such as `GMT` or `CET`. |
| `CQ_OVERDUE_UNCLAIMED_MINUTES` | Minutes before an unclaimed task counts as overdue (see [Overdue tasks](COMMANDS.md#overdue-tasks)). Whole number, 1 or more. | 30 |
| `CQ_OVERDUE_CLAIMED_MINUTES` | Minutes before a claimed task with no `cq done` counts as overdue. Whole number, 1 or more. | 120 |

How each agent's session key is chosen is in [Claude Code setup](CLAUDE-CODE.md#session-identity) and [Codex setup](CODEX.md#session-identity).

The macOS default path contains a space, so quote it in shell commands: `ls "$HOME/Library/Application Support/cq"`.

To bake your own defaults into your build instead, create `.cargo/config.toml` in the repo (it is gitignored). A variable set at run time still wins. Cargo does not apply a value from `[env]` if the same variable is already set in the shell that runs the build.

```toml
[env]
CQ_DEFAULT_ROOT = 'D:\queues\cq'
CQ_RULES_PATH = 'D:/queues/SESSION-RULES.md'
CQ_DEFAULT_TZ = 'Europe/London'
CQ_DEFAULT_TZ_LABEL = 'UK'
```

## Upgrading while watchers run

On Windows, a running `cq.exe` is locked, but it can be renamed. To upgrade without stopping the sessions:

1. Build into a separate target folder, so the build does not touch the running binary: `CARGO_TARGET_DIR=../cq-build cargo build --release`.
2. Rename the live binary, for example `cq.exe` to `cq.old.exe` or `cq.exe.old-0156`. Running watchers keep using the old file. Name it `cq.exe.old-*` or `cq.old-*` (`cq.old.exe` also works): `--takeover` stops an old watcher only when its file is `cq.exe` or has one of these names.
3. Copy the new binary into its place.
4. Restart each watcher that keeps running the old binary:

   | Agent | What to restart |
   | --- | --- |
   | Claude Code | Each inbox-mode watcher, from its own session. A Monitor-mode watcher picks up the new binary at its next re-arm (within 30 minutes). See [After a cq upgrade](CLAUDE-CODE.md#after-a-cq-upgrade). |
   | Codex | Each `--to-codex` watcher, outside the chat, with `--takeover`. A `cq dispatch` worker also runs the old binary until you restart it. See [After a new chat or a restart](CODEX.md#after-a-new-chat-or-a-restart). |

5. Delete the old file once no process uses it.

On macOS, `cargo install --path .` replaces the binary while watchers run. Step 4 still applies.

Never run `cargo clean` on the folder your sessions run `cq` from: it deletes the live binary.
