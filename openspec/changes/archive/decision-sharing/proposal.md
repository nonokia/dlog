# Proposal: sharing a decision log between checkouts

> dlog issue #64.

## What

dlog is local-only: there is no command that gets a decision out of one store and
into another. This change adds the smallest thing that makes a log shareable
between people, and nothing else.

- **`dlog export --out <PATH>`** writes **sealed decisions only** as JSONL —
  decisions with their anchors, the tasks they belong to, and the invariants they
  declared. `--since` cuts by time, carrying along whatever the cut needs to stay
  referentially whole. stdout keeps the usual single-document envelope (§9.3),
  reporting what was written.
- **`dlog import <PATH|->`** reads that JSONL back into another store. Known ids
  are skipped; unknown ids are inserted. That is the entire algorithm.
- **`record --author`** adds an optional human identity to the agent block
  (schema v3), so a shared log can say *whose* agent made a call.
- **The store file itself is documented as not shareable.** Putting `.dlog/` on
  NFS / Dropbox / SMB is called out as unsupported in the README.

## Why

§2.2 makes the unit of record a decision precisely because a decision outlives
the commit that carried it. On a team it also outlives the person: the agent that
has to reconstruct why `authenticate` retries three times does not care whose
session recorded that, only that it can read it. Today it cannot, because the
answer is in someone else's `.dlog/dlog.db`.

The precondition is already met. #58 made anchors root-relative, so the same file
has the same anchor spelling in every checkout — the reason a decision recorded
on one machine can resolve on another at all.

And the merge problem that usually makes this hard does not exist here. Sealed
rows are immutable (§7.2, enforced by the BEFORE UPDATE/DELETE triggers in
`schema.sql`), ids are ULIDs and so globally unique, and the append-only
guarantee never covered `INSERT`. Two stores can therefore be combined by
inserting the rows one of them has not seen — no conflict resolution, no
vector clocks, no reconciliation pass. That is why this stays consistent with
"No CRDT, no distributed sync" (§6, CLAUDE.md): there is no sync engine here,
only a serialization format and a replay path.

Fixing the shared unit to **sealed only** is what buys that simplicity. Staging
is one agent's live work area; #59 already showed what touching another agent's
staging costs — `bind --none` sealing the whole staging area is unrecoverable
because the main log is append-only. Sharing staging would reintroduce that
across machines, where it cannot even be noticed. Sealed rows have no such
hazard: they are already final.

Export/import is also the prerequisite for every other option in #64. Whether a
remote store follows or not, a shared log needs a settled on-disk form for a
decision and a way to replay it — so building that first makes the later choice
reversible instead of load-bearing.

## Non-goals

- **Sharing staging.** Not by default, not behind a flag. Export never emits a
  staged row and import rejects an input that contains one.
- **Merge, conflict resolution, or a sync engine.** No CRDT, no daemon (§6.1),
  no background process. `import` is an insert of unknown ids.
- **Transport.** No `dlog push` / `dlog pull`, no network code, no credentials.
  How the JSONL travels — committed to the repo, an S3 object, a CI artifact —
  is the team's choice, not dlog's.
- **Distinguishing imported decisions from local ones.** No `origin` column. A
  decision is a decision; ULIDs are unique, so mixing is safe, and the column can
  be added later over #60's migration path if a real need appears.
- **A remote store (libSQL/Turso).** Tracked separately; whether it is needed at
  all is a question to answer after operating export/import.
- **Querying by author.** `--author` is recorded and shown, not filtered on, and
  it does not appear in compact rows (§9.1 principle 1 keeps those minimal).
- **Relevance ranking for large shared logs.** A whole team's decisions will
  strain `context`, but `--budget` / `elided` (#33) and the rollups (#63) cut by
  recency, not relevance. That is a real gap and a separate change.

## Rejected alternatives

- **OpenTelemetry as the storage layer.** §9's query surface does not fit on it:
  resolution is a 2-axis match of `symbol_path` against `structural_hash` with
  superseded rows excluded (§10.3, §9.1), and trace/log backends are built for
  time-windowed search and aggregation instead. Telemetry retention is days to
  weeks; decisions are designed to outlive commits (§2.2). And `staged → sealed`
  is an *update* — an append-only span stream cannot express §8.2's transition,
  nor keep the CHECK constraints and triggers that make it safe.
- **OpenTelemetry as a side channel** (one span per seal, GenAI semantic
  conventions, so seals correlate with existing agent traces). Coherent, and
  explicitly **not adopted**. It buys correlation only — the same
  "interoperability field, not storage" role §6's `conversation_id` already
  plays — and the price is an external endpoint on the seal path, which is the
  one moment §8.1 says must stay narrow.
- **Reading the author from git `user.email`.** Zero-friction, and it undoes #58,
  which just made the workspace root a dlog concept rather than a git one. Git
  stays optional (only `commit` and `hooks` need it), so identity is explicit or
  absent.
- **Making `--author` required.** Directly against §7.3's minimal-required-fields
  rule; a solo user would pay for a team feature on every `record`.
- **Sharing the SQLite file on a network drive.** The obvious "just put it on the
  shared folder" move. SQLite's locking is not reliable over NFS/SMB/sync
  clients, and the failure mode is a corrupted store rather than an error. The
  README says so explicitly.

## Design references

§2.2 (the unit of record is a decision), §6 / §6.1 (no daemon, no CRDT),
§7.1 (three entities), §7.2 (supersedes, append-only), §7.3 (minimal required
fields), §8.1 / §8.2 (staging + seal, the binding), §9.1 (query principles),
§10.1 / §10.2 (identity resolved at query time, against the local working tree).
Related issues: #58 (root-relative anchors), #59 (task lifecycle), #60 (schema
migrations), #63 (rollups).
