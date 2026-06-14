//! Domain types for the decision log (design §7).
//!
//! These mirror the storage schema but are the types the rest of the crate works
//! with. Serialize/Deserialize is derived where a type also appears on the JSON
//! wire (anchors, bindings, rejected alternatives); the input/output structs
//! that never hit the wire stay plain.

use serde::{Deserialize, Serialize};

/// Identity of the agent that recorded a decision (§4, §7.4). `role` separates
/// e.g. reviewer from implementer; `model`/`session_id` locate it in a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub role: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// A rejected alternative: what was tried and why it was dropped (§7.4).
/// Optional, to keep recording friction low (§7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub approach: String,
    pub reason: String,
}

/// An anchor observation captured at record time (§10.2). Nothing here asserts
/// anything about the present — identity is judged at query time (#8). A
/// file-level anchor (language-independent, §10.5) leaves `symbol_path` and
/// `structural_hash` as `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structural_hash: Option<String>,
    /// Human snapshot only — never used for resolution (§10.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_span: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at_sha: Option<String>,
}

/// The binding stamped on a decision at seal time (§8.2). Main-log decisions
/// always carry one; staged decisions have none ("pending" lives only in
/// staging). Serializes as `{"type":"commit","sha":...}` / `{"type":"none"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Binding {
    /// The decision led to a commit.
    Commit { sha: String },
    /// Investigation/review that led to no commit.
    None,
}

/// Fields supplied when staging a new decision (§7.4). Only `rationale`, an
/// anchor, and `agent` are effectively required (§7.3); the rest are optional.
#[derive(Debug, Clone)]
pub struct NewDecision {
    pub task_id: Option<String>,
    pub agent: Agent,
    pub conversation_id: Option<String>,
    pub rationale: String,
    pub rejected: Vec<Rejected>,
    pub caused_by: Vec<String>,
    pub supersedes: Option<String>,
    pub anchors: Vec<Anchor>,
}

/// A decision as stored, including its lifecycle state. `staged == true` means it
/// is still in the staging area (pending); once sealed it is immutable and
/// carries a `binding`.
#[derive(Debug, Clone, Serialize)]
pub struct StoredDecision {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub agent: Agent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub rationale: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejected>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub caused_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub anchors: Vec<Anchor>,
    pub staged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<Binding>,
    /// Record time, epoch milliseconds.
    #[serde(rename = "ts")]
    pub created_at_ms: i64,
}
