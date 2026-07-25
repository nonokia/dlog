# Proposal: `dlog task list`, task completion, and stranded tasks in `status`

> Resolves #61. Depends on the `schema-migrations` change (#60) for the column.

## What

1. A `completed_at_ms` column on `task` (schema version 2), stamped by
   `dlog task done`.
2. **`dlog task list [--open | --all] [--parent <id>] [--limit N]`** — the
   compact form (§9.1 principle 1): id, instruction summary, parent, staged
   count, completion. Open tasks only by default.
3. `dlog status` gains `stranded_tasks` — unfinished tasks that still hold
   staged decisions — plus `stranded_task_count`.

## Why

`task start` hands back an id that only exists in the agent's head. Lose it —
the session ended, a parent never passed it down, a subagent returned only a
summary — and that task's decisions stay staged with **no way to name them
again**. `record --task <id>` needs the id; `task done <id>` needs the id;
nothing lists them.

`dlog status` reports `staging_count` and `oldest_staged_ms`, which says
*something* is stranded but not *whose*. §8.3 makes `status` the detection point
for staging left behind by a bare `git commit`, and §9.4 puts "check `status` at
task start" in the instruction template — but since `task done` seals per task
(the `task-lifecycle` change), the unit an agent has to act on is the task, and
detection never followed. An agent that reads `staging_count: 7` still cannot
seal anything correctly: `bind --none` would bind all seven, including whatever
another agent has in flight.

Listing open tasks needs a completion state. The seal alone can't stand in for
one: a task legitimately ends with zero decisions, and "has nothing staged" is
also true of a task that hasn't recorded anything *yet*. That is why #61 depends
on #60 — the `task-lifecycle` change deferred the column for want of an ALTER
path.

## Non-goals

- **No task state machine.** `completed_at_ms` is null or set; there is no
  paused / cancelled / reopened. Two values cover "can I still hand this off?",
  which is the question being asked.
- **No cascade of completion to child tasks.** Unchanged from `task-lifecycle`:
  the seal obligation sits with each subagent, and a parent completing its
  children would misreport work that never sealed.
- **No re-completion semantics.** A second `dlog task done` still seals whatever
  is staged, but the *first* completion time stands (see design.md).
- **No retroactive attachment of decisions to a task** — that is fixed at
  `record --task` time.
- **No `hints` / "what to do next" field** in `status` or `task list` (§9.1
  principle 2, and §11's settled rejection). Facts only: which tasks are open,
  how much each has staged, how old.
- No change to `record --task` / `ensure_task`, so an agent inventing its own
  task ids keeps working (those tasks show up in `task list` as open).

## Design references

§7.1 (Task entity), §8.3 (seal triggers and stranded-staging detection), §9.1
(two-stage retrieval; state, not suggestions), §9.4 (`status` timing is the
operator's call), §13.
