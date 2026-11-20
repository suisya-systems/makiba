-- Migration v7: a launch can belong to no run (Issue 166). The row names its role; the folder of a
-- launch without a run is its `worktree`. SQLite cannot drop NOT NULL: the table is rebuilt, its
-- rows kept as worker launches.
BEGIN;

CREATE TABLE run_launch_v7 (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id           INTEGER REFERENCES runs(id) ON DELETE CASCADE,  -- null: a launch without a run
  role             TEXT NOT NULL DEFAULT 'worker' CHECK (role IN ('worker','curator','secretary')),
  spawn_id         TEXT NOT NULL UNIQUE,
  kind             TEXT NOT NULL DEFAULT 'code' CHECK (kind IN ('code','document')),
  agent            TEXT NOT NULL,
  model            TEXT,
  brief            TEXT NOT NULL,
  worktree         TEXT NOT NULL,         -- the workplace, or the folder of a launch without a run
  conversation_id  TEXT,
  stage            TEXT NOT NULL DEFAULT 'planned'
                   CHECK (stage IN ('planned','worktree_ready','settings_ready','launched','dialog_passed','verified','failed')),
  failed_reason    TEXT,
  created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  verified_at      TEXT,
  CHECK ((role = 'worker') = (run_id IS NOT NULL))
);

INSERT INTO run_launch_v7 (id, run_id, spawn_id, kind, agent, model, brief, worktree,
                           conversation_id, stage, failed_reason, created_at, verified_at)
SELECT id, run_id, spawn_id, kind, agent, model, brief, worktree,
       conversation_id, stage, failed_reason, created_at, verified_at
FROM run_launch;

DROP TABLE run_launch;

ALTER TABLE run_launch_v7 RENAME TO run_launch;

CREATE INDEX idx_run_launch_run ON run_launch(run_id);

INSERT INTO schema_migrations (version, description)
VALUES (7, 'makiba: run_launch.role, a launch without a run');

COMMIT;
