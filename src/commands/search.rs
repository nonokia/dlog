//! `dlog search` — full-text search over decision prose (design §9.2).
//!
//! SQLite FTS5 over rationale/rejected text, returned in the compact form so it
//! composes with `dlog show` like `why` does.

use serde::Serialize;

use crate::cli::SearchArgs;
use crate::commands::compact::{self, CompactRow};
use crate::commands::{AppError, open_store};
use crate::output::{QueryEnvelope, emit};
use crate::store::Store;

/// Describes the interpreted query (§9.3).
#[derive(Debug, Serialize)]
struct SearchDesc {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

pub fn run(args: SearchArgs) -> Result<(), AppError> {
    let store = open_store(args.db.clone())?;
    let envelope = build(&store, &args)?;
    emit(&envelope);
    Ok(())
}

fn build(
    store: &Store,
    args: &SearchArgs,
) -> rusqlite::Result<QueryEnvelope<SearchDesc, CompactRow>> {
    let ids = store.search(&args.text)?;
    let (results, truncated) = compact::collect(store, &ids, args.include_superseded, args.limit)?;
    Ok(QueryEnvelope {
        query: SearchDesc {
            kind: "search",
            text: args.text.clone(),
        },
        // A text search resolves no anchor, so there is no `resolved` head.
        resolved: None,
        results,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-search-{}.db", ulid::Ulid::new()))
    }

    fn seed(store: &Store, rationale: &str) -> String {
        store
            .stage_decision(&NewDecision {
                task_id: None,
                agent: Agent {
                    role: "implementer".into(),
                    model: "claude-test".into(),
                    session_id: None,
                },
                conversation_id: None,
                rationale: rationale.into(),
                rejected: vec![],
                caused_by: vec![],
                supersedes: None,
                anchors: vec![Anchor {
                    file: "src/lib.rs".into(),
                    symbol_path: None,
                    node_kind: None,
                    structural_hash: None,
                    line_span: None,
                    recorded_at_sha: None,
                }],
            })
            .unwrap()
    }

    fn search_args(db: &std::path::Path, text: &str) -> SearchArgs {
        SearchArgs {
            text: text.into(),
            include_superseded: false,
            limit: 20,
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn search_finds_decision_by_rationale_text() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let id = seed(&store, "switch to exponential backoff for retries");
        seed(&store, "unrelated styling tweak");

        let env = build(&store, &search_args(&db, "backoff")).unwrap();
        assert_eq!(env.results.len(), 1);
        assert_eq!(env.results[0].id, id);
        assert!(env.resolved.is_none(), "search resolves no anchor");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn search_no_match_is_empty() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "something else entirely");
        let env = build(&store, &search_args(&db, "nonexistentterm")).unwrap();
        assert!(env.results.is_empty());
        let _ = std::fs::remove_file(&db);
    }
}
