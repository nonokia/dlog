# Tasks: rollups for `context` and `trace`

- [x] **Task 1 — `(file, decision)` pairs from the store**

  Add `Store::anchors_under_path(path) -> Vec<(String, String)>`, newest-first by
  decision id, same scope and LIKE-escaping as `decision_ids_under_path` (`.` =
  the whole log).

  Touch: `src/store.rs`.

  Verify: `cargo build`; the context tests below exercise the boundary and
  metacharacter cases through it.

- [x] **Task 2 — Reusable width/cost helpers**

  Make `compact::adaptive_width` `pub(crate)`, split `row_from` into
  `row_from_at(decision, superseded, width)`, and add `row_cost(&CompactRow)` so
  callers doing their own budgeting don't re-derive `ROW_OVERHEAD`.

  Touch: `src/commands/compact.rs`.

  Verify: existing compact tests unchanged and green.

- [x] **Task 3 — `context` rollup**

  `--rollup` / `--flat` (mutually exclusive) on `ContextArgs`; default rollup iff
  the live decisions under the path touch more than one file. Group by file in
  the command layer, emit `{file, count, latest}` rows newest-first under
  `--limit` / `--budget`, and report the mode in `query.mode`.

  Touch: `src/cli.rs`, `src/commands/context.rs`.

  Verify: `a_directory_rolls_up_per_file_by_default`,
  `a_single_file_answers_flat_and_flat_forces_the_stream`,
  `a_decision_spanning_two_files_counts_once_per_file_but_lists_once_flat`,
  `rollup_respects_budget_and_reports_elided_files`,
  `superseded_decisions_are_out_of_the_rollup_count`.

- [x] **Task 4 — Invariants in the `context` response**

  Make `invariants::scope_matches` `pub(crate)` and reuse it; add
  `invariants` + `invariants_elided` to the context envelope, capped at
  `INVARIANT_CAP = 20`, suppressed by `--no-invariants`.

  Touch: `src/commands/invariants.rs`, `src/commands/context.rs`, `src/cli.rs`.

  Verify: `invariants_in_effect_ride_along_unless_suppressed`,
  `invariants_are_capped_and_report_what_was_dropped`.

- [x] **Task 5 — `trace` nested DAG + `--budget`**

  Replace the flat `TraceRow` lists with `TraceNode { row, depth, edges }`. Split
  the walk into `reachable` (ids + BFS tree) and `materialize` (rows, spending a
  shared `Purse`), so the cut lands on branch boundaries and `elided` counts what
  the budget dropped. Add `--budget` (default 4096) to `TraceArgs`.

  Touch: `src/cli.rs`, `src/commands/trace.rs`.

  Verify: `branches_stay_separate_instead_of_flattening`,
  `budget_cuts_whole_branches_and_reports_elided`,
  `budget_is_shared_between_the_two_directions`, plus the pre-existing depth /
  cycle / unknown-root tests still green.

- [x] **Task 6 — Docs + gate**

  Document the rollup response, `--flat` / `--no-invariants`, `trace --budget`
  and `edges` in `templates/AGENTS.md`; update the command surface in
  `README.md` and the query-principles section of `CLAUDE.md`.

  Touch: `templates/AGENTS.md`, `README.md`, `CLAUDE.md`.

  Verify: `cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo clippy
  --all-targets --all-features`, `cargo test --all-features` all green; an
  end-to-end `context <dir>` shows a rollup with invariants and `trace <id>`
  shows nested `edges`.
