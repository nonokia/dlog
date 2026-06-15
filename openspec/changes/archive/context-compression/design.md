# Design: Context Compression for Query Output

## Where it plugs in

`why`, `context`, and `search` all build their results through
`commands/compact.rs::collect(store, ids, include_superseded, limit)`, which
already applies the live-decisions scope (superseded excluded by default),
caps at `limit`, and returns `(rows, truncated)`. Compression is a natural
extension of `collect` plus the query envelopes, so all three commands gain it
through one change.

`output.rs::QueryEnvelope<Q, R>` carries `results` + `truncated`; we add an
`elided` count there. `trace` builds rows via `compact::row_from` (per DAG node)
and is intentionally left out of v1 (see Non-goals).

## Budget model

- A new `--budget <CHARS>` flag (shared by why/context/search), defaulting to a
  generous value (proposed **4096**) so "fits the window" is the default. The
  sentinel `--budget 0` disables the budget (old behavior: bounded only by
  `--limit`).
- Cost estimate per row: `rationale_summary.chars().count() + ROW_OVERHEAD`,
  where `ROW_OVERHEAD` (~96) approximates the fixed JSON of id + binding + flags
  + ts. This avoids serializing twice; it is an estimate, not exact bytes —
  acceptable since the budget is a soft guardrail.
- Inclusion: iterate the (already newest-first, scope-filtered) candidates; keep
  a running total; include a row while `total + cost <= budget`; otherwise stop.
  `limit` still applies as an independent upper bound on row count.

## Adaptive summaries

To prefer "more, terser" over "few, verbose" under a tight budget:

- `compact::summarize` gains a width parameter. The effective width starts at the
  current default (140) and, when the number of in-scope candidates is large
  relative to the budget, steps down toward a floor (proposed **48** chars) so
  additional decisions fit.
- Concretely: `width = clamp(budget / max(candidate_count, 1) - ROW_OVERHEAD, 48, 140)`.
  This is deterministic and needs no second pass. `--summary-width <N>` may
  override it (optional; can be deferred).

## Envelope changes

`QueryEnvelope` gains:

- `elided: usize` — count of in-scope (live) results not emitted because of the
  budget or limit. `truncated` stays as the boolean "there was more"; `elided`
  quantifies it. Both are pure state (§9.1 principle 2) — no "next command" hint.

`collect` returns `(rows, truncated, elided)`; callers thread `elided` into the
envelope. `trace` is unaffected.

## Alternatives considered

- **Token-based budget** (vs characters): closer to real context cost but needs a
  tokenizer dependency; characters are a good, dependency-free proxy for a soft
  guardrail. Chosen: characters.
- **Per-row `--brief` mode** (drop binding/ts): simpler but coarse; the adaptive
  width achieves smoother control. Could be added later.
- **Server-side rollup** (group `context` by file with counts): valuable for very
  large directories but a bigger feature; deferred.

## Risks / mitigations

- Changing the default to a bounded budget alters current output for large result
  sets. Mitigation: the budget is generous (4096) and `--budget 0` restores the
  old behavior; `elided` makes the omission explicit and discoverable.
- Estimate vs actual size drift: acceptable for a soft guardrail; the floor and
  overhead constants are tunable.
