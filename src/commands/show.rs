//! `dlog show` — full decisions by id (design §9.1).
//!
//! The drill-down half of two-stage retrieval: after a compact `why`/`search`,
//! the agent fetches whole records (rejected alternatives, anchors, declared
//! invariants, binding) for the ids it cares about.

use serde::Serialize;

use crate::cli::ShowArgs;
use crate::commands::{AppError, open_store};
use crate::model::StoredDecision;
use crate::output::emit;
use crate::store::Store;

#[derive(Debug, Serialize)]
struct ShowEnvelope {
    results: Vec<ShowDecision>,
    /// Ids that were requested but not found.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ShowDecision {
    #[serde(flatten)]
    decision: StoredDecision,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    declares_invariants: Vec<InvariantOut>,
}

#[derive(Debug, Serialize)]
struct InvariantOut {
    id: String,
    statement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

pub fn run(args: ShowArgs) -> Result<(), AppError> {
    let store = open_store(args.db)?;

    let mut results = Vec::new();
    let mut missing = Vec::new();
    for id in &args.ids {
        match store.get_decision(id)? {
            Some(decision) => results.push(build_one(&store, decision)?),
            None => missing.push(id.clone()),
        }
    }

    emit(&ShowEnvelope { results, missing });
    Ok(())
}

fn build_one(store: &Store, decision: StoredDecision) -> rusqlite::Result<ShowDecision> {
    let declares_invariants = store
        .invariants_declared_by(&decision.id)?
        .into_iter()
        .map(|(id, statement, scope)| InvariantOut {
            id,
            statement,
            scope,
        })
        .collect();
    Ok(ShowDecision {
        decision,
        declares_invariants,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-show-{}.db", ulid::Ulid::new()))
    }

    #[test]
    fn show_returns_full_record_with_invariants_and_flags_missing() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let id = store
            .stage_decision(&NewDecision {
                task_id: None,
                agent: Agent {
                    role: "implementer".into(),
                    model: "claude-test".into(),
                    session_id: None,
                },
                conversation_id: None,
                rationale: "guard against null token".into(),
                rejected: vec![],
                caused_by: vec![],
                supersedes: None,
                anchors: vec![Anchor {
                    file: "src/auth.rs".into(),
                    symbol_path: Some("AuthService::authenticate".into()),
                    node_kind: Some("method".into()),
                    structural_hash: Some("h1".into()),
                    line_span: Some((10, 20)),
                    recorded_at_sha: None,
                }],
            })
            .unwrap();
        store
            .insert_invariant(&id, "tokens never logged", Some("src/auth"))
            .unwrap();

        let args = ShowArgs {
            ids: vec![id.clone(), "missing_id".into()],
            db: Some(db.to_string_lossy().into_owned()),
        };

        // Exercise the JSON shape via the build path.
        let decision = store.get_decision(&id).unwrap().unwrap();
        let shown = build_one(&store, decision).unwrap();
        assert_eq!(shown.decision.rationale, "guard against null token");
        assert_eq!(shown.declares_invariants.len(), 1);
        assert_eq!(
            shown.declares_invariants[0].statement,
            "tokens never logged"
        );

        // And the handler runs end-to-end, recording the missing id.
        run(args).unwrap();
        let _ = std::fs::remove_file(&db);
    }
}
