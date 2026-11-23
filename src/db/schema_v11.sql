-- Migration v11: the belt's step 4 (Issue 284, docs/design/belt.md sections 2.1 and 4.3). Columns
-- are only added, so every row is kept as it is: a range approved before this step keeps
-- `follow_ups` `ask` and `design_ahead` off (it was approved under them; widening is a new
-- approval), and a candidate read before it is a ready one (`waits_json` null).
BEGIN;
ALTER TABLE ranges ADD COLUMN follow_ups TEXT NOT NULL DEFAULT 'ask'
  CHECK (follow_ups IN ('ask','inside'));
ALTER TABLE ranges ADD COLUMN design_ahead INTEGER NOT NULL DEFAULT 0
  CHECK (design_ahead IN (0,1));
ALTER TABLE range_candidates ADD COLUMN waits_json TEXT;
INSERT INTO schema_migrations (version, description)
VALUES (11, 'makiba: ranges.follow_ups, ranges.design_ahead, range_candidates.waits_json (the belt step 4, Issue 284)');
COMMIT;
