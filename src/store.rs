//! SQLite-backed storage: the staging/main-log split, the three entities, and
//! the binding/FTS schema (design §7, §8.2). This layer only *persists* observed
//! anchor values (§10.2); anchor resolution is a separate concern (#8).
//!
//! Each `dlog` invocation opens the store directly — no daemon (§6.1).

use rusqlite::{Connection, OptionalExtension, Row, params};
use ulid::Ulid;

use crate::model::{Agent, Anchor, Binding, NewDecision, Rejected, StoredDecision};

/// Schema migrations, in application order. A migration's version is its
/// 1-based position: `MIGRATIONS[0]` takes an empty store to version 1.
/// Position *is* the version, so the two can never disagree.
///
/// Entry 0 (`schema.sql`) is the frozen baseline every existing store already
/// carries, and stays idempotent. Later entries are applied **exactly once**,
/// which is what buys the `ALTER TABLE` path SQLite has no `IF NOT EXISTS` for
/// (#60). A migration must not open its own transaction — [`Store::migrate`]
/// wraps the whole batch in one.
const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    include_str!("migrations/002_task_completed_at.sql"),
    include_str!("migrations/003_decision_author.sql"),
];

/// The schema version this binary understands. A store above it is refused
/// ([`OpenError::SchemaTooNew`]); a store below it is migrated up on open.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Bootstrap DDL for the version ledger. `schema_meta` is defined inside the v1
/// baseline, so on an empty store the version has to be readable before the
/// migration that creates the table has run. Same statement, and replaying it
/// is free.
const SCHEMA_META_DDL: &str =
    "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);";

/// Failure opening a store. Distinct from [`rusqlite::Error`] so that a store
/// written by a newer dlog is a condition an agent can branch on rather than an
/// opaque store error.
#[derive(Debug)]
pub enum OpenError {
    Sqlite(rusqlite::Error),
    /// The store's schema is newer than this binary understands. Carrying on
    /// would mean answering queries from tables whose meaning has changed, so
    /// it is reported instead (upgrade dlog).
    SchemaTooNew {
        found: i64,
        supported: i64,
    },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Sqlite(e) => write!(f, "{e}"),
            OpenError::SchemaTooNew { found, supported } => write!(
                f,
                "store schema version {found} is newer than this dlog supports \
                 (up to {supported}); upgrade dlog to read it"
            ),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        OpenError::Sqlite(e)
    }
}

/// A live invariant with provenance, returned by [`Store::list_live_invariants`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantRow {
    pub id: String,
    pub statement: String,
    pub scope: Option<String>,
    /// The decision that declared this invariant (§7.1).
    pub declared_by: String,
}

/// Store-wide status, reported by `dlog status` (§9.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreStatus {
    /// Number of unsealed decisions sitting in staging.
    pub staging_count: i64,
    /// Timestamp (epoch ms) of the oldest staged decision, if any — surfaces
    /// staging that has gone stale (§8.3).
    pub oldest_staged_ms: Option<i64>,
    /// How many unfinished tasks still hold staged decisions. The untruncated
    /// total behind [`Store::stranded_tasks`] (#61).
    pub stranded_task_count: i64,
    pub schema_version: i64,
}

/// One row of `dlog task list` (§9.1 compact form). The instruction is returned
/// whole; summarising it is the command layer's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: String,
    pub parent_task_id: Option<String>,
    pub instruction: Option<String>,
    /// Decisions recorded under this task that are still unsealed.
    pub staged_count: i64,
    /// When `dlog task done` first finished the task; `None` while open.
    pub completed_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

/// An unfinished task that still holds staged decisions — the per-task view of
/// stranded staging that `staging_count` alone cannot give (§8.3, #61).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedTask {
    pub task: String,
    pub instruction: Option<String>,
    pub staged_count: i64,
    pub oldest_staged_ms: i64,
}

/// A task row as it crosses the export/import boundary (#64): the stored columns
/// and nothing derived. [`TaskRow`] is the *query* shape (it carries a computed
/// `staged_count`); this is the *storage* shape, so it round-trips.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
}

/// An invariant row as it crosses the export/import boundary (#64). Unlike
/// [`InvariantRow`] it carries `retired` and `created_at_ms`, because a
/// serialization has to be lossless where a query can afford to summarise.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InvariantRecord {
    pub id: String,
    pub declared_by: String,
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default)]
    pub retired: bool,
    pub created_at_ms: i64,
}

/// Lower bound for `dlog export --since` (#64).
///
/// Two spellings of one ordering: ULIDs sort chronologically, so an id bound is
/// a lexicographic comparison, while a date bound compares record time. Kept as
/// separate variants rather than collapsed to milliseconds so `--since <id>`
/// means *that decision onwards* exactly, instead of "everything in that
/// millisecond".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinceBound {
    Id(String),
    Ms(i64),
}

/// How many rows of each kind an import actually inserted. Records whose id the
/// store already had are not counted here — [`Store::import_all`] returns that
/// tally separately, because "already knew it" is the normal outcome of
/// re-importing an overlapping file rather than a kind of write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImportCounts {
    pub tasks: usize,
    pub decisions: usize,
    pub invariants: usize,
}

/// A handle to the SQLite-backed decision log.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open an on-disk store, creating/migrating the schema as needed.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, OpenError> {
        Self::init(Connection::open(path)?)
    }

    /// Open a private in-memory store (tests, throwaway use).
    pub fn open_in_memory() -> Result<Self, OpenError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, OpenError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Bring the store up to [`SCHEMA_VERSION`], applying only the migrations it
    /// has not seen (#60). Idempotent: an already-current store does no work.
    ///
    /// The whole batch runs in one transaction — SQLite executes DDL
    /// transactionally, so a migration that fails leaves the store at its
    /// previous version rather than half-migrated.
    fn migrate(&self) -> Result<(), OpenError> {
        // The ledger first: on an empty store its own table doesn't exist yet.
        self.conn.execute_batch(SCHEMA_META_DDL)?;
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(OpenError::SchemaTooNew {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }
        if current == SCHEMA_VERSION {
            return Ok(());
        }

        let tx = self.conn.unchecked_transaction()?;
        for sql in MIGRATIONS.iter().skip(current.max(0) as usize) {
            tx.execute_batch(sql)?;
        }
        // DO UPDATE, not DO NOTHING: writing the version once is exactly why it
        // never moved before.
        tx.execute(
            "INSERT INTO schema_meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The schema version recorded in the store; 0 when nothing has been applied
    /// yet (a fresh file), so migration starts from the beginning.
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        let recorded: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(recorded.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    // ---- Task -------------------------------------------------------------

    /// Insert a task and return its new id.
    pub fn insert_task(
        &self,
        parent_task_id: Option<&str>,
        instruction: Option<&str>,
    ) -> rusqlite::Result<String> {
        let id = Ulid::new();
        let id_str = id.to_string();
        self.conn.execute(
            "INSERT INTO task(id, parent_task_id, instruction, created_at_ms)
             VALUES(?1, ?2, ?3, ?4)",
            params![
                id_str,
                parent_task_id,
                instruction,
                id.timestamp_ms() as i64
            ],
        )?;
        Ok(id_str)
    }

    /// Ensure a task row exists for `id` (used when a decision references a task
    /// by id). No-op if the task already exists; the instruction is only set on
    /// first creation.
    pub fn ensure_task(&self, id: &str, instruction: Option<&str>) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO task(id, parent_task_id, instruction, created_at_ms)
             VALUES(?1, NULL, ?2, ?3)
             ON CONFLICT(id) DO NOTHING",
            params![id, instruction, Ulid::new().timestamp_ms() as i64],
        )?;
        Ok(())
    }

    /// Whether a task row exists. Used to reject an unknown `--parent` or
    /// `task done <id>` with a specific error instead of a foreign-key failure
    /// or a silent no-op.
    pub fn task_exists(&self, id: &str) -> rusqlite::Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM task WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(found.is_some())
    }

    /// Mark a task finished, stamping the completion time (§7.1, #61).
    ///
    /// The `IS NULL` guard makes the *first* completion the recorded one: a
    /// second `dlog task done` is a follow-up seal, and overwriting would lose
    /// the one fact the column carries — when the work was declared done.
    /// Returns the effective `completed_at_ms`, which may predate this call.
    pub fn complete_task(&self, id: &str) -> rusqlite::Result<i64> {
        self.conn.execute(
            "UPDATE task SET completed_at_ms = ?2
             WHERE id = ?1 AND completed_at_ms IS NULL",
            params![id, Ulid::new().timestamp_ms() as i64],
        )?;
        self.conn.query_row(
            "SELECT completed_at_ms FROM task WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
    }

    /// Tasks in the compact list form, newest-first (ULIDs are time-sortable).
    /// Open tasks only unless `include_completed`; `parent` narrows to one
    /// task's children. Backs `dlog task list` (#61).
    pub fn list_tasks(
        &self,
        include_completed: bool,
        parent: Option<&str>,
    ) -> rusqlite::Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.parent_task_id, t.instruction, t.created_at_ms,
                    t.completed_at_ms,
                    (SELECT COUNT(*) FROM decision d
                      WHERE d.task_id = t.id AND d.staged = 1) AS staged_count
             FROM task t
             WHERE (?1 = 1 OR t.completed_at_ms IS NULL)
               AND (?2 IS NULL OR t.parent_task_id = ?2)
             ORDER BY t.id DESC",
        )?;
        let rows = stmt
            .query_map(params![include_completed, parent], |r| {
                Ok(TaskRow {
                    id: r.get(0)?,
                    parent_task_id: r.get(1)?,
                    instruction: r.get(2)?,
                    created_at_ms: r.get(3)?,
                    completed_at_ms: r.get(4)?,
                    staged_count: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Unfinished tasks that still hold staged decisions, newest-first, capped
    /// at `limit`. The join is what connects the two facts `dlog status` could
    /// previously only report separately (§8.3).
    pub fn stranded_tasks(&self, limit: usize) -> rusqlite::Result<Vec<StrandedTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.instruction, COUNT(d.id), MIN(d.created_at_ms)
             FROM task t
             JOIN decision d ON d.task_id = t.id AND d.staged = 1
             WHERE t.completed_at_ms IS NULL
             GROUP BY t.id
             ORDER BY t.id DESC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(StrandedTask {
                    task: r.get(0)?,
                    instruction: r.get(1)?,
                    staged_count: r.get(2)?,
                    oldest_staged_ms: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Staged decisions belonging to `task_id`, oldest-first. Backs the
    /// non-code seal (`dlog task done`, §8.3): a finishing agent seals its own
    /// task's decisions, not everything currently in staging.
    pub fn staged_decision_ids_for_task(&self, task_id: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM decision WHERE task_id = ?1 AND staged = 1 ORDER BY id")?;
        let ids = stmt
            .query_map(params![task_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    // ---- Decisions --------------------------------------------------------

    /// Write a new decision into staging (§8.2) together with its anchors, in a
    /// single transaction. Returns the new decision id.
    pub fn stage_decision(&self, decision: &NewDecision) -> rusqlite::Result<String> {
        let id = Ulid::new();
        let id_str = id.to_string();
        let rejected_json = json_array_or_null(&decision.rejected);
        let caused_by_json = json_array_or_null(&decision.caused_by);

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO decision(
                id, task_id, supersedes, agent_role, agent_model, agent_session_id,
                agent_author, conversation_id, rationale, rejected, caused_by,
                staged, binding_type, binding_sha, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, NULL, NULL, ?12)",
            params![
                id_str,
                decision.task_id,
                decision.supersedes,
                decision.agent.role,
                decision.agent.model,
                decision.agent.session_id,
                decision.agent.author,
                decision.conversation_id,
                decision.rationale,
                rejected_json,
                caused_by_json,
                id.timestamp_ms() as i64,
            ],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO anchor(
                    decision_id, file, symbol_path, node_kind, structural_hash,
                    line_start, line_end, recorded_at_sha)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for a in &decision.anchors {
                let (line_start, line_end) = match a.line_span {
                    Some((s, e)) => (Some(s as i64), Some(e as i64)),
                    None => (None, None),
                };
                stmt.execute(params![
                    id_str,
                    a.file,
                    a.symbol_path,
                    a.node_kind,
                    a.structural_hash,
                    line_start,
                    line_end,
                    a.recorded_at_sha,
                ])?;
            }
        }
        tx.commit()?;
        Ok(id_str)
    }

    /// Seal a staged decision, stamping its binding and moving it into the
    /// immutable main log (§8.2, §8.3). Errors if the id is unknown or already
    /// sealed.
    pub fn seal(&self, decision_id: &str, binding: &Binding) -> rusqlite::Result<()> {
        let (binding_type, binding_sha) = match binding {
            Binding::Commit { sha } => ("commit", Some(sha.as_str())),
            Binding::None => ("none", None),
        };
        // WHERE staged = 1 means we only ever transition pending rows; sealed
        // rows are left to the immutability trigger as a backstop.
        let changed = self.conn.execute(
            "UPDATE decision SET staged = 0, binding_type = ?2, binding_sha = ?3
             WHERE id = ?1 AND staged = 1",
            params![decision_id, binding_type, binding_sha],
        )?;
        if changed == 0 {
            return Err(rusqlite::Error::StatementChangedRows(0));
        }
        Ok(())
    }

    /// Seal staged decisions in one atomic step, stamping `binding` (§8.2, §8.3).
    /// With `only = Some(ids)`, restrict to those ids (each must be staged);
    /// otherwise seal every staged decision. All-or-nothing: if any target id is
    /// unknown or already sealed, nothing is sealed. Returns the sealed ids.
    pub fn seal_staged(
        &self,
        binding: &Binding,
        only: Option<&[String]>,
    ) -> rusqlite::Result<Vec<String>> {
        let (binding_type, binding_sha) = match binding {
            Binding::Commit { sha } => ("commit", Some(sha.as_str())),
            Binding::None => ("none", None),
        };

        let tx = self.conn.unchecked_transaction()?;
        let ids: Vec<String> = match only {
            Some(list) => list.to_vec(),
            None => {
                let mut stmt =
                    tx.prepare("SELECT id FROM decision WHERE staged = 1 ORDER BY id")?;
                stmt.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        for id in &ids {
            let changed = tx.execute(
                "UPDATE decision SET staged = 0, binding_type = ?2, binding_sha = ?3
                 WHERE id = ?1 AND staged = 1",
                params![id, binding_type, binding_sha],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::StatementChangedRows(0));
            }
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Fetch a decision (with its anchors) by id.
    pub fn get_decision(&self, id: &str) -> rusqlite::Result<Option<StoredDecision>> {
        let decision = self
            .conn
            .query_row(
                "SELECT id, task_id, supersedes, agent_role, agent_model,
                        agent_session_id, conversation_id, rationale, rejected,
                        caused_by, staged, binding_type, binding_sha, created_at_ms,
                        agent_author
                 FROM decision WHERE id = ?1",
                params![id],
                row_to_decision,
            )
            .optional()?;

        match decision {
            Some(mut d) => {
                d.anchors = self.anchors_for(id)?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    /// Anchors recorded for a decision, in insertion order.
    pub fn anchors_for(&self, decision_id: &str) -> rusqlite::Result<Vec<Anchor>> {
        let mut stmt = self.conn.prepare(
            "SELECT file, symbol_path, node_kind, structural_hash,
                    line_start, line_end, recorded_at_sha
             FROM anchor WHERE decision_id = ?1 ORDER BY id",
        )?;
        let anchors = stmt
            .query_map(params![decision_id], |r| {
                let line_start: Option<i64> = r.get(4)?;
                let line_end: Option<i64> = r.get(5)?;
                Ok(Anchor {
                    file: r.get(0)?,
                    symbol_path: r.get(1)?,
                    node_kind: r.get(2)?,
                    structural_hash: r.get(3)?,
                    line_span: match (line_start, line_end) {
                        (Some(s), Some(e)) => Some((s as u32, e as u32)),
                        _ => None,
                    },
                    recorded_at_sha: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(anchors)
    }

    // ---- Anchor resolution queries (#8) -----------------------------------
    //
    // These back the query-time 2-axis match (§10.3). Each returns distinct
    // decision ids, newest-first (ULIDs are time-sortable). The structural_hash
    // lookups are global/cross-file by design, so a moved node is still found.

    /// Decisions with an anchor matching both symbol_path and structural_hash
    /// (the `exact` tier).
    pub fn decision_ids_by_symbol_and_hash(
        &self,
        symbol_path: &str,
        structural_hash: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT decision_id FROM anchor
             WHERE symbol_path = ?1 AND structural_hash = ?2
             ORDER BY decision_id DESC",
        )?;
        let ids = stmt
            .query_map(params![symbol_path, structural_hash], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Decisions with an anchor on this symbol_path (the `drifted` tier).
    pub fn decision_ids_by_symbol(&self, symbol_path: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT decision_id FROM anchor
             WHERE symbol_path = ?1 ORDER BY decision_id DESC",
        )?;
        let ids = stmt
            .query_map(params![symbol_path], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Decisions with an anchor of this structural_hash, across all files (the
    /// `relocated` tier).
    pub fn decision_ids_by_hash(&self, structural_hash: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT decision_id FROM anchor
             WHERE structural_hash = ?1 ORDER BY decision_id DESC",
        )?;
        let ids = stmt
            .query_map(params![structural_hash], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Distinct non-null `symbol_path`s among anchors with this structural_hash.
    /// A global hash that spans more than one symbol is ambiguous, so the
    /// resolver won't trust it as a `relocated` match (#28).
    pub fn symbol_paths_for_hash(&self, structural_hash: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT symbol_path FROM anchor
             WHERE structural_hash = ?1 AND symbol_path IS NOT NULL
             ORDER BY symbol_path",
        )?;
        let paths = stmt
            .query_map(params![structural_hash], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(paths)
    }

    /// Decisions with an anchor on this file (the `file_fallback` tier).
    pub fn decision_ids_by_file(&self, file: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT decision_id FROM anchor
             WHERE file = ?1 ORDER BY decision_id DESC",
        )?;
        let ids = stmt
            .query_map(params![file], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Decisions with an anchor at `path` (an exact file) or anywhere under it
    /// (a directory), newest-first. Backs `dlog context <path>` (#30). The `/`
    /// boundary means `src/au` never matches `src/auth/...`; LIKE metacharacters
    /// in the path are escaped so they match literally.
    pub fn decision_ids_under_path(&self, path: &str) -> rusqlite::Result<Vec<String>> {
        // `.` is the stored spelling of the workspace root, so it means the
        // whole log — including the few anchors kept absolute because they point
        // outside the workspace.
        if path == "." {
            return self.all_anchored_decision_ids();
        }
        let under = format!("{}/%", like_escape(path));
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT decision_id FROM anchor
             WHERE file = ?1 OR file LIKE ?2 ESCAPE '\\'
             ORDER BY decision_id DESC",
        )?;
        let ids = stmt
            .query_map(params![path, under], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// The `(file, decision_id)` pairs under `path`, newest-first by decision.
    /// Same scope as [`Store::decision_ids_under_path`], but keeping the file
    /// each anchor named so `dlog context --rollup` can group by it (#63). A
    /// decision anchored to several files under the path appears once per file.
    pub fn anchors_under_path(&self, path: &str) -> rusqlite::Result<Vec<(String, String)>> {
        let (sql, params): (&str, Vec<String>) = if path == "." {
            (
                "SELECT DISTINCT file, decision_id FROM anchor ORDER BY decision_id DESC",
                vec![],
            )
        } else {
            (
                "SELECT DISTINCT file, decision_id FROM anchor
                 WHERE file = ?1 OR file LIKE ?2 ESCAPE '\\'
                 ORDER BY decision_id DESC",
                vec![path.to_string(), format!("{}/%", like_escape(path))],
            )
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every decision carrying an anchor, newest-first. Backs `dlog context .`
    /// (the whole workspace).
    fn all_anchored_decision_ids(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT decision_id FROM anchor ORDER BY decision_id DESC")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Decisions that list `decision_id` in their `caused_by` — the ones it
    /// directly caused (downstream edges of the DAG, §4). Backs `dlog trace`
    /// (#31). Newest-first.
    pub fn decision_ids_caused_by(&self, decision_id: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT d.id
             FROM decision d, json_each(d.caused_by) je
             WHERE d.caused_by IS NOT NULL AND je.value = ?1
             ORDER BY d.id DESC",
        )?;
        let ids = stmt
            .query_map(params![decision_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// The set of decision ids that have been superseded by a later decision
    /// (§7.2). Used to exclude them from the default "live decisions" scope
    /// (§9.1).
    pub fn superseded_ids(&self) -> rusqlite::Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT supersedes FROM decision WHERE supersedes IS NOT NULL")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
        Ok(ids)
    }

    // ---- Invariants -------------------------------------------------------

    /// Declare an invariant, recording the decision that declared it (§7.1).
    pub fn insert_invariant(
        &self,
        declared_by: &str,
        statement: &str,
        scope: Option<&str>,
    ) -> rusqlite::Result<String> {
        let id = Ulid::new();
        let id_str = id.to_string();
        self.conn.execute(
            "INSERT INTO invariant(id, declared_by, statement, scope, retired, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, 0, ?5)",
            params![
                id_str,
                declared_by,
                statement,
                scope,
                id.timestamp_ms() as i64
            ],
        )?;
        Ok(id_str)
    }

    /// Live (non-retired) invariants as `(id, statement)` pairs.
    pub fn live_invariants(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, statement FROM invariant WHERE retired = 0 ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Live (non-retired) invariants with provenance, for `dlog invariants`.
    pub fn list_live_invariants(&self) -> rusqlite::Result<Vec<InvariantRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, statement, scope, declared_by
             FROM invariant WHERE retired = 0 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(InvariantRow {
                    id: r.get(0)?,
                    statement: r.get(1)?,
                    scope: r.get(2)?,
                    declared_by: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Invariants declared by a decision as `(id, statement, scope)` (for `show`).
    pub fn invariants_declared_by(
        &self,
        decision_id: &str,
    ) -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, statement, scope FROM invariant WHERE declared_by = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![decision_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- Search & status --------------------------------------------------

    /// Full-text search over decision prose (§9.2), returning matching decision
    /// ids best-first. The raw query is normalised to quoted literal terms so
    /// arbitrary agent input can't trip FTS5 syntax (operators, quotes, etc.).
    pub fn search(&self, query: &str) -> rusqlite::Result<Vec<String>> {
        let match_query = fts5_literal_query(query);
        if match_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT decision_id FROM decision_fts
             WHERE decision_fts MATCH ?1 ORDER BY rank",
        )?;
        let ids = stmt
            .query_map(params![match_query], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    // ---- Export / import (#64) --------------------------------------------
    //
    // Sharing moves *sealed* rows only (§8.2): staging is one agent's live work
    // area, and a decision that arrived from another machine was never this
    // store's to seal.
    //
    // Everything below is ordered by id, which approximates FK order — a
    // referenced row always existed first, and ULIDs sort chronologically. It is
    // only an approximation: two ULIDs minted in the same millisecond sort
    // randomly against each other. So the ordering is for readability, and
    // `import_all` defers foreign keys rather than depending on it.

    /// Sealed decision ids, ascending, optionally bounded below by `since`.
    /// Staged rows are never returned; there is no flag that changes that.
    pub fn sealed_decision_ids(&self, since: Option<&SinceBound>) -> rusqlite::Result<Vec<String>> {
        // One statement with both bounds pre-resolved: a NULL bound passes every
        // row, so `--since` and no `--since` share a query plan and a code path.
        let (since_id, since_ms) = match since {
            None => (None, None),
            Some(SinceBound::Id(id)) => (Some(id.as_str()), None),
            Some(SinceBound::Ms(ms)) => (None, Some(*ms)),
        };
        let mut stmt = self.conn.prepare(
            "SELECT id FROM decision
              WHERE staged = 0
                AND (?1 IS NULL OR id >= ?1)
                AND (?2 IS NULL OR created_at_ms >= ?2)
              ORDER BY id",
        )?;
        let ids = stmt
            .query_map(params![since_id, since_ms], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// A task's stored row, for export. `None` when the id is unknown.
    pub fn get_task(&self, id: &str) -> rusqlite::Result<Option<TaskRecord>> {
        self.conn
            .query_row(
                "SELECT id, parent_task_id, instruction, created_at_ms, completed_at_ms
                 FROM task WHERE id = ?1",
                params![id],
                |r| {
                    Ok(TaskRecord {
                        id: r.get(0)?,
                        parent_task_id: r.get(1)?,
                        instruction: r.get(2)?,
                        created_at_ms: r.get(3)?,
                        completed_at_ms: r.get(4)?,
                    })
                },
            )
            .optional()
    }

    /// Every invariant a decision declared, for export — retired ones included.
    /// `dlog invariants` filters those out because a retired constraint is not
    /// in effect; an export must still carry it, or importing would resurrect it.
    pub fn invariant_records_declared_by(
        &self,
        decision_id: &str,
    ) -> rusqlite::Result<Vec<InvariantRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, declared_by, statement, scope, retired, created_at_ms
             FROM invariant WHERE declared_by = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![decision_id], |r| {
                Ok(InvariantRecord {
                    id: r.get(0)?,
                    declared_by: r.get(1)?,
                    statement: r.get(2)?,
                    scope: r.get(3)?,
                    retired: r.get::<_, i64>(4)? != 0,
                    created_at_ms: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Whether a decision id is known. Used by `import` to tell "already have it"
    /// from "dangling reference" *before* opening its transaction — safe to do
    /// early because the main log is append-only, so a row that exists now cannot
    /// stop existing (§7.2).
    pub fn decision_exists(&self, id: &str) -> rusqlite::Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM decision WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(found.is_some())
    }

    /// Whether an invariant id is known.
    pub fn invariant_exists(&self, id: &str) -> rusqlite::Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM invariant WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(found.is_some())
    }

    /// Insert records the store does not already have, in one transaction
    /// (§7.2 — a half-applied import would leave holes in an append-only log
    /// that nothing can repair). Returns what was inserted and how many records
    /// were already known.
    ///
    /// Each row is preceded by an existence check and then inserted plainly,
    /// rather than with `INSERT OR IGNORE`: that would also swallow CHECK and FK
    /// violations, turning a malformed file into a silently short import. Here a
    /// violated §8.2 invariant is an error the caller sees.
    ///
    /// Foreign keys are **deferred** to commit time for the duration. Rows
    /// arrive in id order, which is FK order to millisecond resolution — but
    /// ULIDs minted in the *same* millisecond sort randomly against each other,
    /// so a decision that supersedes one recorded microseconds earlier could
    /// legitimately sort first. Deferring means write order is a readability
    /// property rather than a correctness one; a genuinely dangling reference
    /// still fails, just at COMMIT, and still takes the whole batch with it.
    pub fn import_all(
        &self,
        tasks: &[TaskRecord],
        decisions: &[StoredDecision],
        invariants: &[InvariantRecord],
    ) -> rusqlite::Result<(ImportCounts, usize)> {
        let mut counts = ImportCounts::default();
        let mut skipped = 0usize;

        let tx = self.conn.unchecked_transaction()?;
        // Scoped to this transaction: SQLite resets it at COMMIT.
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;

        for t in tasks {
            if self.task_exists(&t.id)? {
                skipped += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO task(id, parent_task_id, instruction, created_at_ms,
                                  completed_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    t.id,
                    t.parent_task_id,
                    t.instruction,
                    t.created_at_ms,
                    t.completed_at_ms
                ],
            )?;
            counts.tasks += 1;
        }

        for d in decisions {
            if self.decision_exists(&d.id)? {
                skipped += 1;
                continue;
            }
            let (binding_type, binding_sha) = match &d.binding {
                Some(Binding::Commit { sha }) => ("commit", Some(sha.as_str())),
                Some(Binding::None) => ("none", None),
                // Rejected by validation long before here; the CHECK constraint
                // is the backstop if it ever isn't.
                None => ("none", None),
            };
            tx.execute(
                "INSERT INTO decision(
                    id, task_id, supersedes, agent_role, agent_model, agent_session_id,
                    agent_author, conversation_id, rationale, rejected, caused_by,
                    staged, binding_type, binding_sha, created_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, ?13, ?14)",
                params![
                    d.id,
                    d.task_id,
                    d.supersedes,
                    d.agent.role,
                    d.agent.model,
                    d.agent.session_id,
                    d.agent.author,
                    d.conversation_id,
                    d.rationale,
                    json_array_or_null(&d.rejected),
                    json_array_or_null(&d.caused_by),
                    binding_type,
                    binding_sha,
                    d.created_at_ms,
                ],
            )?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO anchor(
                        decision_id, file, symbol_path, node_kind, structural_hash,
                        line_start, line_end, recorded_at_sha)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?;
                for a in &d.anchors {
                    let (line_start, line_end) = match a.line_span {
                        Some((s, e)) => (Some(s as i64), Some(e as i64)),
                        None => (None, None),
                    };
                    stmt.execute(params![
                        d.id,
                        a.file,
                        a.symbol_path,
                        a.node_kind,
                        a.structural_hash,
                        line_start,
                        line_end,
                        a.recorded_at_sha,
                    ])?;
                }
            }
            counts.decisions += 1;
        }

        for i in invariants {
            if self.invariant_exists(&i.id)? {
                skipped += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO invariant(id, declared_by, statement, scope, retired,
                                       created_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    i.id,
                    i.declared_by,
                    i.statement,
                    i.scope,
                    i.retired as i64,
                    i.created_at_ms
                ],
            )?;
            counts.invariants += 1;
        }

        tx.commit()?;
        Ok((counts, skipped))
    }

    /// Store-wide status (§9.2).
    pub fn status(&self) -> rusqlite::Result<StoreStatus> {
        let staging_count =
            self.conn
                .query_row("SELECT COUNT(*) FROM decision WHERE staged = 1", [], |r| {
                    r.get(0)
                })?;
        let oldest_staged_ms = self.conn.query_row(
            "SELECT MIN(created_at_ms) FROM decision WHERE staged = 1",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )?;
        let stranded_task_count = self.conn.query_row(
            "SELECT COUNT(DISTINCT t.id)
             FROM task t
             JOIN decision d ON d.task_id = t.id AND d.staged = 1
             WHERE t.completed_at_ms IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(StoreStatus {
            staging_count,
            oldest_staged_ms,
            stranded_task_count,
            schema_version: self.schema_version()?,
        })
    }
}

/// Turn raw user text into an FTS5 query of quoted literal terms (implicit AND),
/// so operators/quotes/punctuation in agent input can't cause syntax errors.
/// Each whitespace-separated token becomes a `"..."` string with internal double
/// quotes doubled. Empty input yields an empty query.
fn fts5_literal_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escape SQL LIKE metacharacters (`\`, `%`, `_`) so a string matches literally
/// under `LIKE ... ESCAPE '\'`.
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Serialize a slice to a JSON array string, or `None` when empty so the column
/// stays NULL rather than `"[]"`.
fn json_array_or_null<T: serde::Serialize>(items: &[T]) -> Option<String> {
    if items.is_empty() {
        None
    } else {
        // Serializing owned model types cannot fail.
        Some(serde_json::to_string(items).expect("serialize JSON column"))
    }
}

fn parse_json_array<T: serde::de::DeserializeOwned>(raw: Option<String>) -> Vec<T> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn row_to_decision(r: &Row) -> rusqlite::Result<StoredDecision> {
    let rejected: Option<String> = r.get(8)?;
    let caused_by: Option<String> = r.get(9)?;
    let staged: i64 = r.get(10)?;
    let binding_type: Option<String> = r.get(11)?;
    let binding_sha: Option<String> = r.get(12)?;
    let binding = match binding_type.as_deref() {
        Some("commit") => Some(Binding::Commit {
            sha: binding_sha.unwrap_or_default(),
        }),
        Some("none") => Some(Binding::None),
        _ => None,
    };
    Ok(StoredDecision {
        id: r.get(0)?,
        task_id: r.get(1)?,
        supersedes: r.get(2)?,
        agent: Agent {
            role: r.get(3)?,
            model: r.get(4)?,
            session_id: r.get(5)?,
            // Appended after the frozen column list rather than inserted, so the
            // v1 indices above keep meaning what they meant (schema v3, #64).
            author: r.get(14)?,
        },
        conversation_id: r.get(6)?,
        rationale: r.get(7)?,
        rejected: parse_json_array::<Rejected>(rejected),
        caused_by: parse_json_array::<String>(caused_by),
        anchors: Vec::new(),
        staged: staged != 0,
        binding,
        created_at_ms: r.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Agent {
        Agent {
            role: "implementer".into(),
            model: "claude-test".into(),
            session_id: Some("sess-1".into()),
            author: None,
        }
    }

    fn minimal(rationale: &str) -> NewDecision {
        // Minimal required surface (§7.3): rationale + one anchor + agent.
        NewDecision {
            task_id: None,
            agent: agent(),
            conversation_id: None,
            rationale: rationale.into(),
            rejected: vec![],
            caused_by: vec![],
            supersedes: None,
            anchors: vec![Anchor {
                file: "src/auth.rs".into(),
                symbol_path: Some("AuthService::authenticate".into()),
                node_kind: Some("function".into()),
                structural_hash: Some("h_abc".into()),
                line_span: Some((10, 45)),
                recorded_at_sha: Some("deadbeef".into()),
            }],
        }
    }

    fn temp_db(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dlog-store-{tag}-{}.db", Ulid::new()))
    }

    #[test]
    fn author_round_trips_and_is_absent_when_unset() {
        let store = Store::open_in_memory().unwrap();

        let anonymous = store.stage_decision(&minimal("no author")).unwrap();
        let d = store.get_decision(&anonymous).unwrap().unwrap();
        assert!(d.agent.author.is_none());
        // Absent, not empty: a solo store's output is byte-identical to before.
        let json = serde_json::to_value(&d).unwrap();
        assert!(json["agent"].get("author").is_none());

        let mut attributed = minimal("with author");
        attributed.agent.author = Some("lee@example.com".into());
        let id = store.stage_decision(&attributed).unwrap();
        let d = store.get_decision(&id).unwrap().unwrap();
        assert_eq!(d.agent.author.as_deref(), Some("lee@example.com"));
    }

    #[test]
    fn sealed_decision_ids_skip_staging_and_honour_the_bound() {
        let store = Store::open_in_memory().unwrap();
        let first = store.stage_decision(&minimal("first")).unwrap();
        store.seal(&first, &Binding::None).unwrap();
        let second = store.stage_decision(&minimal("second")).unwrap();
        store.seal(&second, &Binding::None).unwrap();
        let staged = store.stage_decision(&minimal("still working")).unwrap();

        // Sorted, not "in the order they were recorded": two ULIDs minted in the
        // same millisecond differ only in random bits, so mint order and id order
        // are the same thing only across millisecond boundaries.
        let all = store.sealed_decision_ids(None).unwrap();
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(all, expected, "ascending by id");
        assert!(!all.contains(&staged), "staging never leaves the store");

        // An id bound means "that decision onwards", inclusive.
        let last = expected.last().unwrap().clone();
        let from_last = store
            .sealed_decision_ids(Some(&SinceBound::Id(last.clone())))
            .unwrap();
        assert_eq!(from_last, vec![last]);

        // A time bound compares record time; 0 passes everything.
        assert_eq!(
            store.sealed_decision_ids(Some(&SinceBound::Ms(0))).unwrap(),
            all
        );
        assert!(
            store
                .sealed_decision_ids(Some(&SinceBound::Ms(i64::MAX)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn export_reads_carry_what_the_query_shapes_drop() {
        let store = Store::open_in_memory().unwrap();
        let parent = store.insert_task(None, Some("parent")).unwrap();
        let child = store.insert_task(Some(&parent), Some("child")).unwrap();

        let task = store.get_task(&child).unwrap().unwrap();
        assert_eq!(task.parent_task_id.as_deref(), Some(parent.as_str()));
        assert_eq!(task.instruction.as_deref(), Some("child"));
        assert!(task.completed_at_ms.is_none());
        store.complete_task(&child).unwrap();
        assert!(
            store
                .get_task(&child)
                .unwrap()
                .unwrap()
                .completed_at_ms
                .is_some()
        );
        assert!(store.get_task("01NOSUCHTASK").unwrap().is_none());

        let decision = store.stage_decision(&minimal("declares things")).unwrap();
        let inv = store
            .insert_invariant(&decision, "tokens never persist", Some("src"))
            .unwrap();
        // Retire it: `dlog invariants` stops showing it, but an export must still
        // carry it or importing would resurrect a constraint that was dropped.
        store
            .conn
            .execute(
                "UPDATE invariant SET retired = 1 WHERE id = ?1",
                params![inv],
            )
            .unwrap();
        assert!(store.list_live_invariants().unwrap().is_empty());

        let records = store.invariant_records_declared_by(&decision).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].retired);
        assert_eq!(records[0].scope.as_deref(), Some("src"));
        assert!(records[0].created_at_ms > 0);
    }

    #[test]
    fn import_all_is_atomic_and_skips_ids_it_already_has() {
        let store = Store::open_in_memory().unwrap();

        let task = TaskRecord {
            id: "01TASK".into(),
            parent_task_id: None,
            instruction: Some("shared work".into()),
            created_at_ms: 1,
            completed_at_ms: Some(2),
        };
        let mut decision = {
            let id = store.stage_decision(&minimal("template")).unwrap();
            let d = store.get_decision(&id).unwrap().unwrap();
            store.seal(&id, &Binding::None).unwrap();
            d
        };
        decision.id = "01DECISION".into();
        decision.task_id = Some(task.id.clone());
        decision.staged = false;
        decision.binding = Some(Binding::Commit { sha: "a3f".into() });
        let invariant = InvariantRecord {
            id: "01INV".into(),
            declared_by: decision.id.clone(),
            statement: "never log tokens".into(),
            scope: None,
            retired: false,
            created_at_ms: 3,
        };

        let tasks = vec![task];
        let decisions = vec![decision.clone()];
        let invariants = vec![invariant];
        let (counts, skipped) = store.import_all(&tasks, &decisions, &invariants).unwrap();
        assert_eq!(counts.tasks, 1);
        assert_eq!(counts.decisions, 1);
        assert_eq!(counts.invariants, 1);
        assert_eq!(skipped, 0);

        // The decision arrives sealed, with its anchors and its binding intact.
        let stored = store.get_decision("01DECISION").unwrap().unwrap();
        assert!(!stored.staged);
        assert_eq!(stored.binding, Some(Binding::Commit { sha: "a3f".into() }));
        assert_eq!(stored.anchors, decision.anchors);

        // Re-running writes nothing.
        let (counts, skipped) = store.import_all(&tasks, &decisions, &invariants).unwrap();
        assert_eq!(counts, ImportCounts::default());
        assert_eq!(skipped, 3);

        // A decision whose foreign key cannot resolve takes the whole batch with
        // it — the command layer catches this first, the transaction is the
        // backstop.
        let mut orphan = decision.clone();
        orphan.id = "01ORPHAN".into();
        orphan.task_id = Some("01NOSUCHTASK".into());
        assert!(store.import_all(&[], &[orphan], &[]).is_err());
        assert!(!store.decision_exists("01ORPHAN").unwrap());
    }

    #[test]
    fn import_does_not_depend_on_write_order() {
        // Ids order FK edges only to millisecond resolution: two ULIDs minted in
        // the same millisecond sort randomly, so a decision can legitimately sort
        // *before* the one it supersedes. Foreign keys are deferred to commit for
        // exactly this case — here forced by importing the pair backwards.
        let store = Store::open_in_memory().unwrap();
        let template = {
            let id = store.stage_decision(&minimal("template")).unwrap();
            let d = store.get_decision(&id).unwrap().unwrap();
            store.seal(&id, &Binding::None).unwrap();
            d
        };

        let mut original = template.clone();
        original.id = "01ZORIGINAL".into();
        original.staged = false;
        original.binding = Some(Binding::None);

        let mut reversal = template;
        reversal.id = "01AREVERSAL".into();
        reversal.supersedes = Some(original.id.clone());
        reversal.staged = false;
        reversal.binding = Some(Binding::None);

        // Sorted ascending, the reversal comes first — before its referent.
        let (counts, _) = store
            .import_all(&[], &[reversal, original], &[])
            .expect("deferred foreign keys let the referent arrive second");
        assert_eq!(counts.decisions, 2);
    }

    #[test]
    fn migrate_is_idempotent_and_records_version() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        // Re-running migration must not error or duplicate the version row.
        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn migrates_a_v1_store_up_without_touching_its_rows() {
        // The regression guard for every future migration: a store created
        // before the migration sequence existed (schema.sql replayed, version 1)
        // must come up to date on open, keeping what it recorded.
        let db = temp_db("v1");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute(
                "INSERT INTO schema_meta(key, value) VALUES('schema_version', '1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO task(id, parent_task_id, instruction, created_at_ms)
                 VALUES('01OLDTASK', NULL, 'legacy work', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO decision(id, agent_role, agent_model, rationale,
                                      staged, binding_type, created_at_ms)
                 VALUES('01OLDDEC', 'implementer', 'old-model', 'legacy call', 0, 'none', 1)",
                [],
            )
            .unwrap();
        }

        let store = Store::open(&db).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        // The v2 column exists and the pre-existing task is open, not lost.
        let open = store.list_tasks(false, None).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "01OLDTASK");
        assert_eq!(open[0].instruction.as_deref(), Some("legacy work"));
        assert!(open[0].completed_at_ms.is_none());

        // The v3 column exists and a decision predating it reads as authorless
        // rather than failing (#64).
        let old = store.get_decision("01OLDDEC").unwrap().unwrap();
        assert_eq!(old.rationale, "legacy call");
        assert!(old.agent.author.is_none());

        // Re-opening applies nothing further.
        drop(store);
        let store = Store::open(&db).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.list_tasks(false, None).unwrap().len(), 1);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn refuses_a_store_from_a_newer_dlog() {
        let db = temp_db("future");
        {
            let store = Store::open(&db).unwrap();
            store
                .conn
                .execute(
                    "UPDATE schema_meta SET value = '999' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        match Store::open(&db) {
            Err(OpenError::SchemaTooNew { found, supported }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            Err(other) => panic!("expected SchemaTooNew, got {other:?}"),
            Ok(_) => panic!("a newer store must not open"),
        }
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn a_fresh_store_applies_every_migration() {
        // An empty file starts at version 0 and converges on the same schema an
        // upgraded store reaches.
        let db = temp_db("fresh");
        let store = Store::open(&db).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        const {
            assert!(
                SCHEMA_VERSION >= 2,
                "the sequence has more than the baseline"
            )
        };
        let task = store.insert_task(None, None).unwrap();
        assert!(store.complete_task(&task).is_ok(), "v2 column is present");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn complete_task_keeps_the_first_completion_time() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .insert_task(None, Some("investigate the flake"))
            .unwrap();
        assert!(
            store.list_tasks(false, None).unwrap()[0]
                .completed_at_ms
                .is_none()
        );

        let first = store.complete_task(&task).unwrap();
        // A second `task done` is a follow-up seal, not a re-completion.
        let again = store.complete_task(&task).unwrap();
        assert_eq!(first, again);

        // Completed tasks drop out of the open list but stay in the full one.
        assert!(store.list_tasks(false, None).unwrap().is_empty());
        let all = store.list_tasks(true, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].completed_at_ms, Some(first));
    }

    #[test]
    fn list_tasks_filters_by_state_and_parent_with_staged_counts() {
        let store = Store::open_in_memory().unwrap();
        let parent = store.insert_task(None, Some("ship it")).unwrap();
        let child = store.insert_task(Some(&parent), None).unwrap();
        let other = store.insert_task(None, None).unwrap();

        store
            .stage_decision(&NewDecision {
                task_id: Some(child.clone()),
                ..minimal("still in flight")
            })
            .unwrap();
        let sealed = store
            .stage_decision(&NewDecision {
                task_id: Some(child.clone()),
                ..minimal("already sealed")
            })
            .unwrap();
        store.seal(&sealed, &Binding::None).unwrap();
        store.complete_task(&other).unwrap();

        // Open only: the completed one is gone, and rows come back newest-first.
        let open = store.list_tasks(false, None).unwrap();
        let ids: Vec<&str> = open.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&child.as_str()) && ids.contains(&parent.as_str()));
        assert!(ids.windows(2).all(|w| w[0] > w[1]), "descending by id");

        let row = |id: &str| open.iter().find(|t| t.id == id).unwrap().clone();
        // Only unsealed decisions count towards staged_count.
        assert_eq!(row(&child).staged_count, 1);
        assert_eq!(row(&parent).staged_count, 0);
        assert_eq!(row(&child).parent_task_id.as_deref(), Some(parent.as_str()));
        assert_eq!(row(&parent).instruction.as_deref(), Some("ship it"));

        assert_eq!(store.list_tasks(true, None).unwrap().len(), 3);
        // --parent narrows to that task's children.
        let kids = store.list_tasks(true, Some(&parent)).unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].id, child);
        assert!(store.list_tasks(true, Some("01NOPE")).unwrap().is_empty());
    }

    #[test]
    fn stranded_tasks_are_unfinished_ones_holding_staging() {
        let store = Store::open_in_memory().unwrap();
        let stranded = store.insert_task(None, Some("left behind")).unwrap();
        let finished = store.insert_task(None, None).unwrap();
        let quiet = store.insert_task(None, None).unwrap();

        store
            .stage_decision(&NewDecision {
                task_id: Some(stranded.clone()),
                ..minimal("never sealed")
            })
            .unwrap();
        store
            .stage_decision(&NewDecision {
                task_id: Some(finished.clone()),
                ..minimal("sealed at task end")
            })
            .unwrap();
        // `quiet` is open but has nothing staged; task-less staging is invisible
        // here by construction (it belongs to no task).
        store.stage_decision(&minimal("no task at all")).unwrap();

        let ids = store.staged_decision_ids_for_task(&finished).unwrap();
        store.seal_staged(&Binding::None, Some(&ids)).unwrap();
        store.complete_task(&finished).unwrap();

        let rows = store.stranded_tasks(20).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task, stranded);
        assert_eq!(rows[0].instruction.as_deref(), Some("left behind"));
        assert_eq!(rows[0].staged_count, 1);
        assert!(rows[0].oldest_staged_ms > 0);
        assert_eq!(store.status().unwrap().stranded_task_count, 1);
        assert!(!quiet.is_empty());

        // The cap is honoured.
        assert!(store.stranded_tasks(0).unwrap().is_empty());
    }

    #[test]
    fn stage_then_read_roundtrips_as_pending() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .stage_decision(&minimal("add retry around flaky API"))
            .unwrap();

        let d = store.get_decision(&id).unwrap().expect("decision exists");
        assert!(d.staged);
        assert!(d.binding.is_none());
        assert_eq!(d.rationale, "add retry around flaky API");
        assert_eq!(d.agent, agent());
        assert_eq!(d.anchors.len(), 1);
        assert_eq!(
            d.anchors[0].symbol_path.as_deref(),
            Some("AuthService::authenticate")
        );
        assert_eq!(d.anchors[0].line_span, Some((10, 45)));
    }

    #[test]
    fn seal_commit_moves_to_immutable_main_log() {
        let store = Store::open_in_memory().unwrap();
        let id = store.stage_decision(&minimal("seal me")).unwrap();

        store
            .seal(&id, &Binding::Commit { sha: "a3f9".into() })
            .unwrap();

        let d = store.get_decision(&id).unwrap().unwrap();
        assert!(!d.staged);
        assert_eq!(d.binding, Some(Binding::Commit { sha: "a3f9".into() }));

        // Re-sealing a sealed decision is rejected (already out of staging).
        assert!(store.seal(&id, &Binding::None).is_err());
    }

    #[test]
    fn seal_none_for_non_code_decisions() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .stage_decision(&minimal("investigation only"))
            .unwrap();
        store.seal(&id, &Binding::None).unwrap();
        let d = store.get_decision(&id).unwrap().unwrap();
        assert_eq!(d.binding, Some(Binding::None));
    }

    #[test]
    fn staged_ids_are_scoped_to_their_task() {
        let store = Store::open_in_memory().unwrap();
        let mine = store.insert_task(None, Some("resilience")).unwrap();
        let theirs = store.insert_task(None, None).unwrap();

        let a = store
            .stage_decision(&NewDecision {
                task_id: Some(mine.clone()),
                ..minimal("mine, staged")
            })
            .unwrap();
        let sealed = store
            .stage_decision(&NewDecision {
                task_id: Some(mine.clone()),
                ..minimal("mine, already sealed")
            })
            .unwrap();
        store.seal(&sealed, &Binding::None).unwrap();
        store
            .stage_decision(&NewDecision {
                task_id: Some(theirs.clone()),
                ..minimal("another agent's, still in flight")
            })
            .unwrap();
        store.stage_decision(&minimal("no task at all")).unwrap();

        // Only this task's *staged* decisions — not sealed ones, not another
        // task's, not the task-less ones.
        assert_eq!(store.staged_decision_ids_for_task(&mine).unwrap(), vec![a]);
        assert_eq!(
            store.staged_decision_ids_for_task("nope").unwrap(),
            Vec::<String>::new()
        );

        assert!(store.task_exists(&mine).unwrap());
        assert!(store.task_exists(&theirs).unwrap());
        assert!(!store.task_exists("nope").unwrap());
    }

    #[test]
    fn insert_task_records_the_hierarchy() {
        let store = Store::open_in_memory().unwrap();
        let parent = store.insert_task(None, Some("ship the feature")).unwrap();
        let child = store.insert_task(Some(&parent), None).unwrap();

        let got: (Option<String>, Option<String>) = store
            .conn
            .query_row(
                "SELECT parent_task_id, instruction FROM task WHERE id = ?1",
                params![child],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(got.0.as_deref(), Some(parent.as_str()));
        assert_eq!(got.1, None);
    }

    #[test]
    fn main_log_is_append_only() {
        let store = Store::open_in_memory().unwrap();
        let id = store.stage_decision(&minimal("locked")).unwrap();
        store.seal(&id, &Binding::None).unwrap();

        // Direct mutation/deletion of a sealed row is blocked by the triggers.
        let update = store.conn.execute(
            "UPDATE decision SET rationale = 'tampered' WHERE id = ?1",
            params![id],
        );
        assert!(update.is_err());
        let delete = store
            .conn
            .execute("DELETE FROM decision WHERE id = ?1", params![id]);
        assert!(delete.is_err());
    }

    #[test]
    fn file_level_anchor_has_no_symbol() {
        let store = Store::open_in_memory().unwrap();
        let mut d = minimal("doc decision");
        d.anchors = vec![Anchor {
            file: "README.md".into(),
            symbol_path: None,
            node_kind: None,
            structural_hash: None,
            line_span: None,
            recorded_at_sha: None,
        }];
        let id = store.stage_decision(&d).unwrap();
        let got = store.get_decision(&id).unwrap().unwrap();
        assert_eq!(got.anchors[0].file, "README.md");
        assert!(got.anchors[0].symbol_path.is_none());
    }

    #[test]
    fn rejected_and_caused_by_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let mut d = minimal("with extras");
        d.rejected = vec![Rejected {
            approach: "polling".into(),
            reason: "wasteful".into(),
        }];
        let first = store.stage_decision(&minimal("first")).unwrap();
        d.caused_by = vec![first.clone()];
        d.supersedes = Some(first.clone());
        let id = store.stage_decision(&d).unwrap();

        let got = store.get_decision(&id).unwrap().unwrap();
        assert_eq!(got.rejected.len(), 1);
        assert_eq!(got.rejected[0].approach, "polling");
        assert_eq!(got.caused_by, vec![first.clone()]);
        assert_eq!(got.supersedes.as_deref(), Some(first.as_str()));
    }

    #[test]
    fn invariant_records_provenance_and_survives() {
        let store = Store::open_in_memory().unwrap();
        let dec = store.stage_decision(&minimal("declares inv")).unwrap();
        let inv = store
            .insert_invariant(&dec, "tokens never logged", Some("src/auth"))
            .unwrap();

        let live = store.live_invariants().unwrap();
        assert_eq!(live, vec![(inv, "tokens never logged".to_string())]);
    }

    #[test]
    fn fts_finds_decision_by_rationale() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .stage_decision(&minimal("switch to exponential backoff for retries"))
            .unwrap();
        store
            .stage_decision(&minimal("unrelated styling tweak"))
            .unwrap();

        let hits = store.search("backoff").unwrap();
        assert_eq!(hits, vec![id]);
    }

    #[test]
    fn fts_query_with_special_chars_does_not_error() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .stage_decision(&minimal("use exponential backoff on errors"))
            .unwrap();

        // Operators / punctuation / unbalanced quotes must not raise a syntax
        // error — they are normalised to literal terms.
        store.search("backoff OR (retry)").unwrap();
        store.search("\"unbalanced").unwrap();
        store.search(":*foo").unwrap();

        // Whitespace-only yields no results.
        assert!(store.search("   ").unwrap().is_empty());

        // A plain term still matches; multiple terms are implicit AND.
        assert_eq!(store.search("backoff").unwrap(), vec![id.clone()]);
        assert_eq!(store.search("backoff errors").unwrap(), vec![id]);
        assert!(store.search("backoff missingword").unwrap().is_empty());
    }

    #[test]
    fn status_counts_staging_and_reports_version() {
        let store = Store::open_in_memory().unwrap();
        let a = store.stage_decision(&minimal("one")).unwrap();
        store.stage_decision(&minimal("two")).unwrap();
        store.seal(&a, &Binding::None).unwrap();

        let status = store.status().unwrap();
        assert_eq!(status.staging_count, 1); // one sealed, one still staged
        assert!(status.oldest_staged_ms.is_some());
        assert_eq!(status.schema_version, SCHEMA_VERSION);
        // Neither decision belongs to a task, so nothing is stranded per-task.
        assert_eq!(status.stranded_task_count, 0);
    }
}
