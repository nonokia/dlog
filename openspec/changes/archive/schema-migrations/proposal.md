# Proposal: Versioned schema migrations (an ALTER path)

> Resolves #60. Mechanism only — the first schema change that uses it ships with
> the `task-list` change (#61).

## What

Replace the single idempotent replay of `schema.sql` with a **versioned
migration sequence**. `Store::migrate` reads `schema_meta.schema_version` and
applies only the migrations above it, inside one transaction, then writes the
new version back. Opening a store written by a *newer* dlog is an explicit
error instead of a silent partial-compatibility mode.

## Why

`Store::migrate` today is `execute_batch(schema.sql)` where every statement is
`CREATE TABLE/INDEX/TRIGGER IF NOT EXISTS`. That is idempotent-replay only:

- SQLite has no `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so adding a column
  to `schema.sql` fails on the *second* open of every existing store. There is
  no way to evolve a table.
- `SCHEMA_VERSION` and `schema_meta` already exist but are decorative. The
  version is inserted once (`ON CONFLICT DO NOTHING`) and only ever read back
  for display by `dlog status`; nothing branches on it. A store at an older
  version is indistinguishable from a current one.
- A store written by a newer dlog opens happily against an older binary, which
  will then read tables whose meaning has changed.

This has already cost a design decision: the `task-lifecycle` change (§13,
"現実解・未実装") records "no completion column on `task`" with the missing ALTER
path as one of its stated reasons. #61 needs exactly that column, and every
future schema change hits the same wall.

## Non-goals

- **No actual schema change here.** The sequence ships with one entry (the
  existing `schema.sql` as version 1). #61 adds version 2.
- **No down migrations.** The main log is append-only (§7.2); a rollback path
  for it is meaningless, and the store is a local cache of decisions, not a
  system of record to be restored to an earlier shape.
- **No automatic backup/copy before migrating.** A migration runs in a
  transaction and rolls back as a unit on failure.
- **No support for other database engines** (§6.1 fixes SQLite, no daemon).
- No change to any command's behaviour or output, other than `dlog status`
  continuing to report `schema_version` (now genuinely derived) and the new
  `schema_too_new` error being possible on any command that opens a store.

## Design references

§6.1 (no daemon; each invocation opens SQLite directly), §7.2 (append-only main
log — why there is no down path), §9.2 (`dlog status` reports the schema
version), §13 (the deferred completion column and its stated reason).
