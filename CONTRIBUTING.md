# Contributing to cq

Thanks for helping. Bug reports, test results from other platforms, documentation fixes and code changes are all welcome.

## Good places to start

- [Issues labelled "help wanted"](https://github.com/niklasrichardson/session-queue/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22), especially testing on macOS and Linux.
- [Issues labelled "good first issue"](https://github.com/niklasrichardson/session-queue/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22).
- [Test status](docs/TESTING.md) lists what has not been tested yet.

## Reporting a problem

Open an issue with:

- your platform and version (Windows, macOS or Linux), and your Rust version (`rustc --version`);
- which agent you use and how it runs (for example the Claude Code desktop app, CLI or Remote Control, or the Codex Desktop app or CLI);
- the command you ran, its exit code and any `cq:` line it printed (see [Commands](docs/COMMANDS.md#exit-codes));
- for a watcher problem, the stderr line from `cq watch ... 2>&1`.

Do not paste message bodies or paths that contain private information.

## Building and testing

You need stable Rust.

```bash
cargo build
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` takes about 70 seconds, because one test runs a watcher for 65 seconds.

If your own sessions are using `cq` while you develop, build into a separate folder so you never replace or delete the binary they run: `CARGO_TARGET_DIR=../cq-build cargo test`. Never run `cargo clean` on the folder your sessions run `cq` from. Upgrading a live install is covered in [Setup](docs/SETUP.md#upgrading-while-watchers-run).

## Pull requests

1. Open an issue first for anything larger than a small fix, so we can agree the approach.
2. Keep each pull request to one topic.
3. Run the four commands above. All must pass.
4. Add or update tests for changed behaviour.
5. Update the docs when behaviour changes: [Commands](docs/COMMANDS.md) for commands and exit codes, [Design decisions](docs/DESIGN.md) for a new choice, [Test status](docs/TESTING.md) for what you tested.
6. Say in the pull request which platforms you tested on.

## Platform code

Platform-specific code lives in `src/platform.rs` (stopping an old watcher, checking stdin) and `src/root.rs` (the default queue root). Windows is tested live. macOS is partly tested ([#1](https://github.com/niklasrichardson/session-queue/issues/1)): most tests pass there, `tests/dispatch.rs` fails intermittently, and only inbox mode in one session was checked live. Linux has not been built.

## Writing style for docs

Short sentences, plain words, one idea per sentence. Use the same term for the same thing throughout (for example "queue root", "watcher", "re-arm").

## Licence

By contributing, you agree that your contributions are licensed under the [MIT licence](LICENSE).
