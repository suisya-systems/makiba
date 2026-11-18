-- Migration v5: the launch identity of a key (spec section 5.1), added to the v4 schema.
BEGIN;

ALTER TABLE agents ADD COLUMN spawn_id TEXT;

INSERT INTO schema_migrations (version, description)
VALUES (5, 'makiba: agents.spawn_id, the launch identity of a terminal');

COMMIT;
