-- Migration v4: the tables of spec section 5 (docs/design/spec.md), added to the carried v3 schema.
-- Applied by makiba.db.ensure_v4_schema in one transaction.
BEGIN;

CREATE TABLE agents (
  id          INTEGER PRIMARY KEY,
  agent_id    TEXT NOT NULL,              -- 'secretary', 'w-<task_id>', 'curator', 'person'
  role        TEXT NOT NULL CHECK (role IN ('secretary','worker','curator','person')),
  run_id      INTEGER REFERENCES runs(id) ON DELETE SET NULL,
  key_sha256  TEXT NOT NULL UNIQUE,
  created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  revoked_at  TEXT
);
CREATE INDEX idx_agents_agent ON agents(agent_id);

CREATE TABLE commands (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  received_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  actor        TEXT NOT NULL,             -- agent id, or 'person'
  verb         TEXT NOT NULL,
  args_json    TEXT NOT NULL DEFAULT '{}',
  idem         TEXT,
  status       TEXT NOT NULL DEFAULT 'accepted'
               CHECK (status IN ('accepted','done','rejected','failed')),
  result_json  TEXT,
  error_kind   TEXT,
  finished_at  TEXT,
  UNIQUE (actor, idem)
);
CREATE INDEX idx_commands_status ON commands(status);

CREATE TABLE run_launch (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id           INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  spawn_id         TEXT NOT NULL UNIQUE,  -- random; also the tag makiba.spawn
  kind             TEXT NOT NULL DEFAULT 'code' CHECK (kind IN ('code','document')),
  agent            TEXT NOT NULL,         -- agent module name, 'claude_code' or 'fake'
  model            TEXT,
  brief            TEXT NOT NULL,         -- the task text given at launch
  worktree         TEXT NOT NULL,         -- the workplace: a git worktree (code) or a working folder (document)
  conversation_id  TEXT,                  -- the agent's own conversation id, for resume
  stage            TEXT NOT NULL DEFAULT 'planned'
                   CHECK (stage IN ('planned','worktree_ready','settings_ready','launched','dialog_passed','verified','failed')),
  failed_reason    TEXT,
  created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  verified_at      TEXT
);
CREATE INDEX idx_run_launch_run ON run_launch(run_id);

CREATE TABLE messages (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id    INTEGER NOT NULL UNIQUE REFERENCES events(id) ON DELETE CASCADE,
  sender      TEXT NOT NULL,
  recipient   TEXT NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('message','report_start','report_done','report_blocked','notice')),
  run_id      INTEGER REFERENCES runs(id) ON DELETE SET NULL,
  body        TEXT NOT NULL
);
CREATE INDEX idx_messages_recipient ON messages(recipient);

CREATE TABLE pending (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  run_id        INTEGER REFERENCES runs(id) ON DELETE SET NULL,
  kind          TEXT NOT NULL CHECK (kind IN ('question','prompt')),
  question      TEXT NOT NULL,
  context_json  TEXT NOT NULL DEFAULT '{}',   -- background, if_yes, if_no, recommend, why
  prompt_text   TEXT,                         -- kind = 'prompt': the screen lines recorded
  prompt_sum    TEXT,                         -- SHA-256 of prompt_text, normalized
  status        TEXT NOT NULL DEFAULT 'open'
                CHECK (status IN ('open','escalated','answered','withdrawn')),
  answer        TEXT,
  answered_by   TEXT,
  answered_at   TEXT
);
CREATE INDEX idx_pending_status ON pending(status);

CREATE TABLE pr_watches (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  repo         TEXT NOT NULL,             -- owner/name
  pr           INTEGER NOT NULL,
  head_oid     TEXT NOT NULL,             -- 40 hex
  run_id       INTEGER REFERENCES runs(id) ON DELETE SET NULL,
  status       TEXT NOT NULL DEFAULT 'watching'
               CHECK (status IN ('watching','completed','failed','superseded')),
  conclusion   TEXT,
  started_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  finished_at  TEXT,
  UNIQUE (repo, pr, head_oid)
);

CREATE TABLE dispatcher (
  id            INTEGER PRIMARY KEY CHECK (id = 1),
  pid           INTEGER,
  started_at    TEXT,
  heartbeat_at  TEXT,
  base          TEXT,                      -- Base.name: 'herdr-core'
  base_version  TEXT,
  subscribed    INTEGER NOT NULL DEFAULT 0   -- 1 once Base.events() has yielded its first Resync
);

INSERT INTO schema_migrations (version, description)
VALUES (4, 'makiba: agents, commands, run_launch, messages, pending, pr_watches, dispatcher');

COMMIT;
