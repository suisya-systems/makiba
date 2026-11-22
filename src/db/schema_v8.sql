-- Migration v8: a range as a record (Issue 257, docs/design/belt.md section 2). A range is the
-- person's approval of what may go in without asking; its newest read of the project's Issues is
-- kept beside it, and a run names the range it was dispatched under.
BEGIN;

CREATE TABLE ranges (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id        INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
  issues_json       TEXT NOT NULL DEFAULT '[]',   -- the explicit list of Issue numbers
  takes             TEXT NOT NULL DEFAULT '',     -- a condition: label:, no-label:, milestone:, clause:<=G<n>, joined by AND
  at_once           INTEGER NOT NULL CHECK (at_once >= 1),
  budget_tasks      INTEGER NOT NULL CHECK (budget_tasks >= 1),
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
  ended_reason      TEXT CHECK (ended_reason IN ('person','secretary','budget_tasks','used_up','scan_error','replaced')),
  ended_at          TEXT
);
CREATE INDEX idx_ranges_project ON ranges(project_id, status);

CREATE TABLE range_candidates (
  range_id      INTEGER NOT NULL REFERENCES ranges(id) ON DELETE CASCADE,
  read_id       INTEGER NOT NULL,                 -- the range_read event of this read
  position      INTEGER NOT NULL,                 -- the read's order: goal clause, then number
  issue         INTEGER NOT NULL,
  title         TEXT NOT NULL,
  body          TEXT NOT NULL,
  comments_json TEXT NOT NULL DEFAULT '[]',
  admitted      INTEGER NOT NULL,
  left_out_why  TEXT,
  PRIMARY KEY (range_id, issue)
);

ALTER TABLE runs ADD COLUMN range_id INTEGER REFERENCES ranges(id);
CREATE INDEX idx_runs_range ON runs(range_id);

INSERT INTO schema_migrations (version, description)
VALUES (8, 'makiba: ranges, range_candidates, runs.range_id (the belt, Issue 257)');

COMMIT;
