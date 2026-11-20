//! Issue 36: a close fails the run's launch under way, so no pass starts a worker for a closed run.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::sync::Arc;

#[test]
fn close_abandon_fails_a_launch_under_way_and_leaves_a_finished_one() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    let w = StateWriter::open(&db).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t",
        project_slug: "p",
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    for (spawn, stage) in [("old", "verified"), ("new", "settings_ready")] {
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't'), ?1, 'document', 'fake', 'b',
                         '/nowhere', ?2)",
                [spawn, stage],
            )
            .unwrap();
    }
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
    door.handle(
        Some(&person),
        "close",
        json!({"task_id": "t", "abandon": true}),
        None,
        false,
    )
    .unwrap();

    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let stage = |spawn: &str| -> (String, Option<String>) {
        conn.query_row(
            "SELECT stage, failed_reason FROM run_launch WHERE spawn_id = ?1",
            [spawn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(stage("old"), ("verified".into(), None));
    assert_eq!(stage("new"), ("failed".into(), Some("makiba close".into())));
}
