//! `state.db`: the records of makiba (spec section 5).
//!
//! The SQL is carried from claude-org-ja and kept as SQL: `schema.sql` is v1 to v3, `schema_v4.sql`
//! is the tables of spec section 5. Both are embedded here and are the only place the DDL is written.
//! `schema_v5.sql` adds the launch identity to `agents`. `open` opens a database with the project's pragmas and `migrate` brings it to v5.

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
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    Ok(conn)
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

/// Bring a database to v5: apply v3 to an empty one, upgrade an older one in place, then add v4 and v5.
/// Idempotent. Refuses to run inside a transaction, where it could not leave the caller's writes alone.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    if !conn.is_autocommit() {
        return Err(Error::InTransaction("migrate"));
    }
    if !has_table(conn, "schema_migrations")? {
        conn.execute_batch(SCHEMA)?;
    }
    // Databases made before org_sessions (version 2) and before event_deliveries (version 3).
    let tx = conn.transaction()?;
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
    // v5, the same way.
    let done: i64 = conn.query_row(
        "SELECT COUNT(*) FROM schema_migrations WHERE version = 5",
        [],
        |r| r.get(0),
    )?;
    if done == 0
        && let Err(e) = conn.execute_batch(SCHEMA_V5)
    {
        if !conn.is_autocommit() {
            let _ = conn.execute_batch("ROLLBACK");
        }
        return Err(e.into());
    }
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
