# Tasks: Task Lifecycle — the non-code seal trigger

- [ ] **Task 1 — Store queries for task-scoped sealing**

  In `src/store.rs` add `task_exists(&self, id: &str) -> rusqlite::Result<bool>`
  and `staged_decision_ids_for_task(&self, task_id: &str) ->
  rusqlite::Result<Vec<String>>` (`WHERE task_id = ?1 AND staged = 1 ORDER BY
  id`, using the existing `idx_decision_task`).

  Touch: `src/store.rs`.

  Verify: unit tests in `store::tests` — staged decisions are returned for their
  task only, sealed ones are excluded, and an unknown task yields an empty vec /
  `task_exists == false`.

- [ ] **Task 2 — `dlog task` CLI surface**

  In `src/cli.rs` add `TaskArgs { command: TaskCommand }` with a `TaskCommand`
  enum (`Start { parent, instruction, db }`, `Done { id, db }`), a
  `Command::Task(TaskArgs)` variant, and a `"task"` arm in `Command::name()`.
  Dispatch from `src/lib.rs`. Register the module in `src/commands/mod.rs`.

  Touch: `src/cli.rs`, `src/lib.rs`, `src/commands/mod.rs`.

  Verify: `cargo build`; `dlog task start --help` and `dlog task done --help`
  parse; `dlog task` with no subcommand is a usage error (exit 2). Add cli tests
  alongside the existing ones.

- [ ] **Task 3 — `dlog task start`**

  New `src/commands/task.rs`. `start` validates `--parent` with `task_exists`
  (error `unknown_task`), then calls the so-far-unused `Store::insert_task` and
  emits `{id, parent_task_id?, instruction?}` (optional fields skipped when
  absent).

  Touch: `src/commands/task.rs`.

  Verify: unit tests — a root task returns an id and no parent; `--parent` with a
  real id is stored and round-trips; an unknown parent errors with
  `unknown_task`.

- [ ] **Task 4 — `dlog task done`**

  In `src/commands/task.rs`, `done` errors `unknown_task` for an unknown id,
  otherwise seals that task's staged decisions via
  `seal_staged(&Binding::None, Some(&ids))` — passing the empty slice explicitly
  so zero staged decisions never falls through to "seal everything" — and emits
  `{task, count, sealed, binding}`.

  Touch: `src/commands/task.rs`.

  Verify: unit tests — seals only the finishing task's decisions and leaves
  another task's staging untouched (the §8.3 subagent case); a task with nothing
  staged returns `count: 0` and is not an error; an unknown id errors; a sealed
  decision reads back with `binding {"type":"none"}` and `staged: false`.

- [ ] **Task 5 — Docs + gate**

  `templates/AGENTS.md`: add the `task start` → `record --task` → `task done`
  flow, and make the subagent rule point at `task done` (keeping `bind --none`
  as the escape hatch for stranded staging). `README.md`: add both subcommands to
  the command list. `agent-first-vcs-design.md` §13: move `dlog task done` out of
  "現実解・未実装" — it is now implemented — and note that the task hierarchy is
  populated by `task start`.

  Touch: `templates/AGENTS.md`, `README.md`, `agent-first-vcs-design.md`.

  Verify: `cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo clippy
  --all-targets --all-features`, `cargo test --all-features` all green; manual
  end-to-end `task start` → `record --task` → `task done` in a temp workspace.
