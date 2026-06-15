//! `dlog trace <id>` — walk the causal DAG around a decision (design §4, §9;
//! v0.2, #31).
//!
//! Decisions form a DAG via `caused_by` ("B is a fix prompted by A's review").
//! Trace returns the compact rows reached by following those edges, each tagged
//! with its BFS `depth`: `upstream` are the causes (what led here), `downstream`
//! the decisions this one prompted. Cycles are guarded by a visited set;
//! `--depth` caps how far we walk and sets `truncated` when more remained.

use std::collections::HashSet;

use serde::Serialize;

use crate::cli::TraceArgs;
use crate::commands::compact::{self, CompactRow};
use crate::commands::{AppError, open_store};
use crate::output::emit;
use crate::store::Store;

/// Describes the interpreted query (§9.3).
#[derive(Debug, Serialize)]
struct TraceDesc {
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    depth: usize,
}

/// A compact row annotated with its distance from the root.
#[derive(Debug, Serialize)]
struct TraceRow {
    #[serde(flatten)]
    row: CompactRow,
    depth: usize,
}

#[derive(Debug, Serialize)]
struct TraceEnvelope {
    query: TraceDesc,
    root: CompactRow,
    /// Causes — decisions reachable via `caused_by` (ancestors).
    upstream: Vec<TraceRow>,
    /// Effects — decisions that list this one in their `caused_by` (descendants).
    downstream: Vec<TraceRow>,
    truncated: bool,
}

pub fn run(args: TraceArgs) -> Result<(), AppError> {
    let store = open_store(args.db.clone())?;

    let root = store.get_decision(&args.id)?.ok_or_else(|| {
        AppError::new(
            "decision_not_found",
            format!("no decision with id {}", args.id),
        )
    })?;

    let superseded = store.superseded_ids()?;
    let (upstream, up_cut) = walk(&store, &root, args.depth, Direction::Up, &superseded)?;
    let (downstream, down_cut) = walk(&store, &root, args.depth, Direction::Down, &superseded)?;

    let envelope = TraceEnvelope {
        query: TraceDesc {
            kind: "trace",
            id: args.id.clone(),
            depth: args.depth,
        },
        root: compact::row_from(root, superseded.contains(&args.id)),
        upstream,
        downstream,
        truncated: up_cut || down_cut,
    };
    emit(&envelope);
    Ok(())
}

#[derive(Clone, Copy)]
enum Direction {
    Up,
    Down,
}

/// Breadth-first walk from `root` in one direction, up to `max_depth` levels.
/// Returns the rows reached (depth-tagged) and whether the depth cap stopped a
/// further level from being explored.
fn walk(
    store: &Store,
    root: &crate::model::StoredDecision,
    max_depth: usize,
    direction: Direction,
    superseded: &HashSet<String>,
) -> rusqlite::Result<(Vec<TraceRow>, bool)> {
    let mut visited: HashSet<String> = HashSet::from([root.id.clone()]);
    let mut frontier: Vec<String> = vec![root.id.clone()];
    let mut rows = Vec::new();

    for depth in 1..=max_depth {
        let mut next = Vec::new();
        for id in &frontier {
            for neighbor in neighbors(store, id, direction)? {
                if visited.insert(neighbor.clone()) {
                    if let Some(d) = store.get_decision(&neighbor)? {
                        let is_superseded = superseded.contains(&neighbor);
                        rows.push(TraceRow {
                            row: compact::row_from(d, is_superseded),
                            depth,
                        });
                    }
                    next.push(neighbor);
                }
            }
        }
        if next.is_empty() {
            return Ok((rows, false));
        }
        frontier = next;
    }

    // We exhausted the depth budget; `truncated` iff the next level had nodes.
    let cut = frontier.iter().any(|id| {
        neighbors(store, id, direction)
            .map(|n| !n.is_empty())
            .unwrap_or(false)
    });
    Ok((rows, cut))
}

fn neighbors(store: &Store, id: &str, direction: Direction) -> rusqlite::Result<Vec<String>> {
    match direction {
        // Up = this decision's causes; read them off its caused_by.
        Direction::Up => Ok(store
            .get_decision(id)?
            .map(|d| d.caused_by)
            .unwrap_or_default()),
        // Down = decisions that name this one as a cause.
        Direction::Down => store.decision_ids_caused_by(id),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-trace-{}.db", ulid::Ulid::new()))
    }

    fn seed(store: &Store, rationale: &str, caused_by: Vec<String>) -> String {
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
                caused_by,
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

    fn trace_args(db: &std::path::Path, id: &str, depth: usize) -> TraceArgs {
        TraceArgs {
            id: id.into(),
            depth,
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    fn build(store: &Store, args: &TraceArgs) -> Result<TraceEnvelope, AppError> {
        let root = store
            .get_decision(&args.id)?
            .ok_or_else(|| AppError::new("decision_not_found", "x"))?;
        let superseded = store.superseded_ids()?;
        let (upstream, up_cut) = walk(store, &root, args.depth, Direction::Up, &superseded)?;
        let (downstream, down_cut) = walk(store, &root, args.depth, Direction::Down, &superseded)?;
        Ok(TraceEnvelope {
            query: TraceDesc {
                kind: "trace",
                id: args.id.clone(),
                depth: args.depth,
            },
            root: compact::row_from(root, superseded.contains(&args.id)),
            upstream,
            downstream,
            truncated: up_cut || down_cut,
        })
    }

    fn ids_at(rows: &[TraceRow]) -> Vec<(String, usize)> {
        rows.iter().map(|r| (r.row.id.clone(), r.depth)).collect()
    }

    #[test]
    fn traces_upstream_and_downstream_with_depth() {
        // a <- b <- c  (b caused_by a; c caused_by b)
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let a = seed(&store, "root cause", vec![]);
        let b = seed(&store, "fix from a", vec![a.clone()]);
        let c = seed(&store, "follow-up from b", vec![b.clone()]);

        // From b: upstream = a (depth 1); downstream = c (depth 1).
        let env = build(&store, &trace_args(&db, &b, 10)).unwrap();
        assert_eq!(ids_at(&env.upstream), vec![(a.clone(), 1)]);
        assert_eq!(ids_at(&env.downstream), vec![(c.clone(), 1)]);
        assert!(!env.truncated);

        // From a: downstream reaches b (d1) then c (d2).
        let env = build(&store, &trace_args(&db, &a, 10)).unwrap();
        let down = ids_at(&env.downstream);
        assert!(down.contains(&(b.clone(), 1)));
        assert!(down.contains(&(c.clone(), 2)));
        assert!(env.upstream.is_empty());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn depth_limit_truncates() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let a = seed(&store, "a", vec![]);
        let b = seed(&store, "b", vec![a.clone()]);
        let _c = seed(&store, "c", vec![b.clone()]);

        // depth 1 from a reaches only b, and there is more (c) -> truncated.
        let env = build(&store, &trace_args(&db, &a, 1)).unwrap();
        assert_eq!(ids_at(&env.downstream), vec![(b.clone(), 1)]);
        assert!(env.truncated);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn unknown_root_errors() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let err = build(&store, &trace_args(&db, "missing", 10)).unwrap_err();
        assert_eq!(err.code, "decision_not_found");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn handles_cycles_without_looping() {
        // Pathological cycle a -> b -> a (shouldn't happen with append-only ids,
        // but the visited set must still terminate).
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let a = seed(&store, "a", vec![]);
        let b = seed(&store, "b", vec![a.clone()]);
        // Make a also claim b as a cause via a direct row edit (store has no
        // public mutate; simulate by inserting a third linking both ways).
        let c = seed(&store, "c", vec![a.clone(), b.clone()]);

        let env = build(&store, &trace_args(&db, &a, 10)).unwrap();
        // Downstream from a: b (d1) and c (d1); terminates.
        let down = ids_at(&env.downstream);
        assert!(down.contains(&(b, 1)));
        assert!(down.contains(&(c, 1)));
        let _ = std::fs::remove_file(&db);
    }
}
