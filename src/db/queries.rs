//! Reads over `state.db`, as maps of column name to value (carried from claude-org-ja `queries.py`).
//!
//! State semantics (`docs/contracts/state-semantics-contract.md`, section 3.5): four predicates over
//! `runs.status`. `queued` is an active reservation (it holds the project's base-clone slot) but is not
//! user-visible, so a reservation that lasts is an anomaly to surface, not an active work item.

use rusqlite::Connection;

use super::{Result, Row, query_rows};

/// What the resolver counts as occupying a project (contract 3.1).
pub const ACTIVE_RESERVATION_STATUSES: [&str; 3] = ["queued", "in_use", "review"];
/// What a person sees as active work (contract 3.3).
pub const USER_VISIBLE_STATUSES: [&str; 2] = ["in_use", "review"];
/// Runs whose worker is working now.
pub const ACTIVE_EXECUTION_STATUSES: [&str; 1] = ["in_use"];
/// Runs that cannot change without an operator (contract 3.4).
pub const TERMINAL_STATUSES: [&str; 3] = ["completed", "failed", "abandoned"];

/// Runs a person sees as active (`in_use`, `review`), newest first.
pub fn list_active_runs(conn: &Connection) -> Result<Vec<Row>> {
    query_rows(
        conn,
        "SELECT r.id AS run_id, r.task_id AS task_id, r.title AS title, r.pattern AS pattern,
                r.status AS status, r.branch AS branch, r.pr_url AS pr_url, r.pr_state AS pr_state,
                r.issue_refs AS issue_refs, r.verification AS verification,
                r.dispatched_at AS dispatched_at, r.completed_at AS completed_at,
                r.commit_short AS commit_short, r.outcome_note AS outcome_note,
                p.slug AS project_slug, p.display_name AS project_name,
                w.slug AS workstream_slug, w.display_name AS workstream_name,
                d.abs_path AS worker_dir, d.current_branch AS worker_dir_branch,
                d.lifecycle AS worker_dir_lifecycle
         FROM runs r
         JOIN projects p         ON p.id = r.project_id
         LEFT JOIN workstreams w ON w.id = r.workstream_id
         LEFT JOIN worker_dirs d ON d.id = r.worker_dir_id
         WHERE r.status IN ('in_use', 'review')
         ORDER BY r.dispatched_at DESC, r.id DESC",
        [],
    )
}

/// Runs that are only reserved (`queued`). One that stays here is a launch that never became `in_use`.
pub fn list_reserved_runs(conn: &Connection) -> Result<Vec<Row>> {
    query_rows(
        conn,
        "SELECT r.id AS run_id, r.task_id AS task_id, r.title AS title, r.pattern AS pattern,
                r.status AS status, r.branch AS branch, r.dispatched_at AS dispatched_at,
                p.slug AS project_slug, p.display_name AS project_name,
                d.abs_path AS worker_dir, d.lifecycle AS worker_dir_lifecycle
         FROM runs r
         JOIN projects p         ON p.id = r.project_id
         LEFT JOIN worker_dirs d ON d.id = r.worker_dir_id
         WHERE r.status = 'queued'
         ORDER BY r.dispatched_at DESC, r.id DESC",
        [],
    )
}

/// One run by its task id, with its project and worker folder.
pub fn get_run_by_task_id(conn: &Connection, task_id: &str) -> Result<Option<Row>> {
    Ok(query_rows(
        conn,
        "SELECT r.*, p.slug AS project_slug, p.display_name AS project_name,
                d.abs_path AS worker_dir, d.lifecycle AS worker_dir_lifecycle
         FROM runs r
         JOIN projects p         ON p.id = r.project_id
         LEFT JOIN worker_dirs d ON d.id = r.worker_dir_id
         WHERE r.task_id = ?1",
        [task_id],
    )?
    .into_iter()
    .next())
}

/// Worker folders, by path, optionally of one lifecycle.
pub fn list_worker_dirs(conn: &Connection, lifecycle: Option<&str>) -> Result<Vec<Row>> {
    match lifecycle {
        Some(l) => query_rows(
            conn,
            "SELECT * FROM worker_dirs WHERE lifecycle = ?1 ORDER BY abs_path",
            [l],
        ),
        None => query_rows(conn, "SELECT * FROM worker_dirs ORDER BY abs_path", []),
    }
}

/// The newest events, newest first. A negative limit gives none.
pub fn list_recent_events(conn: &Connection, limit: i64) -> Result<Vec<Row>> {
    query_rows(
        conn,
        "SELECT id, occurred_at, actor, kind, run_id, workstream_id, project_id, payload_json
         FROM events ORDER BY id DESC LIMIT ?1",
        [limit.max(0)],
    )
}

/// The one `org_sessions` row. An error if the database has not been migrated (the table is missing).
pub fn get_session(conn: &Connection) -> Result<Option<Row>> {
    Ok(
        query_rows(conn, "SELECT * FROM org_sessions WHERE id = 1", [])?
            .into_iter()
            .next(),
    )
}
