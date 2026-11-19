-- Migration v6: a launch is verified only by an observed first turn (spec section 5.3, Issue M3-6).
-- No row of `events` is a `worker_spawn_verified` unless its payload's `event_id` names a
-- `terminal_status` of state `working` or a `terminal_turn` of the same `spawn_id`, whoever writes it.
BEGIN;

CREATE TRIGGER events_verified_insert BEFORE INSERT ON events
WHEN NEW.kind = 'worker_spawn_verified' AND NOT EXISTS (
  SELECT 1 FROM events e
  WHERE e.id = json_extract(NEW.payload_json, '$.event_id')
    AND json_extract(e.payload_json, '$.spawn_id') = json_extract(NEW.payload_json, '$.spawn_id')
    AND (e.kind = 'terminal_turn'
         OR (e.kind = 'terminal_status' AND json_extract(e.payload_json, '$.state') = 'working')))
BEGIN
  SELECT RAISE(ABORT, 'worker_spawn_verified needs the id of a working status or a turn of its terminal');
END;

CREATE TRIGGER events_verified_update BEFORE UPDATE ON events
WHEN NEW.kind = 'worker_spawn_verified' AND NOT EXISTS (
  SELECT 1 FROM events e
  WHERE e.id = json_extract(NEW.payload_json, '$.event_id')
    AND json_extract(e.payload_json, '$.spawn_id') = json_extract(NEW.payload_json, '$.spawn_id')
    AND (e.kind = 'terminal_turn'
         OR (e.kind = 'terminal_status' AND json_extract(e.payload_json, '$.state') = 'working')))
BEGIN
  SELECT RAISE(ABORT, 'worker_spawn_verified needs the id of a working status or a turn of its terminal');
END;

INSERT INTO schema_migrations (version, description)
VALUES (6, 'makiba: worker_spawn_verified only with the id of a working status or a turn of its terminal');

COMMIT;
