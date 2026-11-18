//! Migration v4: a fresh database has the v3 records plus every table and CHECK of spec section 5.

use std::collections::BTreeSet;

use makiba::db;
use rusqlite::{Connection, ErrorCode, params};

const V4_TABLES: &[(&str, &[&str])] = &[
    (
        "agents",
        &[
            "id",
            "agent_id",
            "role",
            "run_id",
            "key_sha256",
            "created_at",
            "revoked_at",
            "spawn_id",
        ],
    ),
    (
        "commands",
        &[
            "id",
            "received_at",
            "actor",
            "verb",
            "args_json",
            "idem",
            "status",
            "result_json",
            "error_kind",
            "finished_at",
        ],
    ),
    (
        "run_launch",
        &[
            "id",
            "run_id",
            "spawn_id",
            "kind",
            "agent",
            "model",
            "brief",
            "worktree",
            "conversation_id",
            "stage",
            "failed_reason",
            "created_at",
            "verified_at",
        ],
    ),
    (
        "messages",
        &[
            "id",
            "event_id",
            "sender",
            "recipient",
            "kind",
            "run_id",
            "body",
        ],
    ),
    (
        "pending",
        &[
            "id",
            "created_at",
            "run_id",
            "kind",
            "question",
            "context_json",
            "prompt_text",
            "prompt_sum",
            "status",
            "answer",
            "answered_by",
            "answered_at",
        ],
    ),
    (
        "pr_watches",
        &[
            "id",
            "repo",
            "pr",
            "head_oid",
            "run_id",
            "status",
            "conclusion",
            "started_at",
            "finished_at",
        ],
    ),
    (
        "dispatcher",
        &[
            "id",
            "pid",
            "started_at",
            "heartbeat_at",
            "base",
            "base_version",
            "subscribed",
        ],
    ),
];

const V3_TABLES: &[&str] = &[
    "projects",
    "workstreams",
    "worker_dirs",
    "runs",
    "events",
    "event_deliveries",
    "tags",
    "tag_assignments",
    "org_sessions",
    "schema_migrations",
];

/// A fresh database migrated to v4; the folder lives as long as the returned guard.
fn fresh() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = db::open(&dir.path().join("state.db")).unwrap();
    db::migrate(&mut conn).unwrap();
    (dir, conn)
}

fn names(conn: &Connection, sql: &str) -> BTreeSet<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn run_id(conn: &Connection) -> i64 {
    conn.execute(
        "INSERT INTO projects (slug, display_name) VALUES ('p', 'P')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs (task_id, project_id, pattern, title) VALUES ('t1', 1, 'B', 'T')",
        [],
    )
    .unwrap();
    1
}

fn event_id(conn: &Connection) -> i64 {
    conn.execute("INSERT INTO events (kind) VALUES ('message_sent')", [])
        .unwrap();
    conn.last_insert_rowid()
}

fn refused(conn: &Connection, sql: &str, p: impl rusqlite::Params) {
    match conn.execute(sql, p) {
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {}
        other => panic!("expected a constraint violation for {sql}, got {other:?}"),
    }
}

#[test]
fn fresh_database_is_at_v4_with_every_table() {
    let (_dir, conn) = fresh();
    let tables = names(&conn, "SELECT name FROM sqlite_master WHERE type = 'table'");
    for t in V3_TABLES.iter().chain(V4_TABLES.iter().map(|(t, _)| t)) {
        assert!(tables.contains(*t), "missing table {t}");
    }
    let max: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(max, 5);
    for (table, columns) in V4_TABLES {
        let got = names(
            &conn,
            &format!("SELECT name FROM pragma_table_info('{table}')"),
        );
        let want: BTreeSet<String> = columns.iter().map(|c| c.to_string()).collect();
        assert_eq!(got, want, "{table}");
    }
}

#[test]
fn migrate_twice_changes_nothing() {
    let (_dir, mut conn) = fresh();
    db::migrate(&mut conn).unwrap();
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(versions, [1, 2, 3, 4, 5]);
}

#[test]
fn second_delivery_for_same_event_and_recipient_is_refused() {
    let (_dir, conn) = fresh();
    let event = event_id(&conn);
    let insert = "INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?1, ?2)";
    conn.execute(insert, params![event, "secretary"]).unwrap();
    refused(&conn, insert, params![event, "secretary"]);
    conn.execute(insert, params![event, "curator"]).unwrap();
}

#[test]
fn run_launch_unknown_stage_is_refused() {
    let (_dir, conn) = fresh();
    let run = run_id(&conn);
    let insert = "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage) \
                  VALUES (?1, ?2, 'fake', 'b', '/w', ?3)";
    refused(&conn, insert, params![run, "s1", "teleported"]);
    conn.execute(insert, params![run, "s1", "dialog_passed"])
        .unwrap();
    refused(&conn, insert, params![run, "s1", "planned"]); // spawn_id is unique
}

#[test]
fn check_constraints_refuse_bad_values() {
    let (_dir, conn) = fresh();
    run_id(&conn);
    event_id(&conn);
    for sql in [
        "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('a', 'boss', 'k')",
        "INSERT INTO commands (actor, verb, status) VALUES ('person', 'dispatch', 'queued')",
        "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, kind) \
         VALUES (1, 's', 'fake', 'b', '/w', 'other')",
        "INSERT INTO messages (event_id, sender, recipient, kind, body) \
         VALUES (1, 'a', 'b', 'shout', 'x')",
        "INSERT INTO pending (kind, question) VALUES ('other', 'q')",
        "INSERT INTO pending (kind, question, status) VALUES ('question', 'q', 'closed')",
        "INSERT INTO pr_watches (repo, pr, head_oid, status) VALUES ('o/r', 1, 'h', 'gone')",
        "INSERT INTO dispatcher (id) VALUES (2)",
    ] {
        refused(&conn, sql, []);
    }
}

#[test]
fn unique_keys_and_defaults() {
    let (_dir, conn) = fresh();
    conn.execute(
        "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('a', 'worker', 'k')",
        [],
    )
    .unwrap();
    refused(
        &conn,
        "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('b', 'worker', 'k')",
        [],
    );
    conn.execute(
        "INSERT INTO commands (actor, verb, idem) VALUES ('person', 'dispatch', 'i')",
        [],
    )
    .unwrap();
    refused(
        &conn,
        "INSERT INTO commands (actor, verb, idem) VALUES ('person', 'close', 'i')",
        [],
    );
    for _ in 0..2 {
        // a NULL idem is never a duplicate
        conn.execute(
            "INSERT INTO commands (actor, verb) VALUES ('person', 'show')",
            [],
        )
        .unwrap();
    }
    let status: String = conn
        .query_row("SELECT status FROM commands", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "accepted");
    let watch = "INSERT INTO pr_watches (repo, pr, head_oid) VALUES ('o/r', 1, 'h')";
    conn.execute(watch, []).unwrap();
    refused(&conn, watch, []);
    conn.execute("INSERT INTO dispatcher (id) VALUES (1)", [])
        .unwrap();
    let subscribed: i64 = conn
        .query_row("SELECT subscribed FROM dispatcher", [], |r| r.get(0))
        .unwrap();
    assert_eq!(subscribed, 0);
}
