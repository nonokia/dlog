-- dlog storage schema (design §7, §8.2, §9.2, §10.2).
--
-- Staging vs. main log: rather than two physical tables, a single `decision`
-- table carries a `staged` flag. staged = 1 is the mutable staging area
-- ("pending" lives only here); staged = 0 is the immutable main log, which
-- always carries an explicit binding. Sealing is the single 1 -> 0 transition;
-- BEFORE UPDATE/DELETE triggers make sealed rows append-only (§7.2). This keeps
-- the staging/main semantics while avoiding duplicated child tables.
--
-- All DDL is idempotent (IF NOT EXISTS) so opening an existing store is a no-op.

CREATE TABLE IF NOT EXISTS schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Task hierarchy and the human's original instruction (§7.1).
CREATE TABLE IF NOT EXISTS task (
    id             TEXT PRIMARY KEY,            -- ULID
    parent_task_id TEXT REFERENCES task(id),
    instruction    TEXT,
    created_at_ms  INTEGER NOT NULL
);

-- The decision log (§7.4). Append-only once sealed.
CREATE TABLE IF NOT EXISTS decision (
    id               TEXT PRIMARY KEY,          -- ULID (time-sortable)
    task_id          TEXT REFERENCES task(id),
    supersedes       TEXT REFERENCES decision(id),   -- §7.2
    agent_role       TEXT NOT NULL,
    agent_model      TEXT NOT NULL,
    agent_session_id TEXT,
    conversation_id  TEXT,
    rationale        TEXT NOT NULL,             -- required (§7.3)
    rejected         TEXT,                      -- JSON [{approach,reason}] | NULL
    caused_by        TEXT,                      -- JSON [decision_id ...]    | NULL (DAG)
    staged           INTEGER NOT NULL DEFAULT 1,
    binding_type     TEXT,                      -- 'commit' | 'none' (NULL while staged)
    binding_sha      TEXT,
    created_at_ms    INTEGER NOT NULL,
    CHECK (staged IN (0, 1)),
    -- Encode the staging/binding invariant (§8.2): staged rows have no binding;
    -- sealed rows have a commit-with-sha or a none-without-sha.
    CHECK (
        (staged = 1 AND binding_type IS NULL AND binding_sha IS NULL)
        OR (staged = 0 AND (
               (binding_type = 'commit' AND binding_sha IS NOT NULL)
            OR (binding_type = 'none'   AND binding_sha IS NULL)
        ))
    )
);

CREATE INDEX IF NOT EXISTS idx_decision_task ON decision(task_id);
CREATE INDEX IF NOT EXISTS idx_decision_supersedes ON decision(supersedes);
CREATE INDEX IF NOT EXISTS idx_decision_staged ON decision(staged);

-- Append-only enforcement for the main log (§7.2): sealed rows (staged = 0) may
-- not be updated or deleted. Sealing itself (OLD.staged = 1) is allowed through.
CREATE TRIGGER IF NOT EXISTS decision_main_log_no_update
BEFORE UPDATE ON decision
WHEN OLD.staged = 0
BEGIN
    SELECT RAISE(ABORT, 'main-log decision is immutable');
END;

CREATE TRIGGER IF NOT EXISTS decision_main_log_no_delete
BEFORE DELETE ON decision
WHEN OLD.staged = 0
BEGIN
    SELECT RAISE(ABORT, 'main-log decision cannot be deleted');
END;

-- Declared invariants (§7.1). Outlive the declaring decision; queried
-- independently. `declared_by` records provenance.
CREATE TABLE IF NOT EXISTS invariant (
    id            TEXT PRIMARY KEY,             -- ULID
    declared_by   TEXT NOT NULL REFERENCES decision(id),
    statement     TEXT NOT NULL,
    scope         TEXT,                         -- optional path scope (§9.2)
    retired       INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    CHECK (retired IN (0, 1))
);

CREATE INDEX IF NOT EXISTS idx_invariant_declared_by ON invariant(declared_by);
CREATE INDEX IF NOT EXISTS idx_invariant_retired ON invariant(retired);

-- Anchor observations captured at record time (§10.2). Stores only what was
-- observed; identity is judged at query time (#8). A file-level anchor (§10.5)
-- leaves symbol_path/structural_hash NULL. structural_hash is globally indexed
-- so a moved node can be matched across files (§10.3).
CREATE TABLE IF NOT EXISTS anchor (
    id              INTEGER PRIMARY KEY,
    decision_id     TEXT NOT NULL REFERENCES decision(id) ON DELETE CASCADE,
    file            TEXT NOT NULL,
    symbol_path     TEXT,
    node_kind       TEXT,
    structural_hash TEXT,
    line_start      INTEGER,
    line_end        INTEGER,
    recorded_at_sha TEXT
);

CREATE INDEX IF NOT EXISTS idx_anchor_decision ON anchor(decision_id);
CREATE INDEX IF NOT EXISTS idx_anchor_symbol ON anchor(file, symbol_path);
CREATE INDEX IF NOT EXISTS idx_anchor_structural_hash ON anchor(structural_hash);

-- Full-text search over decision prose (§9.2). Standalone FTS5 table kept in
-- sync with `decision` by triggers; `decision_id` is carried UNINDEXED so a
-- match maps back to the decision. Rows are keyed by decision.rowid.
CREATE VIRTUAL TABLE IF NOT EXISTS decision_fts USING fts5(
    decision_id UNINDEXED,
    rationale,
    rejected
);

CREATE TRIGGER IF NOT EXISTS decision_fts_ai AFTER INSERT ON decision BEGIN
    INSERT INTO decision_fts(rowid, decision_id, rationale, rejected)
    VALUES (new.rowid, new.id, new.rationale, COALESCE(new.rejected, ''));
END;

CREATE TRIGGER IF NOT EXISTS decision_fts_au AFTER UPDATE ON decision BEGIN
    UPDATE decision_fts
       SET rationale = new.rationale, rejected = COALESCE(new.rejected, '')
     WHERE rowid = new.rowid;
END;

CREATE TRIGGER IF NOT EXISTS decision_fts_ad AFTER DELETE ON decision BEGIN
    DELETE FROM decision_fts WHERE rowid = old.rowid;
END;
