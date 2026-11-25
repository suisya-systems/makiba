-- Migration v12: a pending entry of kind `merge` (Issue 432, docs/design/merge-act.md section 5):
-- a merge the gate allowed that waits for the person's key. SQLite cannot change a CHECK, so the
-- table is made again with every row kept as it is. No foreign key or trigger refers to `pending`.
BEGIN;
CREATE TABLE pending_v12 (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  run_id        INTEGER REFERENCES runs(id) ON DELETE SET NULL,
  kind          TEXT NOT NULL CHECK (kind IN ('question','prompt','merge')),
  question      TEXT NOT NULL,
  context_json  TEXT NOT NULL DEFAULT '{}',   -- background, if_yes, if_no, recommend, why; a merge's reading
  prompt_text   TEXT,                         -- kind = 'prompt': the screen lines recorded
  prompt_sum    TEXT,                         -- SHA-256 of prompt_text, normalized
  status        TEXT NOT NULL DEFAULT 'open'
                CHECK (status IN ('open','escalated','answered','withdrawn')),
  answer        TEXT,
  answered_by   TEXT,
  answered_at   TEXT
);
INSERT INTO pending_v12 (id, created_at, run_id, kind, question, context_json, prompt_text,
                         prompt_sum, status, answer, answered_by, answered_at)
SELECT id, created_at, run_id, kind, question, context_json, prompt_text,
       prompt_sum, status, answer, answered_by, answered_at FROM pending;
DROP TABLE pending;
ALTER TABLE pending_v12 RENAME TO pending;
CREATE INDEX idx_pending_status ON pending(status);
INSERT INTO schema_migrations (version, description)
VALUES (12, 'makiba: pending.kind merge, a landing the person approves (Issue 432)');
COMMIT;
