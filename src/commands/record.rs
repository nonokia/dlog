//! `dlog record` — write a decision into staging (design §7, §8.2).
//!
//! Decisions are born before a commit, so recording goes straight to the
//! mutable staging area; binding happens later at seal time (#6). This handler
//! maps CLI args to a [`NewDecision`], opens the store, and stages it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::RecordArgs;
use crate::commands::AppError;
use crate::model::{Agent, Anchor, NewDecision, Rejected};
use crate::output::emit;
use crate::store::Store;

/// Success document for `dlog record`.
#[derive(Debug, Serialize)]
struct RecordResult {
    id: String,
    /// Always true: a freshly recorded decision starts in staging (§8.2).
    staged: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    declared_invariants: Vec<String>,
}

pub fn run(args: RecordArgs) -> Result<(), AppError> {
    // An anchor is part of the minimal required surface (§7.3).
    if args.files.is_empty() {
        return Err(AppError::new(
            "missing_anchor",
            "record requires at least one --file anchor (design §7.3)",
        ));
    }

    let anchors: Vec<Anchor> = args.files.iter().map(|s| parse_anchor(s)).collect();
    let rejected: Vec<Rejected> = args.rejected.iter().map(|s| parse_rejected(s)).collect();

    let decision = NewDecision {
        task_id: args.task_id.clone(),
        agent: Agent {
            role: args.agent_role,
            model: args.agent_model,
            session_id: args.agent_session,
        },
        conversation_id: args.conversation_id,
        rationale: args.rationale,
        rejected,
        caused_by: args.caused_by,
        supersedes: args.supersedes,
        anchors,
    };

    let db = resolve_db(args.db);
    if let Some(parent) = db.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&db)?;

    // A decision may reference a task; make sure the row exists so the FK holds.
    if let Some(task_id) = decision.task_id.as_deref() {
        store.ensure_task(task_id, args.instruction.as_deref())?;
    }

    let id = store.stage_decision(&decision)?;

    let declared_invariants = args
        .declares_invariant
        .iter()
        .map(|statement| store.insert_invariant(&id, statement, args.invariant_scope.as_deref()))
        .collect::<rusqlite::Result<Vec<_>>>()?;

    emit(&RecordResult {
        id,
        staged: true,
        declared_invariants,
    });
    Ok(())
}

/// Resolve the store path: explicit `--db`/`$DLOG_DB`, else `.dlog/dlog.db`.
fn resolve_db(arg: Option<String>) -> PathBuf {
    arg.map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".dlog").join("dlog.db"))
}

/// Parse an anchor spec: `FILE`, `FILE:LINE`, or `FILE:START-END`. The trailing
/// `:...` is only treated as a line span when it parses as one; otherwise the
/// whole string is the path. Symbol/structural fields are left empty — that
/// enrichment is the tree-sitter anchor work (#7).
fn parse_anchor(spec: &str) -> Anchor {
    let (file, line_span) = match spec.rsplit_once(':') {
        Some((path, lines)) => match parse_line_spec(lines) {
            Some(span) => (path.to_string(), Some(span)),
            None => (spec.to_string(), None),
        },
        None => (spec.to_string(), None),
    };
    Anchor {
        file,
        symbol_path: None,
        node_kind: None,
        structural_hash: None,
        line_span,
        recorded_at_sha: None,
    }
}

fn parse_line_spec(s: &str) -> Option<(u32, u32)> {
    match s.split_once('-') {
        Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
        None => {
            let n = s.trim().parse().ok()?;
            Some((n, n))
        }
    }
}

/// Parse a rejected alternative `"approach :: reason"`. Without `::`, the whole
/// string is the approach and the reason is empty.
fn parse_rejected(spec: &str) -> Rejected {
    match spec.split_once("::") {
        Some((approach, reason)) => Rejected {
            approach: approach.trim().to_string(),
            reason: reason.trim().to_string(),
        },
        None => Rejected {
            approach: spec.trim().to_string(),
            reason: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_anchor_plain_file() {
        let a = parse_anchor("src/lib.rs");
        assert_eq!(a.file, "src/lib.rs");
        assert!(a.line_span.is_none());
        assert!(a.symbol_path.is_none());
    }

    #[test]
    fn parse_anchor_single_line_and_range() {
        assert_eq!(parse_anchor("src/lib.rs:12").line_span, Some((12, 12)));
        assert_eq!(parse_anchor("src/lib.rs:10-45").line_span, Some((10, 45)));
    }

    #[test]
    fn parse_anchor_non_line_suffix_is_part_of_path() {
        // A trailing colon that isn't a line spec stays in the path.
        let a = parse_anchor("weird:name");
        assert_eq!(a.file, "weird:name");
        assert!(a.line_span.is_none());
    }

    #[test]
    fn parse_rejected_with_and_without_reason() {
        let r = parse_rejected("polling :: wasteful");
        assert_eq!(r.approach, "polling");
        assert_eq!(r.reason, "wasteful");

        let r = parse_rejected("just the approach");
        assert_eq!(r.approach, "just the approach");
        assert_eq!(r.reason, "");
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-record-{}.db", ulid::Ulid::new()))
    }

    fn args_with_db(db: &Path) -> RecordArgs {
        RecordArgs {
            rationale: "switch to exponential backoff for retries".into(),
            files: vec!["src/auth.rs:10-45".into()],
            agent_role: "implementer".into(),
            agent_model: "claude-test".into(),
            agent_session: None,
            conversation_id: None,
            task_id: None,
            instruction: None,
            rejected: vec!["polling :: wasteful".into()],
            caused_by: vec![],
            supersedes: None,
            declares_invariant: vec!["tokens never logged".into()],
            invariant_scope: Some("src/auth".into()),
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn record_stages_decision_with_anchor_rejected_and_invariant() {
        let db = temp_db();
        run(args_with_db(&db)).expect("record should succeed");

        let store = Store::open(&db).unwrap();
        let hits = store.search("backoff").unwrap();
        assert_eq!(hits.len(), 1, "decision should be searchable");

        let d = store.get_decision(&hits[0]).unwrap().unwrap();
        assert!(d.staged, "freshly recorded decision is staged");
        assert!(d.binding.is_none());
        assert_eq!(d.anchors[0].file, "src/auth.rs");
        assert_eq!(d.anchors[0].line_span, Some((10, 45)));
        assert_eq!(d.rejected[0].approach, "polling");

        let invariants = store.live_invariants().unwrap();
        assert_eq!(invariants.len(), 1);
        assert_eq!(invariants[0].1, "tokens never logged");

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn record_links_task_and_records_instruction() {
        let db = temp_db();
        let mut args = args_with_db(&db);
        args.task_id = Some("tsk_demo".into());
        args.instruction = Some("make the client resilient".into());
        run(args).expect("record with task should succeed");

        let store = Store::open(&db).unwrap();
        let id = &store.search("backoff").unwrap()[0];
        let d = store.get_decision(id).unwrap().unwrap();
        assert_eq!(d.task_id.as_deref(), Some("tsk_demo"));

        let _ = std::fs::remove_file(&db);
    }
}
