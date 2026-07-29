# Design: sharing a decision log between checkouts

## The shared unit is a sealed decision, and only that

`export` selects `staged = 0` rows. There is no flag that changes this, and
`import` rejects an input file containing a decision with `staged: true` or
without a binding (`invalid_export`, nothing written).

Both halves matter. The export side keeps an agent from leaking its own work in
progress. The import side keeps a hand-written JSONL file from *injecting* rows
into another store's staging area — which would let a file decide what someone
else's `dlog commit` seals. §8.2 puts "pending" only in staging; a decision that
arrives from outside was pending on a machine that is not this one, and there is
nothing here that could ever seal it correctly.

The same rule is what makes the whole change small: sealed rows are immutable
(§7.2), so combining two stores has no update path to reconcile.

## Import is an insert of unknown ids

Everything below follows from three facts about the existing schema:

1. The append-only guarantee is enforced by `BEFORE UPDATE` / `BEFORE DELETE`
   triggers on `decision` (`src/schema.sql`). `INSERT` was never blocked, so a
   sealed row can be written directly — no trigger to work around, no escape
   hatch to add.
2. ULIDs are globally unique, so "same id" means "same decision" and "unknown id"
   means "new decision". There is nothing to merge and no rename to detect.
3. `foreign_keys = ON` (`Store::init`), and three real FKs exist:
   `decision.supersedes → decision(id)`, `invariant.declared_by → decision(id)`,
   `task.parent_task_id → task(id)`. `caused_by` is a JSON array with **no** FK.

So `import` is: for each record, skip if the id is present, otherwise insert.
That is the algorithm in full.

### Ordering falls out of the ids

A referenced row always existed before the row referencing it, and ULIDs are
time-sortable — so a referent's id always sorts before its referrer's. Writing
**tasks ascending, then decisions ascending, then invariants ascending** therefore
satisfies all three FKs with no topological sort, on both the export and import
side. (Invariants come last because `declared_by` points at a decision.)

### `INSERT OR IGNORE` is the wrong primitive

It would also swallow CHECK and FK violations, turning a malformed input into a
silently short import. Instead: `SELECT 1 FROM <table> WHERE id = ?` decides
skip-vs-insert, and the `INSERT` itself is plain, so a violated CHECK (§8.2's
staged/binding invariant) is an error the caller sees.

A known id whose *content* differs is skipped, not compared. Two ULIDs collide
only if someone edited the file, and byte-comparing every field to produce an
error nobody can act on is not worth the code. Noted as a non-goal rather than
left implicit.

### All-or-nothing

The whole file is parsed and validated first, then applied in one transaction. A
half-applied import leaves a store whose `trace` walks into holes, and since the
main log is append-only the fix would be manual. Validation covers: the header,
the sealed-only rule, and dangling references — every `supersedes`, `task_id`,
`parent_task_id` and `declared_by` must resolve either inside the file or in the
receiving store. A dangling one is `dangling_reference` (with the offending ids)
rather than a raw FK `store_error`.

`caused_by` is exempt, deliberately. It carries no FK, and a missing cause simply
ends a `trace` walk early rather than corrupting anything. Import reports the
ones it saw as `dangling_caused_by` so the agent knows the DAG it received is a
fragment (§9.1 principle 2 — state, not a suggestion to go fetch more).

FTS needs no handling: `decision_fts_ai` fires on `INSERT`, so imported rows are
searchable immediately.

## Wire format

JSONL. One JSON object per line, discriminated by `type`.

```jsonc
{"type":"header","format":1,"schema_version":3,"exported_at":1753...}
{"type":"task","id":"01K…","parent_task_id":null,"instruction":"…","created_at_ms":…,"completed_at_ms":…}
{"type":"decision","id":"01K…","task_id":"01K…","agent":{"role":"implementer","model":"…","author":"lee@example.com"},
 "rationale":"…","rejected":[…],"caused_by":["01K…"],"supersedes":null,
 "anchors":[{"file":"src/net/client.rs","symbol_path":"Client::send","node_kind":"function_item",
             "structural_hash":"…","line_span":[40,58],"recorded_at_sha":"a3f…"}],
 "binding":{"type":"commit","sha":"a3f…"},"ts":…}
{"type":"invariant","id":"01K…","declared_by":"01K…","statement":"…","scope":"src/net","retired":0,"created_at_ms":…}
```

- `format` versions the file, independently of the store's `schema_version`.
  An importer that does not know a `format` refuses the file instead of guessing
  — the same posture `schema_too_new` takes (#60), and the same posture §10.3's
  `drifted` takes: do not answer confidently from a base that may have changed.
  `schema_version` is carried for diagnosis, not for gating.
- The decision record is `StoredDecision`'s existing serialization plus a `type`
  tag. Reusing it means the export shape cannot drift from `dlog show`'s.
- `staged` is never emitted; a record in a file is sealed by definition, and its
  `binding` says which kind (§8.2).
- `retired` is carried through even though nothing sets it today, so a future
  retire path does not need a format bump.

## Commands

```
dlog export --out <PATH> [--since <ULID|YYYY-MM-DD>]
dlog import <PATH|->
```

### Why `--out` instead of writing JSONL to stdout

§9.3 / `output.rs`: every invocation emits **exactly one** JSON document on
stdout, and agents parse it that way. Streaming JSONL there would make `export`
the one command with a different output contract — parsed differently, and with
no room left for a result envelope. So the data goes to a file and stdout keeps
the envelope:

```json
{"path":"decisions.jsonl","format":1,
 "exported":{"tasks":3,"decisions":41,"invariants":5},"since":"2026-07-01"}
```

`import` takes its input as a positional path, with `-` for stdin: stdin carries
no output contract, so the asymmetry costs nothing and the pipe case stays
available.

```json
{"imported":{"tasks":3,"decisions":41,"invariants":5},
 "skipped_existing":12,"dangling_caused_by":["01K…"]}
```

### `--since` carries its closure

`--since` takes a ULID (a decision id — compare lexicographically) or a
`YYYY-MM-DD` date (UTC midnight → `created_at_ms`); both reduce to a lower bound
over the same ordering.

The selected set is then closed over its FK-bearing references before writing:
each decision's `supersedes` chain, each decision's task and its
`parent_task_id` ancestors, and every invariant whose `declared_by` is in the
set. Without this an incremental export produces a file that cannot be imported
anywhere that lacks the older rows — the FK fires and the whole transaction
rolls back. Closing over it at export time is where the information is.

`caused_by` is *not* chased. It has no FK, chasing it would drag in most of the
log's history through one edge, and a missing cause degrades gracefully (above).

## `--author` (schema v3)

`src/migrations/003_decision_author.sql`:

```sql
ALTER TABLE decision ADD COLUMN agent_author TEXT;
```

`Agent` gains `author: Option<String>`, serialized with
`skip_serializing_if = "Option::is_none"` so nothing changes for a solo store.
`record --author <who>` sets it (`env = "DLOG_AUTHOR"` as a fallback, matching
the existing `DLOG_AGENT_*` treatment). It is never required — §7.3's minimal
required set stays `rationale` + anchor + agent role/model.

It lands with this change rather than after it because the export format has to
carry it from `format: 1`; adding it later would mean two file formats in
circulation for one field.

Placement decisions:

- **On `agent`, not a sibling field.** §7.4 already groups "who made this call"
  there; a human is another facet of that identity, not a separate axis.
- **In `show`, not in compact rows.** Compact rows are the token-saving stage
  (§9.1 principle 1); an author string on every row buys nothing until there is
  something to filter on, which is a non-goal here.
- **Explicit only.** No git `user.email` lookup — #58 deliberately made the
  workspace a dlog concept, and git stays optional.
- **No `.dlog/config`.** A per-checkout config file is the better ergonomic
  answer (an author is a property of the machine, not of each call) and is
  deliberately deferred: it introduces a configuration concept dlog does not have
  yet, and that is a change of its own.

## The store file is not the sharing mechanism

CLAUDE.md says concurrent writes are arbitrated by SQLite locking. That is true
on a local filesystem and false on NFS, SMB, and file-sync clients (Dropbox,
Drive), where SQLite's advisory locking either is not honoured or is defeated by
a sync daemon copying the file mid-write. The failure is a corrupted store, not
an error message.

So the README states plainly that `.dlog/` belongs on local disk and that
export/import — not a shared folder — is how a log reaches another person. This
is documentation, not enforcement: dlog cannot reliably tell what a path is
mounted from, and a wrong guess would block a legitimate setup.

## Privacy

A rationale can quote internal discussion or name a customer. Local-first plus an
explicit `export` keeps that opt-in: nothing leaves the machine unless someone
runs a command whose entire purpose is to make it leave. This is the concrete
reason the default store stays local — a remote-by-default store would remove the
opt-in without replacing it.

## Alternatives considered

- **JSONL on stdout** (`dlog export > out.jsonl`, as #64 sketched it). Reads
  better on a command line, but breaks the one-document-per-invocation contract
  every other command holds to, and leaves nowhere to report counts.
- **A single JSON array instead of JSONL.** Requires holding the whole log in
  memory on both sides and makes `cat a.jsonl b.jsonl` — the obvious way to
  combine two exports — stop working.
- **`INSERT OR IGNORE` / `ON CONFLICT DO NOTHING`.** Shorter, and it hides CHECK
  and FK violations as skips.
- **Per-record transactions,** so a bad line only loses itself. Cheaper to
  implement, and it leaves a partially-populated append-only log with no way back.
- **Exporting staging behind `--include-staged`.** Every flag like this is a flag
  someone eventually passes. #59's `bind --none` is the precedent for what
  happens when one agent's tooling can finalize another's pending work.
- **Chasing `caused_by` in the `--since` closure.** One edge into old history
  pulls in most of the log, defeating the point of an incremental export, for an
  edge that degrades safely when absent.
- **An `origin` column marking imported rows.** Deferred (issue #64, 論点3): ULIDs
  make mixing safe, `why` reads better without the split, and #60's migration path
  means the column can be added the day a real query needs it.

## Risks / mitigations

- **A shared log makes `context` and `why` noisier.** Real, and unaddressed here:
  `--budget` / `elided` (#33) and the rollups (#63) cut by recency, not
  relevance. Named as a non-goal so it is a known gap rather than a surprise.
- **Anchors from another checkout may not resolve.** Expected and already
  handled: §10.1 resolves identity against the *local* working tree, so an
  imported decision about code this checkout does not have degrades to
  `file_fallback` (§10.5) instead of failing.
- **Schema v3 on a store an older binary opens.** Covered by #60 — an older
  dlog refuses with `schema_too_new` rather than reading a column it does not
  know.
- **Someone edits the JSONL by hand.** Validation catches the structural cases
  (staged rows, missing bindings, dangling references, unknown `format`). Content
  edits under an existing id are skipped rather than detected; the file is a
  trusted-input format, not an authenticated one.
