# cq-top

`cq-top` is a live terminal view of a queue. It shows which watchers are alive, which messages are waiting or being worked on, and what finished recently. Back to the [README](../README.md).

`cq-top` only reads the queue root. It never claims, sends, moves or deletes a message, and it never writes to the queue root. It does not show message bodies until you press `b`. The [Sessions screen](#sessions-screen) also reads Claude Code transcripts, the process list and git, all read-only and with no network calls.

It is a separate program in `tools/cq-top`. It uses the `cq` library to find the queue root and its folders, so it follows the same `CQ_ROOT` and default root as `cq`.

## Install

You need stable Rust. From the repo root:

```bash
cargo install --path tools/cq-top
```

To build without replacing a `cq-top` that is already installed, use a separate target folder: `CARGO_TARGET_DIR=../cq-build cargo build --release --manifest-path tools/cq-top/Cargo.toml`.

## Run

```bash
cq-top
```

The queue root is `CQ_ROOT`, or the per-user default listed in [Setup](SETUP.md#configuration). If the queue root is missing or not set up, `cq-top` prints the reason and exits with code 70.

| Key | What it does |
| --- | --- |
| `Tab`, `1`, `2`, `3`, `4` | Switch between the Overview, Flow, Rejected and Sessions screens. |
| `t` | Use the next theme: classic, amber, neon. |
| `b` | Show or hide message bodies. They are hidden at start. |
| `q`, `Esc`, `Ctrl-C` | Quit. |

`cq-top` remembers the last screen and theme in a one-line file, such as `overview classic`. The file is `cq-top/state` in `%APPDATA%` on Windows, and in `$XDG_CONFIG_HOME` or `~/.config` on other systems. Without a saved choice, `cq-top` starts on Overview with the classic theme. If no config folder is known, nothing is saved.

## Export

`cq-top` can take one snapshot, write it as HTML and exit, without starting the live view:

```bash
cq-top --html dashboard.html        # a dashboard page
cq-top --html-screens shots         # shots/overview.html, flow.html, rejected.html and sessions.html
```

- `--html PATH` writes one self-contained page: watchers, sessions, counts, busy bars, turnaround, messages per 5-minute slice over the last hour, who talks to whom, in flight messages, the 20 newest rejected and dead-lettered messages with their reasons, and recent messages. It has no scripts and loads nothing from the network. It follows the system light or dark setting. Times are UK local time.
- `--html-screens DIR` draws the four screens at 140 x 42 cells, in the saved theme, with one 9 x 18 pixel box per cell. A browser can turn each page into a picture, for example `msedge --headless=new --screenshot=overview.png --window-size=1260,756 file:///.../overview.html`.
- Both options can be given together. They share one snapshot.
- An export never includes message bodies. Every string from the queue is HTML-escaped.
- `cq-top` refuses a path that is the queue root or inside it, and then writes nothing (exit 70). A bad argument exits with 64.
- The output names sessions and shows message subjects, session titles and spend estimates. Keep it out of public places.
- An export scans sessions once, in the foreground, so it takes a few seconds.

## Overview screen

- **Watchers.** One row per name: heartbeat age, [mode](#watcher-mode), watcher process ID, Claude Code session, and the number of queued, claimed, done, rejected and dead-lettered messages. A heartbeat older than 90 seconds shows as `STALE`. A name that is asleep (`sleep/<name>` exists) shows `asleep` instead, and gets a row even with no heartbeat.
- **In flight.** Queued and claimed messages, oldest first. A message that stays here for a long time is probably stuck.
- **Recent.** The 40 newest finished, rejected and dead-lettered messages.

The state glyphs are `◌` queued, `◐` claimed, `●` done, `✕` rejected and `!` dead-lettered.

## Flow screen

- **Messages sent, last hour.** A sparkline of messages over time. The oldest is on the left and the newest is on the right.
- **Who talks to whom.** The number of messages for each sender and receiver.
- **Turnaround, task to reply.** For each worker, the median time from the task being sent to its reply being sent.
- **Busy now.** Claimed plus queued messages for each worker. The bar is full at 4. A sleeping worker shows `asleep`.

## Rejected screen

- **Rejected and dead-lettered.** The newest messages in `rejected/` and `dlq/`, newest first, with the reason from each `<id>.reason.json` file. The title shows how many are listed and how many there are in all. Up to 40 are read; the screen lists as many as fit.
- A message that cannot be read, which is often why it was dead-lettered, still gets a row. Its time and sender come from its file name and its receiver from its folder.
- Message bodies are never shown on this screen, even with `b`.

## Sessions screen

One row per Claude Code session on this computer, with what it costs today and whether it is working.

- **CPU and Memory.** Two boxes at the top. Each has figures on the left and a graph on the right. The graph shows the whole computer in one colour and Claude's share of it in a second colour, both 0 to 100 %.
  - CPU figures: **Total** (0 to 100 % across all cores), **Claude** (all Claude processes together, as a share of the whole computer, so it fits the same graph), **Busy cores** (cores at 50 % or more of that core) and **Cores**. The table's cpu% column stays in % of one core.
  - Memory figures: **Used** of total, **Available**, **Swap** used of total, and **Claude** (all Claude processes together). GB here are GiB, as Task Manager and `top` count them.
  - The graph keeps one sample per scan, the newest 120 (6 minutes), and fills from the right. The history exists only in the live view. An export takes one snapshot, so it draws the current level as one bar, Claude's share first, with the note "the graph builds in the live view". The `--html` dashboard gives the same figures as text.
  - A box too narrow for its graph shows the figures only.
- **Sessions.** One row per session, sorted working, then recent, then idle. Within each group, the newest activity is first.

| Column | What it shows |
| --- | --- |
| session | The name of the cq watcher running in the session's folder. Else the session's custom title, without a leading `RC: `. Else the folder's last part, or `<repo>/<worktree>` for a worktree under `.claude/worktrees`. |
| state | `● working`, `◐ recent` or `○ idle`. `▲ idle` (warning colour) is an idle session with cq messages queued or claimed. |
| model | The model of the newest reply, without `claude-` and the date, for example `opus-5-5`. |
| context | Tokens in the newest reply's prompt: input plus cache writes plus cache reads. |
| tokens | Tokens since local midnight: input, cache writes, cache reads and output, for the main transcript and its subagents. |
| est.cost | An estimate of those tokens at API list prices. It is not your plan's bill. `?` means a model has no price in the table. |
| cpu% | CPU of the session's process and its child processes. 100 is one core. |
| mem | Memory of the same processes. |
| sub | Subagent transcripts written in the last 10 minutes. |
| cq, wait, claim | The cq watcher's heartbeat age (`off` without a heartbeat, `STALE` after 90 seconds, `asleep` when its name is asleep, `-` when no watcher runs in the folder), and its queued and claimed messages. |
| mode | The cq watcher's [mode](#watcher-mode): `inbox`, or `mon` for monitor. `-` when it is not known. |
| last change | The newest of the last commit on the folder's own `HEAD` and the newest file that `git status` lists in that worktree. Sessions in different worktrees of one repo show their own work. |
| age | Time since the transcript, or a subagent transcript, was last written. |

A narrow terminal drops the least important columns first: mem, sub, claim, mode, last change, est.cost and tokens.

### State rule

- **Working:** a transcript (main or subagent) was written in the last 2 minutes, or the processes use at least 5 % of one core.
- **Recent:** written in the last 45 minutes.
- **Idle:** older.

The rule is `sessions::state`, and the warning is `sessions::warn`.

### How sessions are found and matched

- Transcripts are in `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects` when that is set. On Windows, `~` is `%USERPROFILE%`.
- Claude Code names each project folder after the session's working folder, with every character that is not an ASCII letter or digit replaced by `-`. A session is the newest transcript in a project folder. Its subagents are in `<session id>/subagents/`.
- The real working folder comes from the first `cwd` in the transcript whose encoding matches the folder name. Nothing else is read from that line.
- A folder is shown when a Claude process runs in it, or when its transcript was written in the last 24 hours.
- A Claude process is a process named `claude`. Helper processes with a `--type=` argument count in the Claude totals but are not sessions. Each other process is matched to a folder by encoding its working folder.
- A cq watcher is a `cq` process. Its name comes from `cq watch --as NAME` in its arguments, or else from its `heartbeat/NAME.pid` file in the queue root. It belongs to the nearest session process among its parents, up to 10 steps up, because a session often runs `cq watch` from another folder. A watcher with no session above it belongs to its own working folder.
- An inbox watcher that the Bash tool starts in the background matches the same way. On Windows it runs under three `bash` processes below the session's `claude` process (checked on 28 October 2026).
- A sleeping name has no `cq` process. Its row is the one that last showed that name while `cq-top` ran, or else the folder in line 3 of its `members/` record (the watcher's working folder). A name that matches neither shows only on the Overview screen.

### Tokens and cost

- Each reply is counted once, by its message ID, or by its request ID when there is no message ID. Claude Code writes one reply on several lines.
- `<synthetic>` replies are skipped.
- Today starts at local midnight. Times in transcripts are UTC.
- Transcripts are read incrementally: each scan reads only the lines added since the last one.
- Prices are in `PRICES` in `tools/cq-top/src/sessions.rs`. They come from <https://platform.claude.com/docs/en/about-claude/pricing>, read on 28 October 2026. Cache writes use the 5-minute or 1-hour price as the transcript records them. Haiku 5.5 costs more for a prompt over 100k tokens, so the cost is worked out reply by reply. Fast mode, batch and regional prices are not applied.

### Background work

- A background thread scans every 3 seconds. A key press never waits for a scan. The first scan reads today's transcripts and takes a few seconds. After that, a scan took 90 to 410 ms on the test computer (12 sessions, 32 cores). Reading the process list takes about 80 ms of that.
- Another background thread runs `git` at most once every 60 seconds for each folder, with `GIT_OPTIONAL_LOCKS=0` so that `git status` does not write the index.

### Privacy

The screen and the exports never show message text. They show only counts, times, model names and session titles. `cq-top` uses only the `type`, `cwd`, `customTitle`, `timestamp`, message and request IDs, model and token counts from each transcript line.

### Without process data

If the process list cannot be read, one box says so in place of the CPU and Memory boxes. CPU and memory show `-`, no cq watcher is matched (so the cq columns show `-`), and the state uses file activity only. A process whose working folder cannot be read, for example one owned by another user, is not matched to a session.

### Platforms

Process data and the CPU and Memory figures come from the `sysinfo` crate, with the same code on every system. The screen is tested on Windows. It is expected to work on macOS and Linux, but it is not tested there yet: see issue #17.

## Watcher mode

A watcher runs in one of two modes (see [Commands](COMMANDS.md#watch-into-the-inbox-experimental)):

- **monitor:** `cq watch` under a Claude Code Monitor. It stops after 1790 seconds and the session re-arms it, so it gets a new process ID about every 30 minutes.
- **inbox:** `cq watch --to-inbox`, a background process that posts into the session's inbox. It has no time limit and keeps one process ID until it stops. It survives `/clear`.

`cq-top` takes the mode from `--to-inbox` in the watcher process's arguments while it can see the process. Otherwise it reads line 4 of the name's `members/` record. A record without line 4 (from an older `cq`) means monitor. A name with neither shows `-`.

Nothing else in `cq-top` depends on the mode. Liveness is the heartbeat age in both modes, and `cq-top` shows only the current process ID, so it counts no restarts.

## Themes

| Theme | Look |
| --- | --- |
| `classic` | Your terminal background, one colour for each meaning, single-line borders. |
| `amber` | One amber colour on near-black, double borders, coarse bars. Bold, underline and inverse text carry the meaning. |
| `neon` | Truecolor on dark navy, rounded borders, fine bars. |

A theme is one entry in `THEMES` in `tools/cq-top/src/draw.rs`.

## Limits

- A watcher counts as alive when its heartbeat is less than 90 seconds old. The process ID is shown, but `cq-top` does not check that the process exists.
- `cq-top` never opens a watcher's lock file. Opening it creates the file, and a probe at the wrong moment could stop a starting watcher from starting.
- `cq watch` rewrites the heartbeat file in place, so `cq-top` can read it half written. It then keeps the last good value instead of showing the watcher as stale.
- A message that moves between folders while `cq-top` reads them can be counted twice, or missed, for one refresh. The next refresh is correct.
- The Flow screen counts a queued or claimed message in "last hour" only if it was sent in that hour. An older message that is stuck still shows on the Overview screen.
- The Recent list reads only the 40 newest files on each refresh, and the Rejected screen reads only the 40 newest rejected and dead-lettered files.
- Turnaround includes the time the worker takes to write its reply.
- An `info` message that nobody claims counts as work in the Busy bar.
- `cq` keeps only the latest heartbeat, so `cq-top` shows no heartbeat history.
- The Sessions screen shows only the newest transcript in each project folder. Two sessions in one folder show as one row, and its CPU and memory are those of all Claude processes in that folder.
- The folder encoding loses information: `a-b` and `a.b` encode the same. Claude Code may shorten a very long folder name, and `cq-top` then cannot match its process.
- Session cost is an estimate from list prices. It does not know about plan limits, discounts or fast mode.
- At 140 columns, the header line is too long for all four tabs and the key help, so its end is cut.

## Dependencies

`cq-top` uses `ratatui` for the terminal, `serde` and `serde_json` for heartbeats and transcripts, `jiff` for local midnight, and `sysinfo` (system feature only) for process CPU, memory and working folders.

## Build and test

`tools/cq-top` is a member of the repo's Cargo workspace, and a default member. The four required checks at the repo root build and test it with `cq`:

```bash
cargo build
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

To check only `cq-top`, add `-p cq-top` to each command. The tests use a temporary queue root and never touch a real one.
