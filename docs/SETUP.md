# Setting up cq

How to install `cq`, connect an agent, configure it and upgrade it. Back to the [README](../README.md).

This page holds the steps that are the same for every agent. Where a step differs per agent, it names the agent and links to that agent's page:

| Agent | Page | Tested |
| --- | --- | --- |
| Claude Code | [Claude Code setup](CLAUDE-CODE.md) | Live, in the Claude desktop app's Code tab on Windows 11 |
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

4. Allow `cq` without prompts, so unattended sessions never stop to ask. `cq claim`, `cq done` and `cq send` write files in the queue root.

   | Agent | What to do |
   | --- | --- |
   | Claude Code | Add `Bash(cq *)` to `permissions.allow` in `~/.claude/settings.json`. See [Allow `cq` without prompts](CLAUDE-CODE.md#allow-cq-without-prompts). |
   | Codex | Make the queue root writable from the chat's sandbox. A setup where Codex never asks for approval is not tested yet. See [Allow `cq` without prompts](CODEX.md#allow-cq-without-prompts). |
   | Other agents | See [Allow `cq` without prompts](OTHER-AGENTS.md#allow-cq-without-prompts). |

5. Add the rules. The [queue rules block](QUEUE-RULES.md#1-queue-rules-block) is the same for every agent. Add a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block) to each repo that takes queue work. Where they go:

   | Agent | Instruction file |
   | --- | --- |
   | Claude Code | `~/.claude/CLAUDE.md`, with the Claude Code block from [`CLAUDE-snippets.md`](../CLAUDE-snippets.md#1-user-level-block). See [Give the session its rules](CLAUDE-CODE.md#give-the-session-its-rules). |
   | Codex | `~/.codex/AGENTS.md` or the repo's `AGENTS.md`, with one section per queue name. See [Give the chat its rules](CODEX.md#give-the-chat-its-rules). |
   | Other agents | The agent's own instruction file. See [Give the agent its rules](OTHER-AGENTS.md#give-the-agent-its-rules). |

   Choose one queue name per session, such as `lead` or `worker-a`. Never put a fixed name in a shared instruction file: several sessions can share one folder. Give a Claude Code session its name in its start prompt. Give a Codex chat a section named after its queue name in `AGENTS.md`.

6. Start the watcher that wakes the session. Each agent has its own wake route:

   | Agent | Watcher | Where it runs |
   | --- | --- | --- |
   | Claude Code | `cq watch --as NAME --to-inbox 2>&1` posts into the session's own inbox. See [Wake a session](CLAUDE-CODE.md#wake-a-session-with-cq-watch---to-inbox). | In the session, as a background Bash command. |
   | Codex | `cq watch --as NAME --to-codex THREAD_ID` runs `codex queue` with a fixed prompt. See [Wake a Codex chat](CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex). | Outside the chat, in the background. |
   | Other agents | A wake feature, a fresh run per task, or polling. See [Wake the agent](OTHER-AGENTS.md#wake-the-agent). | Depends on the route. |

7. Send a test message from another session:

   ```bash
   cq send --from lead --to worker-a --type task <<'EOF'
   {"subject": "Run the tests", "body": "Run cargo test and report failures."}
   EOF
   ```

   Expected result: the receiving session starts a turn within seconds, claims the task and replies. A Claude Code session gets a message from `cq` with a `CQ-NEW` line. A Codex watcher prints one `queued a check` line, and the chat starts a turn.

8. Plan for recovery after `/clear`, a new chat or a restart. This differs per agent: see [Claude Code](CLAUDE-CODE.md#after-clear-or-a-restart), [Codex](CODEX.md#after-a-new-chat-or-a-restart) or [Other agents](OTHER-AGENTS.md#after-a-restart).

## Sleep and wake

A name that will be idle for a long time can sleep, so that it takes no turns. `cq sleep --as A B` puts `B` to sleep, and its watcher stops with exit 7. Messages to a sleeping name still arrive, and `cq send` warns the sender. `cq wake --as A B` wakes it. See [Sleep and wake](PROTOCOL.md#sleep-and-wake) and [Commands](COMMANDS.md#sleep-and-wake).

To let a session sleep other sessions, add its name on its own line in `CQ_ROOT/coordinators`.

| Agent | How to sleep and wake |
| --- | --- |
| Claude Code | The `/cq-stop` and `/cq-start` skills. See [Sleep and wake](CLAUDE-CODE.md#sleep-and-wake). |
| Codex | No skills. Outside the chat, run `cq sleep --as NAME NAME`, and run `cq wake --as NAME NAME` before you start the watcher again. See [Sleep and wake](CODEX.md#sleep-and-wake). |
| Other agents | No skills. Run `cq sleep` and `cq wake` as for Codex. See [Sleep and wake](OTHER-AGENTS.md#sleep-and-wake). |

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
