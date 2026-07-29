//! `dlog context <path>` — the decision summary for a file or directory (design
//! §3, §9; v0.2, #30; rollups #63).
//!
//! Task-start context restoration: aggregate the live decisions anchored at a
//! file, or anywhere under a directory, and return them in the compact form so
//! the agent can rebuild "why is this area the way it is" before touching it.
//!
//! A directory answers with a **per-file rollup** — count plus latest decision —
//! rather than one long stream, so a large tree reports *where* its decisions
//! are and `dlog context <file>` is the drill-down (§9.1's two-stage retrieval,
//! one level up). `--flat` asks for the stream; a path naming a single file is
//! flat either way, since there is nothing to group. The invariants in effect at
//! the path ride along, because they are what must be read before touching the
//! code (§7.1) and a second command is one the agent may not run.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::cli::ContextArgs;
use crate::commands::compact::{self, CompactRow};
use crate::commands::{AppError, Workspace};
use crate::output::emit;
use crate::store::Store;

/// Most invariants a `context` response carries. They are short and few in
/// practice; the cap only stops a pathological store from swamping the command
/// an agent runs at every task start (the `stranded_tasks` cap in `status`
/// serves the same purpose).
const INVARIANT_CAP: usize = 20;

/// Describes the interpreted query (§9.3). `mode` names which shape `results`
/// took, so the agent reads the response without having to re-derive it from the
/// flags it passed.
#[derive(Debug, Serialize)]
struct ContextDesc {
    #[serde(rename = "type")]
    kind: &'static str,
    path: String,
    mode: &'static str,
}

/// One file's decisions, rolled up (§9.1: count and recency are facts; ranking
/// them would not be).
#[derive(Debug, Serialize)]
struct FileRollup {
    file: String,
    /// Live decisions anchored to this file under the queried path.
    count: usize,
    /// The newest of them, in the usual compact form.
    latest: CompactRow,
}

/// `results` is either the flat decision stream or the per-file rollup. Untagged
/// so the wire shape stays "a list of rows"; `query.mode` says which.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ContextRows {
    Flat(Vec<CompactRow>),
    Rollup(Vec<FileRollup>),
}

/// An invariant in effect at the queried path (§7.1).
#[derive(Debug, Serialize)]
struct InvariantOut {
    id: String,
    statement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    declared_by: String,
}

#[derive(Debug, Serialize)]
struct ContextEnvelope {
    query: ContextDesc,
    results: ContextRows,
    /// Invariants in effect at the path, omitted entirely with `--no-invariants`.
    #[serde(skip_serializing_if = "Option::is_none")]
    invariants: Option<Vec<InvariantOut>>,
    /// Invariants beyond [`INVARIANT_CAP`] that were not listed.
    #[serde(skip_serializing_if = "is_zero")]
    invariants_elided: usize,
    truncated: bool,
    /// In-scope results omitted by the budget/limit — decisions in flat mode,
    /// files in rollup mode (§9.1 principle 2).
    elided: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

pub fn run(args: ContextArgs) -> Result<(), AppError> {
    let workspace = Workspace::discover(args.db.clone())?;
    let cwd = std::env::current_dir()?;
    let store = workspace.open()?;
    let envelope = build(&store, &workspace, &cwd, &args)?;
    emit(&envelope);
    Ok(())
}

fn build(
    store: &Store,
    workspace: &Workspace,
    cwd: &Path,
    args: &ContextArgs,
) -> rusqlite::Result<ContextEnvelope> {
    // Normalized against the root, so `dlog context .` from a subdirectory means
    // that subdirectory, and from the root means the whole workspace.
    let path = workspace.relativize(cwd, &args.path);
    let superseded = store.superseded_ids()?;

    // (file, decision) pairs, newest-first, filtered to the live scope (§9.1).
    let pairs: Vec<(String, String)> = store
        .anchors_under_path(&path)?
        .into_iter()
        .filter(|(_, id)| args.include_superseded || !superseded.contains(id))
        .collect();

    let rollup = wants_rollup(args, &pairs);
    let (results, truncated, elided) = if rollup {
        let (rows, truncated, elided) = collect_rollup(store, &pairs, &superseded, args)?;
        (ContextRows::Rollup(rows), truncated, elided)
    } else {
        // Distinct decisions, still newest-first: a decision anchored to several
        // files under the path must not be listed once per file.
        let mut ids: Vec<String> = Vec::new();
        for (_, id) in &pairs {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let (rows, truncated, elided) = compact::collect(
            store,
            &ids,
            args.include_superseded,
            args.limit,
            args.budget,
        )?;
        (ContextRows::Flat(rows), truncated, elided)
    };

    let (invariants, invariants_elided) = if args.no_invariants {
        (None, 0)
    } else {
        let (rows, elided) = invariants_for(store, &path)?;
        (Some(rows), elided)
    };

    Ok(ContextEnvelope {
        query: ContextDesc {
            kind: "context",
            path,
            mode: if rollup { "rollup" } else { "flat" },
        },
        results,
        invariants,
        invariants_elided,
        truncated,
        elided,
    })
}

/// Rollup unless `--flat` says otherwise — but a path whose decisions all sit in
/// one file has nothing to group, so it answers flat (`dlog context <file>` keeps
/// behaving as it always did) unless `--rollup` was asked for explicitly.
fn wants_rollup(args: &ContextArgs, pairs: &[(String, String)]) -> bool {
    if args.flat {
        return false;
    }
    if args.rollup {
        return true;
    }
    let mut files = pairs.iter().map(|(f, _)| f);
    let first = files.next();
    files.any(|f| Some(f) != first)
}

/// Group `pairs` by file, newest-first by each file's latest decision, and emit
/// rows until the budget or limit is reached. The budget is spent on the same
/// adaptive-width basis as the flat path (#33) — a row here costs one compact
/// row plus its file path.
fn collect_rollup(
    store: &Store,
    pairs: &[(String, String)],
    superseded: &std::collections::HashSet<String>,
    args: &ContextArgs,
) -> rusqlite::Result<(Vec<FileRollup>, bool, usize)> {
    // `pairs` is newest-first, so the first id seen for a file is its latest and
    // first-seen file order is already newest-first across files.
    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, (String, usize)> = HashMap::new();
    for (file, id) in pairs {
        match by_file.get_mut(file) {
            Some((_, count)) => *count += 1,
            None => {
                order.push(file.clone());
                by_file.insert(file.clone(), (id.clone(), 1));
            }
        }
    }

    let width = compact::adaptive_width(args.budget, order.len());
    let mut rows = Vec::new();
    let mut running = 0usize;
    for file in &order {
        if rows.len() >= args.limit {
            break;
        }
        let (latest_id, count) = &by_file[file];
        let Some(decision) = store.get_decision(latest_id)? else {
            continue;
        };
        let latest = compact::row_from_at(decision, superseded.contains(latest_id), width);
        let cost = compact::row_cost(&latest) + file.chars().count();
        // Always emit at least one row; otherwise stop when the budget is hit.
        if args.budget > 0 && !rows.is_empty() && running + cost > args.budget {
            break;
        }
        running += cost;
        rows.push(FileRollup {
            file: file.clone(),
            count: *count,
            latest,
        });
    }

    let elided = order.len() - rows.len();
    Ok((rows, elided > 0, elided))
}

/// The live invariants in effect at `path`: the same relevance rule as
/// `dlog invariants --scope` (a scopeless invariant is global; otherwise the two
/// paths must contain one another), capped at [`INVARIANT_CAP`].
fn invariants_for(store: &Store, path: &str) -> rusqlite::Result<(Vec<InvariantOut>, usize)> {
    let matched: Vec<_> = store
        .list_live_invariants()?
        .into_iter()
        .filter(|row| crate::commands::invariants::scope_matches(&row.scope, path))
        .collect();
    let total = matched.len();
    let rows: Vec<InvariantOut> = matched
        .into_iter()
        .take(INVARIANT_CAP)
        .map(|row| InvariantOut {
            id: row.id,
            statement: row.statement,
            scope: row.scope,
            declared_by: row.declared_by,
        })
        .collect();
    let elided = total - rows.len();
    Ok((rows, elided))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::commands::RootSource;
    use crate::model::{Agent, Anchor, NewDecision};

    /// A workspace rooted at `/proj`, so path normalization is exercised without
    /// depending on the process cwd.
    fn workspace() -> Workspace {
        Workspace::rooted(PathBuf::from("/proj"), RootSource::Dlog, None)
    }

    fn root() -> &'static Path {
        Path::new("/proj")
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-context-{}.db", ulid::Ulid::new()))
    }

    fn seed(store: &Store, file: &str, rationale: &str) -> String {
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
                supersedes: None,
                anchors: vec![Anchor {
                    file: file.into(),
                    symbol_path: None,
                    node_kind: None,
                    structural_hash: None,
                    line_span: None,
                    recorded_at_sha: None,
                }],
            })
            .unwrap()
    }

    fn context_args(db: &std::path::Path, path: &str) -> ContextArgs {
        ContextArgs {
            path: path.into(),
            rollup: false,
            // Tests that predate the rollup assert on the decision stream.
            flat: true,
            no_invariants: true,
            include_superseded: false,
            limit: 20,
            budget: 0,
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    fn flat_rows(env: &ContextEnvelope) -> &Vec<CompactRow> {
        match &env.results {
            ContextRows::Flat(rows) => rows,
            ContextRows::Rollup(_) => panic!("expected flat results"),
        }
    }

    fn rollup_rows(env: &ContextEnvelope) -> &Vec<FileRollup> {
        match &env.results {
            ContextRows::Rollup(rows) => rows,
            ContextRows::Flat(_) => panic!("expected rollup results"),
        }
    }

    fn ids(env: &ContextEnvelope) -> Vec<String> {
        flat_rows(env).iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn directory_aggregates_decisions_under_it() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let login = seed(&store, "src/auth/login.rs", "a");
        let token = seed(&store, "src/auth/token.rs", "b");
        seed(&store, "src/net/client.rs", "c");

        let env = build(&store, &workspace(), root(), &context_args(&db, "src/auth")).unwrap();
        let got = ids(&env);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&login) && got.contains(&token));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn exact_file_path_matches_that_file() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let c = seed(&store, "src/net/client.rs", "c");
        seed(&store, "src/auth/login.rs", "a");

        let env = build(
            &store,
            &workspace(),
            root(),
            &context_args(&db, "src/net/client.rs"),
        )
        .unwrap();
        assert_eq!(ids(&env), vec![c]);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn respects_path_component_boundary() {
        // `src/au` must not match `src/auth/...` (the `/` boundary).
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "src/auth/login.rs", "a");

        let env = build(&store, &workspace(), root(), &context_args(&db, "src/au")).unwrap();
        assert!(flat_rows(&env).is_empty());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn dot_at_the_root_covers_the_whole_workspace() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "src/auth/login.rs", "a");
        seed(&store, "README.md", "b");

        let env = build(&store, &workspace(), root(), &context_args(&db, ".")).unwrap();
        assert_eq!(flat_rows(&env).len(), 2);
        assert_eq!(env.query.path, ".");

        // From a subdirectory, `.` means that subdirectory.
        let env = build(
            &store,
            &workspace(),
            Path::new("/proj/src"),
            &context_args(&db, "."),
        )
        .unwrap();
        assert_eq!(flat_rows(&env).len(), 1);
        assert_eq!(env.query.path, "src");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn like_metacharacters_in_path_match_literally() {
        // An underscore in the query must not act as a LIKE wildcard.
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "src/aXb/f.rs", "wild");
        let literal = seed(&store, "src/a_b/f.rs", "literal");

        let env = build(&store, &workspace(), root(), &context_args(&db, "src/a_b")).unwrap();
        assert_eq!(
            ids(&env),
            vec![literal],
            "_ must be literal, not a wildcard"
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn a_directory_rolls_up_per_file_by_default() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "src/auth/login.rs", "first login");
        seed(&store, "src/auth/token.rs", "token");
        let newest_login = seed(&store, "src/auth/login.rs", "second login");

        let mut args = context_args(&db, "src/auth");
        args.flat = false;
        let env = build(&store, &workspace(), root(), &args).unwrap();

        assert_eq!(env.query.mode, "rollup");
        let rows = rollup_rows(&env);
        assert_eq!(rows.len(), 2, "one row per file");
        // Newest-first across files: login.rs holds the newest decision.
        assert_eq!(rows[0].file, "src/auth/login.rs");
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].latest.id, newest_login);
        assert_eq!(rows[1].file, "src/auth/token.rs");
        assert_eq!(rows[1].count, 1);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn a_single_file_answers_flat_and_flat_forces_the_stream() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        seed(&store, "src/auth/login.rs", "a");
        seed(&store, "src/auth/login.rs", "b");

        // Everything under the path lives in one file -> nothing to group.
        let mut args = context_args(&db, "src/auth");
        args.flat = false;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        assert_eq!(env.query.mode, "flat");
        assert_eq!(flat_rows(&env).len(), 2);

        // ...but `--rollup` still groups it explicitly.
        args.rollup = true;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        assert_eq!(env.query.mode, "rollup");
        assert_eq!(rollup_rows(&env)[0].count, 2);

        // And `--flat` on a multi-file directory keeps the old stream.
        seed(&store, "src/auth/token.rs", "c");
        let env = build(&store, &workspace(), root(), &context_args(&db, "src/auth")).unwrap();
        assert_eq!(env.query.mode, "flat");
        assert_eq!(flat_rows(&env).len(), 3);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn a_decision_spanning_two_files_counts_once_per_file_but_lists_once_flat() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let both = store
            .stage_decision(&NewDecision {
                task_id: None,
                agent: Agent {
                    role: "implementer".into(),
                    model: "claude-test".into(),
                    session_id: None,
                },
                conversation_id: None,
                rationale: "spans two files".into(),
                rejected: vec![],
                caused_by: vec![],
                supersedes: None,
                anchors: vec![
                    Anchor {
                        file: "src/auth/login.rs".into(),
                        symbol_path: None,
                        node_kind: None,
                        structural_hash: None,
                        line_span: None,
                        recorded_at_sha: None,
                    },
                    Anchor {
                        file: "src/auth/token.rs".into(),
                        symbol_path: None,
                        node_kind: None,
                        structural_hash: None,
                        line_span: None,
                        recorded_at_sha: None,
                    },
                ],
            })
            .unwrap();

        // Flat: one row, not one per anchor.
        let env = build(&store, &workspace(), root(), &context_args(&db, "src/auth")).unwrap();
        assert_eq!(ids(&env), vec![both]);

        // Rollup: it is a decision about each file, so it appears under both.
        let mut args = context_args(&db, "src/auth");
        args.flat = false;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        assert_eq!(rollup_rows(&env).len(), 2);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn rollup_respects_budget_and_reports_elided_files() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        for i in 0..10 {
            seed(&store, &format!("src/auth/f{i}.rs"), &"detail ".repeat(40));
        }

        let mut args = context_args(&db, "src/auth");
        args.flat = false;
        args.budget = 300;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        let rows = rollup_rows(&env);
        assert!(!rows.is_empty() && rows.len() < 10);
        assert!(env.truncated);
        assert_eq!(env.elided, 10 - rows.len());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn superseded_decisions_are_out_of_the_rollup_count() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let old = seed(&store, "src/auth/login.rs", "old");
        store
            .stage_decision(&NewDecision {
                task_id: None,
                agent: Agent {
                    role: "implementer".into(),
                    model: "claude-test".into(),
                    session_id: None,
                },
                conversation_id: None,
                rationale: "replacement".into(),
                rejected: vec![],
                caused_by: vec![],
                supersedes: Some(old.clone()),
                anchors: vec![Anchor {
                    file: "src/auth/login.rs".into(),
                    symbol_path: None,
                    node_kind: None,
                    structural_hash: None,
                    line_span: None,
                    recorded_at_sha: None,
                }],
            })
            .unwrap();
        seed(&store, "src/auth/token.rs", "other");

        let mut args = context_args(&db, "src/auth");
        args.flat = false;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        let login = rollup_rows(&env)
            .iter()
            .find(|r| r.file == "src/auth/login.rs")
            .unwrap();
        assert_eq!(login.count, 1, "the superseded decision is out of scope");

        args.include_superseded = true;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        let login = rollup_rows(&env)
            .iter()
            .find(|r| r.file == "src/auth/login.rs")
            .unwrap();
        assert_eq!(login.count, 2);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn invariants_in_effect_ride_along_unless_suppressed() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let dec = seed(&store, "src/auth/login.rs", "a");
        store.insert_invariant(&dec, "global rule", None).unwrap();
        store
            .insert_invariant(&dec, "auth rule", Some("src/auth"))
            .unwrap();
        store
            .insert_invariant(&dec, "net rule", Some("src/net"))
            .unwrap();

        let mut args = context_args(&db, "src/auth");
        args.no_invariants = false;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        let statements: Vec<String> = env
            .invariants
            .as_ref()
            .unwrap()
            .iter()
            .map(|i| i.statement.clone())
            .collect();
        assert!(statements.contains(&"global rule".to_string()));
        assert!(statements.contains(&"auth rule".to_string()));
        assert!(
            !statements.contains(&"net rule".to_string()),
            "a sibling path's invariant is not in effect here"
        );
        assert_eq!(env.invariants_elided, 0);

        // `--no-invariants` drops the field entirely.
        let env = build(&store, &workspace(), root(), &context_args(&db, "src/auth")).unwrap();
        assert!(env.invariants.is_none());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn invariants_are_capped_and_report_what_was_dropped() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let dec = seed(&store, "src/auth/login.rs", "a");
        for i in 0..(INVARIANT_CAP + 3) {
            store
                .insert_invariant(&dec, &format!("rule {i}"), Some("src/auth"))
                .unwrap();
        }

        let mut args = context_args(&db, "src/auth");
        args.no_invariants = false;
        let env = build(&store, &workspace(), root(), &args).unwrap();
        assert_eq!(env.invariants.as_ref().unwrap().len(), INVARIANT_CAP);
        assert_eq!(env.invariants_elided, 3);
        let _ = std::fs::remove_file(&db);
    }
}
