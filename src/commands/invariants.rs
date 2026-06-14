//! `dlog invariants` — list the live declared constraints (design §7.1, §9.2).
//!
//! Invariants outlive the decision that declared them and are queried
//! independently of the decision log. Each carries its provenance
//! (`declared_by`). `--scope <path>` narrows to invariants in effect at, or
//! within, a path; scopeless invariants are global and always returned.

use serde::Serialize;

use crate::cli::InvariantsArgs;
use crate::commands::{AppError, open_store};
use crate::output::emit;

#[derive(Debug, Serialize)]
struct InvariantsEnvelope {
    results: Vec<InvariantOut>,
}

#[derive(Debug, Serialize)]
struct InvariantOut {
    id: String,
    statement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    /// The decision that declared this invariant (§7.1).
    declared_by: String,
}

pub fn run(args: InvariantsArgs) -> Result<(), AppError> {
    let store = open_store(args.db)?;
    let results = store
        .list_live_invariants()?
        .into_iter()
        .filter(|row| match &args.scope {
            Some(query) => scope_matches(&row.scope, query),
            None => true,
        })
        .map(|row| InvariantOut {
            id: row.id,
            statement: row.statement,
            scope: row.scope,
            declared_by: row.declared_by,
        })
        .collect();

    emit(&InvariantsEnvelope { results });
    Ok(())
}

/// Whether an invariant's `scope` is relevant to a `--scope` query path. A
/// scopeless invariant is global; otherwise the two are related when one path
/// contains the other (ancestor invariants in effect at the path, or invariants
/// living under the queried subtree).
fn scope_matches(invariant_scope: &Option<String>, query: &str) -> bool {
    match invariant_scope {
        None => true,
        Some(scope) => {
            scope == query
                || query.starts_with(&format!("{scope}/"))
                || scope.starts_with(&format!("{query}/"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};
    use crate::store::Store;

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-inv-{}.db", ulid::Ulid::new()))
    }

    fn seed_decision(store: &Store) -> String {
        store
            .stage_decision(&NewDecision {
                task_id: None,
                agent: Agent {
                    role: "implementer".into(),
                    model: "claude-test".into(),
                    session_id: None,
                },
                conversation_id: None,
                rationale: "decl".into(),
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

    #[test]
    fn scope_matching_rules() {
        assert!(scope_matches(&None, "anything")); // global
        assert!(scope_matches(&Some("src/auth".into()), "src/auth"));
        assert!(scope_matches(&Some("src/auth".into()), "src/auth/login.rs")); // ancestor in effect
        assert!(scope_matches(&Some("src/auth/login.rs".into()), "src/auth")); // under subtree
        assert!(!scope_matches(&Some("src/auth".into()), "src/net"));
    }

    #[test]
    fn lists_all_then_filters_by_scope() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let dec = seed_decision(&store);
        store.insert_invariant(&dec, "global rule", None).unwrap();
        store
            .insert_invariant(&dec, "auth rule", Some("src/auth"))
            .unwrap();
        store
            .insert_invariant(&dec, "net rule", Some("src/net"))
            .unwrap();

        // No scope -> all three.
        let all = store.list_live_invariants().unwrap();
        assert_eq!(all.len(), 3);

        // Scope src/auth -> global + auth rule, not net.
        let matched: Vec<_> = all
            .iter()
            .filter(|row| scope_matches(&row.scope, "src/auth"))
            .map(|row| row.statement.clone())
            .collect();
        assert!(matched.contains(&"global rule".to_string()));
        assert!(matched.contains(&"auth rule".to_string()));
        assert!(!matched.contains(&"net rule".to_string()));

        // Handler runs end-to-end.
        run(InvariantsArgs {
            scope: Some("src/auth".into()),
            db: Some(db.to_string_lossy().into_owned()),
        })
        .unwrap();
        let _ = std::fs::remove_file(&db);
    }
}
