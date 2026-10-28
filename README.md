# cq: a message queue for AI coding sessions

`cq` lets AI coding sessions on one computer pass work to each other with no person in the loop. Each message is one JSON file in a shared folder. Claude Code sessions run `cq watch --to-inbox`, which posts queue events into the session's own Claude Code inbox. Codex workers use `cq dispatch` to start a fresh task run for each message.

## Why

Claude Code sessions can already send each other messages, but after 10 in a row a person has to type something before they carry on. When several sessions work on one project (for example a lead, two developers and a designer), that limit stops the hand-offs whenever nobody is at the keyboard.

With `cq`, a message reaches the receiving session as a queue event instead. In an overnight test with the earlier Monitor mode, a session handled 27 queue turns in a row with no limit reached. In its first two days of real use, `cq` carried 361 messages between 10 sessions, and the median time from send to pick-up was 12 seconds.

## What it does

- **One file per message.** `cq send` writes the message to a temporary file, then renames it into the recipient's inbox, so a reader never sees half a message.
- **Exactly-once handling.** `cq claim` moves the file into a `.processing/` folder; if two turns try, only one wins.
- **Replies and rejections.** `cq done` finishes a message. A task gets its reply, or a rejection with a reason, sent back to the sender.
- **Storm limits.** Per-pair, per-recipient and per-sender rates, a hop limit and a body size limit stop sessions flooding each other. A `PAUSE` file stops all traffic at once.
- **Dead letters.** An invalid message goes to `dlq/` and the sender is told.
- **Rejoin after `/clear`.** A SessionStart hook reminds a cleared session of its queue name and tells it whether its watcher still runs.
- **Plain files.** No server, no database and no network. You can inspect the queue with `ls` and `cat`.

## Codex

`cq dispatch` runs one fresh Codex task per message. It requires trusted scope rules and allowed senders, handles replies, and stops for review after uncertain completion. Start with [Codex setup](docs/CODEX.md). Claude Code sessions use `cq watch` instead.

## Quick start (Claude Code)

You need Claude Code, stable Rust, and Windows (with Git Bash) or macOS. macOS support is new and not yet tested on a Mac.

```bash
cargo install --path .
cq init
```

Then allow `Bash(cq *)` in `~/.claude/settings.json`, restart the Claude app, and give each session a queue name with the start prompt in [`CLAUDE-snippets.md`](CLAUDE-snippets.md). One session sends work to another like this:

```bash
cq send --from lead --to worker-a --type task <<'EOF'
{"subject": "Run the tests", "body": "Run cargo test and report failures."}
EOF
```

The full steps, including the rejoin hook, are in [Setup](docs/SETUP.md).

## Configuration

Every session must use the same absolute CQ_ROOT. [Setup configuration](docs/SETUP.md#configuration) lists queue settings. Adapt the [example session rules](docs/SESSION-RULES.example.md) into your private CQ_RULES file; do not replace a shared file with one fixed session identity.

## Documentation

| Document | What it covers |
| --- | --- |
| [Codex setup](docs/CODEX.md) | Native dispatcher, scope rules, shell examples and recovery |
| [Generic rules](docs/SESSION-RULES.example.md) | Queue rules for any coding agent |
| [Setup](docs/SETUP.md) | Requirements, install steps, configuration, upgrading |
| [`CLAUDE-snippets.md`](CLAUDE-snippets.md) | CLAUDE.md blocks and session prompts to paste |
| [Session protocol](docs/PROTOCOL.md) | How a session watches, claims, replies and rejoins; send limits |
| [Commands](docs/COMMANDS.md) | Every command, exit codes, the queue folder layout |
| [cq-top](docs/CQ-TOP.md) | A live, read-only terminal view of the queue |
| [Design decisions](docs/DESIGN.md) | Why `cq` behaves as it does |
| [Test status](docs/TESTING.md) | What has been tested live, and what has not |
| [Specification](docs/SPEC.md) | The original requirements, architecture and test plan |

## Contributing

Issues and pull requests are welcome. Testing on macOS and Linux is the most useful help right now. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

MIT. See [`LICENSE`](LICENSE).
