# Tasks: `dlog task list`, task completion, stranded tasks

- [x] **Task 1 — Schema v2: `task.completed_at_ms`**

  Add `src/migrations/002_task_completed_at.sql` with the single
  `ALTER TABLE task ADD COLUMN completed_at_ms INTEGER;` and register it in
  `MIGRATIONS` in `src/store.rs` (the first user of #60).

  Touch: `src/migrations/002_task_completed_at.sql`, `src/store.rs`.

  Verify: `cargo test` — a fresh store reports `schema_version == 2`, and the
  existing v1-upgrade test still passes with the column present afterwards.

- [x] **Task 2 — Store queries**

  In `src/store.rs` add `TaskRow` / `StrandedTask` structs and
  `list_tasks(include_completed, parent)`, `complete_task(id)` (stamps only when
  `completed_at_ms IS NULL`, returns the effective value), `stranded_tasks(limit)`,
  plus `stranded_task_count` on `StoreStatus`.

  Touch: `src/store.rs`.

  Verify: unit tests — `list_tasks` filters open vs all and by parent, reports
  `staged_count` per task, and orders newest-first; `complete_task` keeps the
  first timestamp on a second call; `stranded_tasks` returns only unfinished
  tasks holding staged decisions (not completed ones, not task-less staging).

- [x] **Task 3 — `dlog task done` stamps completion**

  In `src/commands/task.rs` call `complete_task` after the seal and add
  `completed_at_ms` to `DoneResult`.

  Touch: `src/commands/task.rs`.

  Verify: unit tests — `done` sets the timestamp; a second `done` returns the
  same timestamp; the task then reads as completed via `list_tasks`.

- [x] **Task 4 — `dlog task list`**

  `src/cli.rs`: `TaskCommand::List { open, all, parent, limit, db }` with
  `--open` and `--all` mutually exclusive, `--limit` defaulting to 20.
  `src/commands/task.rs`: build a `QueryEnvelope` with `resolved: None`, rows
  summarised via `compact::summarize` at `SUMMARY_MAX`, `truncated`/`elided`
  from the limit.

  Touch: `src/cli.rs`, `src/commands/task.rs`, `src/commands/compact.rs`
  (make `SUMMARY_MAX`/`summarize` reachable from `task`).

  Verify: cli tests — `task list` parses bare, `--open --all` is a usage error.
  Handler tests — open-only by default, `--all` includes completed, `--parent`
  narrows to children, `--limit` truncates and reports `elided`.

- [x] **Task 5 — Stranded tasks in `dlog status`**

  In `src/commands/status.rs` add `stranded_tasks` to the result document,
  mapping `StrandedTask` to a compact output row (`instruction_summary`).

  Touch: `src/commands/status.rs`.

  Verify: unit test — a store with one open task holding staged decisions and
  one completed task reports exactly the open one, with its `staged_count` and
  `oldest_staged_ms`; the field is an empty array when nothing is stranded.

- [x] **Task 6 — Docs + gate**

  `templates/AGENTS.md`: at task start, read `stranded_tasks` from `status` and
  hand off with `record --task` / `task done <id>`; add `task list` to the
  queries. `README.md`: add `dlog task list` to the command list and mention
  stranded tasks under `status`. `agent-first-vcs-design.md` §13: move the
  "completion state not in the schema" note out of 現実解・未実装.

  Touch: `templates/AGENTS.md`, `README.md`, `agent-first-vcs-design.md`.

  Verify: `cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo clippy
  --all-targets --all-features`, `cargo test --all-features` all green; manual
  end-to-end `task start` → `record --task` → `status` (stranded) → `task list`
  → `task done` → `task list --all` in a temp workspace.
