//! `dlog context <path>` — the decision summary for a file or directory (design
//! §3, §9; v0.2, #30).
//!
//! Task-start context restoration: aggregate the live decisions anchored at a
//! file, or anywhere under a directory, and return them in the compact form so
//! the agent can rebuild "why is this area the way it is" before touching it.

use serde::Serialize;

use crate::cli::ContextArgs;
use crate::commands::compact::{self, CompactRow};
use crate::commands::{AppError, open_store};
use crate::output::{QueryEnvelope, emit};
use crate::store::Store;

/// Describes the interpreted query (§9.3).
#[derive(Debug, Serialize)]
struct ContextDesc {
    #[serde(rename = "type")]
    kind: &'static str,
    path: String,
}

pub fn run(args: ContextArgs) -> Result<(), AppError> {
    let store = open_store(args.db.clone())?;
    let envelope = build(&store, &args)?;
    emit(&envelope);
    Ok(())
}

fn build(
    store: &Store,
    args: &ContextArgs,
) -> rusqlite::Result<QueryEnvelope<ContextDesc, CompactRow>> {
    let ids = store.decision_ids_under_path(&args.path)?;
    let (results, truncated, elided) = compact::collect(
        store,
        &ids,
        args.include_superseded,
        args.limit,
        args.budget,
    )?;
    Ok(QueryEnvelope {
        query: ContextDesc {
            kind: "context",
            path: args.path.clone(),
        },
        // A path aggregate resolves no single node, so there is no `resolved` head.
        resolved: None,
        results,
        truncated,
        elided,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-context-{}.db", ulid::Ulid::new()))
    }

    fn seed(store: &Store, file: &str, rationale: &str) -> String {
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
                    file: file.into(),
                    symbol_path: None,
                    node_kind: None,
                    structural_hash: None,
                    line_span: None,
                    recorded_at_sha: None,
                }],
            })
            .unwrap()
    }

    fn context_args(db: &std::path::Path, path: &str) -> ContextArgs {
        ContextArgs {
            path: path.into(),
            include_superseded: false,
            limit: 20,
            budget: 0,
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    fn ids(env: &QueryEnvelope<ContextDesc, CompactRow>) -> Vec<String> {
        env.results.iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn directory_aggregates_decisions_under_it() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let login = seed(&store, "src/auth/login.rs", "a");
        let token = seed(&store, "src/auth/token.rs", "b");
        seed(&store, "src/net/client.rs", "c");

        let env = build(&store, &context_args(&db, "src/auth")).unwrap();
        let got = ids(&env);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&login) && got.contains(&token));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn exact_file_path_matches_that_file() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let c = seed(&store, "src/net/client.rs", "c");
        seed(&store, "src/auth/login.rs", "a");

        let env = build(&store, &context_args(&db, "src/net/client.rs")).unwrap();
        assert_eq!(ids(&env), vec![c]);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn respects_path_component_boundary() {
        // `src/au` must not match `src/auth/...` (the `/` boundary).
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "src/auth/login.rs", "a");

        let env = build(&store, &context_args(&db, "src/au")).unwrap();
        assert!(env.results.is_empty());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn like_metacharacters_in_path_match_literally() {
        // An underscore in the query must not act as a LIKE wildcard.
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "src/aXb/f.rs", "wild");
        let literal = seed(&store, "src/a_b/f.rs", "literal");

        let env = build(&store, &context_args(&db, "src/a_b")).unwrap();
        assert_eq!(
            ids(&env),
            vec![literal],
            "_ must be literal, not a wildcard"
        );
        let _ = std::fs::remove_file(&db);
    }
}
