-- Migration v9: the budgets of a range (Issue 282, docs/design/belt.md section 2.1, step 2):
-- `budget_hours`, `budget_fixes`, and `budget_hours` as a reason a range ended. A CHECK cannot be
-- changed in place, so `ranges` is made again and its rows copied (SQLite's own procedure: foreign
-- keys off around the transaction, so that dropping the old table deletes no candidate and breaks
-- no run; the tables that name `ranges` then name the new one).
PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE ranges_v9 (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id        INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
  issues_json       TEXT NOT NULL DEFAULT '[]',   -- the explicit list of Issue numbers
  takes             TEXT NOT NULL DEFAULT '',     -- a condition: label:, no-label:, milestone:, clause:<=G<n>, joined by AND
  at_once           INTEGER NOT NULL CHECK (at_once >= 1),
  budget_tasks      INTEGER NOT NULL CHECK (budget_tasks >= 1),
  budget_hours      REAL CHECK (budget_hours > 0),          -- wall-clock hours from approval; null: none
  budget_fixes      INTEGER NOT NULL DEFAULT 3 CHECK (budget_fixes >= 1),  -- heads pushed to a task's pull request after the first
  brief_head        TEXT NOT NULL,
  replaces          INTEGER REFERENCES ranges(id),
  proposed_by       TEXT NOT NULL,
  proposed_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  approval_mode     TEXT,
  approved_by       TEXT,
  approved_at       TEXT,
  approved_read_id  INTEGER,                      -- the range_read event shown at approval
  status            TEXT NOT NULL DEFAULT 'proposed'
                    CHECK (status IN ('proposed','active','paused','ended')),
  ended_reason      TEXT CHECK (ended_reason IN ('person','secretary','budget_tasks','budget_hours','used_up','scan_error','replaced')),
  ended_at          TEXT
);
INSERT INTO ranges_v9 (id, project_id, issues_json, takes, at_once, budget_tasks, brief_head, replaces, proposed_by, proposed_at, approval_mode, approved_by, approved_at, approved_read_id, status, ended_reason, ended_at)
  SELECT id, project_id, issues_json, takes, at_once, budget_tasks, brief_head, replaces, proposed_by, proposed_at, approval_mode, approved_by, approved_at, approved_read_id, status, ended_reason, ended_at FROM ranges;
DROP TABLE ranges;
ALTER TABLE ranges_v9 RENAME TO ranges;
CREATE INDEX idx_ranges_project ON ranges(project_id, status);

INSERT INTO schema_migrations (version, description)
VALUES (9, 'makiba: ranges.budget_hours, budget_fixes, ended_reason budget_hours (the belt step 2, Issue 282)');

COMMIT;
PRAGMA foreign_keys = ON;
