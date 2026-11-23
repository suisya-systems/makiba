-- Migration v10: a run may be `parked` (Issue 283, docs/design/belt.md section 4.1, step 3): it
-- waits only for the person, its terminal is ended and its conversation and workplace are kept.
-- A CHECK cannot be changed in place, so `runs` is made again and its rows copied (SQLite's own
-- procedure, as v9 did for `ranges`: foreign keys off around the transaction, so that dropping
-- the old table deletes no launch and nulls no event; the tables that name `runs` then name the
-- new one). The script leaves its transaction open: `db::migrate` reads `foreign_key_check`
-- inside it and commits only when it finds nothing, else rolls back.
PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE runs_v10 (
  id              INTEGER PRIMARY KEY,
  task_id         TEXT NOT NULL UNIQUE,
  project_id      INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
  workstream_id   INTEGER,
  pattern         TEXT NOT NULL CHECK (pattern IN ('A','B','C','D')),
  title           TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'in_use'
                  CHECK (status IN ('queued','in_use','review','completed','failed','suspended','abandoned','parked')),
  branch          TEXT,
  pr_url          TEXT,
  pr_state        TEXT CHECK (pr_state IN ('draft','open','merged','closed') OR pr_state IS NULL),
  issue_refs      TEXT,
  verification    TEXT NOT NULL DEFAULT 'standard'
                  CHECK (verification IN ('minimal','standard','deep')),
  dispatched_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  completed_at    TEXT,
  worker_dir_id   INTEGER REFERENCES worker_dirs(id) ON DELETE SET NULL,
  commit_short    TEXT,
  commit_full     TEXT,
  outcome_note    TEXT,
  range_id        INTEGER REFERENCES ranges(id),
  FOREIGN KEY (workstream_id, project_id)
    REFERENCES workstreams(id, project_id)
);
INSERT INTO runs_v10 (id, task_id, project_id, workstream_id, pattern, title, status, branch,
                      pr_url, pr_state, issue_refs, verification, dispatched_at, completed_at,
                      worker_dir_id, commit_short, commit_full, outcome_note, range_id)
  SELECT id, task_id, project_id, workstream_id, pattern, title, status, branch,
         pr_url, pr_state, issue_refs, verification, dispatched_at, completed_at,
         worker_dir_id, commit_short, commit_full, outcome_note, range_id FROM runs;
DROP TABLE runs;
ALTER TABLE runs_v10 RENAME TO runs;
CREATE INDEX idx_runs_status ON runs(status);
CREATE INDEX idx_runs_project ON runs(project_id);
CREATE INDEX idx_runs_workstream ON runs(workstream_id);
CREATE INDEX idx_runs_dispatched ON runs(dispatched_at);
CREATE INDEX idx_runs_range ON runs(range_id);

INSERT INTO schema_migrations (version, description)
VALUES (10, 'makiba: runs.status parked (the belt step 3, Issue 283)');
