//! Parking (Issue 283, docs/design/belt.md section 4.1, the belt's step 3): a run that waits only
//! for the person lets its terminal go, and comes back on its conversation.
//!
//! A run is parked when, read from the records on each pass (never a latch, so a run with a second
//! open question is decided again after each wake; review finding 6), all of these hold: it is
//! `in_use`; it has an `escalated` entry of kind `question`; it has no entry `open` (the
//! secretary's to answer) and no prompt entry `escalated` (a prompt's answer needs the terminal
//! that showed it, spec section 7); its newest launch is `verified`; no message to its worker is
//! still waiting for delivery, nor taken but not yet seen taken (Issue 332: a typed one until a
//! turn of the launch later than the turn it was typed at is recorded, any other one until a
//! terminal record of the launch written after it and that arrived after it reached the
//! terminal); and the newest `terminal_status` and
//! `terminal_turn` of that launch say `idle` or `done`, written at least `MAKIBA_PARK_AFTER_MS`
//! ago. The base is read once more before anything is written: the terminal must still show
//! `idle` or `done` and no later turn. The transaction that parks checks the records again, and a
//! typed send checks the run in the transaction that records its message (`delivery::send_typed`),
//! so a text is either typed to a run that is not parked after it or stored for a parked one.
//!
//! Parking is one transaction (the run `parked`, its worker's keys revoked, `run_parked`), then the
//! run's terminals of this organization are ended; one that cannot be ended is ended on a later
//! pass. The workplace, the branch and the launches (so the conversation id) are kept.
//!
//! `unpark`, in the caller's transaction, makes the run `suspended` with a relaunch that resumes
//! the conversation of its newest verified launch (taken at once, so it counts again at once,
//! belt.md 3.2), and writes `run_unparked` with the reason. `settle` wakes every parked run left
//! with no escalated question, whatever took the last one away.

use crate::base::{Base, State};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{TAG_ORG, TAG_SPAWN, TAG_TASK_ID};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How long a terminal must have been idle before its run is parked (belt.md 4.1).
pub const PARK_AFTER: Duration = Duration::from_secs(120);
/// The test seam that sets the wait in milliseconds.
pub const PARK_AFTER_ENV: &str = "MAKIBA_PARK_AFTER_MS";

/// The wait: the seam when it holds a number, otherwise `PARK_AFTER`.
pub fn after_from_env() -> Duration {
    std::env::var(PARK_AFTER_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(PARK_AFTER)
}

/// The newest launch of the run `r`, as `l`.
const NEWEST: &str = "run_launch l ON l.id = (SELECT max(id) FROM run_launch WHERE run_id = r.id)";

/// The newest terminal record (status or turn) of the launch `l`, by event id.
const LAST_SEEN: &str = "(SELECT max(e.id) FROM events e WHERE e.run_id = r.id
    AND e.kind IN ('terminal_status', 'terminal_turn')
    AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id)";

/// The newest turn recorded for the terminal of the launch `l`, 0 for none.
const TURN: &str = "coalesce((SELECT max(json_extract(t.payload_json, '$.turn')) FROM events t
    WHERE t.run_id = r.id AND t.kind = 'terminal_turn'
      AND json_extract(t.payload_json, '$.spawn_id') = l.spawn_id), 0)";

/// Whether the run `r` with its newest launch `l` may be parked now; `?1` is the wait in
/// milliseconds. Read by the pass and again by the update that parks.
///
/// A message to its worker keeps it while it waits for delivery, and after it until the terminal
/// is seen past it (Issue 332): for one typed at a turn of this launch, a recorded turn above
/// that one; for any other, a terminal record of the launch written after the message and that
/// arrived after it reached the terminal (`occurred_at` is the arrival time the monitor keeps, so
/// a record written late of something seen before does not count). A typed text reached it when
/// `Base::send` returned (`delivered_at`); a pushed one at its last push (`last_attempt_at`), as
/// the turn that confirms a push is written before the delivery pass confirms it; one read from
/// the box when it was read (`delivered_at`).
fn parkable() -> String {
    format!(
        "r.status = 'in_use'
         AND (EXISTS (SELECT 1 FROM pending p WHERE p.run_id = r.id
                      AND p.status = 'escalated' AND p.kind = 'question')
              OR {landing})
         AND NOT EXISTS (SELECT 1 FROM pending p WHERE p.run_id = r.id
                         AND (p.status = 'open' OR (p.status = 'escalated' AND p.kind = 'prompt')))
         AND l.stage = 'verified' AND l.conversation_id IS NOT NULL
         AND NOT EXISTS (SELECT 1 FROM events g WHERE g.run_id = r.id AND g.kind = 'terminal_gone'
                         AND json_extract(g.payload_json, '$.spawn_id') = l.spawn_id)
         AND NOT EXISTS (SELECT 1 FROM event_deliveries d JOIN events m ON m.id = d.source_event_id
                         WHERE d.recipient = '{prefix}' || r.task_id
                           AND (d.status = 'pending'
                                OR (json_extract(m.payload_json, '$.turn') IS NOT NULL
                                    AND json_extract(m.payload_json, '$.spawn_id') = l.spawn_id
                                    AND {TURN} <= json_extract(m.payload_json, '$.turn'))
                                OR (json_extract(m.payload_json, '$.turn') IS NULL
                                    AND NOT EXISTS (SELECT 1 FROM events s
                                        WHERE s.run_id = r.id
                                          AND s.kind IN ('terminal_status', 'terminal_turn')
                                          AND json_extract(s.payload_json, '$.spawn_id') = l.spawn_id
                                          AND s.id > d.source_event_id
                                          AND s.occurred_at > CASE
                                              WHEN json_extract(m.payload_json, '$.typed')
                                              THEN coalesce(d.delivered_at, d.last_attempt_at,
                                                            d.created_at)
                                              ELSE coalesce(d.last_attempt_at, d.delivered_at,
                                                            d.created_at) END))))
         AND (SELECT json_extract(s.payload_json, '$.state') FROM events s
              WHERE s.run_id = r.id AND s.kind = 'terminal_status'
                AND json_extract(s.payload_json, '$.spawn_id') = l.spawn_id
              ORDER BY s.id DESC LIMIT 1) IN ('idle', 'done')
         AND (julianday('now') - julianday((SELECT occurred_at FROM events WHERE id = {LAST_SEEN})))
             * 86400000.0 >= ?1",
        prefix = crate::spec::WORKER_AGENT_PREFIX,
        landing = super::belt::LANDING_WAIT
    )
}

/// A run the records say may be parked.
struct Candidate {
    run: i64,
    task: String,
    spawn: String,
    /// The newest turn recorded for its terminal.
    turn: u64,
    idle_since: Option<String>,
}

/// How often the pass runs: it lists the base while a run is parked or may be.
const EVERY: Duration = Duration::from_millis(500);

pub struct Park {
    org: String,
    after: Duration,
    next: Instant,
}

impl Park {
    /// `org` is this organization's `makiba.org` (its canonical state folder).
    pub fn new(org: &str, after: Duration) -> Park {
        Park {
            org: org.into(),
            after,
            next: Instant::now(),
        }
    }

    /// One pass: wake the parked runs nothing holds, end what a parked run still has of a
    /// terminal, then park what may be parked.
    pub fn pass(&mut self, w: &StateWriter, base: &dyn Base) -> Result<Vec<String>> {
        if Instant::now() < self.next {
            return Ok(Vec::new());
        }
        self.next = Instant::now() + EVERY;
        let parked_now = |w: &StateWriter| -> Result<Vec<String>> {
            Ok(w.conn()
                .prepare("SELECT task_id FROM runs WHERE status = 'parked' ORDER BY id")?
                .query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?)
        };
        // Read first: a write transaction takes the write lock even when it changes nothing.
        if !parked_now(w)?.is_empty() {
            w.transaction(settle)?;
        }
        let candidates = self.candidates(w)?;
        let parked_left = parked_now(w)?;
        if candidates.is_empty() && parked_left.is_empty() {
            return Ok(Vec::new());
        }
        // Nothing is parked or ended while the base cannot be read.
        let Ok(terminals) = base.list() else {
            return Ok(Vec::new());
        };
        let ours = |task: &str| {
            terminals
                .iter()
                .filter(|t| {
                    t.tags.get(TAG_ORG) == Some(&self.org)
                        && t.tags.get(TAG_TASK_ID).map(String::as_str) == Some(task)
                })
                .collect::<Vec<_>>()
        };
        for task in &parked_left {
            for t in ours(task) {
                if let Err(e) = base.end(&t.id) {
                    eprintln!(
                        "makibad: cannot end the terminal {} of parked {task}: {e}",
                        t.id.0
                    );
                }
            }
        }
        let mut parked = Vec::new();
        for c in candidates {
            // The base once more: a turn the monitor has not written yet is not cut.
            // Exactly one terminal of the launch, alive: a worker that exited or vanished is the
            // anomaly rules' and a failure, never parked.
            let mine: Vec<_> = terminals
                .iter()
                .filter(|t| t.tags.get(TAG_SPAWN) == Some(&c.spawn))
                .collect();
            let quiet = mine.len() == 1
                && mine.iter().all(|t| {
                    t.alive
                        && t.exit_code.is_none()
                        && t.status
                            .as_ref()
                            .is_some_and(|s| matches!(s.state, State::Idle | State::Done))
                        && t.turn.unwrap_or(0) <= c.turn
                });
            if !quiet || !self.park(w, &c)? {
                continue;
            }
            for t in ours(&c.task) {
                if let Err(e) = base.end(&t.id) {
                    eprintln!(
                        "makibad: cannot end the terminal {} of parked {}: {e}",
                        t.id.0, c.task
                    );
                }
            }
            parked.push(c.task);
        }
        Ok(parked)
    }

    fn after_ms(&self) -> f64 {
        self.after.as_millis() as f64
    }

    fn candidates(&self, w: &StateWriter) -> Result<Vec<Candidate>> {
        let sql = format!(
            "SELECT r.id, r.task_id, l.spawn_id, {TURN},
                    (SELECT occurred_at FROM events WHERE id = {LAST_SEEN})
             FROM runs r JOIN {NEWEST} WHERE {} ORDER BY r.id",
            parkable()
        );
        let mut st = w.conn().prepare(&sql)?;
        let rows = st
            .query_map([self.after_ms()], |r| {
                Ok(Candidate {
                    run: r.get(0)?,
                    task: r.get(1)?,
                    spawn: r.get(2)?,
                    turn: r.get::<_, i64>(3)?.max(0) as u64,
                    idle_since: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Park `c` if the records still allow it: `false` when they no longer do.
    fn park(&self, w: &StateWriter, c: &Candidate) -> Result<bool> {
        w.transaction(|w| {
            let n = w.conn().execute(
                &format!(
                    "UPDATE runs SET status = 'parked' WHERE id IN (
                       SELECT r.id FROM runs r JOIN {NEWEST}
                       WHERE r.id = ?2 AND l.spawn_id = ?3 AND {})",
                    parkable()
                ),
                params![self.after_ms(), c.run, c.spawn],
            )?;
            if n == 0 {
                return Ok(false);
            }
            w.conn().execute(
                "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE role = 'worker' AND run_id = ?1 AND revoked_at IS NULL",
                [c.run],
            )?;
            let pending: Vec<i64> = w
                .conn()
                .prepare(
                    "SELECT id FROM pending WHERE run_id = ?1 AND status = 'escalated' ORDER BY id",
                )?
                .query_map([c.run], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            // A run waiting for a landing (Issue 284): the Issues it waits for.
            let landing: bool = w.conn().query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM runs r WHERE r.id = ?1 AND {})",
                    super::belt::LANDING_WAIT
                ),
                [c.run],
                |r| r.get(0),
            )?;
            let waits_for = if landing {
                super::ahead::open_wait(w.conn(), &c.task)?.map(|wait| wait.issues)
            } else {
                None
            };
            w.append_event(&NewEvent {
                kind: "run_parked",
                actor: Some("makibad"),
                payload: Some(&json!({
                    "task_id": c.task, "spawn_id": c.spawn, "pending": pending,
                    "idle_since": c.idle_since, "waits_for": waits_for,
                })),
                run_task_id: Some(&c.task),
                ..Default::default()
            })?;
            Ok(true)
        })
    }
}

/// Bring the parked run `task` back, in the caller's transaction: `suspended`, a relaunch resuming
/// its newest verified conversation, `run_unparked` with `reason` and `detail`. `false`, and
/// nothing written, when the run is not parked.
pub fn unpark(w: &StateWriter, task: &str, reason: &str, detail: Value) -> Result<bool> {
    w.transaction(|w| {
        let n = w.conn().execute(
            "UPDATE runs SET status = 'suspended' WHERE task_id = ?1 AND status = 'parked'",
            [task],
        )?;
        if n == 0 {
            return Ok(false);
        }
        let (from, conversation, pending_max): (i64, Option<String>, i64) = w.conn().query_row(
            "SELECT (SELECT max(id) FROM run_launch WHERE run_id = r.id),
                    (SELECT v.conversation_id FROM run_launch v WHERE v.run_id = r.id
                       AND v.stage = 'verified' ORDER BY v.id DESC LIMIT 1),
                    coalesce((SELECT max(id) FROM pending WHERE run_id = r.id), 0)
             FROM runs r WHERE r.task_id = ?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let spawn = super::launch::add_relaunch(w, from, conversation.as_deref(), "makibad")?;
        w.append_event(&NewEvent {
            kind: "run_unparked",
            actor: Some("makibad"),
            payload: Some(&json!({
                "task_id": task, "reason": reason, "detail": detail, "spawn_id": spawn,
                "pending_max": pending_max,
            })),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        Ok(true)
    })
}

/// `unpark` for something makibad saw itself (a failed CI, a landing), with a notice to the woken
/// worker saying what, so that its first turn has the reason to read. Nothing for a run not parked.
pub fn wake_with_notice(
    w: &StateWriter,
    task: &str,
    reason: &str,
    detail: Value,
    body: &str,
) -> Result<bool> {
    w.transaction(|w| {
        if !unpark(w, task, reason, detail)? {
            return Ok(false);
        }
        super::door::notice(w, &crate::spec::worker_agent_id(task), task, body)?;
        Ok(true)
    })
}

/// The parked run of the worker `agent` (`w-<task_id>`), if it is one.
pub fn parked_worker(w: &StateWriter, agent: &str) -> Result<Option<String>> {
    let Some(task) = agent.strip_prefix(crate::spec::WORKER_AGENT_PREFIX) else {
        return Ok(None);
    };
    Ok(w.conn()
        .query_row(
            "SELECT task_id FROM runs WHERE task_id = ?1 AND status = 'parked'",
            [task],
            |r| r.get(0),
        )
        .optional()?)
}

/// Wake every parked run that no longer has an escalated question, in the caller's transaction:
/// its last one was answered or withdrawn (by the secretary, the person or `makibad`), so nothing
/// would bring it back otherwise. The reason is what became of its newest entry. A run waiting
/// for a landing (Issue 284) is left parked: its landing wakes it (`ahead::settle`).
pub fn settle(w: &StateWriter) -> Result<()> {
    type Row = (
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    let runs: Vec<Row> = w
        .conn()
        .prepare(&format!(
            "SELECT r.task_id, p.status, p.id, p.question, p.answer FROM runs r
             LEFT JOIN pending p ON p.id = (SELECT max(id) FROM pending WHERE run_id = r.id)
             WHERE r.status = 'parked'
               AND NOT EXISTS (SELECT 1 FROM pending q WHERE q.run_id = r.id
                               AND q.status = 'escalated' AND q.kind = 'question')
               AND NOT {}
             ORDER BY r.id",
            super::belt::LANDING_WAIT
        ))?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for (task, status, id, question, answer) in runs {
        let reason = match status.as_deref() {
            Some("answered") => "answered",
            Some("withdrawn") => "withdrawn",
            _ => "not_waiting",
        };
        // The worker is told what became of what it waited for: nothing else reaches it, and it
        // cannot read the pending entries itself.
        let what = match (id, question.as_deref()) {
            (Some(id), Some(q)) => format!(
                "Pending entry {id} ({}) was {reason}{}",
                q.lines().next().unwrap_or_default(),
                answer.map(|a| format!(" with {a:?}")).unwrap_or_default()
            ),
            _ => "Nothing is escalated to the person for you any more".to_string(),
        };
        let body = format!(
            "{what}. You no longer wait for the person; ask the secretary if you still need an answer, else go on with the task."
        );
        wake_with_notice(w, &task, reason, json!(id), &body)?;
    }
    Ok(())
}

/// The read surface (spec section 12): `waiting` for a run with an escalated entry, `unparked`
/// for a run not parked that was woken. Added to each row of `runs[]` by its `task_id`.
pub fn read_fields(c: &rusqlite::Connection, task: &str, status: &str) -> rusqlite::Result<Value> {
    let mut out = serde_json::Map::from_iter([
        ("waiting".to_string(), Value::Null),
        ("unparked".to_string(), Value::Null),
    ]);
    let pending: Vec<(i64, String)> = c
        .prepare(
            "SELECT p.id, p.question FROM pending p JOIN runs r ON r.id = p.run_id
             WHERE r.task_id = ?1 AND p.status = 'escalated' ORDER BY p.id",
        )?
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if let Some((_, newest)) = pending.last() {
        out.insert(
            "waiting".into(),
            json!({
                "for": "person",
                "pending": pending.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                "why": newest.lines().next().unwrap_or_default(),
            }),
        );
    } else if let Some(landing) = super::ahead::waiting_field(c, task)? {
        // A design-only run waiting for a landing (Issue 284).
        out.insert("waiting".into(), landing);
    }
    if status != "parked" {
        let woken: Option<(String, String)> = c
            .query_row(
                "SELECT e.payload_json, e.occurred_at FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE r.task_id = ?1 AND e.kind = 'run_unparked' ORDER BY e.id DESC LIMIT 1",
                [task],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((payload, at)) = woken {
            let p: Value = serde_json::from_str(&payload).unwrap_or_default();
            out.insert(
                "unparked".into(),
                json!({"reason": p["reason"], "detail": p["detail"], "at": at}),
            );
        }
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::writer::RunUpsert;

    /// A run `t` at work with a verified launch `s1` of conversation `c1`, one live worker key,
    /// and an idle status of its terminal written `ago_ms` ago.
    fn org(ago_ms: i64) -> (tempfile::TempDir, StateWriter) {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute_batch(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
                 VALUES (1, 's1', 'fake', 'b', '/w', 'verified', 'c1');
                 INSERT INTO agents (agent_id, role, run_id, key_sha256, spawn_id)
                 VALUES ('w-t', 'worker', 1, 'h1', 's1');",
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO events (kind, run_id, occurred_at, payload_json)
                 VALUES ('terminal_status', 1,
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1),
                         json_object('spawn_id', 's1', 'state', 'idle'))",
                [format!("-{} seconds", ago_ms as f64 / 1000.0)],
            )
            .unwrap();
        (dir, w)
    }

    fn entry(w: &StateWriter, kind: &str, status: &str) -> i64 {
        w.conn()
            .execute(
                "INSERT INTO pending (run_id, kind, question, status) VALUES (1, ?1, 'q?', ?2)",
                [kind, status],
            )
            .unwrap();
        w.conn().last_insert_rowid()
    }

    fn status(w: &StateWriter) -> String {
        w.conn()
            .query_row("SELECT status FROM runs WHERE id = 1", [], |r| r.get(0))
            .unwrap()
    }

    /// One pass on a base where the launch `s1` has one terminal, alive and idle.
    fn park(w: &StateWriter) -> Vec<String> {
        let stub = crate::base::stub::Stub::new();
        let t = terminal(&stub);
        stub.set_state(&t, State::Idle);
        Park::new("/org", Duration::from_millis(1000))
            .pass(w, &stub)
            .unwrap()
    }

    fn terminal(stub: &crate::base::stub::Stub) -> crate::base::TerminalRef {
        stub.launch(&crate::base::LaunchSpec {
            argv: vec![],
            cwd: "/".into(),
            name: "w-t".into(),
            tags: std::collections::BTreeMap::from([
                (TAG_SPAWN.to_string(), "s1".to_string()),
                (TAG_ORG.to_string(), "/org".to_string()),
                (TAG_TASK_ID.to_string(), "t".to_string()),
            ]),
            env: std::collections::BTreeMap::new(),
            unset: vec![],
        })
        .unwrap()
    }

    /// A verified launch with no conversation to resume, a terminal recorded gone, or a base with
    /// no live terminal of the launch: the run is not parked (it would lose its context, or it is
    /// a failure for the anomaly rules).
    #[test]
    fn a_run_that_could_not_come_back_or_whose_worker_is_gone_is_not_parked() {
        let (_d, w) = org(5000);
        entry(&w, "question", "escalated");
        w.conn()
            .execute("UPDATE run_launch SET conversation_id = NULL", [])
            .unwrap();
        assert!(park(&w).is_empty());

        let (_d, w) = org(5000);
        entry(&w, "question", "escalated");
        w.conn()
            .execute(
                "INSERT INTO events (kind, run_id, payload_json)
                 VALUES ('terminal_gone', 1, json_object('spawn_id', 's1', 'reason', 'finished'))",
                [],
            )
            .unwrap();
        assert!(park(&w).is_empty());

        let once = |stub: &crate::base::stub::Stub| {
            let (d, w) = org(5000);
            entry(&w, "question", "escalated");
            let out = Park::new("/org", Duration::from_millis(1000))
                .pass(&w, stub)
                .unwrap();
            (d, w, out)
        };
        let stub = crate::base::stub::Stub::new();
        let (_d, w, out) = once(&stub);
        assert!(out.is_empty(), "no terminal at all");
        assert_eq!(status(&w), "in_use");
        let t = terminal(&stub);
        stub.set_state(&t, State::Idle);
        stub.exit(&t, 1);
        let (_d, w, out) = once(&stub);
        assert!(out.is_empty(), "an exited terminal");
        assert_eq!(status(&w), "in_use");
    }

    #[test]
    fn an_escalated_question_after_the_wait_parks_and_revokes() {
        let (_d, w) = org(5000);
        entry(&w, "question", "escalated");
        assert_eq!(park(&w), ["t"]);
        assert_eq!(status(&w), "parked");
        let live: i64 = w
            .conn()
            .query_row(
                "SELECT count(*) FROM agents WHERE revoked_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, 0);
    }

    #[test]
    fn what_does_not_wait_only_for_the_person_is_not_parked() {
        // Too recent.
        let (_d, w) = org(100);
        entry(&w, "question", "escalated");
        assert!(park(&w).is_empty());
        // An open entry: the secretary's to answer.
        let (_d, w) = org(5000);
        entry(&w, "question", "escalated");
        entry(&w, "question", "open");
        assert!(park(&w).is_empty());
        // A prompt needs its terminal.
        let (_d, w) = org(5000);
        entry(&w, "question", "escalated");
        entry(&w, "prompt", "escalated");
        assert!(park(&w).is_empty());
        // No escalated question.
        let (_d, w) = org(5000);
        assert!(park(&w).is_empty());
        // In review.
        let (_d, w) = org(5000);
        entry(&w, "question", "escalated");
        w.conn()
            .execute("UPDATE runs SET status = 'review'", [])
            .unwrap();
        assert!(park(&w).is_empty());
        // A message still waiting for delivery to the worker.
        let (_d, w) = org(5000);
        entry(&w, "question", "escalated");
        w.conn()
            .execute_batch(
                "INSERT INTO events (kind) VALUES ('message_sent');
                 INSERT INTO event_deliveries (source_event_id, recipient)
                 VALUES ((SELECT max(id) FROM events), 'w-t');",
            )
            .unwrap();
        assert!(park(&w).is_empty());
        assert_eq!(status(&w), "in_use");
    }

    /// A message typed with no turn counter (`typed_at` seconds ago, delivered then), followed by
    /// a terminal record of the launch that arrived `seen` seconds ago, written after the message.
    fn typed_then_seen(typed_at: i64, seen: i64) -> (tempfile::TempDir, StateWriter) {
        let (d, w) = org(20_000);
        entry(&w, "question", "escalated");
        let ago = |s: i64| format!("-{s} seconds");
        w.conn()
            .execute(
                "INSERT INTO events (kind, payload_json) VALUES ('message_sent',
                   json_object('typed', json('true'), 'spawn_id', 's1'))",
                [],
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, delivered_at)
                 VALUES ((SELECT max(id) FROM events), 'w-t', 'delivered',
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1))",
                [ago(typed_at)],
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO events (kind, run_id, occurred_at, payload_json)
                 VALUES ('terminal_status', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1),
                         json_object('spawn_id', 's1', 'state', 'idle'))",
                [ago(seen)],
            )
            .unwrap();
        (d, w)
    }

    /// Issue 332, with no turn counter: an observation the monitor writes after a typed message
    /// but that arrived before the text was typed does not let the run park; one that arrived
    /// after it does.
    #[test]
    fn a_typed_message_without_a_turn_keeps_the_run_until_a_record_that_arrived_after_it() {
        let (_d, w) = typed_then_seen(3, 5);
        assert!(park(&w).is_empty());
        assert_eq!(status(&w), "in_use");
        let (_d, w) = typed_then_seen(10, 5);
        assert_eq!(park(&w), ["t"]);
    }

    /// A message pushed by the channel 10 seconds ago is confirmed by the delivery pass only after
    /// the monitor wrote its turn (`working` 8 seconds ago, `idle` 5): that turn releases the run,
    /// although both records arrived before `delivered_at`.
    #[test]
    fn a_pushed_message_is_released_by_the_turn_after_its_push_even_when_confirmed_later() {
        let (_d, w) = org(20_000);
        entry(&w, "question", "escalated");
        w.conn()
            .execute_batch(
                "INSERT INTO events (kind) VALUES ('message_sent');
                 INSERT INTO event_deliveries (source_event_id, recipient, status, attempt,
                                               first_attempt_at, last_attempt_at, delivered_at)
                 VALUES ((SELECT max(id) FROM events), 'w-t', 'delivered', 1,
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 seconds'),
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 seconds'),
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 seconds'));
                 INSERT INTO events (kind, run_id, occurred_at, payload_json)
                 VALUES ('terminal_status', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-8 seconds'),
                         json_object('spawn_id', 's1', 'state', 'working'));
                 INSERT INTO events (kind, run_id, occurred_at, payload_json)
                 VALUES ('terminal_status', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds'),
                         json_object('spawn_id', 's1', 'state', 'idle'));",
            )
            .unwrap();
        assert_eq!(park(&w), ["t"]);
    }

    #[test]
    fn unpark_relaunches_on_the_conversation_and_the_run_counts_again() {
        let (_d, w) = org(5000);
        let first = entry(&w, "question", "escalated");
        entry(&w, "question", "escalated");
        park(&w);
        let waiting = |w: &StateWriter| -> i64 {
            w.conn()
                .query_row(
                    &format!(
                        "SELECT count(*) FROM runs r WHERE {}",
                        super::super::belt::WAITING
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(waiting(&w), 1);
        assert!(unpark(&w, "t", "message", json!("secretary")).unwrap());
        assert!(!unpark(&w, "t", "message", json!("secretary")).unwrap());
        assert_eq!(status(&w), "suspended");
        let (conv, stage): (Option<String>, String) = w
            .conn()
            .query_row(
                "SELECT conversation_id, stage FROM run_launch ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (conv.as_deref(), stage.as_str()),
            (Some("c1"), "worktree_ready")
        );
        // Woken with two questions still escalated: it counts again (finding 6, F4).
        assert_eq!(waiting(&w), 0);
        // Answering the first leaves the second: nothing changes for the count.
        w.conn()
            .execute(
                "UPDATE pending SET status = 'answered' WHERE id = ?1",
                [first],
            )
            .unwrap();
        assert_eq!(waiting(&w), 0);
        // A new question after the wake: waiting again.
        entry(&w, "question", "escalated");
        assert_eq!(waiting(&w), 1);
    }

    #[test]
    fn settle_wakes_a_parked_run_whose_last_question_went() {
        let (_d, w) = org(5000);
        let id = entry(&w, "question", "escalated");
        park(&w);
        w.transaction(settle).unwrap();
        assert_eq!(status(&w), "parked");
        w.conn()
            .execute(
                "UPDATE pending SET status = 'withdrawn' WHERE id = ?1",
                [id],
            )
            .unwrap();
        w.transaction(settle).unwrap();
        assert_eq!(status(&w), "suspended");
        let reason: String = w
            .conn()
            .query_row(
                "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'run_unparked'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "withdrawn");
        // The worker is told which entry went, since nothing else reaches it.
        let told: String = w
            .conn()
            .query_row(
                "SELECT body FROM messages WHERE recipient = 'w-t' AND kind = 'notice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            told.starts_with(&format!("Pending entry {id} (q?) was withdrawn.")),
            "{told}"
        );
    }
}
