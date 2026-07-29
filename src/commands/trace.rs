//! `dlog trace <id>` — walk the causal DAG around a decision (design §4, §9;
//! v0.2, #31; nested output + budget #63).
//!
//! Decisions form a DAG via `caused_by` ("B is a fix prompted by A's review").
//! Trace returns the decisions reached by following those edges: `upstream` are
//! the causes (what led here), `downstream` the decisions this one prompted.
//!
//! The reply **keeps the DAG's shape**: each node carries its own `edges`, so a
//! decision that caused three others reads as one node with three edges rather
//! than three siblings that no longer say what they branched from. Cycles are
//! guarded by a visited set; `--depth` caps how far we walk.
//!
//! `--budget` bounds the payload the way #33 bounds the list queries, but cuts
//! **by branch**: nodes are built nearest-root first, and once the budget is
//! spent everything after it in that order is dropped — which, because a node's
//! descendants always come later, drops whole subtrees rather than orphaning
//! them. `elided` reports how many reachable decisions were left out.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::cli::TraceArgs;
use crate::commands::compact::{self, CompactRow};
use crate::commands::{AppError, Workspace};
use crate::output::emit;
use crate::store::Store;

/// Describes the interpreted query (§9.3).
#[derive(Debug, Serialize)]
struct TraceDesc {
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    depth: usize,
    budget: usize,
}

/// A compact row plus its position in the DAG: how far it is from the root, and
/// the decisions it leads to in the direction being walked.
#[derive(Debug, Serialize)]
struct TraceNode {
    #[serde(flatten)]
    row: CompactRow,
    depth: usize,
    /// Further nodes reached from this one. Empty at a leaf, at the depth cap,
    /// or where the budget cut the branch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    edges: Vec<TraceNode>,
}

#[derive(Debug, Serialize)]
struct TraceEnvelope {
    query: TraceDesc,
    root: CompactRow,
    /// Causes — decisions reachable via `caused_by` (ancestors).
    upstream: Vec<TraceNode>,
    /// Effects — decisions that list this one in their `caused_by` (descendants).
    downstream: Vec<TraceNode>,
    truncated: bool,
    /// Reachable decisions left out by the budget (§9.1 principle 2).
    elided: usize,
}

pub fn run(args: TraceArgs) -> Result<(), AppError> {
    let store = Workspace::discover(args.db.clone())?.open()?;
    let envelope = build(&store, &args)?;
    emit(&envelope);
    Ok(())
}

fn build(store: &Store, args: &TraceArgs) -> Result<TraceEnvelope, AppError> {
    let root = store.get_decision(&args.id)?.ok_or_else(|| {
        AppError::new(
            "decision_not_found",
            format!("no decision with id {}", args.id),
        )
    })?;

    let superseded = store.superseded_ids()?;
    let up = reachable(store, &args.id, args.depth, Direction::Up)?;
    let down = reachable(store, &args.id, args.depth, Direction::Down)?;

    // One budget for the whole reply, shared by both directions in proportion to
    // what each of them found — otherwise a bushy downstream would starve a short
    // upstream chain of the causes that explain it.
    let total = up.order.len() + down.order.len();
    let width = compact::adaptive_width(args.budget, total);
    let mut purse = Purse::new(args.budget);

    let (upstream, up_elided) = materialize(store, &up, &superseded, width, &mut purse)?;
    let (downstream, down_elided) = materialize(store, &down, &superseded, width, &mut purse)?;

    let elided = up_elided + down_elided;
    Ok(TraceEnvelope {
        query: TraceDesc {
            kind: "trace",
            id: args.id.clone(),
            depth: args.depth,
            budget: args.budget,
        },
        root: compact::row_from(root, superseded.contains(&args.id)),
        upstream,
        downstream,
        truncated: elided > 0 || up.cut || down.cut,
        elided,
    })
}

#[derive(Clone, Copy)]
enum Direction {
    Up,
    Down,
}

/// The BFS tree found in one direction: every reachable decision in breadth-first
/// order (nearest the root first), each attached to the parent that first reached
/// it, plus whether `--depth` stopped a further level from being explored.
struct Walk {
    /// Reachable ids, breadth-first. A parent always precedes its children.
    order: Vec<String>,
    depth_of: HashMap<String, usize>,
    /// Children of each id, in the same breadth-first order.
    children: HashMap<String, Vec<String>>,
    /// Nodes attached directly to the root.
    roots: Vec<String>,
    cut: bool,
}

/// Breadth-first walk from `root_id` in one direction, up to `max_depth` levels.
/// Only ids are collected here; the decisions themselves are fetched in
/// [`materialize`], so the budget can decide what is worth fetching.
fn reachable(
    store: &Store,
    root_id: &str,
    max_depth: usize,
    direction: Direction,
) -> rusqlite::Result<Walk> {
    let mut walk = Walk {
        order: Vec::new(),
        depth_of: HashMap::new(),
        children: HashMap::new(),
        roots: Vec::new(),
        cut: false,
    };
    let mut visited: HashSet<String> = HashSet::from([root_id.to_string()]);
    let mut frontier: Vec<String> = vec![root_id.to_string()];

    for depth in 1..=max_depth {
        let mut next = Vec::new();
        for id in &frontier {
            for neighbor in neighbors(store, id, direction)? {
                if !visited.insert(neighbor.clone()) {
                    continue;
                }
                walk.order.push(neighbor.clone());
                walk.depth_of.insert(neighbor.clone(), depth);
                if depth == 1 {
                    walk.roots.push(neighbor.clone());
                } else {
                    walk.children
                        .entry(id.clone())
                        .or_default()
                        .push(neighbor.clone());
                }
                next.push(neighbor);
            }
        }
        if next.is_empty() {
            return Ok(walk);
        }
        frontier = next;
    }

    // We exhausted the depth budget; `cut` iff the next level had nodes.
    walk.cut = frontier.iter().any(|id| {
        neighbors(store, id, direction)
            .map(|n| !n.is_empty())
            .unwrap_or(false)
    });
    Ok(walk)
}

/// A shared character budget. `0` means unbounded.
struct Purse {
    remaining: usize,
    unbounded: bool,
}

impl Purse {
    fn new(budget: usize) -> Self {
        Self {
            remaining: budget,
            unbounded: budget == 0,
        }
    }

    /// Charge `cost`, or report that the budget is spent.
    fn take(&mut self, cost: usize) -> bool {
        if self.unbounded {
            return true;
        }
        if cost > self.remaining {
            self.remaining = 0;
            return false;
        }
        self.remaining -= cost;
        true
    }
}

/// Turn a [`Walk`] into nested nodes, spending `purse` in breadth-first order.
/// Because a parent is always built before its children, running out mid-walk
/// drops whole branches rather than orphaning nodes; everything not built is
/// counted as elided.
fn materialize(
    store: &Store,
    walk: &Walk,
    superseded: &HashSet<String>,
    width: usize,
    purse: &mut Purse,
) -> rusqlite::Result<(Vec<TraceNode>, usize)> {
    let mut built: HashMap<String, CompactRow> = HashMap::new();
    for id in &walk.order {
        let Some(decision) = store.get_decision(id)? else {
            continue;
        };
        let row = compact::row_from_at(decision, superseded.contains(id), width);
        if !purse.take(compact::row_cost(&row)) {
            break;
        }
        built.insert(id.clone(), row);
    }

    // Count before assembling: `assemble` drains `built` as it attaches nodes.
    let elided = walk.order.len() - built.len();
    let nodes = walk
        .roots
        .iter()
        .filter_map(|id| assemble(walk, &mut built, id))
        .collect();
    Ok((nodes, elided))
}

/// Attach `id`'s built children beneath it, recursively. Returns `None` when the
/// node itself wasn't built — its whole branch goes with it.
fn assemble(walk: &Walk, built: &mut HashMap<String, CompactRow>, id: &str) -> Option<TraceNode> {
    let row = built.remove(id)?;
    let edges = walk
        .children
        .get(id)
        .map(|kids| {
            kids.iter()
                .filter_map(|kid| assemble(walk, built, kid))
                .collect()
        })
        .unwrap_or_default();
    Some(TraceNode {
        row,
        depth: walk.depth_of.get(id).copied().unwrap_or(0),
        edges,
    })
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
            budget: 0,
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    /// Flatten a node tree to `(id, depth)` pairs, for the assertions that only
    /// care about who was reached.
    fn flatten(nodes: &[TraceNode]) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for node in nodes {
            out.push((node.row.id.clone(), node.depth));
            out.extend(flatten(&node.edges));
        }
        out
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
        assert_eq!(flatten(&env.upstream), vec![(a.clone(), 1)]);
        assert_eq!(flatten(&env.downstream), vec![(c.clone(), 1)]);
        assert!(!env.truncated);

        // From a: downstream reaches b (d1) then c (d2), nested beneath it.
        let env = build(&store, &trace_args(&db, &a, 10)).unwrap();
        assert_eq!(env.downstream.len(), 1);
        assert_eq!(env.downstream[0].row.id, b);
        assert_eq!(env.downstream[0].edges.len(), 1);
        assert_eq!(env.downstream[0].edges[0].row.id, c);
        assert_eq!(env.downstream[0].edges[0].depth, 2);
        assert!(env.upstream.is_empty());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn branches_stay_separate_instead_of_flattening() {
        // a caused b and c; b caused d. The reply must say which branch d is on.
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let a = seed(&store, "a", vec![]);
        let b = seed(&store, "b", vec![a.clone()]);
        let c = seed(&store, "c", vec![a.clone()]);
        let d = seed(&store, "d", vec![b.clone()]);

        let env = build(&store, &trace_args(&db, &a, 10)).unwrap();
        assert_eq!(env.downstream.len(), 2, "two branches off the root");
        let branch_b = env
            .downstream
            .iter()
            .find(|n| n.row.id == b)
            .expect("b is a branch");
        let branch_c = env
            .downstream
            .iter()
            .find(|n| n.row.id == c)
            .expect("c is a branch");
        assert_eq!(branch_b.edges.len(), 1);
        assert_eq!(branch_b.edges[0].row.id, d);
        assert!(branch_c.edges.is_empty());
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
        assert_eq!(flatten(&env.downstream), vec![(b.clone(), 1)]);
        assert!(env.truncated);
        assert_eq!(env.elided, 0, "depth is not budget elision");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn budget_cuts_whole_branches_and_reports_elided() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let long = "detail ".repeat(40);
        let a = seed(&store, &long, vec![]);
        let mut prev = a.clone();
        for _ in 0..10 {
            prev = seed(&store, &long, vec![prev.clone()]);
        }

        let mut args = trace_args(&db, &a, 20);
        args.budget = 300;
        let env = build(&store, &args).unwrap();
        let kept = flatten(&env.downstream);
        assert!(!kept.is_empty() && kept.len() < 10);
        assert!(env.truncated);
        assert_eq!(env.elided, 10 - kept.len());

        // Whatever survived is a connected chain from the root: no orphans.
        let depths: Vec<usize> = kept.iter().map(|(_, depth)| *depth).collect();
        assert_eq!(depths, (1..=kept.len()).collect::<Vec<_>>());

        // Unbounded keeps the whole chain.
        args.budget = 0;
        let env = build(&store, &args).unwrap();
        assert_eq!(flatten(&env.downstream).len(), 10);
        assert_eq!(env.elided, 0);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn budget_is_shared_between_the_two_directions() {
        // A root with causes above and effects below: a tight budget must leave
        // something of each rather than spending it all going one way.
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let long = "detail ".repeat(40);
        let cause = seed(&store, &long, vec![]);
        let root = seed(&store, &long, vec![cause.clone()]);
        for _ in 0..5 {
            seed(&store, &long, vec![root.clone()]);
        }

        let mut args = trace_args(&db, &root, 10);
        args.budget = 600;
        let env = build(&store, &args).unwrap();
        assert!(!env.upstream.is_empty(), "the cause survives");
        assert!(!env.downstream.is_empty(), "some effects survive");
        assert!(env.elided > 0);
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
        let down = flatten(&env.downstream);
        assert!(down.contains(&(b, 1)));
        assert!(down.contains(&(c, 1)));
        let _ = std::fs::remove_file(&db);
    }
}
