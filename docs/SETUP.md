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
- Windows 10 or 11, or macOS. macOS is partly tested: what passed, what failed and what is not checked is in [Test status](TESTING.md#macos-29-october-2026) and [#1](https://github.com/niklasrichardson/session-queue/issues/1). Please open an issue if something does not work.
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

   The queue lives in a per-user folder by default (see [Configuration](#configuration)). Keep that default unless you need another drive ([why](#configuration)), or an agent's sandbox does not write it for real ([check](#check-that-agents-write-the-root-for-real)). Every session must use the same absolute queue root. If you set `CQ_ROOT`, use the same value for every `cq` command: in each session, for each watcher and for each sender.

## Connect each session

These facts are the same for every agent:

- Each session gets its own queue name, such as `lead` or `worker-a`. Never put a fixed name in a shared instruction file: several sessions can share one folder.
- The [queue rules block](QUEUE-RULES.md#1-queue-rules-block) is the same for every agent. Codex has a [Codex-only block](CODEX.md#give-the-chat-its-rules) that holds the same rules. Each repo that takes queue work also gets a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block).
- `cq claim`, `cq done` and `cq send` write files in the queue root, so an unattended session must be able to run them without an approval prompt.
- Each agent page also says how to recover after `/clear`, a new chat or a restart.

The rest differs per agent. Follow the page for your agent, then come back to [Send a test message](#send-a-test-message):

| Agent | Rules file | No prompts | Wake route | Page |
| --- | --- | --- | --- | --- |
| Claude Code | `~/.claude/CLAUDE.md`, with the Claude Code block. The queue name goes in the start prompt. | `Bash(cq *)` in `permissions.allow` | `cq watch --to-inbox`, in the session | [Claude Code setup](CLAUDE-CODE.md) |
| Codex | `~/.codex/AGENTS.md` or the repo's `AGENTS.md`, with the Codex-only block once per queue name | The queue root writable from the chat's sandbox. A setup where Codex never asks for approval is not tested yet. | `cq watch --to-codex`, outside the chat | [Codex setup](CODEX.md) |
| Other agents | The agent's own instruction file | Depends on the agent | A wake feature, a fresh run per task, or polling | [Other agents](OTHER-AGENTS.md) |

## Send a test message

Do this after your agent's page. From another session:

```bash
cq send --from lead --to worker-a --type task <<'EOF'
{"subject": "Queue test", "body": "Summarise README.md in one line. Do not edit files."}
EOF
```

The task asks only for a one-line summary, so it fits the sample scope ("summarise a file") on every agent page. If the receiving repo has no `README.md`, name another file.

Expected result: the receiving session starts a turn within seconds, claims the task and replies. A Claude Code session gets a message from `cq` with a `CQ-NEW` line. A Codex watcher prints one `queued a check` line, and the chat starts a turn. An agent that polls handles the task at its next run.

PowerShell has no heredoc. The PowerShell form of this send is in [A round trip in PowerShell](CODEX.md#a-round-trip-in-powershell).

## Sleep and wake

A name that will be idle for a long time can sleep. `cq sleep --as A B` writes a sleep record for `B`, and `cq wake --as A B` removes it. Messages to a sleeping name still arrive, and `cq send` warns the sender. See [Sleep and wake](PROTOCOL.md#sleep-and-wake) and [Commands](COMMANDS.md#sleep-and-wake).

What sleep stops depends on the wake route:

- **A watcher** (`--to-inbox`, `--to-codex`, `--to-agy`, Monitor mode) sees the record, prints `CQ-SLEEP by=... reason="..."` and exits 7. After that, no watcher wakes the session until someone runs `cq wake` and starts its watcher again.
- **Polling** is not stopped by `cq sleep`. A scheduled run still starts and still costs a model turn. `cq list` still lists the `UNCLAIMED` messages, after a first line `ASLEEP by=... reason="..."`. So pause the agent's own scheduler as well, and resume it after `cq wake`. The polling rules must end the run with no other work when `cq list` prints an `ASLEEP` line.

To let a session sleep other sessions, add its name on its own line in `CQ_ROOT/coordinators`.

| Agent | How to sleep and wake |
| --- | --- |
| Claude Code | The `/cq-stop` and `/cq-start` skills. See [Sleep and wake](CLAUDE-CODE.md#sleep-and-wake). |
| Codex | The same skills, as `$cq-stop` and `$cq-start` (not yet tested in Codex). Or, outside the chat, run `cq sleep --as NAME NAME`, and run `cq wake --as NAME NAME` before you start the watcher again. Either way, the watcher starts again outside the chat. With the scheduled automation fallback, also pause the automation. See [Sleep and wake](CODEX.md#sleep-and-wake). |
| Other agents | The same skills, if the agent reads Agent Skills (not tested). Otherwise run `cq sleep` and `cq wake` as for Codex. Pause a polling scheduler yourself. See [Sleep and wake](OTHER-AGENTS.md#sleep-and-wake). |

### Install the skills

The `cq-start` and `cq-stop` skills in [`skills/`](../skills/) follow the [Agent Skills](https://agentskills.io/specification) format, so one copy works in Claude Code and Codex. Each skill has shared steps, then steps for Claude Code, for Codex and for other agents. Copy both folders to the folder that your agent reads:

| Agent | Skills folder |
| --- | --- |
| Claude Code | `~/.claude/skills/<name>/` |
| Codex | `~/.agents/skills/<name>/` ([Codex skills guide](https://learn.chatgpt.com/docs/build-skills)). On Windows this is probably `C:\Users\<you>\.agents\skills`: the guide gives only `$HOME/.agents/skills`. |
| Other agents | The agent's own skills folder, if it reads Agent Skills |

From the cq repo, in Git Bash:

```bash
cp -r skills/cq-start skills/cq-stop ~/.claude/skills/
mkdir -p ~/.agents/skills && cp -r skills/cq-start skills/cq-stop ~/.agents/skills/
```

In PowerShell 7, for Codex:

```powershell
New-Item -ItemType Directory -Force "$HOME/.agents/skills" | Out-Null
Copy-Item -Recurse -Force skills/cq-start, skills/cq-stop "$HOME/.agents/skills/"
```

Copy them again after you update the repo. A session runs these skills without being asked, for example `cq-stop` on a `CQ-SLEEP` line, so cq does not make them manual-only. To make your installed copy manual-only anyway: in Claude Code, add `disable-model-invocation: true` to its frontmatter; in Codex, add `agents/openai.yaml` with `allow_implicit_invocation: false` under `policy` ([Codex skills guide](https://learn.chatgpt.com/docs/build-skills)). The session then no longer reacts to `CQ-SLEEP` by itself.

## Configuration

`cq` keeps all its data and settings in the queue root. Use the per-user default root: it is already outside every git repo, so `git clean`, a fresh clone, or an agent that works in a repo cannot touch live queue data. Set `CQ_ROOT` only if you need the queue on another drive.

| System | Default queue root |
| --- | --- |
| Windows | `%LOCALAPPDATA%\cq` |
| macOS | `~/Library/Application Support/cq` |
| Linux | `$XDG_DATA_HOME/cq`, or `~/.local/share/cq` |

The macOS default path contains a space, so quote it in shell commands: `ls "$HOME/Library/Application Support/cq"`.

Put your own cq files in the queue root too. `cq` and `cq-top` ignore files and folders in the root that they do not use.

```text
%LOCALAPPDATA%\cq\
  config.toml          settings (see below)
  SESSION-RULES.md     your own session rules, made from SESSION-RULES.example.md
  watchers.json        the watcher list for start-watchers.ps1 (see Watchers)
  inbox\, done\, ...   the queue itself, made by cq init
```

### Check that agents write the root for real

Some agent sandboxes do not write every folder for real. On Windows on 6 November 2026, Claude Code's Bash tool redirected writes under `%LOCALAPPDATA%` into a private copy. Later Bash commands in that session saw the files. PowerShell, other programs and other sessions did not. With the default root, the queue then split into separate copies: sessions that ran `cq` through the sandbox saw one queue, and watchers, Codex and `cq-top` saw another. Details are in [Test status](TESTING.md#sandbox-writes-to-the-default-root-on-windows-6-november-2026).

Before you use the default root, check each agent:

1. From the agent's own shell (the tool that it uses to run `cq`), create a test file in the root. For example, in Git Bash: `touch "$LOCALAPPDATA/cq/sandbox-check.txt"`.
2. From a normal terminal, check that the file is there. In PowerShell: `Test-Path "$env:LOCALAPPDATA\cq\sandbox-check.txt"`. Expected result: `True`.
3. Delete the test file.

If the result is `False`, the agent does not write the root for real. Use a root in a folder that every agent writes for real, for example under your projects folder, outside every git repo:

- Point every program at that root: set `CQ_ROOT`, or build `cq` with `CQ_DEFAULT_ROOT` (see [Optional: build-time defaults](#optional-build-time-defaults)).
- Prefer the build-time default if some programs might start without `CQ_ROOT`. A program that does not get `CQ_ROOT` uses the default root without a warning, so it uses a different queue.
- Keep `config.toml`, `watchers.json` and your other cq files in that root.
- To move a queue that is in use, see [Move an existing queue root](#move-an-existing-queue-root).

### config.toml

`config.toml` in the queue root holds settings that are the same for every session. The file is optional. Start from [`examples/config.example.toml`](../examples/config.example.toml).

```toml
rules = 'SESSION-RULES.md'
tz = 'Europe/London'
tz_label = 'UK'
```

| Key | What it sets | Default |
| --- | --- | --- |
| `rules` | The rules file that the rejoin reminder tells a session to re-read. A relative path is relative to the queue root. | None: the reminder names the rules in general terms. |
| `tz` | The time zone of the local time in `CQ-NEW` and list lines (an IANA name such as `Europe/London`). `cq-top` also starts "today" at midnight in this zone. | The system time zone. |
| `tz_label` | The label after that time, such as `UK`. | The zone's abbreviation, such as `GMT` or `CET`. |

A problem in the file never stops a command. `cq` and `cq-top` print one warning on stderr that names the file and each problem, then run as usual:

- An unknown key, such as `tz_lable`, is ignored. The other keys still apply.
- A value of the wrong type is ignored. A `tz` that is not a known time zone is ignored, so the next source applies (see [Which value cq uses](#which-value-cq-uses)).
- A file that is not valid TOML is ignored whole.

The hooks (`session-start`, `turn-end`, `idle-alert`) print the warning only with `--debug`. A running watcher keeps the settings it read at start, so restart watchers after you change the file.

### Environment variables

An environment variable wins over `config.toml`. Set queue-wide values before you start the agent app, such as the Claude app or the Codex app (on Windows: Settings > System > About > Advanced system settings > Environment Variables > User variables). A user environment variable is seen only by processes that start after you set it. Restart the agent app, and open a new terminal, after you set or change one.

| Variable | What it sets | Default |
| --- | --- | --- |
| `CQ_SESSION_ID` | Per-command or dedicated session-process membership key, before either Claude identity variable. Never set one global value shared by Claude sessions. Empty values fall back. | None |
| `CQ_ROOT` | The queue root. Must be an absolute path. `config.toml` cannot set it, because `cq` finds the file in the root. | The per-user default above. |
| `CQ_RULES` | As `rules` in `config.toml`. | `rules` in `config.toml` |
| `CQ_TZ` | As `tz` in `config.toml`. An unknown name is skipped. | `tz` in `config.toml` |
| `CQ_TZ_LABEL` | As `tz_label` in `config.toml`. | `tz_label` in `config.toml` |
| `CQ_OVERDUE_UNCLAIMED_MINUTES` | Minutes before an unclaimed task counts as overdue (see [Overdue tasks](COMMANDS.md#overdue-tasks)). Whole number, 1 or more. | 30 |
| `CQ_OVERDUE_CLAIMED_MINUTES` | Minutes before a claimed task with no `cq done` counts as overdue. Whole number, 1 or more. | 120 |

How each agent's session key is chosen is in [Claude Code setup](CLAUDE-CODE.md#session-identity) and [Codex setup](CODEX.md#session-identity).

### Which value cq uses

For `rules`, `tz` and `tz_label`, `cq` takes the first value that is set and not empty:

1. The environment variable: `CQ_RULES`, `CQ_TZ` or `CQ_TZ_LABEL`.
2. The key in `config.toml` in the queue root.
3. The build-time default, if your build has one (see below).
4. The built-in default in the table above.

For the queue root itself: `CQ_ROOT`, else the build's `CQ_DEFAULT_ROOT`, else the per-user default.

### Optional: build-time defaults

You do not need these: `config.toml` covers the same settings. To bake defaults into your own build instead, create `.cargo/config.toml` in the repo (it is gitignored). Any value set at run time, or in the root's `config.toml`, still wins. Cargo does not apply a value from `[env]` if the same variable is already set in the shell that runs the build.

```toml
[env]
CQ_DEFAULT_ROOT = 'E:\cq'
CQ_RULES_PATH = 'E:/cq/SESSION-RULES.md'
CQ_DEFAULT_TZ = 'Europe/London'
CQ_DEFAULT_TZ_LABEL = 'UK'
```

`CQ_DEFAULT_ROOT` changes where your build looks for the queue root. Scripts that do not run `cq`, such as [`start-watchers.ps1`](WATCHERS.md), do not know it.

### Move an existing queue root

To move a queue root into the default location, or to another drive:

1. Drain the queue. Ask every session to finish or reject each task it has claimed, and to handle every message in its inbox, replies included. Do this before step 2: under `PAUSE`, `cq done` cannot send its reply. The queue is drained when the old root's `inbox` folder holds no files:

   ```powershell
   $old = 'D:\old-cq-root'   # your old root
   (Get-ChildItem "$old\inbox" -Recurse -File -Force).Count   # expect 0
   ```

2. Create a file named `PAUSE` in the old root, so that `cq send` refuses new messages.
3. Stop every queue writer. `PAUSE` stops new messages only: sessions can still claim, finish and reject tasks, and the hooks still write their files. Stopping the watchers is not enough. Close every session that uses the queue (Claude Code, Codex, Antigravity CLI and any other agent), and stop every watcher and dispatcher. To check that no watcher runs for `NAME`, run `cq status --as NAME`: it exits 10 for an awake name, and 7 with `watcher=stopped` for a sleeping name.
4. Copy the old root's contents into the new root, which must be empty. Keep the old root until step 7.
5. Compare the file counts. They must be equal; if not, copy again before you go on:

   ```powershell
   $new = "$env:LOCALAPPDATA\cq"   # your new root
   (Get-ChildItem $old -Recurse -File -Force).Count
   (Get-ChildItem $new -Recurse -File -Force).Count
   ```

6. Remove `CQ_ROOT`, or set it to the new root. Restart the agent apps and open new terminals.
7. Delete `PAUSE` in the new root. Start the watchers again, and check each one with `cq status --as NAME`. When the new root works, delete the old one.

To run Codex and Antigravity CLI watchers from one window, see [Watchers](WATCHERS.md).

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
