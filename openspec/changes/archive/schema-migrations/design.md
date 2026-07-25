# Design: Versioned schema migrations

## The sequence

```rust
/// Schema migrations in application order. A migration's version is its index
/// + 1: `MIGRATIONS[0]` takes an empty store to version 1.
const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),   // v1 — the baseline
];

/// The schema version this binary understands.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;
```

Position *is* the version, so the two can't drift apart — there is no
hand-maintained `(version, sql)` pair to get wrong, and `SCHEMA_VERSION` stops
being a constant someone has to remember to bump.

`schema.sql` stays as version 1 and is **frozen**: it is the baseline every
existing store already has (all of them recorded `schema_version = 1`), so
replaying it must remain a no-op. Later changes are separate files under
`src/migrations/`, applied exactly once each — which is what buys the ALTER
path, since a statement that runs once needn't be idempotent.

The trade-off: `schema.sql` no longer shows the current shape of the schema,
only its starting shape. Called out in a header comment on both files.

## Applying

```rust
fn migrate(&self) -> Result<(), OpenError> {
    self.conn.execute_batch(SCHEMA_META_DDL)?;      // bootstrap the ledger
    let current = self.schema_version()?;           // 0 on an empty store
    if current > SCHEMA_VERSION {
        return Err(OpenError::SchemaTooNew { found: current, supported: SCHEMA_VERSION });
    }
    if current == SCHEMA_VERSION { return Ok(()); }

    let tx = self.conn.unchecked_transaction()?;
    for sql in MIGRATIONS.iter().skip(current.max(0) as usize) {
        tx.execute_batch(sql)?;
    }
    tx.execute(
        "INSERT INTO schema_meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SCHEMA_VERSION.to_string()],
    )?;
    tx.commit()
}
```

Three things worth naming:

- **The ledger is bootstrapped first.** `schema_meta` is defined *inside*
  `schema.sql`, so on an empty store we can't read the version before creating
  the very table that holds it. The bootstrap DDL is the same
  `CREATE TABLE IF NOT EXISTS schema_meta` statement, and running it twice is
  free.
- **Version 0 means "nothing applied"** — a fresh file gets every migration in
  order, so a new store and an upgraded old store converge on the same schema.
  An existing store already reads 1 and skips the baseline.
- **The write is `DO UPDATE`, not `DO NOTHING`.** The current insert-once
  behaviour is precisely why the version never moved. SQLite runs DDL inside
  transactions, so a failing migration leaves the store at its previous version
  rather than half-migrated.

## Refusing a newer store

`Store::open` gains an error type so this is a distinguishable condition rather
than a generic store failure:

```rust
pub enum OpenError {
    Sqlite(rusqlite::Error),
    SchemaTooNew { found: i64, supported: i64 },
}
```

`AppError` maps it to `{"error":{"code":"schema_too_new", ...}}` (exit 1), with
the found and supported versions in the message so the agent can act on it —
"upgrade dlog", not "the store is corrupt". Everything else keeps mapping to
`store_error`.

The alternative — carry on and hope — is worse in exactly the way dlog cares
about: an older binary reading a newer store returns *decisions* that look fine
and may be missing whatever the new columns mean. Failing loudly is the same
instinct as `resolution: drifted` (§10.3): never present an answer as sound
when its basis has changed underneath.

Downgrade detection is the only direction that needs a check. Going forward is
handled by applying migrations; going backward can't be handled at all, so it
is reported.

## Alternatives considered

- **`PRAGMA user_version`.** The idiomatic SQLite spelling, and it needs no
  bootstrap. Rejected because every existing store already carries its version
  in `schema_meta` while `user_version` reads 0 there, so adoption would need a
  reconciliation step — and `schema_meta` is already what `dlog status`
  reports.
- **Hand-written `(version, sql)` pairs**, as sketched in #60. Equivalent, but
  it adds a number that can disagree with the array it sits in. Index + 1 is
  the same information with one fewer thing to keep consistent.
- **Keep `schema.sql` as the whole schema and diff at runtime** (introspect
  `PRAGMA table_info` and add what's missing). Self-healing but unpredictable:
  it can't express data backfills or renames, and the code that decides what
  "missing" means becomes the real schema. Rejected.
- **Tolerate a newer store read-only.** Would need per-version knowledge of
  which reads stay valid — the maintenance cost of a compatibility matrix for a
  local store that a `dlog` upgrade fixes in one command. Rejected.

## Risks / mitigations

- **A migration that fails midway.** Wrapped in one transaction, so the store
  stays at its previous version and the next open retries. Migrations must not
  contain their own `BEGIN`/`COMMIT`.
- **A migration that trips the append-only triggers.** `decision` rows with
  `staged = 0` reject UPDATE and DELETE, so a future data-backfill migration
  touching the main log will abort — by design (§7.2). A migration needing that
  has to drop and recreate the trigger deliberately, in its own file, where it
  is reviewable.
- **Two processes opening an unmigrated store at once.** SQLite's write lock
  serialises them; the loser either waits and then finds `current ==
  SCHEMA_VERSION` (no-op) or gets a busy error and is retried by the agent, the
  same as any other concurrent write (§6.1).
