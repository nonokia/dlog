//! `dlog why` — explain the decisions behind a location or symbol (design §9).
//!
//! The primary query (§9.2). It resolves the target to a node, asks the
//! resolver (#8) which decisions match and at what confidence, and returns the
//! **compact** form (§9.1): id + rationale summary + binding + flags + ts. The
//! agent drills into full detail with `dlog show <id>`.
//!
//! Scope follows §9.1: superseded decisions are excluded by default
//! (`--include-superseded` for history), staging is included and flagged
//! `staged: true`, and the main-log/staging union is presented as one list.

use serde::Serialize;

use crate::anchor;
use crate::cli::WhyArgs;
use crate::commands::compact::{self, CompactRow};
use crate::commands::{AppError, open_store, parse_line_spec};
use crate::output::{QueryEnvelope, Resolved as ResolvedHead, emit};
use crate::resolve::{self, QueryNode};
use crate::store::Store;

/// Describes the interpreted query (§9.3).
#[derive(Debug, Serialize)]
struct QueryDesc {
    #[serde(rename = "type")]
    kind: &'static str,
    target: String,
}

pub fn run(args: WhyArgs) -> Result<(), AppError> {
    let store = open_store(args.db.clone())?;
    let envelope = build(&store, &args)?;
    emit(&envelope);
    Ok(())
}

/// Core of `why`, separated from emission so it can be unit-tested.
fn build(store: &Store, args: &WhyArgs) -> rusqlite::Result<QueryEnvelope<QueryDesc, CompactRow>> {
    let query_node = build_query_node(&args.target);
    let resolved = resolve::resolve(store, &query_node)?;

    let (results, truncated) = compact::collect(
        store,
        &resolved.decisions,
        args.include_superseded,
        args.limit,
    )?;

    let node = query_node
        .symbol_path
        .clone()
        .or_else(|| query_node.file.clone())
        .unwrap_or_else(|| args.target.clone());

    Ok(QueryEnvelope {
        query: QueryDesc {
            kind: "why",
            target: args.target.clone(),
        },
        resolved: Some(ResolvedHead {
            node,
            resolution: resolved.resolution,
        }),
        results,
        truncated,
    })
}

/// Interpret a target into a [`QueryNode`]. `file:line` / `file:start-end` read
/// the file and resolve the enclosing Rust definition (§10.4); a path with no
/// line is a file-level query; anything else is treated as a symbol path.
fn build_query_node(target: &str) -> QueryNode {
    if let Some((path, lines)) = target.rsplit_once(':')
        && let Some((start, _end)) = parse_line_spec(lines)
    {
        return node_for_file_line(path, start);
    }
    if target.contains('/') || (target.contains('.') && !target.contains("::")) {
        return QueryNode::file_level(target);
    }
    QueryNode {
        file: None,
        symbol_path: Some(target.to_string()),
        structural_hash: None,
    }
}

fn node_for_file_line(path: &str, line: u32) -> QueryNode {
    if path.ends_with(".rs")
        && let Ok(source) = std::fs::read_to_string(path)
        && let Some(def) = anchor::definition_at_line(&source, line)
    {
        return QueryNode::from_definition(path, &def);
    }
    QueryNode::file_level(path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};
    use crate::output::Resolution;

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-why-{}.db", ulid::Ulid::new()))
    }

    fn seed(
        store: &Store,
        rationale: &str,
        symbol: Option<&str>,
        supersedes: Option<&str>,
    ) -> String {
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
                supersedes: supersedes.map(str::to_string),
                anchors: vec![Anchor {
                    file: "src/auth.rs".into(),
                    symbol_path: symbol.map(str::to_string),
                    node_kind: symbol.map(|_| "function".to_string()),
                    structural_hash: Some("hash_1".into()),
                    line_span: Some((10, 20)),
                    recorded_at_sha: None,
                }],
            })
            .unwrap()
    }

    fn why_args(db: &std::path::Path, target: &str) -> WhyArgs {
        WhyArgs {
            target: target.into(),
            include_superseded: false,
            limit: 20,
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn build_query_node_classifies_targets() {
        assert_eq!(
            build_query_node("AuthService::authenticate")
                .symbol_path
                .as_deref(),
            Some("AuthService::authenticate")
        );
        let file = build_query_node("README.md");
        assert_eq!(file.file.as_deref(), Some("README.md"));
        assert!(file.symbol_path.is_none());
    }

    #[test]
    fn why_symbol_returns_matching_decision_as_drifted() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let id = seed(
            &store,
            "add retry with backoff",
            Some("AuthService::authenticate"),
            None,
        );

        let env = build(&store, &why_args(&db, "AuthService::authenticate")).unwrap();
        // Symbol-only query can't confirm the hash, so it's drifted.
        assert_eq!(
            env.resolved.as_ref().unwrap().resolution,
            Resolution::Drifted
        );
        assert_eq!(env.results.len(), 1);
        assert_eq!(env.results[0].id, id);
        assert_eq!(env.results[0].rationale_summary, "add retry with backoff");
        assert!(env.results[0].staged);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn why_excludes_superseded_by_default() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let old = seed(&store, "first attempt", Some("svc::f"), None);
        let new = seed(&store, "revised", Some("svc::f"), Some(&old));

        let env = build(&store, &why_args(&db, "svc::f")).unwrap();
        let ids: Vec<&str> = env.results.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec![new.as_str()], "superseded `old` is hidden");

        let mut args = why_args(&db, "svc::f");
        args.include_superseded = true;
        let env = build(&store, &args).unwrap();
        assert_eq!(env.results.len(), 2, "history includes superseded");
        let superseded_row = env.results.iter().find(|r| r.id == old).unwrap();
        assert!(superseded_row.superseded);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn why_file_fallback_when_symbol_unknown() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "x", Some("svc::f"), None);

        let env = build(&store, &why_args(&db, "totally::unknown")).unwrap();
        assert_eq!(env.resolved.unwrap().resolution, Resolution::FileFallback);
        assert!(env.results.is_empty());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn why_truncates_and_flags() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        for i in 0..3 {
            seed(&store, &format!("decision {i}"), Some("svc::f"), None);
        }
        let mut args = why_args(&db, "svc::f");
        args.limit = 2;
        let env = build(&store, &args).unwrap();
        assert_eq!(env.results.len(), 2);
        assert!(env.truncated);
        let _ = std::fs::remove_file(&db);
    }
}
