//! The reads over `state.db`: ported from claude-org-ja `test_queries.py`.
//! Not carried with the code: the resume briefing and the organization summary.

use makiba::db::queries::{
    ACTIVE_EXECUTION_STATUSES, ACTIVE_RESERVATION_STATUSES, TERMINAL_STATUSES,
    USER_VISIBLE_STATUSES, get_run_by_task_id, get_session, list_active_runs, list_recent_events,
    list_reserved_runs, list_worker_dirs,
};
use makiba::db::{self, Row};
use rusqlite::Connection;

fn migrated() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = db::open(&dir.path().join("state.db")).unwrap();
    db::migrate(&mut conn).unwrap();
    (dir, conn)
}

/// One project, an active and an archived worker folder, one in_use run with no workstream, and
/// a suspend and a resume event.
fn seeded() -> (tempfile::TempDir, Connection) {
    let (dir, conn) = migrated();
    conn.execute_batch(
        "INSERT INTO projects (id, slug, display_name) VALUES (1, 'pj', 'pj');
         INSERT INTO worker_dirs (abs_path, layout, lifecycle) VALUES ('/w/active', 'flat', 'active');
         INSERT INTO worker_dirs (abs_path, layout, lifecycle) VALUES ('/w/old', 'flat', 'archived');
         INSERT INTO runs (task_id, project_id, pattern, title, status)
           VALUES ('t-null-ws', 1, 'B', 't-null-ws', 'in_use');
         INSERT INTO events (occurred_at, actor, kind, payload_json)
           VALUES ('2026-04-01T00:00:00Z', 'secretary', 'suspend', '{\"reason\":\"end of day\"}');
         INSERT INTO events (occurred_at, actor, kind, payload_json)
           VALUES ('2026-04-02T00:00:00Z', 'secretary', 'resume', '{}');",
    )
    .unwrap();
    (dir, conn)
}

fn task_ids(rows: &[Row]) -> Vec<&str> {
    rows.iter()
        .map(|r| r["task_id"].as_str().unwrap())
        .collect()
}

#[test]
fn an_empty_database_gives_empty_answers() {
    let (_dir, c) = migrated();
    assert!(list_active_runs(&c).unwrap().is_empty());
    assert!(list_reserved_runs(&c).unwrap().is_empty());
    assert!(list_worker_dirs(&c, None).unwrap().is_empty());
    assert!(list_worker_dirs(&c, Some("active")).unwrap().is_empty());
    assert!(list_recent_events(&c, 50).unwrap().is_empty());
    assert!(get_run_by_task_id(&c, "no-such-task").unwrap().is_none());
}

#[test]
fn the_session_row_is_there_after_migration() {
    let (_dir, c) = migrated();
    assert_eq!(get_session(&c).unwrap().unwrap()["status"], "IDLE");
}

#[test]
fn worker_dirs_can_be_filtered_by_lifecycle() {
    let (_dir, c) = seeded();
    let paths = |rows: Vec<Row>| -> Vec<String> {
        rows.iter()
            .map(|r| r["abs_path"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        paths(list_worker_dirs(&c, Some("active")).unwrap()),
        ["/w/active"]
    );
    assert_eq!(
        paths(list_worker_dirs(&c, Some("archived")).unwrap()),
        ["/w/old"]
    );
    assert_eq!(list_worker_dirs(&c, None).unwrap().len(), 2);
}

#[test]
fn an_active_run_without_a_workstream_is_listed_with_nulls() {
    let (_dir, c) = seeded();
    let rows = list_active_runs(&c).unwrap();
    assert_eq!(task_ids(&rows), ["t-null-ws"]);
    assert!(rows[0]["workstream_slug"].is_null());
    assert_eq!(rows[0]["project_slug"], "pj");
}

#[test]
fn a_run_is_found_by_task_id_with_its_project_and_folder() {
    let (_dir, c) = seeded();
    c.execute_batch(
        "UPDATE runs SET worker_dir_id = (SELECT id FROM worker_dirs WHERE abs_path = '/w/active')",
    )
    .unwrap();
    let run = get_run_by_task_id(&c, "t-null-ws").unwrap().unwrap();
    assert_eq!(run["task_id"], "t-null-ws");
    assert_eq!(run["project_name"], "pj");
    assert_eq!(run["worker_dir"], "/w/active");
    assert_eq!(run["worker_dir_lifecycle"], "active");
    assert!(run["pr_url"].is_null());
    assert_eq!(run["id"].as_i64(), Some(1));
}

#[test]
fn status_sets_match_the_contract() {
    // state-semantics-contract section 3.5: pinned so an edit cannot drift the resolver and the readers apart.
    assert_eq!(ACTIVE_RESERVATION_STATUSES, ["queued", "in_use", "review"]);
    assert_eq!(USER_VISIBLE_STATUSES, ["in_use", "review"]);
    assert_eq!(ACTIVE_EXECUTION_STATUSES, ["in_use"]);
    assert_eq!(TERMINAL_STATUSES, ["completed", "failed", "abandoned"]);
}

#[test]
fn reserved_runs_are_only_the_queued_and_are_not_user_visible() {
    let (_dir, c) = seeded();
    c.execute_batch(
        "INSERT INTO runs (task_id, project_id, pattern, title, status) VALUES ('t-queued', 1, 'A', 't-queued', 'queued');
         INSERT INTO runs (task_id, project_id, pattern, title, status) VALUES ('t-review', 1, 'A', 't-review', 'review');
         INSERT INTO runs (task_id, project_id, pattern, title, status) VALUES ('t-done', 1, 'A', 't-done', 'completed');",
    )
    .unwrap();
    let reserved = list_reserved_runs(&c).unwrap();
    assert_eq!(task_ids(&reserved), ["t-queued"]);
    assert_eq!(reserved[0]["status"], "queued");
    let active = list_active_runs(&c).unwrap();
    // newest first: the later insert comes first when dispatched_at ties
    assert_eq!(task_ids(&active), ["t-review", "t-null-ws"]);
}

#[test]
fn recent_events_are_newest_first_and_a_negative_limit_gives_none() {
    let (_dir, c) = seeded();
    let rows = list_recent_events(&c, 50).unwrap();
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["resume", "suspend"]);
    assert_eq!(rows[1]["payload_json"], "{\"reason\":\"end of day\"}");
    assert_eq!(list_recent_events(&c, 1).unwrap().len(), 1);
    assert!(list_recent_events(&c, -1).unwrap().is_empty());
    assert!(list_recent_events(&c, 0).unwrap().is_empty());
}
