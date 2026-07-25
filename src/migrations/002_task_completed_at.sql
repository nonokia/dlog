-- Schema v2: task completion state (design §7.1).
--
-- NULL means the task is still open. `dlog task done` stamps it, `dlog task
-- list` filters on it, and `dlog status` joins it with staged decisions to name
-- the tasks whose work never got sealed (§8.3).
--
-- Applied exactly once, so it may ALTER — unlike the v1 baseline, which is
-- replayed on every store that predates the migration sequence.

ALTER TABLE task ADD COLUMN completed_at_ms INTEGER;
