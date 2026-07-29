# Design: rollups for `context` and `trace`

## `dlog context` — per-file rollup

```
dlog context <PATH> [--rollup | --flat] [--no-invariants] [--limit N] [--budget CHARS]
```

```json
{"query":{"type":"context","path":"src/auth","mode":"rollup"},
 "results":[{"file":"src/auth/login.rs","count":22,
             "latest":{"id":"01K…","rationale_summary":"retry 3x…",
                       "binding":null,"staged":true,"superseded":false,"ts":178…}}],
 "invariants":[{"id":"01K…","statement":"tokens never persist to disk",
                "scope":"src/auth","declared_by":"01K…"}],
 "truncated":false,"elided":0}
```

`query.mode` is `rollup` or `flat`. It is there because the shape of `results`
changes and the agent should read that off the response rather than re-derive it
from the flags it passed (§9.1 principle 2 — state, not inference).

### Which mode is the default

`--flat` → flat. `--rollup` → rollup. Neither → **rollup iff the decisions under
the path touch more than one file.** So `dlog context src/auth/login.rs` behaves
exactly as it always has, and `dlog context src/auth` groups.

The alternative — "rollup iff the path is a directory" — needs a filesystem stat
of a path that may not exist any more (a decision about a deleted file is still a
decision). Deciding from the data avoids both the stat and the case where a
directory happens to hold decisions in exactly one file, where a one-row rollup
would be strictly worse than the stream.

### Grouping

`Store::anchors_under_path` returns `(file, decision_id)` pairs newest-first by
decision id, the same scope as `decision_ids_under_path`. Since ULIDs sort by
time, the first id seen for a file is that file's latest, and first-seen file
order is already newest-first across files — so grouping is one pass with no
sort. Live-scope filtering (§9.1) happens on the pairs *before* grouping, so
`count` and `latest` agree with each other and with `--include-superseded`.

A decision anchored to two files under the path appears under **both** files in
rollup mode — it is a decision about each of them — but exactly **once** in flat
mode, which is why the flat path de-duplicates ids rather than reusing the raw
pairs.

`count` is deliberately the only aggregate. Not "most decided file", not a
staleness score: those are dlog forming an opinion about the agent's priorities
(§9.1 principle 2, and the `hints` rejection in §11).

### Invariants ride along

`context` returns the invariants in effect at the path, using
`invariants::scope_matches` — the *same* function `dlog invariants --scope` uses,
made `pub(crate)` rather than duplicated, so the two commands cannot disagree
about what is in effect where.

They are on by default. §3 makes `context` the task-start command and §7.1 makes
invariants the constraints that outlive the decision that declared them; an
invariant the agent has to remember to ask for separately is an invariant that
gets violated. `--no-invariants` exists for the caller that already has them.

The list is capped at `INVARIANT_CAP = 20` with `invariants_elided` reporting the
rest — the same treatment `status` gives `stranded_tasks`, and for the same
reason: a command that runs at every task start must not be unbounded. The cap is
a constant, not a flag; a knob here would be one more thing for every caller to
think about for a list that is a handful of rows in practice.

Invariants are *not* charged against `--budget`. The budget exists to trade off
how many decisions are worth their summaries; a constraint is not that kind of
row — you either see it or you break it.

## `dlog trace` — nested DAG and a budget

```json
{"query":{"type":"trace","id":"01K…","depth":10,"budget":4096},
 "root":{…compact row…},
 "upstream":[…],
 "downstream":[{…compact row…,"depth":1,
                "edges":[{…compact row…,"depth":2}]}],
 "truncated":false,"elided":0}
```

`TraceNode` is a flattened `CompactRow` plus `depth` plus `edges`. `edges` is
omitted when empty, so a leaf costs exactly what a row cost before. The old
output was the same rows with the same `depth` tags in one flat list, so a reader
that ignores `edges` sees no regression in the fields it reads — only in the
nesting.

### Why nest at all

§4 says decisions form a DAG. `depth` alone tells you how far a node is from the
root but not what it hangs off, so a decision that caused three follow-ups is
indistinguishable from three unrelated decisions at the same distance — and the
branch point is the interesting part of a causal chain.

Each node appears **once**, under the parent that first reached it in the BFS.
A DAG node can have several parents, and reproducing it under each would turn a
diamond into an exponential blowup for no added information. The first BFS parent
is the shortest path from the root, which is the one worth showing.

### Budgeting a graph

`--budget` (default 4096, `0` = unbounded) shares one purse across both
directions, with the adaptive summary width computed from the *total* reachable
count so a bushy `downstream` doesn't starve a short `upstream` chain of the
causes that explain it.

Rows are built in BFS order — nearest the root first — and the walk stops at the
first node that doesn't fit. Because a parent always precedes its children in BFS
order, stopping mid-walk removes every descendant of everything it removed:
**the cut is by branch, never leaving an orphan**. That is what #63 asked for,
and it falls out of the ordering rather than needing subtree accounting.

Two phases, deliberately: `reachable` collects ids only, then `materialize`
fetches decisions and builds rows while the purse lasts. So a 500-node subgraph
under a tight budget costs 500 cheap id lookups and a handful of decision
fetches, not 500 full fetches thrown away.

`elided` counts reachable decisions the budget dropped. `truncated` is now
"either the depth cap or the budget held something back", which keeps its
existing meaning (it already covered the depth cap) and extends it.

`trace` keeps showing superseded decisions, flagged. It is the one query about
*history*; hiding the decision that got reversed would hide the reason the
reversal exists.

## Alternatives considered

- **A separate `dlog rollup` command.** The question "what happened in this
  area" is already `context`'s; a second command would split the answer across
  two invocations at exactly the moment (task start) when round trips cost most.
- **Rollup by directory instead of file**, one level down from the queried path.
  Neater for a huge tree, but the unit an agent then opens is a file, and a
  directory row would need a second drill-down to become actionable.
- **Include a `files` count in flat mode** so the agent can tell whether a rollup
  would help. That is dlog suggesting a next command (§11's `hints` rejection);
  the mode is already chosen from the same fact.
- **Charge invariants to `--budget`.** Then a wide directory silently drops the
  constraints, which inverts their priority — they are the thing you must not
  miss.
- **Reproduce multi-parent nodes under every parent.** Exponential in a diamond,
  and the duplicate rows carry no information the single placement lacks.
- **Cut `trace` depth-first** (whole branches, deepest last). Depth-first would
  spend the budget on one long chain and never reach a sibling one hop from the
  root. Distance from the root is the better relevance proxy.
- **A separate `--trace-budget`.** One budget name across the query surface is
  the point of #33.

## Risks / mitigations

- **Output shape change for existing `context <dir>` callers.** Deliberate (the
  issue asks for it), signposted by `query.mode`, and `--flat` restores the old
  response exactly. `templates/AGENTS.md` documents both.
- **Rollup double-counts a multi-file decision across files.** True, and correct
  per file; the flat mode and `dlog show` remain the per-decision views.
- **`trace` nesting deepens the JSON.** Bounded by `--depth` (default 10) and now
  also by `--budget`, which the old flat output had no equivalent of.
