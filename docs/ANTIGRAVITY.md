# Antigravity CLI (Gemini) setup

This page holds only what differs for Google Antigravity (`agy`). Before this page, do the [shared install steps](SETUP.md#install): install, `PATH` and `cq init`. After this page, [send a test message](SETUP.md#send-a-test-message). The Claude Code steps are in [Claude Code setup](CLAUDE-CODE.md), the Codex steps in [Codex setup](CODEX.md), and the steps for any other agent in [Other agents](OTHER-AGENTS.md).

Tested on Windows 11 with the Antigravity CLI (`agy`): a background `run_command` watcher script woke an idle session when a message arrived from a Claude Code session, and the session claimed the message, finished it with `cq done` and re-armed the watcher. Back to the [README](../README.md).

## Quick path for an Antigravity session

1. Finish the shared install steps in [Setup](SETUP.md#install). Use one absolute `CQ_ROOT` for every session.
2. Check that `cq --version` works in the session's `run_command` tool.
3. Add the queue rules to `~/.gemini/config/GEMINI.md` or the repository's `GEMINI.md` / `AGENTS.md`, and add a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block) in each repository that takes queue work ([Give the session its rules](#give-the-session-its-rules)).
4. Allow the session to run `cq` and write to the queue root without prompts ([Allow `cq` without prompts](#allow-cq-without-prompts)).
5. Save `cq-watch-once.ps1` in the session's scratch folder and start it as a background `run_command` task ([Wake a session with a background watcher task](#wake-a-session-with-a-background-watcher-task)).
6. Send NAME a test message, as in [Setup](SETUP.md#send-a-test-message), and check that the session starts a turn.

## Set up an Antigravity session

An Antigravity session needs four things: its rules, permission to run `cq` without prompts, a wake route, and a way to recover after a restart.

### Give the session its rules

Antigravity reads `GEMINI.md`, `AGENTS.md` and `.agents/rules/*.md` in the repository, and global rules in `~/.gemini/config/`.

1. Put the [queue rules block](QUEUE-RULES.md#1-queue-rules-block) and the Antigravity block below in `~/.gemini/config/GEMINI.md` (or in the repository's `GEMINI.md`).
2. Put a ["Queue scope" block](QUEUE-RULES.md#2-per-repo-scope-block) in each repository that takes queue work.
3. Give each session its queue name in its start prompt. Do not put a fixed queue name in a shared instruction file.

````markdown
## Antigravity queue watcher

- Keep one background `run_command` task (`IsDaemon: false`, `WaitMsBeforeAsync: 500`) running `cq-watch-once.ps1` from your conversation's `scratch/` folder, with your queue name and conversation ID.
- When the background watcher task completes, Antigravity resumes the turn with the task output:
  - On each `CQ-NEW` line, claim and finish the message as the queue rules say, then re-arm `cq-watch-once.ps1` in the background.
  - On `cq watch: exiting: --max-seconds 1790 reached` (exit 0 with no event lines), re-arm `cq-watch-once.ps1` in the background.
  - On `CQ-SLEEP` (exit 7), do not re-arm.
- On Windows, `run_command` runs PowerShell. Pipe JSON to `cq send` with a single-quoted here-string (`@' ... '@ | cq send ...`), or pass `--input FILE` from a UTF-8 file without a byte order mark. Never put message text on the command line.
````

If you start from [SESSION-RULES.example.md](SESSION-RULES.example.md), keep its "Antigravity CLI" part and delete the Claude Code and Codex parts.

### Allow `cq` without prompts

`cq claim`, `cq done` and `cq send` write files in the queue root. An unattended session must run them without stopping at an approval prompt.

- **In the session:** approve `cq` commands (or tell the session that it is authorised to run `cq` commands and access the queue root).
- **With a `PreToolUse` hook (not tested):** a `hooks.json` in `~/.gemini/config/` or `.agents/` can match `run_command` calls that invoke `cq` and return `{"decision": "allow"}`.

### Wake a session with a background watcher task

Antigravity automatically wakes an idle session when a background `run_command` task (`IsDaemon: false`) finishes. A plain `cq watch --as NAME` prints `CQ-NEW` lines to stdout and stays running until `--max-seconds` (1790 seconds), so stdout alone does not wake the session until the 30-minute timer ends.

To wake the session as soon as a message arrives while keeping `cq watch` and its heartbeat live:

1. Have the session write `cq-watch-once.ps1` into its conversation scratch folder (`<appDataDir>/brain/<conversation-id>/scratch/cq-watch-once.ps1`):

   ```powershell
   param(
       [Parameter(Mandatory = $true)][string]$Name,
       [Parameter(Mandatory = $true)][string]$SessionId,
       [string]$CqRoot = "",
       [int]$MaxSeconds = 1790
   )

   $ErrorActionPreference = "Stop"
   if ($CqRoot) { $env:CQ_ROOT = $CqRoot }
   $env:CQ_SESSION_ID = $SessionId

   $cqPath = (Get-Command cq -ErrorAction Stop).Source

   $psi = [System.Diagnostics.ProcessStartInfo]::new()
   $psi.FileName = $cqPath
   $psi.Arguments = "watch --as $Name --takeover --max-seconds $MaxSeconds"
   $psi.UseShellExecute = $false
   $psi.RedirectStandardOutput = $true
   $psi.RedirectStandardError = $true
   $psi.CreateNoWindow = $true
   $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
   $psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8

   $proc = [System.Diagnostics.Process]::Start($psi)
   $stderrTask = $proc.StandardError.ReadToEndAsync()
   $readTask = $proc.StandardOutput.ReadLineAsync()

   $lines = [System.Collections.Generic.List[string]]::new()

   while (-not $proc.HasExited -and -not $readTask.IsCompleted) {
       Start-Sleep -Milliseconds 200
   }

   if ($readTask.IsCompleted -and $null -ne $readTask.Result) {
       $lines.Add($readTask.Result)
       $deadline = [DateTime]::UtcNow.AddMilliseconds(1500)
       $readTask = $proc.StandardOutput.ReadLineAsync()
       while (-not $proc.HasExited -and [DateTime]::UtcNow -lt $deadline) {
           if ($readTask.IsCompleted) {
               if ($null -ne $readTask.Result) {
                   $lines.Add($readTask.Result)
                   $readTask = $proc.StandardOutput.ReadLineAsync()
               } else {
                   break
               }
           } else {
               Start-Sleep -Milliseconds 100
           }
       }
       if (-not $proc.HasExited) {
           try {
               $proc.Kill()
               $proc.WaitForExit(2000) | Out-Null
           } catch {}
           foreach ($line in $lines) {
               [Console]::Out.WriteLine($line)
           }
           exit 0
       }
   }

   $proc.WaitForExit()
   while ($readTask.IsCompleted -and $null -ne $readTask.Result) {
       $lines.Add($readTask.Result)
       $readTask = $proc.StandardOutput.ReadLineAsync()
   }
   foreach ($line in $lines) {
       [Console]::Out.WriteLine($line)
   }
   if ($stderrTask.Wait(1000) -and $stderrTask.Result) {
       [Console]::Error.Write($stderrTask.Result)
   }
   exit $proc.ExitCode
   ```

2. The session starts the script with `run_command` (`IsDaemon: false`, `WaitMsBeforeAsync: 500`):

   ```powershell
   powershell -NoProfile -File "<scratch>/cq-watch-once.ps1" -Name worker-a -SessionId "<conversation-id>"
   ```

   Pass `-CqRoot "D:/queues/cq"` as well if `CQ_ROOT` is not baked into the `cq` binary.

How it works:

- **While idle.** `cq.exe watch` holds the watcher lock, writes `members/<conversation-id>` with mode `monitor`, and updates `heartbeat/NAME` every 30 seconds. No model turns run until a message arrives or 1790 seconds pass.
- **When a message arrives.** `cq.exe watch` flushes a `CQ-NEW` line to stdout. The script reads that line, waits 1.5 seconds for any more lines in the same batch, stops `cq.exe`, prints the lines and exits 0. Antigravity then wakes the session with the task output. The session claims and finishes each message, and starts `cq-watch-once.ps1` again in the background.
- **When the name is put to sleep.** `cq.exe watch` prints `CQ-SLEEP` and exits 7 within 250 ms. The script exits 7, waking the session, which does not re-arm.

Live test on Windows 11 (2 Nov 2026): a `cq-watch-once.ps1` background task ran a full 1790-second idle cycle and re-armed, then woke the session within 1 second when `photo-lead` sent an `info` message (`20261102T122753Z-photo-lead-dc20`). The session claimed the message, finished it with `cq done`, and re-armed the watcher.

### Fallback: scheduled polling with the `schedule` tool

If a background watcher script cannot be used, the session can call its built-in `schedule` tool with `CronExpression` (for example `*/10 * * * *`) and a prompt telling it to run `cq list --as NAME` and handle any `UNCLAIMED` messages.

- **Cost.** Every cron trigger starts a model turn, even when the inbox is empty.
- **No heartbeat.** `cq list` does not write a watcher heartbeat or a `members/` record, so [cq-top](CQ-TOP.md) does not show the session as an active watcher.
- **Sleep.** `cq sleep` does not stop the schedule. Cancel the schedule task with `manage_task` (`Action: "kill"`), and make the polling prompt end with no other work when `cq list` prints an `ASLEEP` line.

### Sending and replying in PowerShell

On Windows, Antigravity's `run_command` tool runs PowerShell, which has no heredoc. Pipe a single-quoted here-string (`@' ... '@`) to `cq send` or `cq done`, or pass `--input FILE` with a UTF-8 file written without a byte order mark. See [A round trip in PowerShell](CODEX.md#a-round-trip-in-powershell).

### After a restart

When an Antigravity session restarts or starts a new conversation:

1. Start `cq-watch-once.ps1` in the background with the session's queue name and new conversation ID (`--takeover` replaces any older watcher record).
2. Run `cq list --as NAME` and handle any `UNCLAIMED` messages, oldest first.

Do not install `cq session-start` as an Antigravity `SessionStart` hook: its output is plain text written for Claude Code's workflow, whereas Antigravity hooks expect JSON (`{"injectSteps": ...}`) on stdout.

### Sleep and wake

The `cq-start` and `cq-stop` skills follow the Agent Skills format. Copy `skills/cq-start` and `skills/cq-stop` to `~/.gemini/config/skills/` (global) or `.agents/skills/` (workspace), as in [Install the skills](SETUP.md#install-the-skills). They have not been tested in Antigravity yet.

Without the skills:

- To sleep: kill the background watcher task with `manage_task` (`Action: "kill"`), and run `cq sleep --as NAME NAME`.
- To wake: run `cq wake --as NAME NAME`, then start `cq-watch-once.ps1` in the background again.

## Session identity

Pass the Antigravity conversation ID as `-SessionId` to `cq-watch-once.ps1`, which sets `CQ_SESSION_ID` for that watcher process only. `cq members` then lists `<conversation-id> -> NAME`. Never set `CQ_SESSION_ID` in global user or system environment variables: every session on the machine would share one key and overwrite the others' member records.
