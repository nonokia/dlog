# Proposal: Context Compression for Query Output

> dlog issue #33. First OpenSpec change in this repo (OpenSpec adopted from here
> on, design §12).

## What

Bound the size of the compact result payload returned by the list-style queries
(`dlog why`, `dlog context`, `dlog search`) so it fits an agent's context-window
budget, while preserving the most valuable signal. Concretely:

- An **output budget** (`--budget <CHARS>`, on by default with a generous cap):
  after the existing live-decisions scope and newest-first ordering, include
  result rows until the next row would exceed the budget; omit the rest.
- **Adaptive rationale summaries**: when the budget is tight, `rationale_summary`
  is trimmed shorter (down to a floor) so more decisions fit, rather than a few
  verbose ones.
- **Explicit elision**: add an `elided` count (live results omitted) to the
  envelope alongside the existing `truncated`, so the agent knows how much it is
  not seeing.

This is **presentation-time only** — it changes how results are summarized and
how many are emitted, not what is stored.

## Why

`dlog` is consumed inside an agent's context window, and "necessary history that
fits the window" is the whole point of the query layer (design §2.1, §9.1). Two
mechanisms already help: compact rows (id + one-line rationale summary + binding
+ ts, §9.1) and a count `--limit`. But a count limit is a poor proxy for size —
20 decisions with long rationales can still blow the budget, while 20 terse ones
waste little. A **size budget** with **adaptive summaries** makes "stay within
the window" the default, predictable behavior. Reporting `elided` keeps the
response self-describing via state, not suggestions (§9.1 principle 2): the agent
can decide to widen the budget or drill in with `dlog show`. This is the §11
"context compression" item, deferred until the tool worked — it now does.

## Non-goals

- No change to the stored data, schema, or recording path — presentation only.
- No semantic/LLM summarization of rationale; trimming stays a deterministic,
  first-line/character operation (agents do their own summarizing).
- `dlog trace` is out of scope for v1 (it is already bounded by `--depth` and
  uses a different row builder); budgeting it is a follow-up.
- No removal of the existing `--limit` (count cap); budget and limit coexist.
- No changes to `.github/` or the CI workflow.
