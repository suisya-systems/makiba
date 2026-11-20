//! Verification (Issue M3-6, spec section 5.3): a launch goes from `dialog_passed` to `verified`
//! only once a `working` status or a turn of its own terminal is recorded, or to `failed` when
//! none is recorded within the timeout.
//!
//! "Its own terminal" is the launch's `spawn_id`: the monitor records every status and turn with
//! the terminal's `makiba.spawn` tag, and that value is minted for the launch, so a record that
//! carries it is of this launch's terminal and was made after the launch. A record of another
//! terminal of the same task or role (an earlier launch, or one without the tag) never counts.
//! `worker_spawn_verified` names the record in its payload (`event_id`); migration v6 refuses the
//! event without one, whoever writes it.
//!
//! A failure, here or in the dialogs (M3-5), is one transaction: `worker_spawn_failed`, stage
//! `failed`, the run `failed`, and a notice to the secretary. The terminal is left for inspection.

use crate::agents;
use crate::base::Base;
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{AGENT_SECRETARY, TAG_SPAWN};
use rusqlite::params;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a launch has to be verified, from its `worker_spawned` (the Issue's default).
pub const TIMEOUT: Duration = Duration::from_secs(180);
/// The test seam that sets the timeout in milliseconds (spec section 2).
pub const TIMEOUT_ENV: &str = "MAKIBA_VERIFY_TIMEOUT_MS";
/// How often a pass runs at most.
const EVERY: Duration = Duration::from_secs(1);

/// The timeout: the seam when it holds a number, otherwise `TIMEOUT`.
pub fn timeout_from_env() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

struct Row {
    id: i64,
    run_id: i64,
    task: String,
    spawn: String,
    stage: String,
    agent: String,
    worktree: String,
    /// The first working status or turn recorded with the launch's spawn id within the timeout
    /// (one recorded later does not count, even if a pass sees it before the launch is failed).
    evidence: Option<i64>,
    /// Milliseconds since the launch.
    waited: i64,
}

pub struct Verify {
    timeout: Duration,
    next: Instant,
}

impl Verify {
    pub fn new(timeout: Duration) -> Verify {
        Verify {
            timeout,
            next: Instant::now(),
        }
    }

    /// One pass over the launches at `launched` and `dialog_passed`, at most once a second.
    pub fn pass(&mut self, w: &StateWriter, base: &dyn Base) -> Result<()> {
        if Instant::now() < self.next {
            return Ok(());
        }
        self.next = Instant::now() + EVERY;
        let rows: Vec<Row> = {
            let mut st = w.conn().prepare(
                "SELECT id, run_id, task_id, spawn_id, stage, agent, worktree,
                   (SELECT min(e.id) FROM events e
                    WHERE json_extract(e.payload_json, '$.spawn_id') = spawn_id
                      AND (e.kind = 'terminal_turn' OR (e.kind = 'terminal_status'
                           AND json_extract(e.payload_json, '$.state') = 'working'))
                      AND julianday(e.occurred_at) <= julianday(launched) + ?1 / 86400000.0),
                   CAST((julianday('now') - julianday(launched)) * 86400000 AS INTEGER)
                 FROM (SELECT l.id, r.id AS run_id, r.task_id, l.spawn_id, l.stage, l.agent,
                         l.worktree, COALESCE(
                           (SELECT max(e.occurred_at) FROM events e WHERE e.kind = 'worker_spawned'
                            AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id),
                           l.created_at) AS launched
                       FROM run_launch l JOIN runs r ON r.id = l.run_id
                       WHERE l.stage IN ('launched', 'dialog_passed'))
                 ORDER BY id",
            )?;
            st.query_map([self.timeout.as_millis() as i64], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    task: r.get(2)?,
                    spawn: r.get(3)?,
                    stage: r.get(4)?,
                    agent: r.get(5)?,
                    worktree: r.get(6)?,
                    evidence: r.get(7)?,
                    waited: r.get(8)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        for row in rows {
            match row.evidence {
                Some(event) if row.stage == "dialog_passed" => verified(w, &row, event)?,
                // Seen working in time, still at `launched` (a restart came after the timeout):
                // the dialog step moves it on (Issue M3-7).
                Some(_) => {}
                _ if row.waited >= self.timeout.as_millis() as i64 => {
                    let why = format!(
                        "no working status or turn of its terminal within {} ms of the launch",
                        self.timeout.as_millis()
                    );
                    let screen = screen_of(base, &row.spawn);
                    fail(w, row.id, &row.task, &row.spawn, &row.stage, &why, &screen)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The screen of the terminal carrying `spawn`, for the record; empty when it cannot be read.
fn screen_of(base: &dyn Base, spawn: &str) -> Vec<String> {
    base.list()
        .ok()
        .and_then(|ts| {
            ts.into_iter()
                .find(|t| t.tags.get(TAG_SPAWN).map(String::as_str) == Some(spawn))
        })
        .and_then(|t| base.read(&t.id, 0).ok())
        .unwrap_or_default()
}

/// `worker_spawn_verified`, stage `verified` and the run `in_use`, together; the conversation id
/// when the agent module has one (a resumed launch keeps the id it resumed).
fn verified(w: &StateWriter, row: &Row, event: i64) -> Result<()> {
    let conversation = agents::by_name(&row.agent)
        .and_then(|a| a.conversation_id(Path::new(&row.worktree), &agents::session_id(&row.spawn)));
    w.transaction(|w| {
        w.append_event(&NewEvent {
            kind: "worker_spawn_verified",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": row.task, "spawn_id": row.spawn, "event_id": event})),
            run_task_id: Some(&row.task),
            ..Default::default()
        })?;
        w.conn().execute(
            "UPDATE run_launch SET stage = 'verified',
               conversation_id = COALESCE(conversation_id, ?2),
               verified_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1",
            params![row.id, conversation],
        )?;
        // ponytail: a run closed while its launch was under way is not brought back.
        w.conn().execute(
            "UPDATE runs SET status = 'in_use' WHERE id = ?1 AND status IN ('queued', 'suspended')",
            [row.run_id],
        )?;
        Ok(())
    })
}

/// A launch that cannot be verified, at `stage`: `worker_spawn_failed` with `screen`, stage
/// `failed`, the run `failed`, and a notice to the secretary if the gate sends it, together. The
/// terminal is left.
///
/// A resume of a `suspended` run (Issue 41: a launch with a conversation id before it is
/// verified) does not fail its run: its keys are revoked and a fresh launch is added with the
/// same brief and workplace, and the relaunch ends the terminal left here.
pub(super) fn fail(
    w: &StateWriter,
    id: i64,
    task: &str,
    spawn: &str,
    stage: &str,
    why: &str,
    screen: &[String],
) -> Result<()> {
    w.transaction(|w| {
        w.append_event(&NewEvent {
            kind: "worker_spawn_failed",
            actor: Some("makibad"),
            payload: Some(&json!({
                "spawn_id": spawn,
                "stage": stage,
                "reason": why,
                "screen": screen.join("\n"),
            })),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        let resumed: bool = w.conn().query_row(
            "SELECT l.conversation_id IS NOT NULL AND l.stage != 'verified' AND r.status = 'suspended'
             FROM run_launch l JOIN runs r ON r.id = l.run_id WHERE l.id = ?1",
            [id],
            |r| r.get(0),
        )?;
        w.conn().execute(
            "UPDATE run_launch SET stage = 'failed', failed_reason = ?2 WHERE id = ?1",
            params![id, why],
        )?;
        if resumed {
            super::resume::revoke_keys(w, spawn)?;
            super::launch::add_relaunch(w, id, None)?;
            return Ok(());
        }
        w.conn().execute(
            "UPDATE runs SET status = 'failed', outcome_note = ?2,
               completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE task_id = ?1 AND status IN ('queued', 'suspended')",
            params![task, format!("launch failed: {why}")],
        )?;
        if !super::notify_gate::decide("worker_spawn_failed").send {
            return Ok(());
        }
        // The notice (spec 5.4): the event, the message, the pending delivery with no attempt.
        let event = w.append_event(&NewEvent {
            kind: "message_sent",
            actor: Some("makibad"),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        let body = format!(
            "Launch of {task} failed at stage {stage}: {why}. Its terminal is left for inspection."
        );
        w.conn().execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
             VALUES (?1, 'makibad', ?2, 'notice', (SELECT id FROM runs WHERE task_id = ?3), ?4)",
            params![event, AGENT_SECRETARY, task, body],
        )?;
        w.conn().execute(
            "UPDATE events SET payload_json = json_object('message_id', ?1) WHERE id = ?2",
            params![w.conn().last_insert_rowid(), event],
        )?;
        w.conn().execute(
            "INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?1, ?2)",
            params![event, AGENT_SECRETARY],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::stub::Stub;
    use crate::base::{LaunchSpec, TerminalRef};
    use crate::db::writer::RunUpsert;
    use std::collections::BTreeMap;

    /// A queued run `t1` with a launch `s1` at `stage`, launched `ago_ms` before now.
    fn org(stage: &str, ago_ms: i64) -> (tempfile::TempDir, StateWriter) {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                 VALUES (1, 's1', 'fake', 'b', '/w', ?1)",
                [stage],
            )
            .unwrap();
        let at: String = w
            .conn()
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
                [format!("-{} seconds", ago_ms as f64 / 1000.0)],
                |r| r.get(0),
            )
            .unwrap();
        w.append_event(&NewEvent {
            kind: "worker_spawned",
            payload: Some(&json!({"task_id": "t1", "spawn_id": "s1"})),
            occurred_at: Some(&at),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap();
        (dir, w)
    }

    fn record(w: &StateWriter, kind: &str, spawn: &str, extra: serde_json::Value) -> i64 {
        let mut p = json!({"task_id": "t1", "role": "worker", "spawn_id": spawn});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        w.append_event(&NewEvent {
            kind,
            payload: Some(&p),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap()
    }

    fn one<T: rusqlite::types::FromSql>(w: &StateWriter, sql: &str) -> T {
        w.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Two passes, as a second apart.
    fn passes(v: &mut Verify, w: &StateWriter, base: &dyn Base) {
        for _ in 0..2 {
            v.next = Instant::now();
            v.pass(w, base).unwrap();
        }
    }

    #[test]
    fn the_timeout_is_180_s_unless_the_seam_says_otherwise() {
        assert_eq!(TIMEOUT, Duration::from_secs(180));
        // SAFETY: no other test of this binary reads or writes this variable.
        unsafe { std::env::remove_var(TIMEOUT_ENV) };
        assert_eq!(timeout_from_env(), TIMEOUT);
    }

    /// A turn of the launch's own terminal verifies it.
    #[test]
    fn a_turn_verifies() {
        let (_d, w) = org("dialog_passed", 0);
        let turn = record(&w, "terminal_turn", "s1", json!({"turn": 1}));
        let mut v = Verify::new(TIMEOUT);
        v.pass(&w, &Stub::new()).unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "verified"
        );
        assert_eq!(one::<String>(&w, "SELECT status FROM runs"), "in_use");
        let p: String = one(
            &w,
            "SELECT payload_json FROM events WHERE kind = 'worker_spawn_verified'",
        );
        let p: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(p["event_id"], turn);
    }

    /// Records of another terminal, an idle status, or a launch still at `launched` verify
    /// nothing; within the timeout nothing fails either.
    #[test]
    fn nothing_but_the_launchs_own_working_or_turn_verifies() {
        let (_d, w) = org("dialog_passed", 0);
        record(&w, "terminal_status", "s1", json!({"state": "idle"}));
        record(&w, "terminal_status", "other", json!({"state": "working"}));
        record(&w, "terminal_turn", "other", json!({"turn": 1}));
        let mut v = Verify::new(TIMEOUT);
        passes(&mut v, &w, &Stub::new());
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "dialog_passed"
        );
        let (_d, w) = org("launched", 0);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        passes(&mut v, &w, &Stub::new());
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
    }

    /// Past the timeout: failed with the screen, the run failed, one notice; the terminal is not
    /// ended. A launch at `launched` times out even with evidence (it never left the dialogs).
    #[test]
    fn a_timeout_fails_the_launch_and_the_run_and_leaves_the_terminal() {
        let (_d, w) = org("launched", 10_000);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        let stub = Stub::new();
        stub.launch(&LaunchSpec {
            argv: vec![],
            cwd: "/".into(),
            name: "w-t1".into(),
            tags: BTreeMap::from([(TAG_SPAWN.to_string(), "s1".to_string())]),
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(&w, &stub).unwrap();
        assert_eq!(one::<String>(&w, "SELECT stage FROM run_launch"), "failed");
        assert_eq!(one::<String>(&w, "SELECT status FROM runs"), "failed");
        let p: String = one(
            &w,
            "SELECT payload_json FROM events WHERE kind = 'worker_spawn_failed'",
        );
        let p: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(p["stage"], "launched");
        assert_eq!(p["spawn_id"], "s1");
        assert_eq!(
            one::<i64>(
                &w,
                "SELECT count(*) FROM messages m JOIN event_deliveries d
                 ON d.source_event_id = m.event_id
                 WHERE m.kind = 'notice' AND m.recipient = 'secretary' AND m.run_id = 1"
            ),
            1
        );
        assert!(stub.get(&TerminalRef("w-t1".into())).unwrap().alive);
    }

    /// Evidence recorded after the timeout does not verify: the launch fails.
    #[test]
    fn evidence_recorded_after_the_timeout_does_not_verify() {
        let (_d, w) = org("dialog_passed", 10_000);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(&w, &Stub::new()).unwrap();
        assert_eq!(one::<String>(&w, "SELECT stage FROM run_launch"), "failed");
        // Recorded within it, the same evidence verifies however late the pass.
        let (_d, w) = org("dialog_passed", 10_000);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        let mut v = Verify::new(Duration::from_secs(60));
        v.pass(&w, &Stub::new()).unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "verified"
        );
    }

    /// A dialog failure (M3-5) takes the same path: the run failed and the secretary told.
    #[test]
    fn a_dialog_failure_fails_the_run_and_tells_the_secretary() {
        let (_d, w) = org("launched", 0);
        fail(&w, 1, "t1", "s1", "launched", "an unknown dialog", &[]).unwrap();
        assert_eq!(one::<String>(&w, "SELECT status FROM runs"), "failed");
        assert_eq!(
            one::<i64>(
                &w,
                "SELECT count(*) FROM messages WHERE kind = 'notice' AND recipient = 'secretary'"
            ),
            1
        );
    }
}
