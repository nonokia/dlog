# Design: `dlog task list`, task completion, stranded tasks

## Schema (version 2)

```sql
-- src/migrations/002_task_completed_at.sql
ALTER TABLE task ADD COLUMN completed_at_ms INTEGER;
```

One nullable column, the first user of the migration sequence from #60. NULL
means open. No CHECK, no default, no index — the `task` table is small (one row
per agent task) and every query over it is a scan either way.

## Completion is stamped once

`task done` seals the task's staged decisions, then:

```sql
UPDATE task SET completed_at_ms = ?2 WHERE id = ?1 AND completed_at_ms IS NULL
```

The `IS NULL` guard makes the **first** completion the recorded one. A second
`task done` is a follow-up seal — an agent that recorded more after finishing —
and reporting the task as freshly completed would overwrite the one fact the
column exists to carry: when the work was declared done. `done` still returns
the effective `completed_at_ms` either way, so a caller always learns the real
value rather than the one it just tried to write.

`task done` on an already-completed task stays a success (it is idempotent
today, and `count: 0` already covers "nothing to seal"). Nothing "reopens" a
task: recording against a completed task is allowed and shows up as staged
decisions on a completed task in `status` — a fact worth surfacing rather than
an error to enforce.

## `dlog task list`

```
dlog task list [--open | --all] [--parent <TASK_ID>] [--limit N]
```

Default is open-only. The reason to run this command is "what is still in
flight?"; `--all` is for history. `--open` exists as its explicit spelling so a
script that means it can say so, and the two flags conflict.

```json
{"query":{"type":"task_list","scope":"open"},
 "results":[{"id":"01K...","instruction_summary":"make the client resilient",
             "staged_count":2,"completed":false,"ts":1781...}],
 "truncated":false,"elided":0}
```

Compact per §9.1 principle 1: the instruction is summarised to its first line at
`compact::SUMMARY_MAX`, and full detail comes from the decisions themselves
(`dlog why` / `show`). `parent_task_id` and `completed_at_ms` are omitted when
absent, matching `task start`'s shape. Ordering is newest-first by id (ULIDs are
time-sortable), like every other list query.

It reuses `QueryEnvelope` — `resolved: None` (nothing anchors here), plus
`truncated` / `elided` from `--limit`. No `--budget`: a row is bounded by the
summary width and there is no long prose to trade off, so the adaptive-width
machinery of #33 has nothing to adapt. `--limit` alone bounds the payload, and
`elided` reports what it left out.

`staged_count` is the number of decisions still unsealed under the task — the
number that tells an agent whether picking this task up recovers anything.

## `dlog status`

```json
{"root":"...","staging_count":7,"oldest_staged_ms":1781...,"schema_version":2,
 "stranded_task_count":2,
 "stranded_tasks":[{"task":"01K...","instruction_summary":"...",
                    "staged_count":5,"oldest_staged_ms":1781...}]}
```

A stranded task is `completed_at_ms IS NULL` **and** has staged decisions — the
join of the two facts `status` could not previously connect. The list is capped
at 20 rows (newest-first) with the untruncated total in `stranded_task_count`,
so a store with a long tail of forgotten tasks can't blow up the payload of the
one command agents are told to run at every task start (§9.4).

Note what this deliberately does not say. It does not suggest sealing, does not
rank, does not flag "stale" past a threshold — §9.1 principle 2 and §11 item on
`hints`. `oldest_staged_ms` is the raw fact; whether an hour is stale is the
agent's call.

Decisions staged with no task at all remain visible only through
`staging_count`. They belong to no task by construction, so there is nothing to
list them under; the difference between `staging_count` and the sum of
`staged_count` is exactly that residue, and `bind --none` is still its escape
hatch.

## Store surface

```rust
pub struct TaskRow {            // one row of `task list`
    pub id: String,
    pub parent_task_id: Option<String>,
    pub instruction: Option<String>,   // summarised by the command layer
    pub staged_count: i64,
    pub completed_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

pub struct StrandedTask { pub task: String, pub instruction: Option<String>,
                          pub staged_count: i64, pub oldest_staged_ms: i64 }

fn list_tasks(&self, include_completed: bool, parent: Option<&str>) -> Result<Vec<TaskRow>>
fn complete_task(&self, id: &str) -> Result<i64>          // effective completed_at_ms
fn stranded_tasks(&self, limit: usize) -> Result<Vec<StrandedTask>>
```

`list_tasks` gets `staged_count` from a correlated subquery over
`idx_decision_task`; `stranded_tasks` from an inner join on `staged = 1` grouped
by task, which is also what makes the "has staged decisions" half of stranded
free. Truncation to `--limit` happens in the command layer, so `elided` counts
in-scope rows rather than re-querying.

`StoreStatus` grows only the scalar `stranded_task_count`; the rows come from
the separate `stranded_tasks` call, keeping summarisation (a presentation
concern) in `commands/status.rs` next to the other output shaping.

## Alternatives considered

- **Infer "open" from having staged decisions**, no column. Then a task that
  finished with nothing staged looks identical to a task that just started, and
  `task done` on an empty task would be unobservable. It is precisely the
  distinction the command exists to draw. Rejected — and it is what forced the
  #60 dependency rather than avoiding it.
- **A `state` enum column** (`open` / `done` / `cancelled`). Cancellation has no
  meaning here: a task nobody sealed leaves its decisions staged either way, and
  the record of what happened is the decisions themselves. §7.3 minimalism.
- **`task list` under `status`** instead of its own command. `status` is
  store-wide state (§9.3 separates it from query results); a filtered, limited,
  parent-scoped listing is a query. Keeping them apart also keeps `status`
  cheap enough to run at every task start.
- **Return the full instruction in `task list`.** The instruction is the human's
  original prompt and can be long; a list of them defeats two-stage retrieval.
  Summarised, like every other list row.
- **A `stale` boolean on stranded tasks**, computed from a threshold. That is a
  suggestion wearing a fact's clothes — the threshold would be dlog's opinion
  about the agent's workflow. `oldest_staged_ms` says the same thing without
  deciding. Rejected (§9.1 principle 2).
- **Overwrite `completed_at_ms` on every `task done`.** Makes the column mean
  "last seal", which `binding` already records per decision. Rejected.

## Risks / mitigations

- **Adopting someone else's stranded task.** `status` names the task; the
  handoff is `record --task <id>` / `task done <id>` with that id. Nothing
  stops two agents doing this at once, but sealing is atomic per call and each
  decision seals once, so the loser sees `count: 0` rather than a corrupt state.
- **Tasks created implicitly by `record --task <invented-id>`** appear as open
  tasks with no instruction. Correct — they are open — and the instruction
  column is optional in `task list` output.
- **`stranded_tasks` cap hides tasks.** `stranded_task_count` reports the total,
  and `task list` (with its own `--limit`) is the unbounded view.
