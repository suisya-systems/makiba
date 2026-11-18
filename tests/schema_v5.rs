//! Migration v5: a database that is at v4 gets `agents.spawn_id`, and its rows keep their keys.

use makiba::db;
use rusqlite::Connection;

#[test]
fn a_v4_database_gains_the_launch_identity_column_and_keeps_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = Connection::open(dir.path().join("state.db")).unwrap();
    db::migrate(&mut conn).unwrap();
    // Back to v4: no column, no version row, one key issued before the column existed.
    conn.execute_batch(
        "ALTER TABLE agents DROP COLUMN spawn_id;
         DELETE FROM schema_migrations WHERE version = 5;
         INSERT INTO agents (agent_id, role, key_sha256) VALUES ('secretary', 'secretary', 'k');",
    )
    .unwrap();
    db::migrate(&mut conn).unwrap();
    let spawn: Option<String> = conn
        .query_row(
            "SELECT spawn_id FROM agents WHERE key_sha256 = 'k'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(spawn, None);
    let v5: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 5",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v5, 1);
}
