# Run watchers outside an agent session

A `--to-codex` or `--to-agy` watcher runs outside the chat or conversation that it wakes. [`examples/start-watchers.ps1`](../examples/start-watchers.ps1) starts all of them from one PowerShell window, so you do not need an agent session to keep them running.

Tested on Windows 11 with PowerShell 7.6 and a fake `cq` only: the config checks, the output, the exit lines, and six watchers running at once. It has not been tested with real watchers yet.

## What the script does

- It reads a JSON config and starts each enabled watcher with `--takeover`, as a thread job. `--takeover` replaces an older watcher of the same name.
- It prints each watcher's output in the same window, with the watcher's name at the start of each line.
- When a watcher exits, it prints `[NAME] exited with code N`. The codes are in [Commands](COMMANDS.md#exit-codes).
- It does not restart a watcher that exits. A restart loop with `--takeover` would fight another watcher of that name. Run the script again instead.
- Close the window, or press Ctrl+C, to stop every watcher.

## Before you start

1. Install PowerShell 7 (`pwsh`). The script does not run in Windows PowerShell 5.1.
2. Use the default queue root, or set `CQ_ROOT` as in [Configuration](SETUP.md#configuration). The watchers get the environment of the window.
3. Set up each chat or conversation first: [Wake a Codex chat](CODEX.md#wake-a-codex-chat-with-cq-watch---to-codex) and [Antigravity CLI](OTHER-AGENTS.md#antigravity-cli). You need its queue name, and the Codex thread ID or the `agy` conversation ID.

## Set up the script

1. Copy `examples/watchers.example.json` to `watchers.json` in the queue root, for example `%LOCALAPPDATA%\cq\watchers.json` on Windows.
2. Replace the example names and IDs with your own.
3. Copy `examples/start-watchers.ps1` to any folder outside the repo, for example the queue root.
4. Run the script once from a terminal, to see any config error:

   ```powershell
   pwsh -NoLogo -File "$env:LOCALAPPDATA\cq\start-watchers.ps1"
   ```

   Expected result: `Started N watcher(s).`, then each watcher's start line.
5. In another terminal, check each watcher: `cq status --as NAME`. Expected result: `running`.

The script finds its config in this order: `-Config PATH`, else the `CQ_WATCHERS` environment variable, else `watchers.json` in the queue root (`CQ_ROOT`, else the per-user default). A build with `CQ_DEFAULT_ROOT` uses a root that the script does not know: give it `-Config` or `CQ_WATCHERS`. Add `-NoWait` to close the window at the end without waiting for Enter.

## The config

```json
{
  "cq": "cq",
  "watchers": [
    { "name": "worker-a-codex", "mode": "codex", "id": "CODEX_THREAD_ID", "enabled": true }
  ]
}
```

| Field | What it sets | Default |
| --- | --- | --- |
| `cq` | The `cq` executable. Use a full path if `cq` is not on `PATH`. | `cq` on `PATH` |
| `watchers` | A list of watchers. | None: required |
| `name` | The queue name, for `--as`. | None: required |
| `mode` | `codex` for `--to-codex`, or `agy` for `--to-agy`. Inbox mode needs a Claude Code session, so it is not allowed here. | None: required |
| `id` | The Codex thread ID or the `agy` conversation ID. | None: required |
| `prompt` | The fixed wake prompt, for `--codex-prompt` or `--agy-prompt`. Never put text from a queue message in it. | The watcher's default prompt |
| `bin` | The Codex or `agy` executable, for `--codex-bin` or `--agy-bin`. | `codex` or `agy` on `PATH` |
| `enabled` | `false` skips the watcher. | `true` |

The script stops with a clear message, and starts nothing, if the config is missing or not valid JSON, `cq` cannot be found, a field is unknown or has a wrong value, two enabled watchers have the same name, or no watcher is enabled.

## Start the watchers from a desktop shortcut

1. Right-click the desktop and select **New** > **Shortcut**.
2. For the location, enter:

   ```text
   pwsh -NoLogo -File "<folder>\start-watchers.ps1"
   ```

   Replace `<folder>` with the folder that holds your copy of the script.
3. Name the shortcut, for example `cq watchers`.

The window stays open while the watchers run. At the end, it waits for Enter, so you can read the last lines.

## Limits

- The watchers stop when the window closes, when you sign out and when the PC restarts. Start the script again after a restart. Issue #63 tracks an app that could keep them running.
- A watcher that exits stays stopped until you run the script again. Check with `cq status --as NAME`, or watch the heartbeats in [cq-top](CQ-TOP.md).
- A `--to-codex` wake starts a turn only while the chat is loaded in the Codex app. See [If a wake does not start a turn](CODEX.md#if-a-wake-does-not-start-a-turn).
