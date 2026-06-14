//! `dlog status` — store-wide state (design §8.3, §9.2).
//!
//! Reports unsealed staging (count and oldest, to surface staging that has gone
//! stale after a bare `git commit`) and the schema version. Kept separate from
//! query-result warnings: this is about the whole store, not one query (§9.3).

use crate::cli::StatusArgs;
use crate::commands::{AppError, open_store};
use crate::output::emit;

pub fn run(args: StatusArgs) -> Result<(), AppError> {
    let store = open_store(args.db)?;
    emit(&store.status()?);
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
