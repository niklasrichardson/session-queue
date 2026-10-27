# cq-top

`cq-top` is a live terminal view of a queue. It shows which watchers are alive, which messages are waiting or being worked on, and what finished recently. Back to the [README](../README.md).

`cq-top` only reads the queue root. It never claims, sends, moves or deletes a message, and it never writes to the queue root. It does not show message bodies until you press `b`.

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
| `Tab`, `1`, `2`, `3` | Switch between the Overview, Flow and Rejected screens. |
| `t` | Use the next theme: classic, amber, neon. |
| `b` | Show or hide message bodies. They are hidden at start. |
| `q`, `Esc`, `Ctrl-C` | Quit. |

`cq-top` remembers the last screen and theme in a one-line file, such as `overview classic`. The file is `cq-top/state` in `%APPDATA%` on Windows, and in `$XDG_CONFIG_HOME` or `~/.config` on other systems. Without a saved choice, `cq-top` starts on Overview with the classic theme. If no config folder is known, nothing is saved.

## Export

`cq-top` can take one snapshot, write it as HTML and exit, without starting the live view:

```bash
cq-top --html dashboard.html        # a dashboard page
cq-top --html-screens shots         # shots/overview.html, flow.html and rejected.html
```

- `--html PATH` writes one self-contained page: watchers, counts, busy bars, turnaround, messages per 5-minute slice over the last hour, who talks to whom, in flight messages, the 20 newest rejected and dead-lettered messages with their reasons, and recent messages. It has no scripts and loads nothing from the network. It follows the system light or dark setting. Times are UK local time.
- `--html-screens DIR` draws the three screens at 140 x 42 cells, in the saved theme, with one 9 x 18 pixel box per cell. A browser can turn each page into a picture, for example `msedge --headless=new --screenshot=overview.png --window-size=1260,756 file:///.../overview.html`.
- Both options can be given together. They share one snapshot.
- An export never includes message bodies. Every string from the queue is HTML-escaped.
- `cq-top` refuses a path that is the queue root or inside it, and then writes nothing (exit 70). A bad argument exits with 64.
- The output names sessions and shows message subjects. Keep it out of public places.

## Overview screen

- **Watchers.** One row per name: heartbeat age, watcher process ID, Claude Code session, and the number of queued, claimed, done, rejected and dead-lettered messages. A heartbeat older than 90 seconds shows as `STALE`.
- **In flight.** Queued and claimed messages, oldest first. A message that stays here for a long time is probably stuck.
- **Recent.** The 40 newest finished, rejected and dead-lettered messages.

The state glyphs are `◌` queued, `◐` claimed, `●` done, `✕` rejected and `!` dead-lettered.

## Flow screen

- **Messages sent, last hour.** A sparkline of messages over time. The oldest is on the left and the newest is on the right.
- **Who talks to whom.** The number of messages for each sender and receiver.
- **Turnaround, task to reply.** For each worker, the median time from the task being sent to its reply being sent.
- **Busy now.** Claimed plus queued messages for each worker. The bar is full at 4.

## Rejected screen

- **Rejected and dead-lettered.** The newest messages in `rejected/` and `dlq/`, newest first, with the reason from each `<id>.reason.json` file. The title shows how many are listed and how many there are in all. Up to 40 are read; the screen lists as many as fit.
- A message that cannot be read, which is often why it was dead-lettered, still gets a row. Its time and sender come from its file name and its receiver from its folder.
- Message bodies are never shown on this screen, even with `b`.

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

## Build and test

`tools/cq-top` is a member of the repo's Cargo workspace, and a default member. The four required checks at the repo root build and test it with `cq`:

```bash
cargo build
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

To check only `cq-top`, add `-p cq-top` to each command. The tests use a temporary queue root and never touch a real one.
