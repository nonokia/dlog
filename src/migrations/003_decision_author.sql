-- Schema v3: the human behind the agent (design §7.4, #64).
--
-- A decision already records *which agent* made it (role / model / session), but
-- nothing about whose agent it was. That is invisible in a solo store and the
-- first question asked of a shared one, so `dlog export` / `dlog import` need the
-- column to exist from the first version of the wire format.
--
-- Nullable and never required: §7.3 keeps the required set to rationale + anchor
-- + agent role/model, and a solo user should not pay for a team field on every
-- `record`. `record --author` (or $DLOG_AUTHOR) sets it explicitly — it is never
-- inferred from git config, because #58 made the workspace a dlog concept rather
-- than a git one.
--
-- Applied exactly once, so it may ALTER.

ALTER TABLE decision ADD COLUMN agent_author TEXT;
