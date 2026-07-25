//! `dlog status` — store-wide state (design §8.3, §9.2).
//!
//! Reports the workspace it resolved (which store is answering, and why that
//! directory is the root) plus unsealed staging (count and oldest, to surface
//! staging that has gone stale after a bare `git commit`) and the schema
//! version. Kept separate from query-result warnings: this is about the whole
//! store, not one query (§9.3).

use serde::Serialize;

use crate::cli::StatusArgs;
use crate::commands::{AppError, RootSource, Workspace};
use crate::output::emit;
use crate::store::StoreStatus;

/// Success document for `dlog status`: the workspace, then the store's own state.
#[derive(Debug, Serialize)]
struct StatusResult {
    root: String,
    db: String,
    /// How the root was found — `dlog` (a `.dlog/` directory), `git`, or `cwd`.
    /// Without this a split store is invisible: two directories can look
    /// identical while answering from different logs.
    root_source: RootSource,
    #[serde(flatten)]
    store: StoreStatus,
}

pub fn run(args: StatusArgs) -> Result<(), AppError> {
    let workspace = Workspace::discover(args.db)?;
    let store = workspace.open()?;
    emit(&StatusResult {
        root: workspace.root.display().to_string(),
        db: workspace.db.display().to_string(),
        root_source: workspace.root_source,
        store: store.status()?,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};
    use crate::store::Store;

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-status-{}.db", ulid::Ulid::new()))
    }

    #[test]
    fn status_runs_against_a_store() {
        let db = temp_db();
        {
            let store = Store::open(&db).unwrap();
            store
                .stage_decision(&NewDecision {
                    task_id: None,
                    agent: Agent {
                        role: "implementer".into(),
                        model: "claude-test".into(),
                        session_id: None,
                    },
                    conversation_id: None,
                    rationale: "pending".into(),
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
                .unwrap();
        }
        run(StatusArgs {
            db: Some(db.to_string_lossy().into_owned()),
        })
        .unwrap();
        let _ = std::fs::remove_file(&db);
    }
}
