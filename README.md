# dlog

An **agent-first decision log** that sits *alongside* Git. Git records *what*
changed (diffs); `dlog` records the *decisions* behind code — the rationale,
rejected alternatives, assumptions/invariants, and the original instruction — so
AI agents can reconstruct context across sessions and multi-agent hand-offs. The
unit of record is a **decision**, not a commit or an edit.

It is built to be consumed almost entirely by agents: a small CLI with JSON
in/out, no daemon, backed by a single SQLite store next to your repo. See
[`agent-first-vcs-design.md`](agent-first-vcs-design.md) for the full design.

> **Status: v0.1 (PoC).** The decision log, staging/seal + git binding, AST-node
> anchoring with query-time resolution (Rust), and the query surface are
> working. Out of scope for v0.1: the `dlog commit` wrapper, post-commit
> auto-binding, context compression, and tree-sitter grammars other than Rust.

## Build

```bash
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
- **Invariants** — declared constraints, queried independently of the log.

## Commands

```text
dlog record   --rationale <why> --file <FILE[:LINES]> [...]   # log a decision (to staging)
dlog bind     <SHA> | --none [--decision <id>...]             # seal staged decisions
dlog why      <FILE:LINE | SYMBOL> [--include-superseded]     # decisions behind a location
dlog show     <id>...                                         # full record(s)
dlog search   --text <query>                                  # full-text search (FTS5)
dlog invariants [--scope <path>]                              # live declared constraints
dlog status                                                   # store state (staging, schema)
```

Every command prints one JSON document; failures are `{"error":{...}}` (exit 1),
usage errors exit 2. Agent identity comes from `--agent-role`/`--agent-model`
(or `DLOG_AGENT_ROLE`/`DLOG_AGENT_MODEL`); the store path from `--db` or
`DLOG_DB` (default `.dlog/dlog.db`).

### Example

```bash
export DLOG_AGENT_ROLE=implementer DLOG_AGENT_MODEL=<model-id>

dlog record --rationale "retry with backoff; upstream API is flaky" \
            --file src/net/client.rs:42 \
            --rejected "fixed sleep :: too slow under load"
git commit -m "add retry" && dlog bind "$(git rev-parse HEAD)"

dlog why src/net/client.rs:42      # -> resolution + compact results
dlog show <id>                     # -> full decision
```

## Using dlog from an agent

`dlog` is meant to be driven by coding agents. Give your agent the instruction
template in [`templates/AGENTS.md`](templates/AGENTS.md) — paste it into your
repo's `AGENTS.md` / `CLAUDE.md`. It covers: setting identity, checking
`dlog status` at task start, recording decisions as you make them, sealing after
commits (and subagents sealing at task end), and reading `resolution` before
trusting a decision.
