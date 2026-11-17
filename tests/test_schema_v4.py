"""Migration v4: a fresh database has the v3 records plus every table and CHECK of spec section 5."""
import sqlite3

import pytest

from makiba.db import connect, migrate

V4_TABLES = {
    "agents": {"id", "agent_id", "role", "run_id", "key_sha256", "created_at", "revoked_at"},
    "commands": {"id", "received_at", "actor", "verb", "args_json", "idem", "status",
                 "result_json", "error_kind", "finished_at"},
    "run_launch": {"id", "run_id", "spawn_id", "kind", "agent", "model", "brief", "worktree",
                   "conversation_id", "stage", "failed_reason", "created_at", "verified_at"},
    "messages": {"id", "event_id", "sender", "recipient", "kind", "run_id", "body"},
    "pending": {"id", "created_at", "run_id", "kind", "question", "context_json", "prompt_text",
                "prompt_sum", "status", "answer", "answered_by", "answered_at"},
    "pr_watches": {"id", "repo", "pr", "head_oid", "run_id", "status", "conclusion",
                   "started_at", "finished_at"},
    "dispatcher": {"id", "pid", "started_at", "heartbeat_at", "base", "base_version",
                   "subscribed"},
}
V3_TABLES = {"projects", "workstreams", "worker_dirs", "runs", "events", "event_deliveries",
             "tags", "tag_assignments", "org_sessions", "schema_migrations"}


@pytest.fixture()
def conn(tmp_path):
    c = connect(tmp_path / "state.db")
    migrate(c)
    yield c
    c.close()


def _run_id(conn):
    conn.execute("INSERT INTO projects (slug, display_name) VALUES ('p', 'P')")
    conn.execute("INSERT INTO runs (task_id, project_id, pattern, title) "
                 "VALUES ('t1', 1, 'B', 'T')")
    return 1


def _event_id(conn):
    return conn.execute("INSERT INTO events (kind) VALUES ('message_sent')").lastrowid


def test_fresh_database_is_at_v4_with_every_table(conn):
    tables = {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    assert V3_TABLES | set(V4_TABLES) <= tables
    assert conn.execute("SELECT MAX(version) FROM schema_migrations").fetchone()[0] == 4
    for table, columns in V4_TABLES.items():
        got = {r[1] for r in conn.execute(f"PRAGMA table_info({table})")}
        assert got == columns, table


def test_migrate_twice_changes_nothing(conn):
    migrate(conn)
    assert [r[0] for r in conn.execute("SELECT version FROM schema_migrations ORDER BY 1")] == [
        1, 2, 3, 4]


def _refused(conn, sql, params=()):
    with pytest.raises(sqlite3.IntegrityError):
        conn.execute(sql, params)


def test_second_delivery_for_same_event_and_recipient_is_refused(conn):
    event = _event_id(conn)
    conn.execute("INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?, 'secretary')",
                 (event,))
    _refused(conn, "INSERT INTO event_deliveries (source_event_id, recipient) "
                   "VALUES (?, 'secretary')", (event,))
    conn.execute("INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?, 'curator')",
                 (event,))


def test_run_launch_unknown_stage_is_refused(conn):
    run = _run_id(conn)
    insert = ("INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage) "
              "VALUES (?, ?, 'fake', 'b', '/w', ?)")
    _refused(conn, insert, (run, "s1", "teleported"))
    conn.execute(insert, (run, "s1", "dialog_passed"))
    _refused(conn, insert, (run, "s1", "planned"))  # spawn_id is unique


@pytest.mark.parametrize("sql", [
    "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('a', 'boss', 'k')",
    "INSERT INTO commands (actor, verb, status) VALUES ('person', 'dispatch', 'queued')",
    ("INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, kind) "
     "VALUES (1, 's', 'fake', 'b', '/w', 'other')"),
    ("INSERT INTO messages (event_id, sender, recipient, kind, body) "
     "VALUES (1, 'a', 'b', 'shout', 'x')"),
    "INSERT INTO pending (kind, question) VALUES ('other', 'q')",
    "INSERT INTO pending (kind, question, status) VALUES ('question', 'q', 'closed')",
    "INSERT INTO pr_watches (repo, pr, head_oid, status) VALUES ('o/r', 1, 'h', 'gone')",
    "INSERT INTO dispatcher (id) VALUES (2)",
])
def test_check_constraints_refuse_bad_values(conn, sql):
    _run_id(conn)
    _event_id(conn)
    _refused(conn, sql)


def test_unique_keys_and_defaults(conn):
    conn.execute("INSERT INTO agents (agent_id, role, key_sha256) VALUES ('a', 'worker', 'k')")
    _refused(conn, "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('b', 'worker', 'k')")
    conn.execute("INSERT INTO commands (actor, verb, idem) VALUES ('person', 'dispatch', 'i')")
    _refused(conn, "INSERT INTO commands (actor, verb, idem) VALUES ('person', 'close', 'i')")
    conn.execute("INSERT INTO commands (actor, verb) VALUES ('person', 'show')")
    conn.execute("INSERT INTO commands (actor, verb) VALUES ('person', 'show')")  # NULL idem
    assert conn.execute("SELECT status FROM commands").fetchone()[0] == "accepted"
    conn.execute("INSERT INTO pr_watches (repo, pr, head_oid) VALUES ('o/r', 1, 'h')")
    _refused(conn, "INSERT INTO pr_watches (repo, pr, head_oid) VALUES ('o/r', 1, 'h')")
    conn.execute("INSERT INTO dispatcher (id) VALUES (1)")
    assert conn.execute("SELECT subscribed FROM dispatcher").fetchone()[0] == 0
