# Security policy

## Supported versions

Only the latest release on `main` gets security fixes.

## Report a vulnerability

Please do not open a public issue for a security problem.

1. Open the repository's **Security** tab.
2. Select **Report a vulnerability**.
3. Describe the problem, the `cq` version (`cq --version`), and the steps to reproduce it.

This is a personal project, so replies are best effort. The reply comes in the private advisory thread.

## Scope

In scope: the `cq` and `cq-top` binaries, how they read and write the queue folder, message validation, and the watcher's inbox and Codex wake paths.

Out of scope: what an agent does with a message it receives. `cq` treats every message as data, and each session's own rules and permissions decide what it may do. See the [safety model](docs/OVERVIEW.md#safety-model).

Not a boundary: the control files in the queue root (`coordinators`, `idle-alerts`, `sleep/`, `PAUSE` and `config.toml`). `cq` does not authenticate who writes them. Any process with write access to the root can edit them, so they have the same trust level as a message. Sessions without a sandbox run as the same OS user, so each of them can edit these files. An OS or agent sandbox still applies: for example, Antigravity broker and session mode keep their turns from writing the root or `config.toml`. A process with write access that edits these files is not a vulnerability in `cq`.
