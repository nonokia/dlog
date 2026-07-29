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
  mutable staging area first; sealing moves them into the immutable log with a
  binding. Two triggers: the code path (`dlog commit` / `dlog bind <sha>` →
  `{type:commit,sha}`) and the non-code path (`dlog task done` → `{type:none}`,
  for investigation or review that led to no commit).
- **Task** — a unit of work with the human's original instruction and an
  optional parent, so multi-agent hand-offs keep their structure. `dlog task
  done` seals only *that* task's decisions, so a subagent finishing up can't
  seal work another agent still has in flight. A task whose decisions were never
  sealed is named by `dlog status` as a *stranded task*, so the next agent can
  pick it up by id instead of sealing blind.
- **AST-node anchors** — decisions anchor to named definitions (not line
  numbers), so they survive refactors. Identity is judged **at query time** and
  surfaced as a `resolution` (`exact` / `drifted` / `relocated` / `file_fallback`).
  Rust, TypeScript/TSX, Go, PHP, Python, Java, and Ruby get node anchoring
  (tree-sitter); other files anchor at the file level.
- **Invariants** — declared constraints, queried independently of the log.

## Commands

```text
dlog init                                                     # mark this directory as the workspace root
dlog task start [--parent <id>] [--instruction <text>]        # start a task, get its id
dlog task list  [--open | --all] [--parent <id>]              # tasks in flight, with their unsealed counts
dlog task done  <id>                                          # finish a task, sealing its decisions (binding: none)
dlog record   --rationale <why> (--file <FILE[:LINES]> | --changed) [...]  # log a decision (to staging)
dlog bind     <SHA> | --none [--decision <id>...]             # seal staged decisions
dlog commit   [-- <git commit args>]                          # git commit, then auto-seal staging
dlog hooks    <install | uninstall>                           # repo post-commit auto-seal hook
dlog why      <FILE:LINE | SYMBOL> [--budget <chars>]         # decisions behind a location
dlog context  <PATH> [--rollup | --flat] [--no-invariants]    # decision summary for a file/dir
dlog trace    <id> [--depth <n>] [--budget <chars>]           # walk the caused_by DAG (causes/effects)
dlog show     <id>...                                         # full record(s)
dlog search   --text <query>                                  # full-text search (FTS5)
dlog invariants [--scope <path>]                              # live declared constraints
dlog status                                                   # store state (staging, stranded tasks, schema)
```

Every command prints one JSON document; failures are `{"error":{...}}` (exit 1),
usage errors exit 2. Agent identity comes from `--agent-role`/`--agent-model`
(or `DLOG_AGENT_ROLE`/`DLOG_AGENT_MODEL`); the store path from `--db` or
`DLOG_DB` (default `<workspace root>/.dlog/dlog.db`). The list queries
(`why`/`context`/`search`/`trace`) bound their output to a `--budget` of
characters and report `elided` when results are left out. `dlog context` on a
directory rolls up per file (count + latest decision) and carries the invariants
in effect there, so it is the one command to run before touching an area.

### Workspace

Commands resolve a **workspace root** by walking up from the current directory:
the nearest ancestor with a `.dlog/`, else the nearest git repository, else the
current directory. Anchors are stored relative to that root, so a decision
records and resolves to the same file whichever subdirectory you run from.
`dlog status` reports the root it picked and why (`root_source`).

Git is optional. Only `dlog commit` and `dlog hooks` need it — run `dlog init` to
declare a root without one, record as usual, and seal with `dlog task done`.

### Example

```bash
TASK=$(dlog task start --instruction "make the API client resilient" | jq -r .id)

dlog record --task "$TASK" \
            --rationale "retry with backoff; upstream API is flaky" \
            --file src/net/client.rs:42 \
            --rejected "fixed sleep :: too slow under load" \
            --agent-role implementer --agent-model <model-id>
dlog commit -- -m "add retry"      # git commit + auto-seal staging to it
# no commit to make? seal the task instead:
dlog task done "$TASK"             # -> binding {"type":"none"}

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

### Agent discovery (ARD)

dlog advertises itself for
[Agentic Resource Discovery](https://github.com/ards-project/ard-spec) so agents
can find it by searching rather than being told. The catalog is served at
`/.well-known/ai-catalog.json` (via GitHub Pages from [`docs/`](docs/)) and
lists dlog as an `application/ai-skill` whose `url` points at the skill doc
above (install + usage). The catalog is schema-validated in CI — see
[`scripts/ard/`](scripts/ard/). Strict ARD discovery expects the catalog at a
domain root; the project-Pages subpath is a v1 step, with domain-anchored
hosting as a follow-up.
