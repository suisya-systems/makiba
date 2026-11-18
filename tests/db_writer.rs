//! The writer and the migration steps of `state.db`: ported from claude-org-ja `test_writer.py`.
//! Not carried with the code: the snapshot hooks, the worker-file archive and the root detection.

use makiba::db::writer::{NewEvent, Patch, RunUpsert, SessionField, WorkerDir};
use makiba::db::{self, Error, StateWriter};
use rusqlite::Connection;
use serde_json::json;

/// A writer on a fresh database in a folder that lives as long as the guard.
fn fresh() -> (tempfile::TempDir, StateWriter) {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    (dir, w)
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn text(c: &Connection, sql: &str) -> String {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn has_table(c: &Connection, name: &str) -> bool {
    count(
        c,
        &format!("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '{name}'"),
    ) > 0
}

// ---- migration of older databases ------------------------------------------------------------

/// A database made before org_sessions (M1): only the tables the writer reads.
const M1_SCHEMA: &str = "
    CREATE TABLE projects (id INTEGER PRIMARY KEY, slug TEXT NOT NULL UNIQUE, display_name TEXT NOT NULL);
    CREATE TABLE workstreams (id INTEGER PRIMARY KEY, project_id INTEGER, slug TEXT, display_name TEXT,
                              UNIQUE (id, project_id));
    CREATE TABLE worker_dirs (id INTEGER PRIMARY KEY, abs_path TEXT UNIQUE, layout TEXT, lifecycle TEXT,
                              last_seen_at TEXT);
    CREATE TABLE runs (id INTEGER PRIMARY KEY, task_id TEXT UNIQUE, project_id INTEGER, pattern TEXT,
                       title TEXT, status TEXT, dispatched_at TEXT, worker_dir_id INTEGER);
    CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, occurred_at TEXT, actor TEXT, kind TEXT,
                         run_id INTEGER, project_id INTEGER, workstream_id INTEGER,
                         payload_json TEXT NOT NULL DEFAULT '{}');
    CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT, description TEXT);
    INSERT INTO schema_migrations (version, applied_at, description)
      VALUES (1, '2026-01-01T00:00:00.000Z', 'M0 initial');";

/// A database made after org_sessions and before the delivery ledger (M2).
const PRE_LEDGER_SCHEMA: &str = "
    CREATE TABLE org_sessions (id INTEGER PRIMARY KEY CHECK (id = 1),
                               status TEXT NOT NULL DEFAULT 'IDLE', last_writer_at TEXT NOT NULL DEFAULT '');
    INSERT INTO org_sessions (id, status, last_writer_at) VALUES (1, 'IDLE', '');
    INSERT INTO schema_migrations (version, applied_at, description) VALUES (2, '2026-01-01T00:00:00.000Z', 'M2');";

fn old_db(schema: &[&str]) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = db::open(&dir.path().join("old.db")).unwrap();
    for s in schema {
        conn.execute_batch(s).unwrap();
    }
    (dir, conn)
}

#[test]
fn writer_migrates_a_pre_m2_database() {
    let (_dir, conn) = old_db(&[M1_SCHEMA]);
    assert!(!has_table(&conn, "org_sessions"));
    let w = StateWriter::new(conn).unwrap();
    let c = w.conn();
    assert_eq!(
        count(c, "SELECT COUNT(*) FROM org_sessions WHERE id = 1"),
        1
    );
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 2"
        ),
        1
    );
    assert!(has_table(c, "event_deliveries"));
    assert_eq!(count(c, "SELECT MAX(version) FROM schema_migrations"), 5);
}

#[test]
fn writer_migrates_a_pre_ledger_database() {
    let (_dir, conn) = old_db(&[M1_SCHEMA, PRE_LEDGER_SCHEMA]);
    assert!(!has_table(&conn, "event_deliveries"));
    let w = StateWriter::new(conn).unwrap();
    assert!(has_table(w.conn(), "event_deliveries"));
    assert_eq!(
        count(
            w.conn(),
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 3"
        ),
        1
    );
    // The ledger it made has the idempotency key and its indexes.
    assert_eq!(
        count(
            w.conn(),
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = 'event_deliveries' AND name LIKE 'idx_%'"
        ),
        2
    );
}

#[test]
fn migration_and_writer_refuse_to_run_in_a_transaction_and_leave_the_writes_alone() {
    let (_dir, mut conn) = old_db(&[M1_SCHEMA]);
    conn.execute_batch(
        "BEGIN; INSERT INTO projects (slug, display_name) VALUES ('uncommitted', 'x')",
    )
    .unwrap();
    assert!(matches!(
        db::migrate(&mut conn),
        Err(Error::InTransaction(_))
    ));
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM projects"), 0);

    conn.execute_batch(
        "BEGIN; INSERT INTO projects (slug, display_name) VALUES ('uncommitted', 'x')",
    )
    .unwrap();
    assert!(matches!(
        StateWriter::new(conn),
        Err(Error::InTransaction(_))
    ));
}

#[test]
fn failed_v4_migration_is_rolled_back_and_can_be_retried() {
    let (_dir, mut conn) = old_db(&[]);
    db::migrate(&mut conn).unwrap();
    // Take v4 away, then make its second table collide.
    conn.execute_batch(
        "DROP TABLE agents; DROP TABLE commands; DROP TABLE run_launch; DROP TABLE messages;
         DROP TABLE pending; DROP TABLE pr_watches; DROP TABLE dispatcher;
         DELETE FROM schema_migrations WHERE version IN (4, 5);
         CREATE TABLE commands (x INTEGER);",
    )
    .unwrap();
    assert!(matches!(db::migrate(&mut conn), Err(Error::Sqlite(_))));
    assert!(
        conn.is_autocommit(),
        "the failed script must not leave a transaction open"
    );
    assert!(
        !has_table(&conn, "agents"),
        "agents was created before the failure and must be undone"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 4"
        ),
        0
    );
    conn.execute_batch("DROP TABLE commands").unwrap();
    db::migrate(&mut conn).unwrap();
    assert!(has_table(&conn, "agents"));
    assert_eq!(
        count(&conn, "SELECT MAX(version) FROM schema_migrations"),
        5
    );
}

#[test]
fn writer_turns_foreign_keys_back_on() {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("bare.db")).unwrap();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    assert_eq!(count(&conn, "PRAGMA foreign_keys"), 0);
    let w = StateWriter::new(conn).unwrap();
    assert_eq!(count(w.conn(), "PRAGMA foreign_keys"), 1);
    assert!(
        w.conn()
            .execute("INSERT INTO events (kind, run_id) VALUES ('x', 999)", [])
            .is_err(),
        "an event of a run that does not exist is refused"
    );
}

// ---- the session row -------------------------------------------------------------------------

fn set(s: &str) -> Patch {
    Patch::Set(s.to_string())
}

#[test]
fn session_row_is_seeded_idle() {
    let (_dir, w) = fresh();
    let s = w.get_session().unwrap().unwrap();
    assert_eq!(s["id"], 1);
    assert_eq!(s["status"], "IDLE");
}

#[test]
fn update_session_patches_listed_fields_only() {
    let (_dir, w) = fresh();
    w.update_session(&[
        (SessionField::Status, set("ACTIVE")),
        (SessionField::Objective, set("ship M2")),
        (SessionField::DispatcherPaneId, set("2")),
    ])
    .unwrap();
    let s = w.get_session().unwrap().unwrap();
    assert_eq!(
        (&s["status"], &s["objective"], &s["dispatcher_pane_id"]),
        (&json!("ACTIVE"), &json!("ship M2"), &json!("2"))
    );
    assert!(s["curator_pane_id"].is_null());
    w.update_session(&[
        (SessionField::Status, set("SUSPENDED")),
        (SessionField::SuspendedAt, set("2026-05-04")),
    ])
    .unwrap();
    let s = w.get_session().unwrap().unwrap();
    assert_eq!(s["status"], "SUSPENDED");
    assert_eq!(s["objective"], "ship M2"); // untouched
    assert_eq!(s["suspended_at"], "2026-05-04");
}

#[test]
fn update_session_clear_nulls_a_column_and_an_empty_patch_changes_nothing_else() {
    let (_dir, w) = fresh();
    w.update_session(&[
        (SessionField::Status, set("ACTIVE")),
        (SessionField::Objective, set("ship")),
    ])
    .unwrap();
    w.update_session(&[(SessionField::Objective, Patch::Clear)])
        .unwrap();
    let s = w.get_session().unwrap().unwrap();
    assert_eq!(s["status"], "ACTIVE");
    assert!(s["objective"].is_null());
    w.update_session(&[]).unwrap();
    assert_eq!(w.get_session().unwrap().unwrap()["status"], "ACTIVE");
}

#[test]
fn a_second_session_row_and_a_bad_status_are_refused() {
    let (_dir, w) = fresh();
    assert!(
        w.conn()
            .execute(
                "INSERT INTO org_sessions (id, status) VALUES (2, 'ACTIVE')",
                []
            )
            .is_err()
    );
    assert!(
        w.update_session(&[(SessionField::Status, set("bogus"))])
            .is_err()
    );
}

// ---- worker folders --------------------------------------------------------------------------

#[test]
fn register_worker_dir_is_idempotent() {
    let (_dir, w) = fresh();
    let a = w
        .register_worker_dir(&WorkerDir {
            abs_path: "/x/dir-a",
            layout: Some("flat"),
            ..Default::default()
        })
        .unwrap();
    let b = w
        .register_worker_dir(&WorkerDir {
            abs_path: "/x/dir-a",
            current_branch: Some("main"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(
        text(
            w.conn(),
            "SELECT current_branch FROM worker_dirs WHERE abs_path = '/x/dir-a'"
        ),
        "main"
    );
    assert_eq!(
        text(
            w.conn(),
            "SELECT lifecycle FROM worker_dirs WHERE abs_path = '/x/dir-a'"
        ),
        "active"
    );
}

#[test]
fn register_worker_dir_keeps_what_the_ping_did_not_say() {
    let (_dir, w) = fresh();
    w.register_worker_dir(&WorkerDir {
        abs_path: "/x/keep",
        is_git_repo: Some(true),
        is_worktree: Some(true),
        origin_url: Some("https://example.invalid/x.git"),
        current_branch: Some("main"),
        ..Default::default()
    })
    .unwrap();
    w.register_worker_dir(&WorkerDir {
        abs_path: "/x/keep",
        current_branch: Some("feature/x"),
        ..Default::default()
    })
    .unwrap();
    let c = w.conn();
    let q = |col: &str| {
        text(
            c,
            &format!("SELECT CAST({col} AS TEXT) FROM worker_dirs WHERE abs_path = '/x/keep'"),
        )
    };
    assert_eq!(
        (q("is_git_repo").as_str(), q("is_worktree").as_str()),
        ("1", "1")
    );
    assert_eq!(q("origin_url"), "https://example.invalid/x.git");
    assert_eq!(q("current_branch"), "feature/x");
}

#[test]
fn worker_dir_lifecycle_and_removal_keep_the_run() {
    let (_dir, w) = fresh();
    w.register_worker_dir(&WorkerDir {
        abs_path: "/x/dir-b",
        ..Default::default()
    })
    .unwrap();
    w.update_worker_dir_lifecycle("/x/dir-b", "archived")
        .unwrap();
    assert_eq!(
        count(
            w.conn(),
            "SELECT archived FROM worker_dirs WHERE abs_path = '/x/dir-b'"
        ),
        1
    );
    w.upsert_run(&RunUpsert {
        task_id: "t",
        project_slug: "p",
        worker_dir_abs_path: Some("/x/dir-b"),
        ..Default::default()
    })
    .unwrap();
    w.remove_worker_dir("/x/dir-b").unwrap();
    assert_eq!(count(w.conn(), "SELECT COUNT(*) FROM worker_dirs"), 0);
    assert_eq!(
        count(
            w.conn(),
            "SELECT COUNT(*) FROM runs WHERE task_id = 't' AND worker_dir_id IS NULL"
        ),
        1
    );
}

// ---- projects and runs -----------------------------------------------------------------------

#[test]
fn ensure_project_returns_the_same_id_and_keeps_the_name() {
    let (_dir, w) = fresh();
    let a = w.ensure_project("proj", Some("Proj"), None).unwrap();
    let b = w
        .ensure_project("proj", None, Some("https://example.invalid/p.git"))
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(
        text(
            w.conn(),
            "SELECT display_name FROM projects WHERE slug = 'proj'"
        ),
        "Proj"
    );
    assert_eq!(
        text(
            w.conn(),
            "SELECT origin_url FROM projects WHERE slug = 'proj'"
        ),
        "https://example.invalid/p.git"
    );
}

#[test]
fn upsert_run_creates_then_updates_without_clearing() {
    let (_dir, w) = fresh();
    w.register_worker_dir(&WorkerDir {
        abs_path: "/x/wd1",
        ..Default::default()
    })
    .unwrap();
    let id = w
        .upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "proj-a",
            pattern: Some("B"),
            title: Some("task one"),
            status: Some("in_use"),
            worker_dir_abs_path: Some("/x/wd1"),
            issue_refs: Some(&["#100", "#101"]),
            ..Default::default()
        })
        .unwrap();
    assert!(id > 0);
    let c = w.conn();
    assert_eq!(
        text(c, "SELECT title FROM runs WHERE task_id = 't1'"),
        "task one"
    );
    assert_eq!(
        text(c, "SELECT issue_refs FROM runs WHERE task_id = 't1'"),
        r##"["#100","#101"]"##
    );
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM runs WHERE worker_dir_id IS NOT NULL"
        ),
        1
    );

    let again = w
        .upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "proj-a",
            status: Some("review"),
            pr_url: Some("https://x/pr/1"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(again, id);
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: "proj-a",
        status: Some("completed"),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        text(c, "SELECT status FROM runs WHERE task_id = 't1'"),
        "completed"
    );
    assert_eq!(
        text(c, "SELECT pr_url FROM runs WHERE task_id = 't1'"),
        "https://x/pr/1"
    );
    // The title, the worker folder and the issues were not named again and are kept.
    assert_eq!(
        text(c, "SELECT title FROM runs WHERE task_id = 't1'"),
        "task one"
    );
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM runs WHERE worker_dir_id IS NOT NULL"
        ),
        1
    );
}

#[test]
fn upsert_run_defaults_on_insert_and_keeps_fields_on_update() {
    let (_dir, w) = fresh();
    w.upsert_run(&RunUpsert {
        task_id: "t-def",
        project_slug: "p",
        ..Default::default()
    })
    .unwrap();
    let c = w.conn();
    let row = |col: &str| {
        text(
            c,
            &format!("SELECT {col} FROM runs WHERE task_id = 't-def'"),
        )
    };
    assert_eq!(
        (row("pattern").as_str(), row("title").as_str()),
        ("B", "t-def")
    );
    assert_eq!(
        (row("status").as_str(), row("verification").as_str()),
        ("in_use", "standard")
    );

    w.upsert_run(&RunUpsert {
        task_id: "t-keep",
        project_slug: "p",
        pattern: Some("B"),
        title: Some("original"),
        verification: Some("deep"),
        ..Default::default()
    })
    .unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t-keep",
        project_slug: "p",
        status: Some("review"),
        ..Default::default()
    })
    .unwrap();
    let keep = |col: &str| {
        text(
            c,
            &format!("SELECT {col} FROM runs WHERE task_id = 't-keep'"),
        )
    };
    assert_eq!(
        (
            keep("title").as_str(),
            keep("verification").as_str(),
            keep("status").as_str()
        ),
        ("original", "deep", "review")
    );
}

#[test]
fn upsert_run_refuses_a_value_the_schema_does_not_allow() {
    let (_dir, w) = fresh();
    let bad = RunUpsert {
        task_id: "t",
        project_slug: "p",
        status: Some("flying"),
        ..Default::default()
    };
    assert!(w.upsert_run(&bad).is_err());
}

#[test]
fn update_run_status_keeps_what_it_is_not_given() {
    let (_dir, w) = fresh();
    w.upsert_run(&RunUpsert {
        task_id: "t2",
        project_slug: "p",
        pattern: Some("C"),
        ..Default::default()
    })
    .unwrap();
    assert!(
        w.update_run_status(
            "t2",
            "completed",
            Some("2026-05-04T00:00:00.000Z"),
            Some("ok")
        )
        .unwrap()
    );
    assert!(w.update_run_status("t2", "failed", None, None).unwrap());
    let c = w.conn();
    assert_eq!(
        text(c, "SELECT status FROM runs WHERE task_id = 't2'"),
        "failed"
    );
    assert_eq!(
        text(c, "SELECT completed_at FROM runs WHERE task_id = 't2'"),
        "2026-05-04T00:00:00.000Z"
    );
    assert_eq!(
        text(c, "SELECT outcome_note FROM runs WHERE task_id = 't2'"),
        "ok"
    );
    assert!(
        !w.update_run_status("no-such-task", "failed", None, None)
            .unwrap()
    );
}

// ---- events ----------------------------------------------------------------------------------

#[test]
fn append_event_resolves_the_run_and_its_project() {
    let (_dir, w) = fresh();
    w.upsert_run(&RunUpsert {
        task_id: "t3",
        project_slug: "proj-c",
        pattern: Some("A"),
        ..Default::default()
    })
    .unwrap();
    let payload = json!({"task": "t3", "n": 1});
    let id = w
        .append_event(&NewEvent {
            kind: "dispatch",
            actor: Some("dispatcher"),
            payload: Some(&payload),
            run_task_id: Some("t3"),
            ..Default::default()
        })
        .unwrap();
    assert!(id > 0);
    let c = w.conn();
    assert_eq!(
        count(
            c,
            &format!(
                "SELECT COUNT(*) FROM events WHERE id = {id} AND run_id IS NOT NULL AND project_id IS NOT NULL"
            )
        ),
        1
    );
    assert_eq!(
        text(
            c,
            &format!("SELECT payload_json FROM events WHERE id = {id}")
        ),
        r#"{"n":1,"task":"t3"}"#
    );
}

#[test]
fn append_event_finds_the_project_and_workstream_by_name() {
    let (_dir, w) = fresh();
    let p = w.ensure_project("proj-w", None, None).unwrap();
    w.conn()
        .execute(
            "INSERT INTO workstreams (project_id, slug, display_name) VALUES (?1, 'ws', 'WS')",
            [p],
        )
        .unwrap();
    let id = w
        .append_event(&NewEvent {
            kind: "x",
            project_slug: Some("proj-w"),
            workstream_slug: Some("ws"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        count(
            w.conn(),
            &format!(
                "SELECT COUNT(*) FROM events WHERE id = {id} AND project_id = {p} AND workstream_id IS NOT NULL"
            )
        ),
        1
    );
}

#[test]
fn append_event_defaults_the_payload_and_takes_an_explicit_time() {
    let (_dir, w) = fresh();
    w.append_event(&NewEvent {
        kind: "ping",
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        text(
            w.conn(),
            "SELECT payload_json FROM events ORDER BY id DESC LIMIT 1"
        ),
        "{}"
    );
    w.append_event(&NewEvent {
        kind: "x",
        occurred_at: Some("2026-05-04T00:00:00.000Z"),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        text(
            w.conn(),
            "SELECT occurred_at FROM events ORDER BY id DESC LIMIT 1"
        ),
        "2026-05-04T00:00:00.000Z"
    );
    // Without a time, SQLite stamps one.
    w.append_event(&NewEvent {
        kind: "y",
        ..Default::default()
    })
    .unwrap();
    assert!(
        text(
            w.conn(),
            "SELECT occurred_at FROM events ORDER BY id DESC LIMIT 1"
        )
        .ends_with('Z')
    );
}

// ---- transactions ----------------------------------------------------------------------------

fn dir_count(c: &Connection, path: &str) -> i64 {
    count(
        c,
        &format!("SELECT COUNT(*) FROM worker_dirs WHERE abs_path = '{path}'"),
    )
}

fn register(w: &StateWriter, path: &str) -> makiba::db::Result<i64> {
    w.register_worker_dir(&WorkerDir {
        abs_path: path,
        ..Default::default()
    })
}

#[test]
fn rollback_undoes_a_bracketed_write_and_commit_keeps_it() {
    let (_dir, w) = fresh();
    w.begin().unwrap();
    register(&w, "/x/gone").unwrap();
    w.rollback().unwrap();
    assert_eq!(dir_count(w.conn(), "/x/gone"), 0);
    w.begin().unwrap();
    w.begin().unwrap(); // a second begin is a no-op
    register(&w, "/x/kept").unwrap();
    w.commit().unwrap();
    assert!(w.conn().is_autocommit());
    assert_eq!(dir_count(w.conn(), "/x/kept"), 1);
}

#[test]
fn a_write_outside_a_transaction_is_committed_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let w = StateWriter::open(&path).unwrap();
    register(&w, "/x/persist").unwrap();
    drop(w);
    let again = db::open(&path).unwrap();
    assert_eq!(dir_count(&again, "/x/persist"), 1);
}

#[test]
fn transaction_commits_on_ok_and_rolls_back_on_err() {
    let (_dir, w) = fresh();
    w.transaction(|t| register(t, "/x/in-tx")).unwrap();
    assert_eq!(dir_count(w.conn(), "/x/in-tx"), 1);
    let out: makiba::db::Result<()> = w.transaction(|t| {
        register(t, "/x/should-rollback")?;
        t.conn()
            .execute("INSERT INTO org_sessions (id) VALUES (2)", [])?; // refused
        Ok(())
    });
    assert!(out.is_err());
    assert_eq!(dir_count(w.conn(), "/x/should-rollback"), 0);
    assert!(w.conn().is_autocommit());
}

#[test]
fn transaction_rolls_back_on_a_panic_the_caller_catches() {
    let (_dir, w) = fresh();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = w.transaction(|t| {
            register(t, "/x/panic")?;
            panic!("boom");
            #[allow(unreachable_code)]
            Ok(())
        });
    }));
    assert!(caught.is_err());
    assert!(w.conn().is_autocommit());
    assert_eq!(dir_count(w.conn(), "/x/panic"), 0);
    register(&w, "/x/after").unwrap(); // the same writer is still usable
    assert_eq!(dir_count(w.conn(), "/x/after"), 1);
}

// ---- the delivery ledger ---------------------------------------------------------------------

fn seed_event(w: &StateWriter, kind: &str) -> i64 {
    w.conn()
        .execute(
            "INSERT INTO events (kind, payload_json) VALUES (?1, '{\"pr\": 1}')",
            [kind],
        )
        .unwrap();
    w.conn().last_insert_rowid()
}

fn seed_event_at(w: &StateWriter, kind: &str, at: &str) -> i64 {
    w.conn()
        .execute(
            "INSERT INTO events (kind, occurred_at) VALUES (?1, ?2)",
            [kind, at],
        )
        .unwrap();
    w.conn().last_insert_rowid()
}

fn ids(rows: &[makiba::db::Row]) -> Vec<i64> {
    rows.iter().map(|r| r["id"].as_i64().unwrap()).collect()
}

const CI: &[&str] = &["ci_completed"];

#[test]
fn a_delivered_event_is_excluded_for_that_recipient_only() {
    let (_dir, w) = fresh();
    let e = seed_event(&w, "ci_completed");
    let pend = w.pending_deliveries("secretary", CI, None, None).unwrap();
    assert_eq!(ids(&pend), [e]);
    assert_eq!(pend[0]["attempt"], 0);
    assert_eq!(w.begin_delivery_attempt(e, "secretary").unwrap(), 1);
    w.mark_delivered(e, "secretary").unwrap();
    assert!(
        w.pending_deliveries("secretary", CI, None, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ids(&w.pending_deliveries("human", CI, None, None).unwrap()),
        [e]
    );
}

#[test]
fn the_idempotency_key_counts_attempts_in_one_row() {
    let (_dir, w) = fresh();
    let e = seed_event(&w, "ci_completed");
    w.begin_delivery_attempt(e, "secretary").unwrap();
    assert_eq!(w.begin_delivery_attempt(e, "secretary").unwrap(), 2);
    assert_eq!(
        count(
            w.conn(),
            &format!("SELECT COUNT(*) FROM event_deliveries WHERE source_event_id = {e}")
        ),
        1
    );
}

#[test]
fn failed_is_retried_and_delivered_is_terminal() {
    let (_dir, w) = fresh();
    let e = seed_event(&w, "ci_completed");
    w.begin_delivery_attempt(e, "secretary").unwrap();
    w.mark_delivery_failed(e, "secretary", "broker unreachable")
        .unwrap();
    assert_eq!(
        ids(&w.pending_deliveries("secretary", CI, None, None).unwrap()),
        [e]
    );
    let q = |col: &str| {
        text(
            w.conn(),
            &format!("SELECT {col} FROM event_deliveries WHERE source_event_id = {e}"),
        )
    };
    assert_eq!(
        (q("status").as_str(), q("last_error").as_str()),
        ("failed", "broker unreachable")
    );
    w.mark_delivered(e, "secretary").unwrap();
    assert!(
        w.pending_deliveries("secretary", CI, None, None)
            .unwrap()
            .is_empty()
    );
    w.mark_delivery_failed(e, "secretary", "late error")
        .unwrap();
    assert_eq!(q("status"), "delivered"); // a late failure does not undo a delivery
}

#[test]
fn since_and_kinds_filter_what_was_never_attempted() {
    let (_dir, w) = fresh();
    let old = seed_event_at(&w, "ci_completed", "2020-01-01T00:00:00.000Z");
    let new = seed_event_at(&w, "ci_completed", "2030-01-01T00:00:00.000Z");
    let other = seed_event(&w, "worker_reported");
    let since = Some("2025-01-01T00:00:00.000Z");
    assert_eq!(
        ids(&w.pending_deliveries("secretary", CI, since, None).unwrap()),
        [new]
    );
    let all = ids(&w.pending_deliveries("secretary", CI, None, None).unwrap());
    assert_eq!(all, [old, new]);
    assert!(!all.contains(&other));
    assert_eq!(
        ids(&w
            .pending_deliveries("secretary", CI, None, Some(1))
            .unwrap()),
        [old]
    );
    assert!(
        w.pending_deliveries("secretary", &[], None, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn since_never_abandons_an_attempted_undelivered_event() {
    let (_dir, w) = fresh();
    let old = seed_event_at(&w, "ci_completed", "2000-01-01T00:00:00.000Z");
    w.begin_delivery_attempt(old, "secretary").unwrap();
    w.mark_delivery_failed(old, "secretary", "down").unwrap();
    let never = seed_event_at(&w, "ci_completed", "2000-01-01T00:00:00.000Z");
    let since = Some("2025-01-01T00:00:00.000Z");
    let got = ids(&w.pending_deliveries("secretary", CI, since, None).unwrap());
    assert!(got.contains(&old) && !got.contains(&never));
    w.mark_delivered(old, "secretary").unwrap();
    assert!(!ids(&w.pending_deliveries("secretary", CI, since, None).unwrap()).contains(&old));
}

#[test]
fn mark_delivered_without_a_prior_attempt() {
    let (_dir, w) = fresh();
    let e = seed_event(&w, "ci_completed");
    w.mark_delivered(e, "secretary").unwrap();
    assert_eq!(
        text(
            w.conn(),
            &format!("SELECT status FROM event_deliveries WHERE source_event_id = {e}")
        ),
        "delivered"
    );
    assert_eq!(
        count(
            w.conn(),
            &format!(
                "SELECT COUNT(*) FROM event_deliveries WHERE source_event_id = {e} AND delivered_at IS NOT NULL"
            )
        ),
        1
    );
}
