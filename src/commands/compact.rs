//! Compact result rows shared by the queries that list decisions (`why`,
//! `search`) — the first stage of two-stage retrieval (design §9.1): id +
//! rationale summary + binding + flags + ts. Full detail comes from `dlog show`.

use serde::Serialize;

use crate::model::Binding;
use crate::store::Store;

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

/// Build compact rows for `ids` (already in priority order), applying the
/// live-decisions scope (§9.1): superseded decisions are dropped unless
/// `include_superseded`, staging is kept and flagged. Returns the rows (capped
/// at `limit`) and whether the live matches exceeded the limit (truncated).
pub(crate) fn collect(
    store: &Store,
    ids: &[String],
    include_superseded: bool,
    limit: usize,
) -> rusqlite::Result<(Vec<CompactRow>, bool)> {
    let superseded = store.superseded_ids()?;
    let mut rows = Vec::new();
    let mut live_matches = 0usize;
    for id in ids {
        let is_superseded = superseded.contains(id);
        if is_superseded && !include_superseded {
            continue;
        }
        live_matches += 1;
        if rows.len() >= limit {
            continue;
        }
        if let Some(d) = store.get_decision(id)? {
            rows.push(CompactRow {
                id: d.id,
                rationale_summary: summarize(&d.rationale),
                binding: d.binding,
                staged: d.staged,
                superseded: is_superseded,
                ts: d.created_at_ms,
            });
        }
    }
    Ok((rows, live_matches > limit))
}

/// Compact the rationale to its first line, capped, for the two-stage form.
pub(crate) fn summarize(rationale: &str) -> String {
    const MAX: usize = 140;
    let first_line = rationale.lines().next().unwrap_or("");
    if first_line.chars().count() <= MAX {
        first_line.to_string()
    } else {
        let head: String = first_line.chars().take(MAX).collect();
        format!("{head}…")
    }
}
