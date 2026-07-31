//! `dlog import` — replay an export file into this store (#64).
//!
//! The whole algorithm is "insert the ids we don't have". That works because the
//! append-only guarantee of §7.2 is enforced by BEFORE UPDATE/DELETE triggers —
//! `INSERT` was never blocked — and because ULIDs are globally unique, so
//! "same id" means "same decision" and there is nothing to merge. No CRDT, no
//! sync engine (§6, CLAUDE.md).
//!
//! Two things are checked before anything is written:
//!
//! * **Sealed only.** A record claiming to be staged, or carrying no binding, is
//!   refused. Otherwise a hand-written file could inject rows into someone
//!   else's staging area and decide what their next `dlog commit` seals — the
//!   cross-machine version of the hazard #59 documented.
//! * **No dangling references.** Every `supersedes` / `task_id` /
//!   `parent_task_id` / `declared_by` must resolve inside the file or in this
//!   store, so a foreign key never fires mid-transaction.
//!
//! `caused_by` is exempt: it has no foreign key, and a missing cause just ends a
//! `trace` walk early. Those are reported back as state (§9.1 principle 2)
//! rather than treated as an error.

use std::collections::BTreeSet;
use std::io::Read;

use serde::Serialize;

use crate::cli::ImportArgs;
use crate::commands::share::{FORMAT_VERSION, Header, Record};
use crate::commands::{AppError, Workspace};
use crate::model::StoredDecision;
use crate::output::emit;
use crate::store::{ImportCounts, InvariantRecord, Store, TaskRecord};

/// Success document for `dlog import`.
#[derive(Debug, Serialize)]
struct ImportResult {
    imported: ImportCounts,
    /// Records whose id this store already had.
    skipped_existing: usize,
    /// `caused_by` edges pointing at decisions neither the file nor this store
    /// has — the received DAG is a fragment, which is a fact worth reporting.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dangling_caused_by: Vec<String>,
}

/// The parsed contents of an export file.
#[derive(Debug, Default)]
struct Parsed {
    header: Option<Header>,
    tasks: Vec<TaskRecord>,
    decisions: Vec<StoredDecision>,
    invariants: Vec<InvariantRecord>,
}

pub fn run(args: ImportArgs) -> Result<(), AppError> {
    let text = read_input(&args.path)?;
    let parsed = parse(&text)?;

    let workspace = Workspace::discover(args.db)?;
    let store = workspace.open()?;

    let dangling_caused_by = validate(&store, &parsed)?;

    let (imported, skipped_existing) =
        store.import_all(&parsed.tasks, &parsed.decisions, &parsed.invariants)?;

    emit(&ImportResult {
        imported,
        skipped_existing,
        dangling_caused_by,
    });
    Ok(())
}

/// Read the file, or stdin for `-`. stdin carries no output contract, so unlike
/// `export`'s destination it can stay a stream.
fn read_input(path: &str) -> Result<String, AppError> {
    if path == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        return Ok(buf);
    }
    std::fs::read_to_string(path).map_err(|e| {
        AppError::new(
            "io_error",
            format!("could not read export file {path:?}: {e}"),
        )
    })
}

/// Parse JSONL into its three record lists, checking the format version and the
/// sealed-only rule as it goes. Blank lines are skipped so concatenating two
/// exports stays valid.
fn parse(text: &str) -> Result<Parsed, AppError> {
    let mut parsed = Parsed::default();

    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(line).map_err(|e| {
            AppError::new(
                "invalid_export",
                format!("line {line_no} is not a dlog export record: {e}"),
            )
        })?;

        match record {
            Record::Header(header) => {
                if header.format != FORMAT_VERSION {
                    return Err(AppError::new(
                        "unsupported_format",
                        format!(
                            "export format {} is not supported by this dlog (expected {}); \
                             upgrade dlog to read it",
                            header.format, FORMAT_VERSION
                        ),
                    ));
                }
                parsed.header = Some(header);
            }
            Record::Task(task) => parsed.tasks.push(task),
            Record::Decision(decision) => {
                check_sealed(&decision, line_no)?;
                parsed.decisions.push(*decision);
            }
            Record::Invariant(invariant) => parsed.invariants.push(invariant),
        }
    }

    // A file with no header is not an export — most likely a truncated or
    // hand-assembled one, and guessing its format is exactly what the version
    // exists to prevent.
    if parsed.header.is_none() {
        return Err(AppError::new(
            "invalid_export",
            "export file has no header record; expected a line with \
             {\"type\":\"header\",...} first",
        ));
    }

    // Concatenated exports overlap; the store dedupes by id anyway, but keeping
    // the in-memory lists unique means the counts add up.
    dedupe(&mut parsed);
    Ok(parsed)
}

/// Reject a record that is not sealed. A decision in a file is by definition one
/// that was already finalized somewhere else (§8.2).
fn check_sealed(decision: &StoredDecision, line_no: usize) -> Result<(), AppError> {
    if decision.staged {
        return Err(AppError::new(
            "invalid_export",
            format!(
                "line {line_no}: decision {} is staged; only sealed decisions can be \
                 shared (design §8.2)",
                decision.id
            ),
        ));
    }
    if decision.binding.is_none() {
        return Err(AppError::new(
            "invalid_export",
            format!(
                "line {line_no}: decision {} has no binding; a main-log decision \
                 always carries one (design §8.2)",
                decision.id
            ),
        ));
    }
    Ok(())
}

/// Drop repeated ids, keeping the first occurrence, and sort each list ascending
/// so the write order satisfies the foreign keys.
fn dedupe(parsed: &mut Parsed) {
    let mut seen = BTreeSet::new();
    parsed.tasks.retain(|t| seen.insert(t.id.clone()));
    seen.clear();
    parsed.decisions.retain(|d| seen.insert(d.id.clone()));
    seen.clear();
    parsed.invariants.retain(|i| seen.insert(i.id.clone()));

    parsed.tasks.sort_by(|a, b| a.id.cmp(&b.id));
    parsed.decisions.sort_by(|a, b| a.id.cmp(&b.id));
    parsed.invariants.sort_by(|a, b| a.id.cmp(&b.id));
}

/// Check every foreign-key-bearing reference against the file plus the store,
/// and return the `caused_by` edges that resolve to neither.
///
/// Doing this before the transaction is safe: sealed rows are append-only, so a
/// reference that resolves now cannot stop resolving (§7.2).
fn validate(store: &Store, parsed: &Parsed) -> Result<Vec<String>, AppError> {
    let file_tasks: BTreeSet<&str> = parsed.tasks.iter().map(|t| t.id.as_str()).collect();
    let file_decisions: BTreeSet<&str> = parsed.decisions.iter().map(|d| d.id.as_str()).collect();

    let mut missing: Vec<String> = Vec::new();
    let task_known = |id: &str| -> Result<bool, AppError> {
        Ok(file_tasks.contains(id) || store.task_exists(id)?)
    };

    for t in &parsed.tasks {
        if let Some(parent) = &t.parent_task_id
            && !task_known(parent)?
        {
            missing.push(format!("task {} -> parent_task_id {parent}", t.id));
        }
    }
    for d in &parsed.decisions {
        if let Some(task_id) = &d.task_id
            && !task_known(task_id)?
        {
            missing.push(format!("decision {} -> task_id {task_id}", d.id));
        }
        if let Some(previous) = &d.supersedes
            && !decision_known(store, &file_decisions, previous)?
        {
            missing.push(format!("decision {} -> supersedes {previous}", d.id));
        }
    }
    for i in &parsed.invariants {
        if !decision_known(store, &file_decisions, &i.declared_by)? {
            missing.push(format!(
                "invariant {} -> declared_by {}",
                i.id, i.declared_by
            ));
        }
    }

    if !missing.is_empty() {
        return Err(AppError::new(
            "dangling_reference",
            format!(
                "export references {} record(s) that are in neither the file nor this \
                 store, so nothing was imported: {}",
                missing.len(),
                missing.join("; ")
            ),
        ));
    }

    // `caused_by` has no foreign key: a fragment of the DAG imports fine, and
    // the gaps are reported rather than repaired.
    let mut dangling: BTreeSet<String> = BTreeSet::new();
    for d in &parsed.decisions {
        for cause in &d.caused_by {
            if !decision_known(store, &file_decisions, cause)? {
                dangling.insert(cause.clone());
            }
        }
    }
    Ok(dangling.into_iter().collect())
}

fn decision_known(store: &Store, in_file: &BTreeSet<&str>, id: &str) -> Result<bool, AppError> {
    Ok(in_file.contains(id) || store.decision_exists(id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::share::{Record, to_line};
    use crate::model::{Agent, Anchor, Binding};

    fn header_line() -> String {
        to_line(&Record::Header(Header {
            format: FORMAT_VERSION,
            schema_version: 3,
            exported_at: 1,
        }))
        .unwrap()
    }

    fn decision(id: &str) -> StoredDecision {
        StoredDecision {
            id: id.into(),
            task_id: None,
            agent: Agent {
                role: "implementer".into(),
                model: "claude-test".into(),
                session_id: None,
                author: Some("lee@example.com".into()),
            },
            conversation_id: None,
            rationale: "retry three times".into(),
            rejected: vec![],
            caused_by: vec![],
            supersedes: None,
            anchors: vec![Anchor {
                file: "src/net/client.rs".into(),
                symbol_path: Some("Client::send".into()),
                node_kind: Some("function_item".into()),
                structural_hash: Some("h_1".into()),
                line_span: Some((40, 58)),
                recorded_at_sha: Some("a3f".into()),
            }],
            staged: false,
            binding: Some(Binding::Commit { sha: "a3f".into() }),
            created_at_ms: 1,
        }
    }

    fn file_of(decisions: Vec<StoredDecision>) -> String {
        let mut lines = vec![header_line()];
        for d in decisions {
            lines.push(to_line(&Record::Decision(Box::new(d))).unwrap());
        }
        lines.join("\n")
    }

    #[test]
    fn imports_then_skips_on_a_second_run() {
        let store = Store::open_in_memory().unwrap();
        let text = file_of(vec![decision("01AAAAAAAAAAAAAAAAAAAAAAAA")]);

        let parsed = parse(&text).unwrap();
        validate(&store, &parsed).unwrap();
        let (counts, skipped) = store
            .import_all(&parsed.tasks, &parsed.decisions, &parsed.invariants)
            .unwrap();
        assert_eq!(counts.decisions, 1);
        assert_eq!(skipped, 0);

        // The row landed in the main log, with its anchor and its author.
        let stored = store
            .get_decision("01AAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap()
            .unwrap();
        assert!(!stored.staged);
        assert_eq!(stored.binding, Some(Binding::Commit { sha: "a3f".into() }));
        assert_eq!(stored.anchors.len(), 1);
        assert_eq!(stored.agent.author.as_deref(), Some("lee@example.com"));
        // The FTS trigger fired on INSERT, so it is searchable immediately.
        assert_eq!(
            store.search("retry").unwrap(),
            vec!["01AAAAAAAAAAAAAAAAAAAAAAAA".to_string()]
        );

        let parsed = parse(&text).unwrap();
        let (counts, skipped) = store
            .import_all(&parsed.tasks, &parsed.decisions, &parsed.invariants)
            .unwrap();
        assert_eq!(counts, ImportCounts::default());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn a_staged_or_unbound_record_is_refused() {
        let mut staged = decision("01AAAAAAAAAAAAAAAAAAAAAAAA");
        staged.staged = true;
        let err = parse(&file_of(vec![staged])).unwrap_err();
        assert_eq!(err.code, "invalid_export");

        let mut unbound = decision("01AAAAAAAAAAAAAAAAAAAAAAAA");
        unbound.binding = None;
        let err = parse(&file_of(vec![unbound])).unwrap_err();
        assert_eq!(err.code, "invalid_export");
    }

    #[test]
    fn a_file_without_a_header_or_with_an_unknown_format_is_refused() {
        let lone = to_line(&Record::Decision(Box::new(decision(
            "01AAAAAAAAAAAAAAAAAAAAAAAA",
        ))))
        .unwrap();
        assert_eq!(parse(&lone).unwrap_err().code, "invalid_export");

        let future = to_line(&Record::Header(Header {
            format: FORMAT_VERSION + 1,
            schema_version: 99,
            exported_at: 1,
        }))
        .unwrap();
        assert_eq!(parse(&future).unwrap_err().code, "unsupported_format");
    }

    #[test]
    fn a_dangling_supersedes_stops_the_whole_import() {
        let store = Store::open_in_memory().unwrap();
        let mut d = decision("01BBBBBBBBBBBBBBBBBBBBBBBB");
        d.supersedes = Some("01MISSINGMISSINGMISSINGMI".into());

        let parsed = parse(&file_of(vec![d])).unwrap();
        let err = validate(&store, &parsed).unwrap_err();
        assert_eq!(err.code, "dangling_reference");
        // Nothing was written: validation runs before the transaction opens.
        assert!(!store.decision_exists("01BBBBBBBBBBBBBBBBBBBBBBBB").unwrap());
    }

    #[test]
    fn a_dangling_caused_by_imports_and_is_reported() {
        let store = Store::open_in_memory().unwrap();
        let mut d = decision("01BBBBBBBBBBBBBBBBBBBBBBBB");
        d.caused_by = vec!["01MISSINGMISSINGMISSINGMI".into()];

        let parsed = parse(&file_of(vec![d])).unwrap();
        let dangling = validate(&store, &parsed).unwrap();
        assert_eq!(dangling, vec!["01MISSINGMISSINGMISSINGMI".to_string()]);

        let (counts, _) = store
            .import_all(&parsed.tasks, &parsed.decisions, &parsed.invariants)
            .unwrap();
        assert_eq!(counts.decisions, 1);
    }

    #[test]
    fn concatenated_exports_parse_as_one_file() {
        // `cat a.jsonl b.jsonl` is the obvious way to combine two exports, so a
        // repeated header and a repeated decision must both be tolerated.
        let a = file_of(vec![decision("01AAAAAAAAAAAAAAAAAAAAAAAA")]);
        let b = file_of(vec![
            decision("01AAAAAAAAAAAAAAAAAAAAAAAA"),
            decision("01CCCCCCCCCCCCCCCCCCCCCCCC"),
        ]);
        let parsed = parse(&format!("{a}\n{b}\n")).unwrap();
        assert_eq!(parsed.decisions.len(), 2);
    }
}
