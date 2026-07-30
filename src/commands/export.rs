//! `dlog export` — write sealed decisions out as JSONL (#64).
//!
//! Sharing a decision log needs no merge algorithm: sealed rows are immutable
//! (§7.2) and ULIDs are globally unique, so combining two stores is inserting
//! the rows one of them has not seen. What it *does* need is a settled
//! serialization and a referentially whole file, which is this command.
//!
//! Staged decisions are never written. There is no flag for it: staging is one
//! agent's live work area (§8.2), and #59 is the standing example of what
//! finalizing another agent's pending work costs.
//!
//! The JSONL goes to `--out`, not stdout, because §9.3 gives every invocation
//! exactly one JSON document on stdout — here, the summary of what was written.

use std::collections::BTreeSet;
use std::io::Write;

use serde::Serialize;

use crate::cli::ExportArgs;
use crate::commands::share::{FORMAT_VERSION, Header, Record, to_line};
use crate::commands::{AppError, Workspace, share};
use crate::model::StoredDecision;
use crate::output::emit;
use crate::store::{InvariantRecord, SinceBound, Store, TaskRecord};

/// Success document for `dlog export`.
#[derive(Debug, Serialize)]
struct ExportResult {
    path: String,
    format: u32,
    exported: Counts,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<String>,
}

#[derive(Debug, Serialize)]
struct Counts {
    tasks: usize,
    decisions: usize,
    invariants: usize,
}

pub fn run(args: ExportArgs) -> Result<(), AppError> {
    let since = args.since.as_deref().map(share::parse_since).transpose()?;

    let workspace = Workspace::discover(args.db)?;
    let store = workspace.open()?;

    let (tasks, decisions, invariants) = collect(&store, since.as_ref())?;

    let mut out = std::fs::File::create(&args.out)?;
    let header = Record::Header(Header {
        format: FORMAT_VERSION,
        schema_version: store.schema_version()?,
        exported_at: ulid::Ulid::new().timestamp_ms() as i64,
    });
    writeln!(out, "{}", to_line(&header)?)?;
    for t in &tasks {
        writeln!(out, "{}", to_line(&Record::Task(t.clone()))?)?;
    }
    for d in &decisions {
        writeln!(out, "{}", to_line(&Record::Decision(Box::new(d.clone())))?)?;
    }
    for i in &invariants {
        writeln!(out, "{}", to_line(&Record::Invariant(i.clone()))?)?;
    }
    out.flush()?;

    emit(&ExportResult {
        path: args.out.to_string_lossy().into_owned(),
        format: FORMAT_VERSION,
        exported: Counts {
            tasks: tasks.len(),
            decisions: decisions.len(),
            invariants: invariants.len(),
        },
        since: args.since,
    });
    Ok(())
}

/// Gather what the file will contain, each list ascending by id.
///
/// `since` selects a starting point, but a selection is not yet a file: three
/// foreign keys (`decision.supersedes`, `task.parent_task_id`,
/// `invariant.declared_by`) mean an incremental export that names only the new
/// rows cannot be imported anywhere lacking the old ones. So the selection is
/// closed over those references here, where the data is, rather than left to
/// fail at import time.
///
/// `caused_by` is deliberately *not* chased: it carries no foreign key, one edge
/// into old history would drag in most of the log, and a missing cause only ends
/// a `trace` walk early (§9.1 — the receiving side reports it as state).
#[allow(clippy::type_complexity)]
fn collect(
    store: &Store,
    since: Option<&SinceBound>,
) -> Result<(Vec<TaskRecord>, Vec<StoredDecision>, Vec<InvariantRecord>), AppError> {
    let selected = store.sealed_decision_ids(since)?;

    // Decisions, plus every `supersedes` ancestor of one. A superseded decision
    // may predate the `--since` cut and still be the thing a selected decision
    // reverses (§7.2).
    let mut decisions: Vec<StoredDecision> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = selected;
    while let Some(id) = queue.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(decision) = store.get_decision(&id)? else {
            continue;
        };
        // A staged row can only get here via a supersedes edge; the export is
        // sealed-only whatever the graph says.
        if decision.staged {
            seen.remove(&id);
            continue;
        }
        if let Some(previous) = decision.supersedes.clone() {
            queue.push(previous);
        }
        decisions.push(decision);
    }
    decisions.sort_by(|a, b| a.id.cmp(&b.id));

    // Tasks the decisions belong to, plus their ancestors (§7.1 hierarchy).
    let mut tasks: Vec<TaskRecord> = Vec::new();
    let mut task_seen: BTreeSet<String> = BTreeSet::new();
    let mut task_queue: Vec<String> = decisions.iter().filter_map(|d| d.task_id.clone()).collect();
    while let Some(id) = task_queue.pop() {
        if !task_seen.insert(id.clone()) {
            continue;
        }
        let Some(task) = store.get_task(&id)? else {
            continue;
        };
        if let Some(parent) = task.parent_task_id.clone() {
            task_queue.push(parent);
        }
        tasks.push(task);
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));

    // Invariants declared by an exported decision — including retired ones, so
    // the file is a faithful copy rather than a filtered view.
    let mut invariants: Vec<InvariantRecord> = Vec::new();
    for d in &decisions {
        invariants.extend(store.invariant_records_declared_by(&d.id)?);
    }
    invariants.sort_by(|a, b| a.id.cmp(&b.id));

    Ok((tasks, decisions, invariants))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Anchor, Binding, NewDecision};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn new_decision(rationale: &str) -> NewDecision {
        NewDecision {
            task_id: None,
            agent: Agent {
                role: "implementer".into(),
                model: "claude-test".into(),
                session_id: None,
                author: None,
            },
            conversation_id: None,
            rationale: rationale.into(),
            rejected: vec![],
            caused_by: vec![],
            supersedes: None,
            anchors: vec![Anchor {
                file: "src/net/client.rs".into(),
                symbol_path: None,
                node_kind: None,
                structural_hash: None,
                line_span: None,
                recorded_at_sha: None,
            }],
        }
    }

    #[test]
    fn staged_decisions_are_never_exported() {
        let store = store();
        let sealed = store.stage_decision(&new_decision("sealed")).unwrap();
        store.seal(&sealed, &Binding::None).unwrap();
        let _staged = store
            .stage_decision(&new_decision("still working"))
            .unwrap();

        let (_, decisions, _) = collect(&store, None).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id, sealed);
    }

    #[test]
    fn everything_is_ascending_by_id() {
        let store = store();
        for i in 0..5 {
            let id = store
                .stage_decision(&new_decision(&format!("d{i}")))
                .unwrap();
            store.seal(&id, &Binding::None).unwrap();
        }
        let (_, decisions, _) = collect(&store, None).unwrap();
        let ids: Vec<&str> = decisions.iter().map(|d| d.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn since_still_carries_what_the_cut_needs() {
        let store = store();

        // An old task with a child, an old decision under the child, and a new
        // decision that supersedes the old one — all of it below the cut except
        // the last.
        let parent = store.insert_task(None, Some("parent work")).unwrap();
        let child = store
            .insert_task(Some(&parent), Some("child work"))
            .unwrap();

        let mut old = new_decision("original call");
        old.task_id = Some(child.clone());
        let old_id = store.stage_decision(&old).unwrap();
        store.seal(&old_id, &Binding::None).unwrap();
        store
            .insert_invariant(&old_id, "tokens never persist", None)
            .unwrap();

        let mut new = new_decision("reversal");
        new.supersedes = Some(old_id.clone());
        let new_id = store.stage_decision(&new).unwrap();
        store.seal(&new_id, &Binding::None).unwrap();

        // Cut so that only the reversal is selected.
        let since = SinceBound::Id(new_id.clone());
        let (tasks, decisions, invariants) = collect(&store, Some(&since)).unwrap();

        let ids: Vec<&str> = decisions.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&new_id.as_str()));
        assert!(
            ids.contains(&old_id.as_str()),
            "the superseded decision is pulled in by the FK closure"
        );
        // ...and with it, its task, that task's parent, and its invariant.
        let task_ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(task_ids.contains(&child.as_str()));
        assert!(task_ids.contains(&parent.as_str()));
        assert_eq!(invariants.len(), 1);
        assert_eq!(invariants[0].declared_by, old_id);
    }

    #[test]
    fn a_cut_above_everything_exports_nothing() {
        let store = store();
        let id = store.stage_decision(&new_decision("only one")).unwrap();
        store.seal(&id, &Binding::None).unwrap();

        // The maximum ULID, so the bound is above the decision whatever
        // millisecond it landed in. (`Ulid::new()` would not do: within one
        // millisecond ULIDs differ only in random bits, so a later-minted id can
        // sort below an earlier one.)
        let later = SinceBound::Id("7ZZZZZZZZZZZZZZZZZZZZZZZZZ".into());
        let (tasks, decisions, invariants) = collect(&store, Some(&later)).unwrap();
        assert!(tasks.is_empty() && decisions.is_empty() && invariants.is_empty());
    }
}
