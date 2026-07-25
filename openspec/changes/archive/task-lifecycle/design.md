# Design: Task Lifecycle — the non-code seal trigger

## Where it plugs in

Everything needed already exists one layer down:

- `Store::insert_task(parent_task_id, instruction)` (`src/store.rs`) writes a
  task with a fresh ULID and a parent. It has never had a caller.
- `Store::seal_staged(binding, only)` seals a set of staged decisions atomically,
  all-or-nothing, stamping the binding. `dlog bind` already drives it with
  `Binding::None` and an optional id list.

So the change is one new command module that picks the id list by task, plus one
store query. No new sealing machinery, and no new binding semantics.

## Command surface

`dlog task` is the repo's first nested subcommand, so `cli.rs` gains a
`TaskArgs { command: TaskCommand }` with a `TaskCommand` enum, and
`Command::name()` reports `"task"` for both.

```
dlog task start [--parent <TASK_ID>] [--instruction <TEXT>]
dlog task done  <TASK_ID>
```

### `task start`

```json
{"id":"01K...","parent_task_id":"01K...","instruction":"make the client resilient"}
```

`--instruction` is the human's original instruction (§7.1); `--parent` is the
task hierarchy (§4, §7.1). Both optional — §7.3 minimalism, and a root task run
without a recorded prompt is a normal case. Optional fields are omitted from the
JSON when absent.

A `--parent` that does not exist is rejected with `unknown_task` rather than
left to the foreign key: the FK error would surface as a generic `store_error`,
and a mistyped parent silently reparenting to nothing is worse than a failure.

### `task done`

```json
{"task":"01K...","count":2,"sealed":["01K...","01K..."],"binding":{"type":"none"}}
```

Mirrors `dlog bind`'s result shape so an agent can treat the two seal paths
alike.

- **An unknown task id is an error** (`unknown_task`). Sealing nothing because
  of a typo would look identical to success.
- **A known task with nothing staged is a success with `count: 0`**, not an
  error. A subagent that investigated and recorded nothing still ends its task;
  making that a failure would train agents to skip the call.
- Only the task's *own* decisions are sealed — see Non-goals on cascading.

## Store

One new query, keeping the id selection in SQL next to the other id lookups:

```rust
/// Staged decisions belonging to `task_id`, oldest-first.
pub fn staged_decision_ids_for_task(&self, task_id: &str) -> rusqlite::Result<Vec<String>>
```

```sql
SELECT id FROM decision WHERE task_id = ?1 AND staged = 1 ORDER BY id
```

plus `task_exists(&self, id: &str) -> rusqlite::Result<bool>` for the two
validation points above. `decision.task_id` is already indexed
(`idx_decision_task` in `schema.sql`), so the scan is on the index.

`task done` then calls `seal_staged(&Binding::None, Some(&ids))`, inheriting its
atomicity. When `ids` is empty it must **not** fall through to `None`, which
would seal the whole store — the empty slice is passed explicitly.

## Interaction with `record --task`

Unchanged. `record --task <id>` still calls `ensure_task`, so an agent that
invents its own ids keeps working and `task start` is an upgrade, not a
precondition. The natural flow becomes:

```bash
TASK=$(dlog task start --instruction "$PROMPT" | jq -r .id)
dlog record --task "$TASK" --rationale ... --file ...
dlog task done "$TASK"        # or dlog commit, for the code path
```

## Alternatives considered

- **Cascade `task done` to descendant tasks.** Tempting as a safety net for a
  subagent that forgot to seal, but it recreates the exact problem this change
  fixes — one agent binding another's in-progress decisions to "no commit",
  irreversibly, because sealed rows are append-only. Stranded staging is already
  visible in `dlog status`. Rejected.
- **A `completed_at_ms` column on `task`.** Would let `dlog status` list open
  tasks, but the store has no ALTER-based migration path (migration is idempotent
  `CREATE TABLE IF NOT EXISTS` replay), and §7.3 argues against fields nothing
  reads yet. Deferred with `task list`.
- **`dlog task done --commit <sha>`.** The code path already has two commands;
  adding a third spelling of it would blur which trigger is which (§8.3 keeps
  them separate on purpose). Rejected.
- **Auto-seal on `task start` of a *sibling*.** Implicit sealing is exactly the
  surprise this change removes. Rejected.

## Risks / mitigations

- **Decisions recorded without `--task` are invisible to `task done`.** That is
  correct — they belong to no task — but an agent that forgets `--task` will find
  its decisions still staged. Mitigation: `dlog status` reports staging, and the
  AGENTS.md template pairs `task start` with `record --task` in one flow.
- **`task done` called twice** seals nothing the second time and returns
  `count: 0`. Harmless, and consistent with "no completion state" above.
