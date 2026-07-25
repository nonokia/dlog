//! `dlog task` — the task hierarchy (§7.1) and the non-code seal (§8.3).
//!
//! §8.3 defines two seal triggers. The code path is `dlog commit` / `dlog bind
//! <sha>`; the non-code path is task completion, sealing with `binding: none`.
//! `task done` is that second trigger.
//!
//! The reason it is not just `dlog bind --none`: §8.3 wants subagents to seal at
//! the end of their own task so their decisions survive when only a summary
//! returns to the parent. `bind --none` seals *everything* staged, so a subagent
//! doing that would also bind the parent's in-progress decisions to "no commit"
//! — irreversibly, since sealed rows are append-only. `task done` seals exactly
//! the finishing task's share.

use serde::Serialize;

use crate::cli::{TaskArgs, TaskCommand};
use crate::commands::{AppError, Workspace};
use crate::model::Binding;
use crate::output::emit;
use crate::store::Store;

/// Success document for `dlog task start`.
#[derive(Debug, Serialize)]
struct StartResult {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instruction: Option<String>,
}

/// Success document for `dlog task done`. Mirrors `dlog bind`'s shape so an
/// agent can treat the two seal paths alike.
#[derive(Debug, Serialize)]
struct DoneResult {
    task: String,
    count: usize,
    sealed: Vec<String>,
    binding: Binding,
}

pub fn run(args: TaskArgs) -> Result<(), AppError> {
    match args.command {
        TaskCommand::Start {
            parent,
            instruction,
            db,
        } => {
            let store = Workspace::discover(db)?.open()?;
            emit(&start(&store, parent.as_deref(), instruction.as_deref())?);
        }
        TaskCommand::Done { id, db } => {
            let store = Workspace::discover(db)?.open()?;
            emit(&done(&store, &id)?);
        }
    }
    Ok(())
}

fn start(
    store: &Store,
    parent: Option<&str>,
    instruction: Option<&str>,
) -> Result<StartResult, AppError> {
    // Checked up front: the foreign key would report this as a generic
    // `store_error`, and a mistyped parent silently reparenting to nothing is
    // worse than a failure.
    if let Some(parent) = parent {
        require_task(store, parent)?;
    }
    let id = store.insert_task(parent, instruction)?;
    Ok(StartResult {
        id,
        parent_task_id: parent.map(str::to_string),
        instruction: instruction.map(str::to_string),
    })
}

fn done(store: &Store, task_id: &str) -> Result<DoneResult, AppError> {
    require_task(store, task_id)?;
    let ids = store.staged_decision_ids_for_task(task_id)?;
    // Pass the ids explicitly even when empty: `None` means "seal every staged
    // decision in the store", which is precisely what this command must not do.
    let sealed = store.seal_staged(&Binding::None, Some(&ids))?;
    Ok(DoneResult {
        task: task_id.to_string(),
        count: sealed.len(),
        sealed,
        binding: Binding::None,
    })
}

/// Reject an unknown task id. Sealing nothing because of a typo would be
/// indistinguishable from a task that simply had nothing staged.
fn require_task(store: &Store, id: &str) -> Result<(), AppError> {
    if store.task_exists(id)? {
        return Ok(());
    }
    Err(AppError::new(
        "unknown_task",
        format!("no task `{id}` in the store"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

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
                    file: "src/auth.rs".into(),
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
    fn start_returns_an_id_and_records_the_hierarchy() {
        let store = Store::open_in_memory().unwrap();

        let root = start(&store, None, Some("make the client resilient")).unwrap();
        assert!(root.parent_task_id.is_none());
        assert_eq!(
            root.instruction.as_deref(),
            Some("make the client resilient")
        );

        let child = start(&store, Some(&root.id), None).unwrap();
        assert_eq!(child.parent_task_id.as_deref(), Some(root.id.as_str()));
        assert!(child.instruction.is_none());
        assert!(store.task_exists(&child.id).unwrap());
    }

    #[test]
    fn start_rejects_an_unknown_parent() {
        let store = Store::open_in_memory().unwrap();
        let err = start(&store, Some("01NOPE"), None).unwrap_err();
        assert_eq!(err.code, "unknown_task");
    }

    #[test]
    fn done_seals_only_the_finishing_tasks_decisions() {
        // The §8.3 subagent case: finishing must not touch what another agent
        // still has in flight.
        let store = Store::open_in_memory().unwrap();
        let subagent = start(&store, None, None).unwrap().id;
        let parent = start(&store, None, None).unwrap().id;

        let mine = stage(&store, Some(&subagent), "investigated the flake");
        let theirs = stage(&store, Some(&parent), "still deciding");
        let orphan = stage(&store, None, "recorded without a task");

        let result = done(&store, &subagent).unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.sealed, vec![mine.clone()]);
        assert_eq!(result.binding, Binding::None);

        let sealed = store.get_decision(&mine).unwrap().unwrap();
        assert!(!sealed.staged);
        assert_eq!(sealed.binding, Some(Binding::None));

        for untouched in [&theirs, &orphan] {
            let d = store.get_decision(untouched).unwrap().unwrap();
            assert!(d.staged, "{untouched} must still be staged");
            assert!(d.binding.is_none());
        }
    }

    #[test]
    fn done_with_nothing_staged_is_a_success_not_an_error() {
        // A subagent that investigated and recorded nothing still ends its task;
        // failing here would train agents to skip the call.
        let store = Store::open_in_memory().unwrap();
        let task = start(&store, None, None).unwrap().id;

        let result = done(&store, &task).unwrap();
        assert_eq!(result.count, 0);
        assert!(result.sealed.is_empty());

        // And it stays a no-op when called twice.
        stage(&store, Some(&task), "one decision");
        assert_eq!(done(&store, &task).unwrap().count, 1);
        assert_eq!(done(&store, &task).unwrap().count, 0);
    }

    #[test]
    fn done_rejects_an_unknown_task() {
        let store = Store::open_in_memory().unwrap();
        stage(&store, None, "staged, but not under any task");

        let err = done(&store, "01NOPE").unwrap_err();
        assert_eq!(err.code, "unknown_task");
    }
}
