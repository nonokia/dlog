# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

**v0.1 and v0.2 are implemented.** The CLI works end-to-end: the decision log
(`record`), the full query surface (`why` / `show` / `status` / `search` /
`invariants` / `context` / `trace`), staging/seal/binding (`bind`) with git
automation (`commit` wrapper + `hooks` post-commit auto-seal), AST-node anchoring
with query-time resolution for **Rust and TypeScript**, and context-budgeted
output. `agent-first-vcs-design.md` (written in Japanese) remains the **source of
truth** for design decisions; do not re-litigate the settled choices listed in
its section 11.

**Workflow:** OpenSpec is adopted from the `context-compression` change onward (design §12) —
change proposals live in `openspec/changes/<name>/` (`proposal.md` / `design.md` / `tasks.md`),
completed ones are moved to `openspec/changes/archive/`, config in `openspec/config.yaml`. Earlier
v0.1/v0.2 work was tracked as GitHub issues + per-issue PRs and is not retro-specced.

## What dlog is

`dlog` is an **agent-first decision log** that sits *alongside* Git (not a replacement). Git
records *what* changed (diffs); dlog records the *decisions* behind code — the rationale,
rejected alternatives, assumptions/invariants, and the original instruction — so that AI agents
can reconstruct context across sessions and multi-agent handoffs. The unit of record is a
**decision**, not a commit or an edit.

It is intended to be consumed almost entirely by agents (via a CLI, JSON in/out); humans read
through agents rather than through a human-facing UI. There is deliberately no checkout / merge /
branch surface.

## Tech stack

- **Language: Rust** — chosen for tree-sitter affinity and the CLI + SQLite + tree-sitter ecosystem.
- **CLI: clap** (with `env` feature), **serialization: serde**, **SQLite: rusqlite** (`bundled`),
  **ids: ulid**, **AST: tree-sitter** (+ `tree-sitter-rust`, `tree-sitter-typescript`).
- **No daemon**: each CLI invocation hits SQLite directly; concurrent writes are arbitrated by
  SQLite locking. No CRDT, no distributed sync.
- **tree-sitter** is used directly (not difftastic) for AST node anchoring.

Standard toolchain: `cargo build`, `cargo test` (`cargo test <name>` for a single test),
`cargo run -- <args>`, `cargo clippy`, `cargo fmt`. **CI gate** runs fmt + clippy + build + test
with `RUSTFLAGS="-D warnings"`, so keep clippy clean.

> **Dependency note:** `rusqlite` is pinned to **0.37** on purpose — 0.40 pulls `libsqlite3-sys`
> 0.38, whose build script uses the still-unstable `cfg_select!` macro and fails on stable rustc.

## Code layout

- `src/main.rs` — thin entry point; maps `lib::run()` to a process exit code.
- `src/lib.rs` — clap dispatch table; routes each subcommand to its handler.
- `src/cli.rs` — clap argument structs (one per command).
- `src/commands/` — one module per command (`record`, `bind`, `commit`, `hooks`, `why`, `show`,
  `status`, `search`, `invariants`, `context`, `trace`), plus `compact` (shared compact result
  rows + context budget) and `mod` (shared `AppError`, `open_store`, `current_git_sha`,
  `parse_line_spec`). A command handler maps args → store/anchor calls → `emit` JSON.
- `src/store.rs` + `src/schema.sql` — the SQLite layer (idempotent migrations). **Staging vs. main
  log is one `decision` table with a `staged` flag, not two physical tables**; sealing flips the
  flag and stamps the binding, and BEFORE UPDATE/DELETE triggers make sealed rows append-only.
- `src/model.rs` — domain types (Decision/Anchor/Binding/Agent/...).
- `src/anchor.rs` — **the only language-dependent code**: tree-sitter extraction of `symbol_path`
  and `structural_hash` at record time. A `LangSupport` table holds the per-language knowledge
  (Rust, TypeScript/TSX); unsupported files degrade to file-level (§10.5).
- `src/resolve.rs` — query-time 2-axis resolution producing the `resolution` confidence.
- `src/output.rs` — the JSON envelope / error / exit-code contract shared by all commands.
- `templates/AGENTS.md` — instruction template shipped for agents that *use* dlog (distinct from
  this file, which guides agents working *on* dlog).

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
  via tree-sitter. Unsupported languages naturally degrade to `file_fallback`. **Rust and
  TypeScript/TSX have node anchoring** (Rust first, for dogfooding); more grammars are cheap to add.

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

- **Context budget (§2.1, #33):** list queries (`why` / `context` / `search`) bound their payload
  to an agent context window via `--budget` (chars; default on), emitting newest-first with adaptive
  summary widths and reporting `elided` (live results left out) alongside `truncated`.

Command surface: `dlog record`, `dlog why <file:line|symbol>`, `dlog show <id>`,
`dlog context <path>`, `dlog trace <id>`, `dlog invariants`, `dlog search --text`, `dlog status`,
`dlog bind <sha>`, `dlog commit`, `dlog hooks <install|uninstall>`. Full-text search uses SQLite FTS5.

## Scope status

**Delivered (v0.1 + v0.2):** the full command surface above; staging/main-log/binding with git
automation (`commit` wrapper + post-commit `hooks` auto-seal); AST-node anchoring with query-time
resolution for Rust and TypeScript/TSX; context-budgeted output; and the agent instruction template
(`templates/AGENTS.md`). Possible later work (not yet scoped): more tree-sitter grammars, richer
`trace`/`context` rollups, and task-lifecycle commands.
