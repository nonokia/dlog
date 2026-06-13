# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

This repository is **pre-implementation**. It currently contains only a design document
(`agent-first-vcs-design.md`, written in Japanese) and a Rust-oriented `.gitignore`. There is
no Cargo project, source code, or tests yet. The design has reached the "OpenSpec migration"
stage — the *why* (design rationale) is settled and ready to be turned into specs and a Rust
PoC. When implementing, treat `agent-first-vcs-design.md` as the source of truth for decisions
already made; do not re-litigate settled choices listed in section 11 of that document.

## What dlog is

`dlog` is an **agent-first decision log** that sits *alongside* Git (not a replacement). Git
records *what* changed (diffs); dlog records the *decisions* behind code — the rationale,
rejected alternatives, assumptions/invariants, and the original instruction — so that AI agents
can reconstruct context across sessions and multi-agent handoffs. The unit of record is a
**decision**, not a commit or an edit.

It is intended to be consumed almost entirely by agents (via a CLI, JSON in/out); humans read
through agents rather than through a human-facing UI. There is deliberately no checkout / merge /
branch surface.

## Planned tech stack (decided, not yet built)

- **Language: Rust** — chosen for tree-sitter affinity and the CLI + SQLite + tree-sitter ecosystem.
- **CLI: clap**, **serialization: serde**, **SQLite: rusqlite**.
- **No daemon**: each CLI invocation hits SQLite directly; concurrent writes are arbitrated by
  SQLite locking. No CRDT, no distributed sync.
- **tree-sitter** is used directly (not difftastic) for AST node anchoring.

Once a Cargo project exists, the standard toolchain applies: `cargo build`, `cargo test`
(`cargo test <name>` for a single test), `cargo run -- <args>`, `cargo clippy`, `cargo fmt`.

## Architecture (the big picture)

### Three entities, not one monolithic record
Decisions are split by lifetime and access pattern (design §7.1):
- **Decision** — the append-only main log. Never UPDATEd; a reversed decision is recorded as a
  new Decision with `supersedes: <old_id>` (§7.2).
- **Task** — task hierarchy (`parent_task_id`) and the human's original instruction.
- **Invariant** — declared constraints; longer-lived than the Decision that declared them, queried
  independently via `dlog invariants`.

Decisions form a **DAG** via `caused_by` (e.g. "B is a fix prompted by A's review comment"), not a
flat log. IDs are ULIDs (time-sortable). Keep required fields minimal — `rationale` + anchor +
agent identity — to avoid recording friction; `rejected` / `assumptions` are optional (§7.3).

### Git integration: staging + seal (§8)
Decisions are born *before* a commit (rejected attempts never reach a commit), so commit-time-only
recording is impossible. The model mirrors Git's index:
1. In-progress decisions are written immediately to a **staging table** (same SQLite, mutable work area).
2. On **seal**, a `binding` is stamped and the record moves to the **immutable main log**.
3. Main-log records always carry an explicit binding: `{ type: "commit", sha }` or `{ type: "none" }`
   (investigation/review that led to no commit). There is no "pending" state in the main log — being
   in staging *is* the pending state.

Seal triggers (§8.3): code path via `dlog commit` / `dlog bind <sha>`; non-code path on task
completion (`dlog task done`) with `binding: none`. Subagents must seal on task end so their
on-the-ground decisions survive even when only a summary returns to the parent. dlog does **not**
proxy all of Git — the only integration point is the moment of commit.

### AST node anchoring: resolve at query time (§10)
Decisions anchor to **named definition nodes** (functions, methods, struct/class, modules) — not
line numbers — so they survive refactors and line insertions. The key inversion: **identity is not
stored, it is judged at query time** and surfaced to the agent as a `resolution` confidence.

An anchor stores only observations at record time: `file`, `symbol_path`, `node_kind`,
`structural_hash` (normalized token hash ignoring identifiers/comments/whitespace), `line_span`
(human snapshot only, not used for resolution), `recorded_at_sha`. Query-time matching is a 2-axis
matrix yielding `resolution ∈ {exact, drifted, relocated, file_fallback}`:

| symbol_path | structural_hash | resolution |
|---|---|---|
| match | match | `exact` |
| match | mismatch | `drifted` (stale decision possible) |
| mismatch | match | `relocated` (renamed/moved, hash match is global/cross-file) |
| mismatch | mismatch | `file_fallback` (degrade to file level) |

Anchor resolution failing never errors — it degrades to a file-level decision so the agent isn't
blocked.

### Responsibility split (§10.5)
- **Language-independent (tool core):** Decision/Task/Invariant recording, staging, sealing,
  binding, and `show`/`trace`/`invariants`/`search`/`status`. File-level anchors are also
  language-independent (path only — non-code files like YAML/Markdown can carry decisions too).
- **Language-dependent (anchor resolution only):** extracting `symbol_path` and `structural_hash`
  via tree-sitter. Unsupported languages naturally degrade to `file_fallback`. **Rust is the first
  language with node anchoring** (dogfooding: dlog is built in Rust).

## Query API principles (§9)
- **Two-stage retrieval:** queries default to a compact form (id + rationale summary + binding +
  timestamp); the agent drills into full detail with `dlog show <id>`. This mirrors
  `git log --oneline` → `git show` and is about saving context-window tokens.
- **Responses self-describe via state, not suggestions:** return only facts the agent can't derive
  (`resolution`, `truncated`). A `hints`/"next command" field was explicitly rejected — dlog records
  agent decisions, it does not steer them.
- **Default scope is "live decisions":** superseded decisions are excluded by default
  (`--include-superseded` for history); staging is **included** by default (the most recent
  decision is the most valuable), flagged with `staged: true`. The main-log/staging UNION is hidden
  from the agent as a single list.

Planned command surface: `dlog record`, `dlog why <file:line|symbol>`, `dlog show <id>`,
`dlog context <path>`, `dlog trace <id>`, `dlog invariants`, `dlog search --text`, `dlog status`,
`dlog bind <sha>`. Full-text search uses SQLite FTS5.

## v0.1 PoC scope
First milestone is a Rust project skeleton with `dlog record` + `dlog why` working. v0.1 includes
the staging/main-log/binding schema and manual `dlog bind <sha>`. Out of scope for v0.1: context
compression, the `dlog commit` wrapper, post-commit auto-binding, and any tree-sitter grammar other
than Rust.
