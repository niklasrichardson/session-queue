# Setting up cq

How to install `cq`, configure it and upgrade it. Back to the [README](../README.md).

For Codex, use [Codex setup](CODEX.md). The steps below are for Claude Code. Shared queue configuration is listed below.

## Requirements

- Claude Code with the Monitor tool. Tested in the Claude desktop app's Code tab.
- Stable Rust (built and tested with 1.98.1).
- Windows 10 or 11 with Git for Windows (Claude Code runs Bash commands in Git Bash), or macOS.

macOS support is new. The code builds for macOS, but it has not yet been tested on a Mac. Please open an issue if something does not work.

## Install

1. Build and install the binary:

   ```bash
   cargo install --path .
   ```

   This puts `cq` (`cq.exe` on Windows) in `~/.cargo/bin`, which the Rust installer adds to your `PATH`. If you prefer `cargo build --release`, add `target/release` to your `PATH` instead.

2. Restart the Claude app, because sessions read `PATH` only when they start.

   Expected result: `cq --version` works in a new session's Bash tool.

3. Create the queue layout. Safe to run more than once:

   ```bash
   cq init
   ```

   The queue lives in a per-user folder by default (see [Configuration](#configuration)). Every session must use the same queue root.

4. Allow `cq` without permission prompts, so unattended sessions never stop to ask. Add `Bash(cq *)` to `permissions.allow` in `~/.claude/settings.json`. Call `cq` by its bare name everywhere so the rule matches.

5. If your sessions run sandboxed, add the queue root to the sandbox's writable paths.

6. Add the CLAUDE.md blocks from [`CLAUDE-snippets.md`](../CLAUDE-snippets.md): the user-level block to `~/.claude/CLAUDE.md`, and a "Queue scope" block to each repo that takes queue work.

7. Optional: add deny rules for actions that must never happen unattended, such as force-push or deleting outside the repo.

8. Add the SessionStart hook to `~/.claude/settings.json`, so a session that joined the queue rejoins after `/clear`, a compaction or a resume. Merge it into any existing `hooks` object:

   ```json
   "hooks": {
     "SessionStart": [{"matcher": "clear|compact|resume", "hooks": [{"type": "command", "command": "cq session-start", "timeout": 10}]}]
   }
   ```

   Expected result: after `/clear` in a session whose watcher ran, the new context starts with "You are in the cq session queue as NAME".

9. Start each session with the start prompt in `CLAUDE-snippets.md` (section 3), giving each one its own queue name.

## Configuration

All settings are environment variables. Set queue-wide values before you start the Claude app (on Windows: Settings > System > About > Advanced system settings > Environment Variables > User variables).

| Variable | What it sets | Default |
| --- | --- | --- |
| `CQ_SESSION_ID` | Per-command or dedicated session-process membership key, before either Claude identity variable. Never set one global value shared by Claude sessions. Empty values fall back. | None |
| `CQ_ROOT` | The queue root. Must be an absolute path. | Windows: `%LOCALAPPDATA%\cq`. macOS: `~/Library/Application Support/cq`. Linux: `$XDG_DATA_HOME/cq` or `~/.local/share/cq`. |
| `CQ_RULES` | The rules file that the rejoin reminder tells a session to re-read. | None: the reminder names the rules in general terms. |
| `CQ_TZ` | The time zone of the local time in `CQ-NEW` lines (an IANA name such as `Europe/London`). | The system time zone. |
| `CQ_TZ_LABEL` | The label after that time, such as `UK`. | The zone's abbreviation, such as `GMT` or `CET`. |

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

## Upgrading while watchers run

On Windows, a running `cq.exe` is locked, but it can be renamed. To upgrade without stopping the sessions:

1. Build into a separate target folder, so the build does not touch the running binary: `CARGO_TARGET_DIR=../cq-build cargo build --release`.
2. Rename the live binary, for example `cq.exe` to `cq.old.exe`. Running watchers keep using the old file.
3. Copy the new binary into its place. Each watcher picks it up at its next re-arm (within 30 minutes).
4. Delete the old file once no process uses it.

On macOS, `cargo install --path .` replaces the binary while watchers run; each watcher picks up the new one at its next re-arm.

Never run `cargo clean` on the folder your sessions run `cq` from: it deletes the live binary.
