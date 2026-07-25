//! `dlog status` — store-wide state (design §8.3, §9.2).
//!
//! Reports the workspace it resolved (which store is answering, and why that
//! directory is the root) plus unsealed staging (count and oldest, to surface
//! staging that has gone stale after a bare `git commit`) and the schema
//! version. Kept separate from query-result warnings: this is about the whole
//! store, not one query (§9.3).
//!
//! Since sealing is per task (`dlog task done`), the count alone doesn't say
//! *whose* work is stranded, so unfinished tasks that still hold staged
//! decisions are named (#61). Facts only — which tasks, how much, how old; what
//! to do about them is the agent's call (§9.1 principle 2, §9.4).

use serde::Serialize;

use crate::cli::StatusArgs;
use crate::commands::compact::{SUMMARY_MAX, summarize};
use crate::commands::{AppError, RootSource, Workspace};
use crate::output::emit;
use crate::store::{Store, StoreStatus};

/// How many stranded tasks `status` names. The full count is reported as
/// `stranded_task_count`, so a store with a long tail of forgotten tasks cannot
/// blow up the payload of the command agents run at every task start.
const STRANDED_LIMIT: usize = 20;

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
    /// Unfinished tasks holding staged decisions, newest-first (up to
    /// `STRANDED_LIMIT`). Each id is what `record --task` / `task done` need to
    /// pick the task back up.
    stranded_tasks: Vec<StrandedOut>,
}

/// One stranded task, in the compact form used by every list (§9.1).
#[derive(Debug, Serialize)]
struct StrandedOut {
    task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    instruction_summary: Option<String>,
    staged_count: i64,
    /// Oldest unsealed decision under this task, epoch ms. Whether that counts
    /// as stale is the agent's judgement, not dlog's.
    oldest_staged_ms: i64,
}

pub fn run(args: StatusArgs) -> Result<(), AppError> {
    let workspace = Workspace::discover(args.db)?;
    let store = workspace.open()?;
    emit(&StatusResult {
        root: workspace.root.display().to_string(),
        db: workspace.db.display().to_string(),
        root_source: workspace.root_source,
        store: store.status()?,
        stranded_tasks: stranded(&store)?,
    });
    Ok(())
}

fn stranded(store: &Store) -> Result<Vec<StrandedOut>, AppError> {
    Ok(store
        .stranded_tasks(STRANDED_LIMIT)?
        .into_iter()
        .map(|t| StrandedOut {
            task: t.task,
            instruction_summary: t.instruction.map(|i| summarize(&i, SUMMARY_MAX)),
            staged_count: t.staged_count,
            oldest_staged_ms: t.oldest_staged_ms,
        })
        .collect())
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

    fn stage(store: &Store, task_id: Option<&str>, rationale: &str) -> String {
        store
            .stage_decision(&NewDecision {
                task_id: task_id.map(str::to_string),
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

    #[test]
    fn stranded_names_unfinished_tasks_that_still_hold_staging() {
        let store = Store::open_in_memory().unwrap();
        assert!(stranded(&store).unwrap().is_empty());

        let left_behind = store
            .insert_task(None, Some("investigate the flake"))
            .unwrap();
        let finished = store.insert_task(None, None).unwrap();
        stage(&store, Some(&left_behind), "one");
        stage(&store, Some(&left_behind), "two");
        let theirs = stage(&store, Some(&finished), "sealed at task end");
        store
            .seal_staged(&crate::model::Binding::None, Some(&[theirs]))
            .unwrap();
        store.complete_task(&finished).unwrap();
        // Task-less staging shows up in staging_count but under no task.
        stage(&store, None, "recorded without a task");

        let rows = stranded(&store).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task, left_behind);
        assert_eq!(
            rows[0].instruction_summary.as_deref(),
            Some("investigate the flake")
        );
        assert_eq!(rows[0].staged_count, 2);
        assert!(rows[0].oldest_staged_ms > 0);

        let status = store.status().unwrap();
        assert_eq!(status.stranded_task_count, 1);
        assert_eq!(status.staging_count, 3, "the task-less one still counts");
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
