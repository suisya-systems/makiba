//! The writes to `state.db` (carried from claude-org-ja `writer.py`).
//!
//! Every mutation is one SQL statement or a few. A write made outside `begin`/`transaction` is
//! committed by SQLite at once. `None` in an option means "the caller did not supply this": an
//! update never clears a column by omission (use `Patch::Clear` where clearing is allowed).

use std::path::Path;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::Value;

use super::{Result, Row, migrate, queries};

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// A write handle on a migrated `state.db`.
pub struct StateWriter {
    conn: Connection,
}

/// Rolls a transaction back when dropped inside one: on an error, and on a panic that unwinds past it.
struct RollbackOnDrop<'a>(&'a Connection);

impl Drop for RollbackOnDrop<'_> {
    fn drop(&mut self) {
        if !self.0.is_autocommit() {
            let _ = self.0.execute_batch("ROLLBACK");
        }
    }
}

/// A column of the one `org_sessions` row.
#[derive(Clone, Copy, Debug)]
pub enum SessionField {
    Status,
    StartedAt,
    UpdatedAt,
    SuspendedAt,
    ResumedAt,
    Objective,
    ResumeInstructions,
    DispatcherPaneId,
    DispatcherPeerId,
    CuratorPaneId,
    CuratorPeerId,
}

impl SessionField {
    fn column(self) -> &'static str {
        match self {
            SessionField::Status => "status",
            SessionField::StartedAt => "started_at",
            SessionField::UpdatedAt => "updated_at",
            SessionField::SuspendedAt => "suspended_at",
            SessionField::ResumedAt => "resumed_at",
            SessionField::Objective => "objective",
            SessionField::ResumeInstructions => "resume_instructions",
            SessionField::DispatcherPaneId => "dispatcher_pane_id",
            SessionField::DispatcherPeerId => "dispatcher_peer_id",
            SessionField::CuratorPaneId => "curator_pane_id",
            SessionField::CuratorPeerId => "curator_peer_id",
        }
    }
}

/// What to write to a session column: a value, or NULL.
#[derive(Clone, Debug)]
pub enum Patch {
    Set(String),
    Clear,
}

/// A worker folder to register. Only the fields given are written when the folder is already known.
#[derive(Default)]
pub struct WorkerDir<'a> {
    pub abs_path: &'a str,
    pub layout: Option<&'a str>,
    pub is_git_repo: Option<bool>,
    pub is_worktree: Option<bool>,
    pub origin_url: Option<&'a str>,
    pub current_branch: Option<&'a str>,
    pub size_mb: Option<f64>,
    pub lifecycle: Option<&'a str>,
}

/// A run to create or update, keyed by `task_id`. Only the fields given are written on update.
#[derive(Default)]
pub struct RunUpsert<'a> {
    pub task_id: &'a str,
    pub project_slug: &'a str,
    pub pattern: Option<&'a str>,
    pub title: Option<&'a str>,
    pub status: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub pr_url: Option<&'a str>,
    pub pr_state: Option<&'a str>,
    pub issue_refs: Option<&'a [&'a str]>,
    pub verification: Option<&'a str>,
    pub worker_dir_abs_path: Option<&'a str>,
    pub commit_short: Option<&'a str>,
    pub commit_full: Option<&'a str>,
    pub outcome_note: Option<&'a str>,
    pub workstream_slug: Option<&'a str>,
}

/// An event to append. The run, project and workstream are looked up from the names given.
#[derive(Default)]
pub struct NewEvent<'a> {
    pub kind: &'a str,
    pub actor: Option<&'a str>,
    pub payload: Option<&'a Value>,
    pub occurred_at: Option<&'a str>,
    pub run_task_id: Option<&'a str>,
    pub project_slug: Option<&'a str>,
    pub workstream_slug: Option<&'a str>,
}

/// The `SET` clause of an update built from the fields that were supplied.
#[derive(Default)]
struct Sets {
    cols: Vec<String>,
    vals: Vec<Sql>,
}

impl Sets {
    fn put(&mut self, col: &str, val: impl Into<Sql>) {
        self.cols.push(format!("{col} = ?"));
        self.vals.push(val.into());
    }

    fn maybe(&mut self, col: &str, val: Option<&str>) {
        if let Some(v) = val {
            self.put(col, v.to_string());
        }
    }
}

fn id_of(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Option<i64>> {
    Ok(conn.query_row(sql, p, |r| r.get(0)).optional()?)
}

impl StateWriter {
    /// Take a connection and bring its database to v4. Fails if it is inside a transaction.
    pub fn new(mut conn: Connection) -> Result<Self> {
        migrate(&mut conn)?;
        super::configure(&conn)?;
        Ok(StateWriter { conn })
    }

    /// Open (or create) the database at `path`, migrated.
    pub fn open(path: &Path) -> Result<Self> {
        Self::new(super::open(path)?)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ---- transactions -------------------------------------------------------------------

    /// Open a transaction. A no-op when one is already open.
    pub fn begin(&self) -> Result<()> {
        if self.conn.is_autocommit() {
            self.conn.execute_batch("BEGIN")?;
        }
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        if !self.conn.is_autocommit() {
            self.conn.execute_batch("COMMIT")?;
        }
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        if !self.conn.is_autocommit() {
            self.conn.execute_batch("ROLLBACK")?;
        }
        Ok(())
    }

    /// Run `f` in a transaction: commit if it returns `Ok`, roll back if it returns `Err` or panics.
    /// Inside a transaction already open, `f` joins it and whoever opened it commits or rolls back.
    pub fn transaction<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            return f(self);
        }
        self.begin()?;
        let _rollback = RollbackOnDrop(&self.conn);
        let out = f(self)?;
        self.commit()?;
        Ok(out)
    }

    // ---- the session row ----------------------------------------------------------------

    /// Write the given columns of the session row. With none given it only stamps `last_writer_at`.
    pub fn update_session(&self, fields: &[(SessionField, Patch)]) -> Result<()> {
        let mut sets = Sets::default();
        for (field, patch) in fields {
            match patch {
                Patch::Set(v) => sets.put(field.column(), v.clone()),
                Patch::Clear => sets.put(field.column(), Sql::Null),
            }
        }
        sets.cols.push(format!("last_writer_at = {NOW}"));
        self.conn.execute(
            &format!(
                "UPDATE org_sessions SET {} WHERE id = 1",
                sets.cols.join(", ")
            ),
            params_from_iter(sets.vals),
        )?;
        Ok(())
    }

    pub fn get_session(&self) -> Result<Option<Row>> {
        queries::get_session(&self.conn)
    }

    // ---- worker folders -----------------------------------------------------------------

    /// Insert a worker folder, or update the fields given of one already registered. Returns its id.
    pub fn register_worker_dir(&self, d: &WorkerDir) -> Result<i64> {
        let existing = id_of(
            &self.conn,
            "SELECT id FROM worker_dirs WHERE abs_path = ?1",
            [d.abs_path],
        )?;
        let Some(id) = existing else {
            self.conn.execute(
                "INSERT INTO worker_dirs (abs_path, layout, is_git_repo, is_worktree, origin_url, \
                 current_branch, size_mb, lifecycle) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    d.abs_path,
                    d.layout.unwrap_or("flat"),
                    d.is_git_repo.unwrap_or(false),
                    d.is_worktree.unwrap_or(false),
                    d.origin_url,
                    d.current_branch,
                    d.size_mb,
                    d.lifecycle.unwrap_or("active"),
                ],
            )?;
            return Ok(self.conn.last_insert_rowid());
        };
        let mut sets = Sets::default();
        sets.maybe("layout", d.layout);
        if let Some(b) = d.is_git_repo {
            sets.put("is_git_repo", b);
        }
        if let Some(b) = d.is_worktree {
            sets.put("is_worktree", b);
        }
        sets.maybe("origin_url", d.origin_url);
        sets.maybe("current_branch", d.current_branch);
        if let Some(mb) = d.size_mb {
            sets.put("size_mb", mb);
        }
        sets.maybe("lifecycle", d.lifecycle);
        sets.cols.push(format!("last_seen_at = {NOW}"));
        sets.vals.push(Sql::from(d.abs_path.to_string()));
        self.conn.execute(
            &format!(
                "UPDATE worker_dirs SET {} WHERE abs_path = ?",
                sets.cols.join(", ")
            ),
            params_from_iter(sets.vals),
        )?;
        Ok(id)
    }

    /// Move a worker folder to another lifecycle (`active`, `scratch`, `archived`, `delete_pending`).
    pub fn update_worker_dir_lifecycle(&self, abs_path: &str, lifecycle: &str) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE worker_dirs SET lifecycle = ?1, last_seen_at = {NOW} WHERE abs_path = ?2"
            ),
            [lifecycle, abs_path],
        )?;
        Ok(())
    }

    /// Delete a worker folder's row. Runs keep their history (`runs.worker_dir_id` becomes NULL).
    pub fn remove_worker_dir(&self, abs_path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM worker_dirs WHERE abs_path = ?1", [abs_path])?;
        Ok(())
    }

    // ---- projects and runs --------------------------------------------------------------

    /// The id of the project `slug`, created if it is new (named after its slug unless a name is given).
    /// A name or URL given replaces the stored one; one not given leaves it.
    pub fn ensure_project(
        &self,
        slug: &str,
        display_name: Option<&str>,
        origin_url: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO projects (slug, display_name, origin_url) VALUES (?1, COALESCE(?2, ?1), ?3) \
             ON CONFLICT(slug) DO UPDATE SET \
               display_name = COALESCE(?2, projects.display_name), \
               origin_url = COALESCE(?3, projects.origin_url)",
            params![slug, display_name, origin_url],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM projects WHERE slug = ?1", [slug], |r| {
                r.get(0)
            })?)
    }

    /// Insert a run, or update the fields given of the one with this `task_id`. Returns `runs.id`.
    /// A worker folder or workstream that is named but not found is written as NULL.
    pub fn upsert_run(&self, r: &RunUpsert) -> Result<i64> {
        let project_id = self.ensure_project(r.project_slug, None, None)?;
        let existing = id_of(
            &self.conn,
            "SELECT id FROM runs WHERE task_id = ?1",
            [r.task_id],
        )?;
        let worker_dir_id = match r.worker_dir_abs_path {
            Some(p) => Some(id_of(
                &self.conn,
                "SELECT id FROM worker_dirs WHERE abs_path = ?1",
                [p],
            )?),
            None => None,
        };
        let workstream_id = match r.workstream_slug {
            Some(s) => Some(id_of(
                &self.conn,
                "SELECT id FROM workstreams WHERE project_id = ?1 AND slug = ?2",
                params![project_id, s],
            )?),
            None => None,
        };
        let issue_refs = r
            .issue_refs
            .map(|v| serde_json::to_string(v).expect("strings serialize"));

        let Some(id) = existing else {
            self.conn.execute(
                "INSERT INTO runs (task_id, project_id, workstream_id, pattern, title, status, \
                 branch, pr_url, pr_state, issue_refs, verification, worker_dir_id, commit_short, \
                 commit_full, outcome_note) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    r.task_id,
                    project_id,
                    workstream_id.flatten(),
                    r.pattern.unwrap_or("B"),
                    r.title.unwrap_or(r.task_id),
                    r.status.unwrap_or("in_use"),
                    r.branch,
                    r.pr_url,
                    r.pr_state,
                    issue_refs,
                    r.verification.unwrap_or("standard"),
                    worker_dir_id.flatten(),
                    r.commit_short,
                    r.commit_full,
                    r.outcome_note,
                ],
            )?;
            return Ok(self.conn.last_insert_rowid());
        };
        let mut sets = Sets::default();
        sets.put("project_id", project_id);
        sets.maybe("pattern", r.pattern);
        sets.maybe("title", r.title);
        sets.maybe("status", r.status);
        sets.maybe("branch", r.branch);
        sets.maybe("pr_url", r.pr_url);
        sets.maybe("pr_state", r.pr_state);
        sets.maybe("verification", r.verification);
        sets.maybe("commit_short", r.commit_short);
        sets.maybe("commit_full", r.commit_full);
        sets.maybe("outcome_note", r.outcome_note);
        if let Some(j) = issue_refs {
            sets.put("issue_refs", j);
        }
        if let Some(w) = worker_dir_id {
            sets.put("worker_dir_id", w);
        }
        if let Some(w) = workstream_id {
            sets.put("workstream_id", w);
        }
        sets.vals.push(Sql::from(r.task_id.to_string()));
        self.conn.execute(
            &format!("UPDATE runs SET {} WHERE task_id = ?", sets.cols.join(", ")),
            params_from_iter(sets.vals),
        )?;
        Ok(id)
    }

    /// Set a run's status. `completed_at` and `outcome_note` are kept when not given.
    /// Returns whether a run with this `task_id` exists.
    pub fn update_run_status(
        &self,
        task_id: &str,
        status: &str,
        completed_at: Option<&str>,
        outcome_note: Option<&str>,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE runs SET status = ?1, completed_at = COALESCE(?2, completed_at), \
             outcome_note = COALESCE(?3, outcome_note) WHERE task_id = ?4",
            params![status, completed_at, outcome_note, task_id],
        )?;
        Ok(n > 0)
    }

    // ---- events -------------------------------------------------------------------------

    /// Append one event and return its id. The payload is stored as JSON with sorted keys.
    pub fn append_event(&self, e: &NewEvent) -> Result<i64> {
        let (mut run_id, mut project_id, mut workstream_id) = (None, None, None);
        if let Some(t) = e.run_task_id.filter(|t| !t.is_empty()) {
            let row = self
                .conn
                .query_row(
                    "SELECT id, project_id, workstream_id FROM runs WHERE task_id = ?1",
                    [t],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((r, p, w)) = row {
                (run_id, project_id, workstream_id) = (Some(r), Some(p), w);
            }
        }
        if let (Some(s), None) = (e.project_slug.filter(|s| !s.is_empty()), project_id) {
            project_id = id_of(&self.conn, "SELECT id FROM projects WHERE slug = ?1", [s])?;
        }
        if let (Some(s), None, Some(p)) = (
            e.workstream_slug.filter(|s| !s.is_empty()),
            workstream_id,
            project_id,
        ) {
            workstream_id = id_of(
                &self.conn,
                "SELECT id FROM workstreams WHERE project_id = ?1 AND slug = ?2",
                params![p, s],
            )?;
        }
        let payload = match e.payload {
            Some(v) => serde_json::to_string(v).expect("a JSON value serializes"),
            None => "{}".to_string(),
        };
        let at = e.occurred_at.filter(|a| !a.is_empty());
        self.conn.execute(
            &format!(
                "INSERT INTO events (occurred_at, actor, kind, run_id, workstream_id, project_id, \
                 payload_json) VALUES (COALESCE(?1, {NOW}), ?2, ?3, ?4, ?5, ?6, ?7)"
            ),
            params![
                at,
                e.actor,
                e.kind,
                run_id,
                workstream_id,
                project_id,
                payload
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    // ---- the delivery ledger ------------------------------------------------------------
    //
    // `events` is the record; `event_deliveries` says whether each event reached a recipient.
    // `UNIQUE (source_event_id, recipient)` is the idempotency key, so a lost push can never leave an
    // event undelivered without a trace. Only `delivered` is terminal; `failed` is retried.

    /// Events of the given kinds not yet `delivered` to `recipient`, oldest first, each with its
    /// ledger `attempt` count (0 if never attempted).
    ///
    /// `since` is a floor on `occurred_at` that keeps pre-ledger history out. It only gates events never
    /// attempted: one that already has a ledger row is returned whatever its age, so a delivery that keeps
    /// failing is retried without end.
    pub fn pending_deliveries(
        &self,
        recipient: &str,
        kinds: &[&str],
        since: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<Row>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; kinds.len()].join(",");
        let mut vals: Vec<Sql> = vec![recipient.to_string().into()];
        vals.extend(kinds.iter().map(|k| Sql::from(k.to_string())));
        vals.push(recipient.to_string().into());
        let mut sql = format!(
            "SELECT e.id AS id, e.kind AS kind, e.occurred_at AS occurred_at, \
             e.payload_json AS payload_json, COALESCE(d.attempt, 0) AS attempt \
             FROM events e \
             LEFT JOIN event_deliveries d ON d.source_event_id = e.id AND d.recipient = ? \
             WHERE e.kind IN ({marks}) \
               AND NOT EXISTS (SELECT 1 FROM event_deliveries dd \
                               WHERE dd.source_event_id = e.id AND dd.recipient = ? \
                                 AND dd.status = 'delivered') "
        );
        if let Some(s) = since {
            sql.push_str("AND (e.occurred_at >= ? OR d.id IS NOT NULL) ");
            vals.push(s.to_string().into());
        }
        sql.push_str("ORDER BY e.id ASC");
        if let Some(n) = limit {
            sql.push_str(" LIMIT ?");
            vals.push(n.into());
        }
        super::query_rows(&self.conn, &sql, params_from_iter(vals))
    }

    /// Record that a relay of the event is being attempted, before the send, and return the attempt
    /// number. The first call creates the row as `pending`; later calls count up. A `delivered` row
    /// keeps its status.
    pub fn begin_delivery_attempt(&self, source_event_id: i64, recipient: &str) -> Result<i64> {
        self.conn.execute(
            &format!(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt, \
                 first_attempt_at, last_attempt_at) VALUES (?1, ?2, 'pending', 1, {NOW}, {NOW}) \
                 ON CONFLICT(source_event_id, recipient) DO UPDATE SET \
                   attempt = attempt + 1, last_attempt_at = {NOW}"
            ),
            params![source_event_id, recipient],
        )?;
        Ok(self.conn.query_row(
            "SELECT attempt FROM event_deliveries WHERE source_event_id = ?1 AND recipient = ?2",
            params![source_event_id, recipient],
            |r| r.get(0),
        )?)
    }

    /// Mark the event delivered to `recipient`, after a confirmed send. Creates the row if no attempt
    /// was recorded, and is harmless to repeat.
    pub fn mark_delivered(&self, source_event_id: i64, recipient: &str) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt, \
                 first_attempt_at, last_attempt_at, delivered_at) \
                 VALUES (?1, ?2, 'delivered', 1, {NOW}, {NOW}, {NOW}) \
                 ON CONFLICT(source_event_id, recipient) DO UPDATE SET \
                   status = 'delivered', delivered_at = {NOW}, last_attempt_at = {NOW}"
            ),
            params![source_event_id, recipient],
        )?;
        Ok(())
    }

    /// Record a failed delivery, to be retried. A row already `delivered` stays delivered.
    pub fn mark_delivery_failed(
        &self,
        source_event_id: i64,
        recipient: &str,
        error: &str,
    ) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt, \
                 first_attempt_at, last_attempt_at, last_error) \
                 VALUES (?1, ?2, 'failed', 1, {NOW}, {NOW}, ?3) \
                 ON CONFLICT(source_event_id, recipient) DO UPDATE SET \
                   status = CASE WHEN status = 'delivered' THEN 'delivered' ELSE 'failed' END, \
                   last_error = excluded.last_error, last_attempt_at = {NOW}"
            ),
            params![source_event_id, recipient, error],
        )?;
        Ok(())
    }

    /// Record that a push of the event to `recipient` was accepted, after the send: the attempt
    /// goes up and `last_attempt_at` is now. A crash before this call leaves the row at its old
    /// `attempt`, so the message is pushed again. The status is not touched.
    pub fn record_push(&self, source_event_id: i64, recipient: &str) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt, \
                 first_attempt_at, last_attempt_at) VALUES (?1, ?2, 'pending', 1, {NOW}, {NOW}) \
                 ON CONFLICT(source_event_id, recipient) DO UPDATE SET \
                   attempt = attempt + 1, last_attempt_at = {NOW}, \
                   first_attempt_at = COALESCE(first_attempt_at, {NOW})"
            ),
            params![source_event_id, recipient],
        )?;
        Ok(())
    }

    /// A `pending` row becomes `delivered` on a confirmation. `last_attempt_at` stays the push time.
    pub fn confirm_push(&self, source_event_id: i64, recipient: &str) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE event_deliveries SET status = 'delivered', delivered_at = {NOW} \
                 WHERE source_event_id = ?1 AND recipient = ?2 AND status = 'pending'"
            ),
            params![source_event_id, recipient],
        )?;
        Ok(())
    }
}
