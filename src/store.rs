//! SQLite-backed storage: the staging/main-log split, the three entities, and
//! the binding/FTS schema (design §7, §8.2). This layer only *persists* observed
//! anchor values (§10.2); anchor resolution is a separate concern (#8).
//!
//! Each `dlog` invocation opens the store directly — no daemon (§6.1).

use rusqlite::{Connection, OptionalExtension, Row, params};
use ulid::Ulid;

use crate::model::{Agent, Anchor, Binding, NewDecision, Rejected, StoredDecision};

/// Bump when `schema.sql` changes incompatibly.
pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Store-wide status, reported by `dlog status` (§9.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreStatus {
    /// Number of unsealed decisions sitting in staging.
    pub staging_count: i64,
    /// Timestamp (epoch ms) of the oldest staged decision, if any — surfaces
    /// staging that has gone stale (§8.3).
    pub oldest_staged_ms: Option<i64>,
    pub schema_version: i64,
}

/// A handle to the SQLite-backed decision log.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open an on-disk store, creating/migrating the schema as needed.
    pub fn open(path: impl AsRef<std::path::Path>) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// Open a private in-memory store (tests, throwaway use).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Apply the schema. Idempotent: the DDL is all `IF NOT EXISTS`, and the
    /// version row is inserted once.
    fn migrate(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO NOTHING",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// The schema version recorded in the store.
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |r| Ok(r.get::<_, String>(0)?.parse().unwrap_or(0)),
        )
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
                conversation_id, rationale, rejected, caused_by, staged,
                binding_type, binding_sha, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, NULL, NULL, ?11)",
            params![
                id_str,
                decision.task_id,
                decision.supersedes,
                decision.agent.role,
                decision.agent.model,
                decision.agent.session_id,
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
                        caused_by, staged, binding_type, binding_sha, created_at_ms
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
    /// ids best-first.
    pub fn search(&self, query: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT decision_id FROM decision_fts
             WHERE decision_fts MATCH ?1 ORDER BY rank",
        )?;
        let ids = stmt
            .query_map(params![query], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
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
        Ok(StoreStatus {
            staging_count,
            oldest_staged_ms,
            schema_version: self.schema_version()?,
        })
    }
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

    #[test]
    fn migrate_is_idempotent_and_records_version() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        // Re-running migration must not error or duplicate the version row.
        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
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
    fn status_counts_staging_and_reports_version() {
        let store = Store::open_in_memory().unwrap();
        let a = store.stage_decision(&minimal("one")).unwrap();
        store.stage_decision(&minimal("two")).unwrap();
        store.seal(&a, &Binding::None).unwrap();

        let status = store.status().unwrap();
        assert_eq!(status.staging_count, 1); // one sealed, one still staged
        assert!(status.oldest_staged_ms.is_some());
        assert_eq!(status.schema_version, SCHEMA_VERSION);
    }
}
