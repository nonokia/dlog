# Proposal: Task Lifecycle — the non-code seal trigger

> Follow-up to the `workspace-root` change. Implements §8.3's second seal
> trigger, listed as unimplemented in §13.

## What

Add `dlog task`, with two subcommands:

- **`dlog task start [--parent <id>] [--instruction <text>]`** — create a Task
  and return its id. This is the first caller of `Store::insert_task`, and the
  first thing that ever populates `parent_task_id`.
- **`dlog task done <id>`** — seal that task's staged decisions with
  `binding: {type:"none"}`, the non-code seal of §8.3.

## Why

§8.3 defines two seal triggers. The code path is fully built (`dlog bind <sha>`,
`dlog commit`, the post-commit hook). The non-code path — "タスク完了時
(`dlog task done` 等) に `binding: none` でシール" — has never been built; §13
records the shipped substitute as "seal with `dlog bind --none`, and make it a
rule in the instruction template".

That substitute has two gaps:

1. **`bind --none` seals *everything* staged, not the finishing task's share.**
   §8.3's reason for the non-code trigger is subagents: "サブエージェントは自
   タスク終了時に必ずシールするルールにすることで、親に要約しか返らなくても判断
   の現物が本ログに残る". A subagent calling `bind --none` also seals whatever
   the parent, or a sibling, currently has in staging — binding another agent's
   in-progress decisions to "no commit" behind its back. Sealed rows are
   append-only, so that is not recoverable. The narrower `--decision <id>...`
   form exists, but it requires the caller to have tracked every id it recorded.
2. **The Task entity is inert.** §7.1 gives Task a hierarchy and the human's
   original instruction, but tasks can only be created as a side effect of
   `dlog record --task <id>` with an id the agent invents, and `ensure_task`
   hardcodes `parent_task_id` to NULL. The multi-agent structure of §4 exists in
   the schema and nowhere else.

`task start` / `task done` closes both: an agent gets a real task id to record
against, and sealing at task end affects exactly that task's decisions.

## Non-goals

- **No schema change.** No completion timestamp or state column on `task`; the
  seal is the observable outcome. Listing *open* tasks would need one — deferred
  with `dlog task list`.
- **No cascade to child tasks.** §8.3 puts the obligation on each subagent to
  seal its own work; a parent sealing its children's staging would recreate gap
  (1) one level up. Anything genuinely stranded is still caught by `dlog status`
  + `dlog bind --none`.
- **No new `binding` type and no change to the enum** (§8.2, §11 item 3).
  `task done` stamps the existing `{type:"none"}`.
- **`dlog bind --none` stays as it is** — the blunt "seal everything" escape
  hatch for stranded staging is still the right tool for that job.
- No change to `record --task` / `ensure_task`, so an agent that invents its own
  task ids keeps working.
- No changes to `.github/` or CI.
