//! Compact result rows shared by the queries that list decisions (`why`,
//! `context`, `search`) — the first stage of two-stage retrieval (design §9.1):
//! id + rationale summary + binding + flags + ts. Full detail comes from
//! `dlog show`.
//!
//! Output is bounded to an agent's context budget (#33): rows are emitted
//! newest-first until a character `budget` is reached (or the count `limit`),
//! and the rationale summary width adapts so a tight budget yields *more, terser*
//! rows rather than a few verbose ones. Omitted in-scope results are reported as
//! `elided` (§9.1 principle 2: state, not suggestions).

use serde::Serialize;

use crate::model::{Binding, StoredDecision};
use crate::store::Store;

/// Default per-row rationale summary width (also the width for non-budgeted
/// callers like `trace`).
pub(crate) const SUMMARY_MAX: usize = 140;
/// Floor the adaptive width never trims below.
const SUMMARY_FLOOR: usize = 48;
/// Approximate fixed JSON cost of a row besides the rationale (id + binding +
/// flags + ts), used to estimate a row's size against the budget.
const ROW_OVERHEAD: usize = 96;

/// One compact result row (§9.3).
#[derive(Debug, Serialize)]
pub(crate) struct CompactRow {
    pub id: String,
    pub rationale_summary: String,
    pub binding: Option<Binding>,
    pub staged: bool,
    pub superseded: bool,
    /// Record time, epoch milliseconds.
    pub ts: i64,
}

/// Build compact rows for `ids` (already in priority order, newest-first),
/// applying the live-decisions scope (§9.1: superseded dropped unless
/// `include_superseded`, staging kept and flagged) and bounding the output by a
/// character `budget` (0 = unbounded) and a count `limit`. Returns the rows, a
/// `truncated` flag, and the `elided` count of in-scope results omitted.
pub(crate) fn collect(
    store: &Store,
    ids: &[String],
    include_superseded: bool,
    limit: usize,
    budget: usize,
) -> rusqlite::Result<(Vec<CompactRow>, bool, usize)> {
    let superseded = store.superseded_ids()?;

    // The in-scope (live) candidates, newest-first. Only a set lookup per id —
    // no decision fetch yet — so we can size the adaptive width up front.
    let live: Vec<&String> = ids
        .iter()
        .filter(|id| include_superseded || !superseded.contains(*id))
        .collect();

    let width = adaptive_width(budget, live.len());

    let mut rows = Vec::new();
    let mut running = 0usize;
    for &id in &live {
        if rows.len() >= limit {
            break;
        }
        let Some(d) = store.get_decision(id)? else {
            continue;
        };
        let summary = summarize(&d.rationale, width);
        let cost = summary.chars().count() + ROW_OVERHEAD;
        // Always emit at least one row; otherwise stop when the budget is hit.
        if budget > 0 && !rows.is_empty() && running + cost > budget {
            break;
        }
        running += cost;
        rows.push(CompactRow {
            id: d.id,
            rationale_summary: summary,
            binding: d.binding,
            staged: d.staged,
            superseded: superseded.contains(id),
            ts: d.created_at_ms,
        });
    }

    let elided = live.len() - rows.len();
    Ok((rows, elided > 0, elided))
}

/// The rationale summary width to use for `count` candidates under `budget`
/// chars: roughly the per-row share minus fixed overhead, clamped to
/// `[SUMMARY_FLOOR, SUMMARY_MAX]`. `budget == 0` means no budget (full width).
fn adaptive_width(budget: usize, count: usize) -> usize {
    if budget == 0 {
        return SUMMARY_MAX;
    }
    let per_row = budget / count.max(1);
    per_row
        .saturating_sub(ROW_OVERHEAD)
        .clamp(SUMMARY_FLOOR, SUMMARY_MAX)
}

/// Build a single compact row from an already-fetched decision (used by `trace`,
/// which walks the DAG node by node rather than from an id list). Full width.
pub(crate) fn row_from(decision: StoredDecision, superseded: bool) -> CompactRow {
    CompactRow {
        rationale_summary: summarize(&decision.rationale, SUMMARY_MAX),
        id: decision.id,
        binding: decision.binding,
        staged: decision.staged,
        superseded,
        ts: decision.created_at_ms,
    }
}

/// Compact the rationale to its first line, capped at `width` chars, for the
/// two-stage form.
pub(crate) fn summarize(rationale: &str, width: usize) -> String {
    let first_line = rationale.lines().next().unwrap_or("");
    if first_line.chars().count() <= width {
        first_line.to_string()
    } else {
        let head: String = first_line.chars().take(width).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

    fn store_with(n: usize, rationale: &str) -> (Store, Vec<String>) {
        let store = Store::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for _ in 0..n {
            let id = store
                .stage_decision(&NewDecision {
                    task_id: None,
                    agent: Agent {
                        role: "r".into(),
                        model: "m".into(),
                        session_id: None,
                    },
                    conversation_id: None,
                    rationale: rationale.into(),
                    rejected: vec![],
                    caused_by: vec![],
                    supersedes: None,
                    anchors: vec![Anchor {
                        file: "f.rs".into(),
                        symbol_path: None,
                        node_kind: None,
                        structural_hash: None,
                        line_span: None,
                        recorded_at_sha: None,
                    }],
                })
                .unwrap();
            ids.push(id);
        }
        // Newest-first, matching how callers order candidates.
        ids.reverse();
        (store, ids)
    }

    #[test]
    fn summarize_caps_at_width() {
        let long = "x".repeat(200);
        assert_eq!(summarize(&long, 10).chars().count(), 11); // 10 + ellipsis
        assert_eq!(summarize("short", 140), "short");
        assert_eq!(summarize("a\nb", 140), "a"); // first line only
    }

    #[test]
    fn budget_bounds_rows_and_reports_elided() {
        let (store, ids) = store_with(10, &"detail ".repeat(40));
        // Tiny budget keeps far fewer than 10; elided accounts for the rest.
        let (rows, truncated, elided) = collect(&store, &ids, false, 100, 300).unwrap();
        assert!(rows.len() < 10 && !rows.is_empty());
        assert!(truncated);
        assert_eq!(elided, 10 - rows.len());

        // budget 0 = unbounded: all 10 within the count limit, nothing elided.
        let (rows, truncated, elided) = collect(&store, &ids, false, 100, 0).unwrap();
        assert_eq!(rows.len(), 10);
        assert!(!truncated);
        assert_eq!(elided, 0);
    }

    #[test]
    fn adaptive_width_shrinks_under_tight_budget() {
        // Many candidates + small budget -> narrow summaries (at the floor);
        // generous budget -> full width.
        assert_eq!(adaptive_width(0, 50), SUMMARY_MAX);
        assert_eq!(adaptive_width(100, 50), SUMMARY_FLOOR);
        assert_eq!(adaptive_width(1_000_000, 1), SUMMARY_MAX);
        let mid = adaptive_width(4096, 10);
        assert!((SUMMARY_FLOOR..=SUMMARY_MAX).contains(&mid));
    }

    #[test]
    fn limit_still_caps_rows() {
        let (store, ids) = store_with(10, "short");
        let (rows, truncated, elided) = collect(&store, &ids, false, 3, 0).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(truncated);
        assert_eq!(elided, 7);
    }
}
