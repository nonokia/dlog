# dlog — agent instructions

> Drop this into your repo's `AGENTS.md` / `CLAUDE.md` (or paste the rules
> below) so agents record and reconstruct decisions with `dlog`.

`dlog` is an agent-first **decision log** that sits *alongside* Git. Git records
*what* changed; `dlog` records the *why* — the rationale, rejected alternatives,
assumptions, and the instruction behind code — so you can reconstruct context
across sessions and hand-offs. It is consumed via a CLI with JSON in/out; every
command prints one JSON document. A failure is `{"error":{"code","message"}}`
with exit code 1; a usage error exits 2.

## Install (if `dlog` isn't on PATH)

```bash
curl -fsSL https://raw.githubusercontent.com/nonokia/dlog/main/install.sh | sh
# or from source: cargo install --git https://github.com/nonokia/dlog dlog
```

The installer fetches a prebuilt binary into `$HOME/.local/bin` (override with
`DLOG_BIN_DIR`); pin a release with `DLOG_VERSION`. Verify with `dlog status`.

## Identity

Every `record` carries your identity. Pass it as flags on each call:

```bash
--agent-role implementer         # or reviewer, investigator, ...
--agent-model <your-model-id>
--agent-session <session-id>     # optional
```

Prefer the flags. Sandboxed harnesses (e.g. Claude Code) run each command in a
fresh shell, so `export` doesn't persist — and prefixing every call with
`DLOG_AGENT_MODEL=... dlog ...` doesn't match command allowlists, triggering a
permission prompt each time. If your shell *does* persist, the same values are
read from `DLOG_AGENT_ROLE` / `DLOG_AGENT_MODEL` / `DLOG_AGENT_SESSION` as
fallbacks (flags win).

## Workspace

Every command resolves a **workspace root** by walking up from the current
directory: the nearest ancestor holding a `.dlog/`, else the nearest git
repository, else the current directory. The store is `<root>/.dlog/dlog.db`
(override with `--db` or `DLOG_DB`) and is created on first use.

Anchors are stored relative to that root, so you can record and query from any
subdirectory and get the same answers. In a project with no git repository, run
`dlog init` once at the top to declare the root:

```bash
dlog init
# {"root":"/path/to/project","db":"/path/to/project/.dlog/dlog.db","created":true}
```

Git is optional — only `dlog commit` and `dlog hooks` require it. Without git,
record as usual and seal with `dlog task done` at the end of the task.

## At the start of a task

Check the store state. It also tells you which store you are talking to — if
`root` isn't the project you expect, you are in the wrong workspace. If decisions
are stranded in staging (e.g. a plain `git commit` was made without sealing),
deal with them before starting:

```bash
dlog status
# {"root":"...","db":"...","root_source":"dlog","staging_count":N,
#  "oldest_staged_ms":...,"stranded_task_count":1,"schema_version":2,
#  "stranded_tasks":[{"task":"01J...","instruction_summary":"...",
#                     "staged_count":2,"oldest_staged_ms":...}]}
```

`stranded_tasks` are **unfinished tasks that still hold unsealed decisions** —
work an earlier session or a subagent recorded and never sealed. Pick each one
up by its id rather than sealing blind:

```bash
dlog task list                       # every open task (add --all for finished ones)
dlog show <id>                       # what it decided, if you need to judge it
dlog task done 01J...                # finish it — seals only that task's decisions
```

If `staging_count` exceeds what the stranded tasks account for, the remainder was
recorded without a `--task`. Seal that with `dlog bind <sha>` if you know the
commit it belongs to, otherwise `dlog bind --none`.

Then start a task and keep its id — it is what ties your decisions together and
lets you seal exactly your own work at the end:

```bash
TASK=$(dlog task start --instruction "<the human's original ask>" | jq -r .id)
# subagents: pass the parent's id
# TASK=$(dlog task start --parent "$PARENT_TASK" --instruction "..." | jq -r .id)
```

## Record a decision (the moment you make one)

Record **as you decide**, before committing — rejected attempts never reach a
commit, so commit-time-only recording loses them. Keep it low-friction: only
`--rationale`, at least one `--file` anchor, and your identity are required.

```bash
dlog record --task "$TASK" \
  --rationale "retry with exponential backoff; the upstream API is flaky" \
  --file src/net/client.rs:42 \
  --agent-role implementer --agent-model <your-model-id>
# {"id":"01J...","staged":true}
```

Anchor with `FILE`, `FILE:LINE`, or `FILE:START-END`. For Rust and
TypeScript/TSX files the enclosing definition (symbol + structural hash) is
captured automatically so the decision survives refactors; other files anchor at
file level.

Lower-friction shortcuts (identity flags elided below — they're still required):

- `--changed` anchors to every file changed in the working tree (`git status`),
  so a decision about the current change needn't list each file:
  ```bash
  dlog record --changed --rationale "extract the retry policy into its own type"
  ```
- `--rationale -` reads the rationale from stdin — handy for long or multi-line
  prose without shell quoting:
  ```bash
  printf '%s' "$LONG_RATIONALE" | dlog record --rationale - --changed
  ```

Optional, when useful:

- `--rejected "approach :: why it was dropped"` (repeatable) — record what you
  tried and discarded, so the next agent doesn't repeat it.
- `--declares-invariant "constraint that must hold"` `--invariant-scope src/net`
  — declare a constraint other agents must respect.
- `--supersedes <id>` — this decision reverses/replaces an earlier one.
- `--caused-by <id>` (repeatable) — this decision was prompted by another (e.g. a
  review comment).
- `--conversation-id <id>` — link to the conversation/transcript.

## Seal decisions

Recorded decisions sit in **staging** until sealed. Sealing moves them into the
immutable log with a binding.

- **After you commit code**, bind the staged decisions to that commit:

  ```bash
  git commit -m "..."          # then:
  dlog bind "$(git rev-parse HEAD)"
  # {"count":N,"sealed":[...],"binding":{"type":"commit","sha":"..."}}
  ```

- **At the end of a task with no commit** (investigation, review), finish the
  task:

  ```bash
  dlog task done "$TASK"
  # {"task":"01J...","count":N,"sealed":[...],"binding":{"type":"none"},
  #  "completed_at_ms":178...}
  ```

  This also marks the task finished, so it drops out of `dlog task list` and
  stops being reported as stranded. Calling it again later still seals anything
  newly recorded; the original completion time stands.

**Subagents: always seal before you return.** Your on-the-ground decisions
otherwise vanish when only a summary goes back to the parent. `dlog task done`
at the end of your task preserves them.

Use `dlog task done`, not `dlog bind --none`, to finish a task: `bind --none`
seals *everything* currently staged, so it would also bind the parent's (or a
sibling's) in-progress decisions to "no commit" — and sealed records are
immutable. `bind --none` is the escape hatch for staging you found stranded at
task start, not the normal end-of-task move.

(Restrict a seal to specific decisions with `--decision <id>` if needed.)

## Reconstruct context (before changing code)

Ask why code is the way it is. Two-stage: a compact list first, then drill in.

```bash
dlog why src/net/client.rs:42        # by file:line
dlog why "Client::connect"           # or by symbol path
```

```jsonc
{
  "query": { "type": "why", "target": "src/net/client.rs:42" },
  "resolved": { "node": "Client::connect", "resolution": "exact" },
  "results": [
    { "id": "01J...", "rationale_summary": "retry with exponential backoff...",
      "binding": { "type": "commit", "sha": "a3f..." },
      "staged": false, "superseded": false, "ts": 1781... }
  ],
  "truncated": false
}
```

Mind `resolution` — it states how well the answer fits the code *now*:

| resolution      | meaning |
|-----------------|---------|
| `exact`         | same node, unchanged — trust it |
| `drifted`       | same symbol, code changed since — the decision **may be stale** |
| `relocated`     | the node was renamed/moved (matched by structure) |
| `file_fallback` | no node match; these are file-level decisions |

Then fetch full detail (rejected alternatives, anchors, declared invariants):

```bash
dlog show 01J...            # one or more ids
```

## Other queries

```bash
dlog search --text "backoff"          # full-text over rationale/rejected
dlog invariants                       # live declared constraints
dlog invariants --scope src/net       # constraints in effect under a path
dlog context src/net/                 # decision summary for a path
dlog trace <id>                       # walk the caused_by chain (causes/effects)
dlog task list                        # open tasks: id, instruction, staged count
dlog task list --all --parent <id>    # include finished ones / only one task's children
```

Superseded decisions are hidden by default; add `--include-superseded` to
`why`/`search` for history. Staging is included by default and flagged
`"staged": true`.

Results are bounded to a context budget: `why`/`context`/`search` take
`--budget <CHARS>` (default 4096; `0` = unbounded). When results don't all fit,
they are emitted newest-first with shorter summaries and the envelope reports
`"elided": N` (how many live results were left out) alongside `"truncated"`.
Widen the budget, or `dlog show <id>` for the full record.

## Harness integration (optional)

dlog doesn't force you to record — it lowers the cost and the harness can nudge.
Two complementary aids:

- **Auto-seal commits** so you never lose a binding: either commit via
  `dlog commit -- -m "..."`, or install the repo hook once with
  `dlog hooks install` and then plain `git commit`s auto-seal staging.
- **A task-end reminder.** If your harness supports stop/end hooks (e.g. Claude
  Code's `Stop` hook), have it nudge when staging is non-empty — so on-the-ground
  decisions get sealed before the session ends:

  ```sh
  # fires when the agent stops; reminds if anything is still unsealed
  if [ "$(dlog status | grep -o '"staging_count":[0-9]*' | cut -d: -f2)" != "0" ]; then
    echo "dlog: unsealed decisions in staging — run 'dlog task done <id>' (or commit) before ending."
  fi
  ```

## Rules of thumb

- Record the *why*, not the *what* — the diff already has the what.
- Record reversals as new decisions with `--supersedes`; never silently change
  your mind.
- Check `resolution` before trusting a decision; `drifted` means verify.
- Subagents seal at task end. Everyone seals after committing.
