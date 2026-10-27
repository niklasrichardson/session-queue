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
| `Tab`, `1`, `2` | Switch between the Overview and Flow screens. |
| `t` | Use the next theme: classic, amber, neon. |
| `b` | Show or hide message bodies. They are hidden at start. |
| `q`, `Esc`, `Ctrl-C` | Quit. |

`cq-top` remembers the last screen and theme in a one-line file, such as `overview classic`. The file is `cq-top/state` in `$XDG_CONFIG_HOME`, or in `~/.config`, or in `%APPDATA%`. Without a saved choice, `cq-top` starts on Overview with the classic theme. If no config folder is known, nothing is saved.

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

## Themes

| Theme | Look |
| --- | --- |
| `classic` | Your terminal background, one colour for each meaning, single-line borders. |
| `amber` | One amber colour on near-black, double borders, coarse bars. Bold, underline and inverse text carry the meaning. |
| `neon` | Truecolor on dark navy, rounded borders, fine bars. |

A theme is one entry in `THEMES` in `tools/cq-top/src/draw.rs`.

## Limits

- A watcher counts as alive when its heartbeat is less than 90 seconds old. The process ID is shown, but `cq-top` does not check that the process exists.
- The Recent list reads only the 40 newest files on each refresh.
- Turnaround includes the time the worker takes to write its reply.
- An `info` message that nobody claims counts as work in the Busy bar.
- `cq` keeps only the latest heartbeat, so `cq-top` shows no heartbeat history.

## Build and test

Run these in `tools/cq-top`. The tests use a temporary queue root and never touch a real one.

```bash
cargo build
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
