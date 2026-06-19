# dlog

An **agent-first decision log** that sits *alongside* Git. Git records *what*
changed (diffs); `dlog` records the *decisions* behind code — the rationale,
rejected alternatives, assumptions/invariants, and the original instruction — so
AI agents can reconstruct context across sessions and multi-agent hand-offs. The
unit of record is a **decision**, not a commit or an edit.

It is built to be consumed almost entirely by agents: a small CLI with JSON
in/out, no daemon, backed by a single SQLite store next to your repo. See
[`agent-first-vcs-design.md`](agent-first-vcs-design.md) for the full design.

> **Status: v0.2.** Working: the decision log, the full query surface, staging /
> seal + git binding with a `dlog commit` wrapper and a post-commit `hooks`
> auto-seal, AST-node anchoring with query-time resolution for **Rust and
> TypeScript**, and context-budgeted output. See the design doc for what's next.

## Install

**Prebuilt binary (no toolchain needed).** Downloads the right binary for your
platform from the [GitHub Releases](https://github.com/nonokia/dlog/releases)
and drops it on your PATH (`$HOME/.local/bin` by default):

```bash
curl -fsSL https://raw.githubusercontent.com/nonokia/dlog/main/install.sh | sh
```

Override the target dir with `DLOG_BIN_DIR`, or pin a tag with `DLOG_VERSION`:

```bash
DLOG_BIN_DIR=/usr/local/bin DLOG_VERSION=v0.2.0 \
  sh -c "$(curl -fsSL https://raw.githubusercontent.com/nonokia/dlog/main/install.sh)"
```

Prebuilt targets: Linux and macOS, x86_64 and aarch64. Each release also ships
`*.tar.gz.sha256` checksums (the installer verifies them automatically).

**With Cargo** (builds from source — needs a Rust + C toolchain, bundles SQLite
and tree-sitter):

```bash
cargo install --git https://github.com/nonokia/dlog dlog
```

> A Homebrew tap is planned as a follow-up once the first release is published.

### Build from source

```bash
git clone https://github.com/nonokia/dlog && cd dlog
cargo build            # or: cargo install --path .
cargo test
```

## Concepts

- **Decision** — the append-only main log. A reversed decision is a *new*
  decision with `--supersedes`; records are never mutated.
- **Staging + seal** — decisions are born before a commit, so they go to a
  mutable staging area first; `dlog bind` seals them into the immutable log with
  a binding (`{type:commit,sha}` or `{type:none}`).
- **AST-node anchors** — decisions anchor to named definitions (not line
  numbers), so they survive refactors. Identity is judged **at query time** and
  surfaced as a `resolution` (`exact` / `drifted` / `relocated` / `file_fallback`).
  Rust and TypeScript get node anchoring (tree-sitter); other files anchor at the
  file level.
- **Invariants** — declared constraints, queried independently of the log.

## Commands

```text
dlog record   --rationale <why> (--file <FILE[:LINES]> | --changed) [...]  # log a decision (to staging)
dlog bind     <SHA> | --none [--decision <id>...]             # seal staged decisions
dlog commit   [-- <git commit args>]                          # git commit, then auto-seal staging
dlog hooks    <install | uninstall>                           # repo post-commit auto-seal hook
dlog why      <FILE:LINE | SYMBOL> [--budget <chars>]         # decisions behind a location
dlog context  <PATH>                                          # decision summary for a file/dir
dlog trace    <id> [--depth <n>]                              # walk the caused_by DAG (causes/effects)
dlog show     <id>...                                         # full record(s)
dlog search   --text <query>                                  # full-text search (FTS5)
dlog invariants [--scope <path>]                              # live declared constraints
dlog status                                                   # store state (staging, schema)
```

Every command prints one JSON document; failures are `{"error":{...}}` (exit 1),
usage errors exit 2. Agent identity comes from `--agent-role`/`--agent-model`
(or `DLOG_AGENT_ROLE`/`DLOG_AGENT_MODEL`); the store path from `--db` or
`DLOG_DB` (default `.dlog/dlog.db`). The list queries (`why`/`context`/`search`)
bound their output to a `--budget` of characters and report `elided` when results
are left out.

### Example

```bash
export DLOG_AGENT_ROLE=implementer DLOG_AGENT_MODEL=<model-id>

dlog record --rationale "retry with backoff; upstream API is flaky" \
            --file src/net/client.rs:42 \
            --rejected "fixed sleep :: too slow under load"
dlog commit -- -m "add retry"      # git commit + auto-seal staging to it

dlog why src/net/client.rs:42      # -> resolution + compact results
dlog context src/net/              # -> decisions across the directory
dlog show <id>                     # -> full decision
```

## Using dlog from an agent

`dlog` is meant to be driven by coding agents. Give your agent the instruction
template in [`templates/AGENTS.md`](templates/AGENTS.md) — paste it into your
repo's `AGENTS.md` / `CLAUDE.md`. It covers: setting identity, checking
`dlog status` at task start, recording decisions as you make them, sealing after
commits (and subagents sealing at task end), and reading `resolution` before
trusting a decision.
