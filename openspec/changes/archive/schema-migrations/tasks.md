# Tasks: Versioned schema migrations

- [x] **Task 1 — The migration sequence**

  In `src/store.rs` replace `const SCHEMA_SQL` with `const MIGRATIONS: &[&str]`
  (index + 1 = version, `schema.sql` as the only entry) and derive
  `pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64`. Add the bootstrap
  `SCHEMA_META_DDL` constant. Note the freeze in the `src/schema.sql` header:
  it is the v1 baseline, later changes go in `src/migrations/`.

  Touch: `src/store.rs`, `src/schema.sql`.

  Verify: `cargo build`; `Store::open_in_memory()` still creates a usable store
  and reports `schema_version == 1`.

- [x] **Task 2 — Apply only what's pending, transactionally**

  Rewrite `Store::migrate` to bootstrap `schema_meta`, read the current version
  (0 when absent), skip already-applied entries, apply the rest via
  `execute_batch` inside one `unchecked_transaction`, and write the version back
  with `ON CONFLICT DO UPDATE`. Make `schema_version()` return 0 rather than
  erroring when the row is missing.

  Touch: `src/store.rs`.

  Verify: unit tests in `store::tests` — a fresh store lands on
  `SCHEMA_VERSION`; `migrate()` twice is a no-op; a file store opened, closed,
  and reopened keeps its rows and version.

- [x] **Task 3 — Refuse a store from a newer dlog**

  Add `OpenError { Sqlite, SchemaTooNew { found, supported } }` in
  `src/store.rs` (with `Display`, `std::error::Error`, `From<rusqlite::Error>`)
  and return it from `open` / `open_in_memory` / `init` / `migrate`. Add
  `impl From<OpenError> for AppError` in `src/commands/mod.rs` mapping to codes
  `schema_too_new` / `store_error`.

  Touch: `src/store.rs`, `src/commands/mod.rs`.

  Verify: unit test — writing `schema_version = 999` into a store and reopening
  it yields `OpenError::SchemaTooNew`, and the mapped `AppError.code` is
  `schema_too_new`. `cargo build` stays clean at every call site of
  `Store::open`.

- [x] **Task 4 — Upgrade path from a v1 store**

  Add a test that builds a *pre-migration* store by hand (raw connection,
  `execute_batch(schema.sql)`, `schema_version = 1`), stages a decision through
  it, then opens it with `Store::open` and asserts the version is
  `SCHEMA_VERSION`, the decision is still readable, and reopening changes
  nothing further. This is the regression guard for every future migration.

  Touch: `src/store.rs`.

  Verify: `cargo test`.

- [x] **Task 5 — Docs + gate**

  `CLAUDE.md`: describe the migration sequence where it currently says
  "idempotent migrations". `agent-first-vcs-design.md` §13: strike the "no ALTER
  path" reason from the deferred-completion-column note and record the mechanism
  under 意図的な改良.

  Touch: `CLAUDE.md`, `agent-first-vcs-design.md`.

  Verify: `cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo clippy
  --all-targets --all-features`, `cargo test --all-features` all green.
