# Proposal: rollups for `context` and `trace`

> dlog issue #63.

## What

Both `dlog context` and `dlog trace` currently flatten structured data into one
list. This change gives each one back the structure it is throwing away, and
brings `trace` under the context budget of #33.

- **`context` rolls up by file.** For a directory, the default response becomes
  one row per file — `count` of live decisions plus the `latest` one — instead of
  every decision in one stream. The flat list stays available via `--flat`; an
  exact file path is still flat, because there is nothing to group.
- **`context` carries the invariants in effect at the path.** They are what an
  agent must read *before* touching the code, so they ship with the context
  restore instead of requiring a second `dlog invariants --scope` call.
  `--no-invariants` opts out.
- **`trace` gets `--budget`** — the item #33 explicitly deferred.
- **`trace` output keeps the DAG shape.** `upstream` / `downstream` become nested
  nodes with `edges`, so a decision that caused three others reads as one node
  with three edges rather than three sibling rows that no longer say what they
  branched from.

## Why

§3 makes `context` the task-start command: the thing an agent runs before it
touches an area. On a real directory that answer is dominated by volume — the
budget from #33 fills up with whatever is newest and `elided` climbs, and the
agent learns nothing about *where* the decisions are. "This directory has 40
decisions, 22 of them in `login.rs`" is the shape of the answer; a rollup says it
in a fraction of the payload, and `dlog context <that file>` is the drill-down —
the same two-stage retrieval §9.1 already applies to decisions, applied one level
up.

Invariants belong in the same response for the same reason: §7.1 makes them
longer-lived than the decision that declared them precisely because they
constrain whoever comes next, and the agent that has to remember to ask for them
separately is the agent that won't.

For `trace`, §4 says decisions form a **DAG**, and a flat `upstream` /
`downstream` pair is the one place dlog reports that DAG as a list. Depth tags
tell you how far a node is but not what it hangs off, so a branch point — the
interesting part of a causal chain — is unreadable. And with only `--depth` to
bound it, a deep chain can still consume the whole context window, which is what
`--budget` exists to prevent.

## Non-goals

- No stored derived data or pre-aggregation. Like #33, this is **presentation
  time only** — rollups are computed per query.
- No LLM summarization; `rationale_summary` stays a deterministic first-line
  trim.
- No change to `why` or `search` output shapes; the compact list is right for
  them.
- No change to the causal scope of `trace`: superseded decisions stay visible
  there (history is the point) rather than adopting `why`'s live-only default.
- No new ranking or "most important file" scoring. A rollup reports counts and
  recency, both facts (§9.1 principle 2).
