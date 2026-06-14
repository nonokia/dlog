//! Query-time anchor resolution (design §10.1, §10.3).
//!
//! Identity is **not stored, it is judged here**. A stored anchor only records
//! what was observed when a decision was made (§10.2); at query time we compare
//! that against what the agent is asking about and report a confidence as a
//! [`Resolution`]. Resolution never errors — when nothing better matches it
//! degrades to file level so the agent isn't blocked (§9.2, §10.5).
//!
//! The 2-axis matrix (§10.3) becomes a precedence cascade:
//!
//! | symbol_path | structural_hash | resolution      |
//! |-------------|-----------------|-----------------|
//! | match       | match           | `exact`         |
//! | match       | mismatch        | `drifted`       |
//! | mismatch    | match           | `relocated`     |
//! | mismatch    | mismatch        | `file_fallback` |
//!
//! An absent query field (e.g. a bare symbol query has no hash) simply can't
//! match on that axis, which collapses to the same table.

use crate::anchor::Definition;
use crate::output::Resolution;
use crate::store::Store;

/// What the query knows about the node it asks about, observed from the current
/// code. Any field may be absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryNode {
    pub file: Option<String>,
    pub symbol_path: Option<String>,
    pub structural_hash: Option<String>,
}

impl QueryNode {
    /// A file-level query (no node resolved), e.g. a non-code path or a line
    /// outside any definition.
    pub fn file_level(file: impl Into<String>) -> Self {
        Self {
            file: Some(file.into()),
            symbol_path: None,
            structural_hash: None,
        }
    }

    /// A query carrying the node observed at a location (from #7's extractor).
    pub fn from_definition(file: impl Into<String>, def: &Definition) -> Self {
        Self {
            file: Some(file.into()),
            symbol_path: Some(def.symbol_path.clone()),
            structural_hash: Some(def.structural_hash.clone()),
        }
    }
}

/// The outcome of resolving a query: the confidence tier plus the matching
/// decision ids (newest-first). Scope filtering (superseded/staging, §9.1) is
/// the caller's job — this returns every decision the anchor matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub resolution: Resolution,
    pub decisions: Vec<String>,
}

/// Resolve a query node against the store via the precedence cascade.
pub fn resolve(store: &Store, query: &QueryNode) -> rusqlite::Result<Resolved> {
    // exact: same symbol AND same structure on a single anchor.
    if let (Some(symbol), Some(hash)) = (&query.symbol_path, &query.structural_hash) {
        let decisions = store.decision_ids_by_symbol_and_hash(symbol, hash)?;
        if !decisions.is_empty() {
            return Ok(Resolved {
                resolution: Resolution::Exact,
                decisions,
            });
        }
    }

    // drifted: the symbol matches but the code has moved on (stale decision
    // possible). A symbol-only query with no current hash lands here too.
    if let Some(symbol) = &query.symbol_path {
        let decisions = store.decision_ids_by_symbol(symbol)?;
        if !decisions.is_empty() {
            return Ok(Resolved {
                resolution: Resolution::Drifted,
                decisions,
            });
        }
    }

    // relocated: a node with this exact structure exists under a different name
    // or in a different file (global hash match, §10.3).
    if let Some(hash) = &query.structural_hash {
        let decisions = store.decision_ids_by_hash(hash)?;
        if !decisions.is_empty() {
            return Ok(Resolved {
                resolution: Resolution::Relocated,
                decisions,
            });
        }
    }

    // file_fallback: degrade to file level (§10.5). Empty is fine — the query
    // simply has no decisions, it is not an error.
    let decisions = match &query.file {
        Some(file) => store.decision_ids_by_file(file)?,
        None => Vec::new(),
    };
    Ok(Resolved {
        resolution: Resolution::FileFallback,
        decisions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

    /// Stage a decision carrying a single anchor with the given observations.
    fn seed(store: &Store, file: &str, symbol: Option<&str>, hash: Option<&str>) -> String {
        store
            .stage_decision(&NewDecision {
                task_id: None,
                agent: Agent {
                    role: "implementer".into(),
                    model: "claude-test".into(),
                    session_id: None,
                },
                conversation_id: None,
                rationale: "seed".into(),
                rejected: vec![],
                caused_by: vec![],
                supersedes: None,
                anchors: vec![Anchor {
                    file: file.into(),
                    symbol_path: symbol.map(str::to_string),
                    node_kind: symbol.map(|_| "function".to_string()),
                    structural_hash: hash.map(str::to_string),
                    line_span: Some((1, 9)),
                    recorded_at_sha: None,
                }],
            })
            .unwrap()
    }

    fn store_with_seed() -> (Store, String) {
        let store = Store::open_in_memory().unwrap();
        let id = seed(&store, "src/a.rs", Some("foo::bar"), Some("hash_1"));
        (store, id)
    }

    #[test]
    fn exact_when_symbol_and_hash_match() {
        let (store, id) = store_with_seed();
        let q = QueryNode {
            file: Some("src/a.rs".into()),
            symbol_path: Some("foo::bar".into()),
            structural_hash: Some("hash_1".into()),
        };
        let got = resolve(&store, &q).unwrap();
        assert_eq!(got.resolution, Resolution::Exact);
        assert_eq!(got.decisions, vec![id]);
    }

    #[test]
    fn drifted_when_symbol_matches_but_hash_differs() {
        let (store, id) = store_with_seed();
        let q = QueryNode {
            file: Some("src/a.rs".into()),
            symbol_path: Some("foo::bar".into()),
            structural_hash: Some("hash_CHANGED".into()),
        };
        let got = resolve(&store, &q).unwrap();
        assert_eq!(got.resolution, Resolution::Drifted);
        assert_eq!(got.decisions, vec![id]);
    }

    #[test]
    fn relocated_when_hash_matches_but_symbol_differs() {
        let (store, id) = store_with_seed();
        let q = QueryNode {
            file: Some("src/elsewhere.rs".into()),
            symbol_path: Some("other::renamed".into()),
            structural_hash: Some("hash_1".into()),
        };
        let got = resolve(&store, &q).unwrap();
        assert_eq!(got.resolution, Resolution::Relocated);
        assert_eq!(got.decisions, vec![id]);
    }

    #[test]
    fn file_fallback_when_neither_axis_matches_but_file_does() {
        let (store, id) = store_with_seed();
        let q = QueryNode {
            file: Some("src/a.rs".into()),
            symbol_path: Some("nope::gone".into()),
            structural_hash: Some("hash_unknown".into()),
        };
        let got = resolve(&store, &q).unwrap();
        assert_eq!(got.resolution, Resolution::FileFallback);
        assert_eq!(got.decisions, vec![id]);
    }

    #[test]
    fn file_fallback_empty_when_nothing_matches() {
        let (store, _id) = store_with_seed();
        let q = QueryNode::file_level("src/unknown.rs");
        let got = resolve(&store, &q).unwrap();
        assert_eq!(got.resolution, Resolution::FileFallback);
        assert!(got.decisions.is_empty());
    }

    #[test]
    fn symbol_only_query_without_hash_is_drifted() {
        // A bare symbol query can't confirm the hash, so it can't be exact.
        let (store, id) = store_with_seed();
        let q = QueryNode {
            file: None,
            symbol_path: Some("foo::bar".into()),
            structural_hash: None,
        };
        let got = resolve(&store, &q).unwrap();
        assert_eq!(got.resolution, Resolution::Drifted);
        assert_eq!(got.decisions, vec![id]);
    }
}
