//! `state.db`: the records of makiba (spec section 5).
//!
//! The SQL is carried from claude-org-ja and kept as SQL: `schema.sql` is v1 to v3, `schema_v4.sql`
//! is the tables of spec section 5. Both are embedded here and are the only place the DDL is written.
//! `schema_v5.sql` adds the launch identity to `agents`, `schema_v6.sql` the triggers that keep a
//! `worker_spawn_verified` to its evidence, `schema_v7.sql` the launch without a run, `schema_v8.sql` the ranges of the belt, `schema_v9.sql` their budgets, `schema_v10.sql`
//! the status `parked` of a run. `open` opens a database with the project's pragmas and `migrate`
//! brings it to v10.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::Value;

pub mod queries;
pub mod writer;

pub use writer::StateWriter;

/// v1 to v3: projects, runs, events, the delivery ledger, the session row, `schema_migrations`.
pub const SCHEMA: &str = include_str!("schema.sql");
/// v4: agents, commands, run_launch, messages, pending, pr_watches, dispatcher.
pub const SCHEMA_V4: &str = include_str!("schema_v4.sql");
/// v5: `agents.spawn_id`.
pub const SCHEMA_V5: &str = include_str!("schema_v5.sql");
/// v6: `worker_spawn_verified` only with the id of a working status or a turn of its terminal.
pub const SCHEMA_V6: &str = include_str!("schema_v6.sql");
/// v7: `run_launch.role` and a nullable `run_launch.run_id`, a launch without a run.
pub const SCHEMA_V7: &str = include_str!("schema_v7.sql");
/// v8: `ranges`, `range_candidates` and `runs.range_id`, the belt (Issue 257).
pub const SCHEMA_V8: &str = include_str!("schema_v8.sql");
/// v9: `ranges.budget_hours`, `ranges.budget_fixes` and the ended reason `budget_hours` (Issue 282).
pub const SCHEMA_V9: &str = include_str!("schema_v9.sql");
/// v10: `runs.status` `parked` (Issue 283). The script leaves its transaction open: `migrate`
/// checks the foreign keys inside it before it commits.
pub const SCHEMA_V10: &str = include_str!("schema_v10.sql");

/// A row as a map of column name to value (what the carried Python returned as a dict).
pub type Row = serde_json::Map<String, Value>;

/// What a database call can fail with.
#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    /// A call that may only run outside a transaction was made inside one.
    InTransaction(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "sqlite: {e}"),
            Error::InTransaction(what) => write!(f, "{what} must be called outside a transaction"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::InTransaction(_) => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

/// The two pragmas every connection must have; also re-asserted on a connection handed in.
pub(crate) fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    Ok(())
}

/// Open `path` with `foreign_keys` on, a 5 s busy timeout and WAL (readers and the writer do not block each other).
/// A ledger already in WAL is only read: setting the mode takes the write lock (Issue 288).
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    }
    Ok(conn)
}

/// Whether `e` is SQLite's "database is locked" (`SQLITE_BUSY` or `SQLITE_LOCKED`): another
/// connection holds the lock asked for.
pub fn is_busy(e: &Error) -> bool {
    matches!(
        e,
        Error::Sqlite(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// An in-memory database with the same pragmas (WAL does not apply to it).
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    Ok(conn)
}

fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// The `CREATE TABLE` statement of `table` and its indexes, cut out of `schema.sql` so the DDL is not written twice.
fn ddl_of(table: &str) -> String {
    let head = format!("CREATE TABLE {table} (");
    let start = SCHEMA
        .find(&head)
        .unwrap_or_else(|| panic!("schema.sql has no table {table}"));
    let len = SCHEMA[start..]
        .find("\n);")
        .expect("a CREATE TABLE ends with `);`")
        + 3;
    let mut ddl = SCHEMA[start..start + len].to_string();
    let on = format!(" ON {table}(");
    for line in SCHEMA
        .lines()
        .filter(|l| l.starts_with("CREATE INDEX") && l.contains(&on))
    {
        ddl.push('\n');
        ddl.push_str(line);
    }
    ddl
}

/// Whether every step of `migrate` is already done: versions 1 to 9 recorded, the session row and
/// the delivery ledger present. It only reads, so a current ledger is opened without the write
/// lock (Issue 288).
fn is_current(conn: &Connection) -> Result<bool> {
    for table in ["schema_migrations", "org_sessions", "event_deliveries"] {
        if !has_table(conn, table)? {
            return Ok(false);
        }
    }
    let (versions, session): (i64, i64) = conn.query_row(
        "SELECT (SELECT COUNT(DISTINCT version) FROM schema_migrations WHERE version BETWEEN 1 AND 10),
                (SELECT COUNT(*) FROM org_sessions WHERE id = 1)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(versions == 10 && session == 1)
}

/// Bring a database to v9: apply v3 to an empty one, upgrade an older one in place, then add v4 to v9.
/// Idempotent. Refuses to run inside a transaction, where it could not leave the caller's writes alone.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    if !conn.is_autocommit() {
        return Err(Error::InTransaction("migrate"));
    }
    if is_current(conn)? {
        return Ok(());
    }
    if !has_table(conn, "schema_migrations")? {
        conn.execute_batch(SCHEMA)?;
    }
    // Databases made before org_sessions (version 2) and before event_deliveries (version 3).
    // IMMEDIATE: it reads before it writes (see `StateWriter::begin`).
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if !has_table(&tx, "org_sessions")? {
        tx.execute_batch(&ddl_of("org_sessions"))?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO org_sessions (id, status, last_writer_at) \
         VALUES (1, 'IDLE', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        [],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, description) \
         VALUES (2, 'M2: org_sessions singleton (Issue #267)')",
        [],
    )?;
    if !has_table(&tx, "event_deliveries")? {
        tx.execute_batch(&ddl_of("event_deliveries"))?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, description) \
         VALUES (3, 'event_deliveries outbox delivery ledger (Refs #653 #658)')",
        [],
    )?;
    tx.commit()?;
    // v4. The script opens and commits its own transaction. SQLite rolls back only the failing
    // statement, so a failure in the middle must be rolled back by hand to leave v3 as it was.
    let done: i64 = conn.query_row(
        "SELECT COUNT(*) FROM schema_migrations WHERE version = 4",
        [],
        |r| r.get(0),
    )?;
    if done == 0
        && let Err(e) = conn.execute_batch(SCHEMA_V4)
    {
        if !conn.is_autocommit() {
            let _ = conn.execute_batch("ROLLBACK");
        }
        return Err(e.into());
    }
    // v5 to v9, the same way. v9 turns the foreign keys off around its transaction (it makes
    // `ranges` again); they are turned on again here whether it ran, failed or was not needed.
    for (version, script) in [
        (5, SCHEMA_V5),
        (6, SCHEMA_V6),
        (7, SCHEMA_V7),
        (8, SCHEMA_V8),
        (9, SCHEMA_V9),
    ] {
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
            [version],
            |r| r.get(0),
        )?;
        if done == 0
            && let Err(e) = conn.execute_batch(script)
        {
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
            let _ = conn.pragma_update(None, "foreign_keys", true);
            return Err(e.into());
        }
    }
    // v10 makes `runs` again with the foreign keys off; its script leaves the transaction open so
    // that they are checked before it commits (Issue 283): a row that breaks one rolls it back.
    let done: i64 = conn.query_row(
        "SELECT COUNT(*) FROM schema_migrations WHERE version = 10",
        [],
        |r| r.get(0),
    )?;
    // A `runs` with no status CHECK (a database made before the carried schema, as an M1 one)
    // already takes `parked`: it is left as it is and only the version is recorded.
    let runs_sql: String = conn.query_row(
        "SELECT coalesce((SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'runs'), '')",
        [],
        |r| r.get(0),
    )?;
    let rebuild = runs_sql.contains("'suspended'") && !runs_sql.contains("'parked'");
    if done == 0 && !rebuild {
        conn.execute(
            "INSERT INTO schema_migrations (version, description)
             VALUES (10, 'makiba: runs.status parked (the belt step 3, Issue 283)')",
            [],
        )?;
    }
    if done == 0 && rebuild {
        let checked = conn
            .execute_batch(SCHEMA_V10)
            .and_then(|()| {
                conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .and_then(|broken| match broken {
                0 => conn.execute_batch("COMMIT"),
                n => Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                    Some(format!(
                        "migration v10: {n} rows break a foreign key; nothing was changed"
                    )),
                )),
            });
        if let Err(e) = checked {
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
            let _ = conn.pragma_update(None, "foreign_keys", true);
            return Err(e.into());
        }
    }
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Read every row of a query as maps. A blob column is read as null (the records hold none).
pub(crate) fn query_rows(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<Row>> {
    use rusqlite::types::ValueRef;
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().map(|n| n.to_string()).collect();
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut map = Row::new();
        for (i, name) in names.iter().enumerate() {
            let v = match row.get_ref(i)? {
                ValueRef::Integer(n) => Value::from(n),
                ValueRef::Real(f) => Value::from(f),
                ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Null | ValueRef::Blob(_) => Value::Null,
            };
            map.insert(name.clone(), v);
        }
        out.push(map);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ledger complete at v8 (every step of v8's `migrate` done) is not current: the fast path
    /// of Issue 288 does not skip it, and `migrate` brings it to v9, after which it is current.
    #[test]
    fn a_complete_v8_ledger_is_not_current_and_is_migrated_to_v9() {
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT OR IGNORE INTO org_sessions (id, status, last_writer_at)
             VALUES (1, 'IDLE', strftime('%Y-%m-%dT%H:%M:%fZ','now'));
             INSERT OR IGNORE INTO schema_migrations (version, description) VALUES (2, 'v2'), (3, 'v3');",
        )
        .unwrap();
        for script in [SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8] {
            conn.execute_batch(script).unwrap();
        }
        let n = |c: &Connection, sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            n(
                &conn,
                "SELECT COUNT(DISTINCT version) FROM schema_migrations WHERE version BETWEEN 1 AND 8"
            ),
            8,
            "the ledger is complete at v8"
        );
        assert!(!is_current(&conn).unwrap());
        migrate(&mut conn).unwrap();
        assert_eq!(n(&conn, "SELECT MAX(version) FROM schema_migrations"), 10);
        assert_eq!(
            n(
                &conn,
                "SELECT count(*) FROM pragma_table_info('ranges') WHERE name = 'budget_fixes'"
            ),
            1
        );
        assert!(is_current(&conn).unwrap());
    }

    /// v9 makes `ranges` again: a range of a v8 database keeps its fields, its candidates and its
    /// runs, takes the default `budget_fixes`, may end `budget_hours`, and the foreign keys hold
    /// afterwards as before.
    #[test]
    fn v9_keeps_the_ranges_their_candidates_and_their_runs() {
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for script in [SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8] {
            conn.execute_batch(script).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO projects (id, slug, display_name) VALUES (1, 'p', 'p');
             INSERT INTO ranges (id, project_id, issues_json, at_once, budget_tasks, brief_head,
                                 proposed_by, status, approval_mode, approved_by)
             VALUES (4, 1, '[7]', 2, 5, 'head', 'secretary', 'active', 'acceptance', 'person');
             INSERT INTO ranges (id, project_id, at_once, budget_tasks, brief_head, proposed_by,
                                 replaces)
             VALUES (5, 1, 1, 1, 'next', 'secretary', 4);
             INSERT INTO range_candidates (range_id, read_id, position, issue, title, body, admitted)
             VALUES (4, 1, 0, 7, 'Seven', 'b', 1);
             INSERT INTO runs (task_id, project_id, pattern, title, range_id)
             VALUES ('r4-7', 1, 'A', 'Seven', 4);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let n = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(n("SELECT MAX(version) FROM schema_migrations"), 10);
        assert_eq!(n("PRAGMA foreign_keys"), 1);
        assert_eq!(n("SELECT count(*) FROM pragma_foreign_key_check"), 0);
        let row: (String, i64, i64, String, String, Option<f64>, i64) = conn
            .query_row(
                "SELECT issues_json, at_once, budget_tasks, brief_head, approval_mode,
                        budget_hours, budget_fixes FROM ranges WHERE id = 4",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "[7]".into(),
                2,
                5,
                "head".into(),
                "acceptance".into(),
                None,
                3
            )
        );
        assert_eq!(n("SELECT replaces FROM ranges WHERE id = 5"), 4);
        assert_eq!(
            n("SELECT count(*) FROM range_candidates WHERE range_id = 4"),
            1
        );
        assert_eq!(n("SELECT range_id FROM runs WHERE task_id = 'r4-7'"), 4);
        // The next range takes the next id, and the children still name `ranges`.
        conn.execute_batch(
            "INSERT INTO ranges (project_id, at_once, budget_tasks, brief_head, proposed_by)
             VALUES (1, 1, 1, 'h', 'secretary');
             UPDATE ranges SET status = 'ended', ended_reason = 'budget_hours' WHERE id = 4;",
        )
        .unwrap();
        assert_eq!(n("SELECT MAX(id) FROM ranges"), 6);
        assert!(
            conn.execute(
                "INSERT INTO range_candidates (range_id, read_id, position, issue, title, body, admitted)
                 VALUES (99, 1, 0, 1, 't', 'b', 1)",
                [],
            )
            .is_err(),
            "a candidate of no range is refused"
        );
        assert!(
            conn.execute("UPDATE runs SET range_id = 99 WHERE task_id = 'r4-7'", [])
                .is_err(),
            "a run of no range is refused"
        );
    }

    /// v10 makes `runs` again (Issue 283): a v9 database keeps its runs, their launches, keys,
    /// entries, messages and events (none deleted or nulled by the drop), a run may be `parked`,
    /// the foreign keys hold as before, and the fast path takes v10 as current.
    #[test]
    fn v10_keeps_every_run_and_what_names_it_and_allows_parked() {
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for script in [
            SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8, SCHEMA_V9,
        ] {
            conn.execute_batch(script).unwrap();
        }
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn.execute_batch(
            "INSERT INTO projects (id, slug, display_name) VALUES (1, 'p', 'p');
             INSERT INTO ranges (id, project_id, at_once, budget_tasks, brief_head, proposed_by)
             VALUES (4, 1, 1, 1, 'head', 'secretary');
             INSERT INTO runs (id, task_id, project_id, pattern, title, status, range_id, issue_refs)
             VALUES (7, 't7', 1, 'A', 'Seven', 'in_use', 4, '[\"p#7\"]');
             INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
             VALUES (7, 's7', 'fake', 'b', '/w', 'verified', 'conv');
             INSERT INTO agents (agent_id, role, run_id, key_sha256, spawn_id)
             VALUES ('w-t7', 'worker', 7, 'h7', 's7');
             INSERT INTO pending (run_id, kind, question, status) VALUES (7, 'question', 'q', 'escalated');
             INSERT INTO events (kind, run_id) VALUES ('x', 7);
             INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
             VALUES ((SELECT max(id) FROM events), 'secretary', 'w-t7', 'message', 7, 'hi');",
        )
        .unwrap();
        assert!(!is_current(&conn).unwrap());
        assert!(
            conn.execute("UPDATE runs SET status = 'parked' WHERE id = 7", [])
                .is_err(),
            "v9 refuses parked"
        );
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        assert!(is_current(&conn).unwrap());
        let n = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(n("SELECT MAX(version) FROM schema_migrations"), 10);
        assert_eq!(n("PRAGMA foreign_keys"), 1);
        assert_eq!(n("SELECT count(*) FROM pragma_foreign_key_check"), 0);
        assert_eq!(n("SELECT count(*) FROM run_launch WHERE run_id = 7"), 1);
        assert_eq!(n("SELECT count(*) FROM agents WHERE run_id = 7"), 1);
        assert_eq!(n("SELECT count(*) FROM pending WHERE run_id = 7"), 1);
        assert_eq!(n("SELECT count(*) FROM events WHERE run_id = 7"), 1);
        assert_eq!(n("SELECT count(*) FROM messages WHERE run_id = 7"), 1);
        assert_eq!(n("SELECT range_id FROM runs WHERE task_id = 't7'"), 4);
        assert_eq!(
            n(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = 'runs'
               AND name LIKE 'idx_runs_%'"
            ),
            5
        );
        conn.execute("UPDATE runs SET status = 'parked' WHERE id = 7", [])
            .unwrap();
        assert!(
            conn.execute("UPDATE runs SET status = 'asleep' WHERE id = 7", [])
                .is_err()
        );
        // The children still name `runs`: a launch of no run is refused, a delete cascades.
        assert!(
            conn.execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree)
                 VALUES (99, 's99', 'fake', 'b', '/w')",
                [],
            )
            .is_err()
        );
    }

    /// A v9 database with a row that already breaks a foreign key is left at v9.
    #[test]
    fn v10_with_a_broken_foreign_key_changes_nothing() {
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for script in [
            SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8, SCHEMA_V9,
        ] {
            conn.execute_batch(script).unwrap();
        }
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute_batch(
            "INSERT INTO runs (id, task_id, project_id, pattern, title) VALUES (1, 't', 42, 'A', 'x');",
        )
        .unwrap();
        assert!(migrate(&mut conn).is_err());
        let n = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(n("SELECT MAX(version) FROM schema_migrations"), 9);
        assert_eq!(n("PRAGMA foreign_keys"), 1);
        assert!(conn.is_autocommit());
    }
}
