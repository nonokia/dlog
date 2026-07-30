# Tasks: `dlog export` / `dlog import`, and `record --author`

- [x] **Task 1 — Schema v3: `decision.agent_author`**

  Add `src/migrations/003_decision_author.sql` with the single
  `ALTER TABLE decision ADD COLUMN agent_author TEXT;` and register it in
  `MIGRATIONS` in `src/store.rs` (position 2 → version 3).

  Touch: `src/migrations/003_decision_author.sql`, `src/store.rs`.

  Verify: `cargo test` — a fresh store reports `schema_version == 3`; the
  existing upgrade test still passes with the column present afterwards; a store
  stamped at version 4 still fails with `schema_too_new`.

- [x] **Task 2 — `Agent.author` and `record --author`**

  Add `author: Option<String>` to `Agent` in `src/model.rs`
  (`skip_serializing_if = "Option::is_none"`). Persist and read it in
  `stage_decision` / `row_to_decision` in `src/store.rs`. Add
  `--author` (`env = "DLOG_AUTHOR"`, optional) to `RecordArgs` in `src/cli.rs`
  and thread it through `src/commands/record.rs`. Do **not** touch `CompactRow`
  in `src/commands/compact.rs`.

  Touch: `src/model.rs`, `src/store.rs`, `src/cli.rs`, `src/commands/record.rs`.

  Verify: unit tests — a decision recorded without `--author` round-trips with no
  `author` key in `dlog show` output; with `--author` the value appears under
  `agent`; compact rows (`why` / `context` / `search`) are byte-identical to
  before either way.

- [x] **Task 3 — Store reads for export**

  In `src/store.rs` add: `sealed_decision_ids(since: Option<&SinceBound>)`
  returning ids ascending; `get_task(id)` returning a full `TaskRow`-shaped row
  including `completed_at_ms`; and an invariant fetch carrying `retired` and
  `created_at_ms` (the existing `invariants_declared_by` drops both). Reuse
  `get_decision` (which already loads anchors) rather than adding a bulk fetch.

  Touch: `src/store.rs`.

  Verify: unit tests — `sealed_decision_ids` never returns a staged id, returns
  ascending ULID order, and honours the lower bound; `get_task` returns `None`
  for an unknown id; the invariant fetch returns retired rows too (export carries
  them; `dlog invariants` still filters).

- [x] **Task 4 — The JSONL record types**

  New `src/commands/share.rs` (shared by both commands): a `Record` enum with
  `#[serde(tag = "type", rename_all = "snake_case")]` over `Header` /
  `TaskRecord` / `StoredDecision` / `InvariantRecord`, `FORMAT_VERSION: u32 = 1`,
  and the `--since` bound parser (ULID or `YYYY-MM-DD` → epoch ms, UTC — written
  out rather than adding a date crate for one conversion). Add `Deserialize` to
  `StoredDecision` in `src/model.rs` with `#[serde(default)]` on every field that
  is skipped when empty. `TaskRecord` / `InvariantRecord` are the storage shapes
  and live in `src/store.rs` beside the query shapes they differ from.

  Touch: `src/commands/share.rs`, `src/commands/mod.rs`, `src/model.rs`,
  `src/store.rs`.

  Verify: unit tests — a `StoredDecision` with empty `rejected`/`caused_by`/
  `anchors` and no `supersedes` round-trips through serialize→deserialize
  unchanged; each record type serializes with its `type` tag; the `--since`
  parser accepts a ULID and `2026-07-01` and rejects `july` and `2026-13-01`.

- [x] **Task 5 — `dlog export`**

  `ExportArgs { out: PathBuf, since: Option<String>, db }` in `src/cli.rs`, a
  `Command::Export` arm in `src/lib.rs`, and `src/commands/export.rs`: select
  sealed ids, close over `supersedes` chains / task ancestry / declared
  invariants, write header + tasks + decisions + invariants each ascending by id,
  and `emit` the summary envelope.

  Touch: `src/cli.rs`, `src/lib.rs`, `src/commands/export.rs`.

  Verify: unit tests over an in-memory store — staged decisions never appear in
  the output; the header is line 1; ids are ascending within each type and tasks
  precede decisions precede invariants; a `--since` that excludes a superseded
  decision still emits it because a selected decision supersedes it; likewise for
  a parent task. Plus `cargo run -- export --out /tmp/x.jsonl` on this repo's own
  store producing a well-formed file.

- [x] **Task 6 — `dlog import`**

  `ImportArgs { path: String, db }` (`-` = stdin) in `src/cli.rs`, a
  `Command::Import` arm in `src/lib.rs`, and `src/commands/import.rs`: parse the
  whole file, validate (known `format`; no staged/binding-less decision; no
  dangling `supersedes` / `task_id` / `parent_task_id` / `declared_by` against
  file ∪ store), then apply in one transaction. Add `Store::import_all` to
  `src/store.rs` — one call so the transaction stays inside the store layer, as
  `seal_staged` does — doing an explicit existence check per row and then a plain
  `INSERT`, **not** `INSERT OR IGNORE`, so CHECK/FK violations surface. New error
  codes: `invalid_export`, `unsupported_format`, `dangling_reference`.

  Touch: `src/cli.rs`, `src/lib.rs`, `src/commands/import.rs`, `src/store.rs`.

  Verify: unit tests — importing the same file twice yields
  `skipped_existing == <all>` and inserts nothing the second time; a file with a
  `staged: true` decision is rejected whole (store unchanged); a dangling
  `supersedes` gives `dangling_reference` and rolls back; a dangling `caused_by`
  imports fine and is reported; an imported decision is findable via
  `dlog search` (proving the FTS trigger fired).

  Plus an end-to-end run: two stores A and B in temp dirs — in A
  `dlog init`, `task start`, `record`, `task done`; `dlog export --out out.jsonl`;
  in B `dlog init` then `dlog import out.jsonl`; `dlog show <id>` in B matches A;
  `dlog why <file:line>` in B returns the decision (`file_fallback` is fine —
  B has no working tree for it); re-importing reports all skipped; a decision
  left staged in A never appears in `out.jsonl`.

- [x] **Task 7 — Documentation**

  README: add `export` / `import` to the command list, an "Sharing a log"
  subsection under Concepts (sealed-only, ULID identity, no merge), and an
  explicit paragraph that `.dlog/` belongs on local disk — NFS / SMB / Dropbox
  are unsupported because SQLite locking is not reliable there.
  `templates/AGENTS.md`: `--author` in the Identity section, and a short
  export/import recipe.

  Touch: `README.md`, `templates/AGENTS.md`.

  Verify: the README command list matches `dlog --help`; every command shown in
  `templates/AGENTS.md` runs as written against a scratch store.

- [x] **Task 8 — Design record**

  Append the change to §13 of `agent-first-vcs-design.md` (implementation notes),
  covering: the sharing unit fixed to sealed-only, why import needs no merge,
  `--author` as an optional field with no git lookup, and OTel recorded as
  considered and not adopted — neither as storage nor as a side channel.

  Touch: `agent-first-vcs-design.md`.

  Verify: §13 reads consistently with the other entries; no settled §11 decision
  is re-litigated.
