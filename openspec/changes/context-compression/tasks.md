# Tasks: Context Compression for Query Output

- [ ] **Task 1 — Add `elided` to the query envelope**

  In `src/output.rs`, add `pub elided: usize` to `QueryEnvelope<Q, R>` (serialized
  after `truncated`).

  Touch: `src/output.rs`.

  Verify: `cargo build` succeeds once callers are updated (Task 3); field appears
  in JSON.

- [ ] **Task 2 — Budget + adaptive summary in the shared compact path**

  In `src/commands/compact.rs`:
  - Add `ROW_OVERHEAD` (~96) and `SUMMARY_FLOOR` (48) consts; keep `SUMMARY_MAX`
    (140).
  - Give `summarize` a `width` parameter (cap at `width`, ellipsis as today).
  - Change `collect` to take a `budget: usize` (0 = unlimited) and return
    `(Vec<CompactRow>, truncated, elided)`. Compute the adaptive width
    `clamp(budget / max(candidates, 1) - ROW_OVERHEAD, FLOOR, MAX)` when budget
    > 0; include rows newest-first while `running + cost <= budget` and
    `rows.len() < limit`; count remaining in-scope rows as `elided`.
  - Keep `row_from` (used by `trace`) on the default `SUMMARY_MAX` width.

  Touch: `src/commands/compact.rs`.

  Verify: unit tests below.

- [ ] **Task 3 — Thread budget/elided through why, context, search**

  Add `--budget <CHARS>` (default 4096) to `WhyArgs`, `ContextArgs`, `SearchArgs`
  in `src/cli.rs`. In `commands/{why,context,search}.rs`, pass the budget to
  `collect` and set `elided` on the emitted `QueryEnvelope`.

  Touch: `src/cli.rs`, `src/commands/why.rs`, `src/commands/context.rs`,
  `src/commands/search.rs`.

  Verify: `cargo build`; `dlog why <target> --budget 200` emits fewer/terser rows
  with `elided > 0` when there are many decisions.

- [ ] **Task 4 — Tests**

  In `src/commands/compact.rs` tests: a tight budget yields fewer rows than a
  large one; `elided` equals the number of omitted in-scope rows; `truncated`
  reflects omission; adaptive width shrinks summaries under a tight budget but
  not under a generous one; `--budget 0` disables bounding (limit still applies).
  Update existing `collect` call sites/tests for the new signature.

  Touch: `src/commands/compact.rs` (+ any test that calls `collect`).

  Verify: `cargo test --all-features` green.

- [ ] **Task 5 — Docs + gate**

  Note `--budget` and the `elided` field in `templates/AGENTS.md` (queries
  section). Run the full CI gate.

  Touch: `templates/AGENTS.md`.

  Verify: `cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo clippy
  --all-targets --all-features`, `cargo test --all-features` all green.
