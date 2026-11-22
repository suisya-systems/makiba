//! Taking queued runs within capacity (Issue M3-1): the secretary asks, the dispatcher decides
//! when there is room.
//!
//! Each pass takes `queued` runs oldest `dispatched_at` first while the runs `in_use` plus the
//! launches not yet `verified` or `failed` are fewer than `max_concurrent_workers`. A run is taken
//! by one `run_taken` event naming its `task_id` and the `spawn_id` of its newest `run_launch`
//! row, which is `planned`. Nothing else changes here: the steps that move that row on (the
//! worktree, the settings, the launch, M3-2 to M3-6) start from a `run_taken` event, and until
//! they do the taken launch keeps counting against capacity.

use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use serde_json::json;

/// The default of `max_concurrent_workers`.
pub const DEFAULT_CAPACITY: usize = 4;
/// Where the organization setting is read from until the `config` verb exists (M6).
pub const CAPACITY_ENV: &str = "MAKIBA_MAX_CONCURRENT_WORKERS";

/// `max_concurrent_workers`: the variable when it holds a number, otherwise the default.
pub fn capacity_from_env() -> usize {
    std::env::var(CAPACITY_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_CAPACITY)
}

/// A launch of `l` has been taken: its `run_taken` event exists.
const TAKEN: &str = "EXISTS (SELECT 1 FROM events e WHERE e.kind = 'run_taken'
                      AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id)";

/// The runs counted against `max_concurrent_workers`: `in_use`, or `queued` / `suspended` with a
/// taken launch not yet `verified` or `failed`; not a run that waits only for the person (an
/// escalated pending entry, belt.md 4.1).
pub fn busy(w: &StateWriter) -> Result<usize> {
    // A run counts once, however it is counted. Only a run still `queued` or `in_use` can be
    // waiting on a launch (ponytail: a launch left open on a run that ended is not counted).
    let n: i64 = w.conn().query_row(
        &format!(
            "SELECT count(*) FROM runs r WHERE (r.status = 'in_use'
             OR (r.status IN ('queued', 'suspended')
             AND EXISTS (SELECT 1 FROM run_launch l WHERE l.run_id = r.id
                         AND l.stage NOT IN ('verified', 'failed') AND {TAKEN})))
             AND NOT {}",
            super::belt::WAITING
        ),
        [],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

pub struct Dispatch {
    capacity: usize,
}

impl Dispatch {
    pub fn new(capacity: usize) -> Dispatch {
        Dispatch { capacity }
    }

    /// One pass: returns the task ids taken, oldest first.
    pub fn pass(&self, w: &StateWriter) -> Result<Vec<String>> {
        let mut taken = Vec::new();
        // One transaction per run, so the count and the event are never apart.
        while let Some(task) = w.transaction(|w| self.take_one(w))? {
            super::crash::point("run_taken");
            taken.push(task);
        }
        Ok(taken)
    }

    fn take_one(&self, w: &StateWriter) -> Result<Option<String>> {
        if busy(w)? >= self.capacity {
            return Ok(None);
        }
        // The oldest queued run whose newest launch is `planned` and not taken yet; a run that
        // waits only for the person is not launched while it waits.
        let next: Option<(String, String)> = {
            let mut st = w.conn().prepare(&format!(
                "SELECT r.task_id, l.spawn_id FROM runs r
                 JOIN run_launch l ON l.id = (SELECT max(id) FROM run_launch WHERE run_id = r.id)
                 WHERE r.status = 'queued' AND l.stage = 'planned' AND NOT {TAKEN} AND NOT {}
                 ORDER BY r.dispatched_at, r.id LIMIT 1",
                super::belt::WAITING
            ))?;
            let mut rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.next().transpose()?
        };
        let Some((task, spawn)) = next else {
            return Ok(None);
        };
        w.append_event(&NewEvent {
            kind: "run_taken",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": task, "spawn_id": spawn})),
            run_task_id: Some(&task),
            ..Default::default()
        })?;
        Ok(Some(task))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::writer::RunUpsert;

    fn run(w: &StateWriter, task: &str, status: &str, at: &str, stage: &str) {
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some(status),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET dispatched_at = ?2 WHERE task_id = ?1",
                [task, at],
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'fake', 'b', '/w', ?3)",
                [task, &format!("s-{task}"), stage],
            )
            .unwrap();
    }

    /// Issue M3-1: the count is runs `in_use` plus launches not yet `verified` or `failed`.
    #[test]
    fn runs_in_use_and_launches_under_way_both_count() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        run(&w, "busy", "in_use", "2026-11-18T10:00:00.000Z", "verified");
        run(&w, "old", "failed", "2026-11-18T10:00:01.000Z", "failed");
        run(&w, "a", "queued", "2026-11-18T10:00:10.000Z", "planned");
        run(&w, "b", "queued", "2026-11-18T10:00:20.000Z", "planned");
        let d = Dispatch::new(2);
        assert_eq!(d.pass(&w).unwrap(), vec!["a"]);
        assert!(
            d.pass(&w).unwrap().is_empty(),
            "a taken launch is not taken twice"
        );
        // The taken launch ends: its place is free.
        w.conn()
            .execute(
                "UPDATE run_launch SET stage = 'failed' WHERE spawn_id = 's-a'",
                [],
            )
            .unwrap();
        w.conn()
            .execute("UPDATE runs SET status = 'failed' WHERE task_id = 'a'", [])
            .unwrap();
        assert_eq!(d.pass(&w).unwrap(), vec!["b"]);
    }
}
