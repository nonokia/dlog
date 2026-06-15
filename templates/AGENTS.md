# dlog — agent instructions

> Drop this into your repo's `AGENTS.md` / `CLAUDE.md` (or paste the rules
> below) so agents record and reconstruct decisions with `dlog`.

`dlog` is an agent-first **decision log** that sits *alongside* Git. Git records
*what* changed; `dlog` records the *why* — the rationale, rejected alternatives,
assumptions, and the instruction behind code — so you can reconstruct context
across sessions and hand-offs. It is consumed via a CLI with JSON in/out; every
command prints one JSON document. A failure is `{"error":{"code","message"}}`
with exit code 1; a usage error exits 2.

## Setup (once per session)

Set your identity via environment variables so you don't repeat it on every call:

```bash
export DLOG_AGENT_ROLE=implementer   # or reviewer, investigator, ...
export DLOG_AGENT_MODEL=<your-model-id>
export DLOG_AGENT_SESSION=<session-id>   # optional
```

The store lives at `.dlog/dlog.db` in the repo (override with `DLOG_DB`). It is
created on first use.

## At the start of a task

Check the store state. If decisions are stranded in staging (e.g. a plain
`git commit` was made without sealing), deal with them before starting:

```bash
dlog status
# {"staging_count":N,"oldest_staged_ms":...,"schema_version":1}
```

If `staging_count > 0` and you know which commit they belong to, seal them with
`dlog bind <sha>`; otherwise seal as non-code with `dlog bind --none`.

## Record a decision (the moment you make one)

Record **as you decide**, before committing — rejected attempts never reach a
commit, so commit-time-only recording loses them. Keep it low-friction: only
`--rationale`, at least one `--file` anchor, and your identity are required.

```bash
dlog record \
  --rationale "retry with exponential backoff; the upstream API is flaky" \
  --file src/net/client.rs:42
# {"id":"01J...","staged":true}
```

Anchor with `FILE`, `FILE:LINE`, or `FILE:START-END`. For Rust files the
enclosing definition (symbol + structural hash) is captured automatically so the
decision survives refactors; other files anchor at file level.

Lower-friction shortcuts:

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
- `--task <id>` `--instruction "the original human ask"` — tie decisions to a task.
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

- **At the end of a task with no commit** (investigation, review), seal as none:

  ```bash
  dlog bind --none
  ```

**Subagents: always seal before you return.** Your on-the-ground decisions
otherwise vanish when only a summary goes back to the parent. Sealing as
`--none` at task end preserves them.

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
    echo "dlog: unsealed decisions in staging — run 'dlog bind --none' (or commit) before ending."
  fi
  ```

## Rules of thumb

- Record the *why*, not the *what* — the diff already has the what.
- Record reversals as new decisions with `--supersedes`; never silently change
  your mind.
- Check `resolution` before trusting a decision; `drifted` means verify.
- Subagents seal at task end. Everyone seals after committing.
