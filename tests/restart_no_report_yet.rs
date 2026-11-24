//! Issue 426, review: a resumed run that has made no report yet still goes through the anomaly
//! pass. The exemption of `tests/restart_done_turn.rs` reads the run's newest report; with none it
//! is no exemption, and it does not stop the pass, so the run itself and every other run still
//! raise `done_no_report` (G7). The scenes use the in-database style of `tests/restart_done_turn.rs`.

use makiba::daemon::anomalies::Anomalies;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use serde_json::{Value, json};

/// `T0 + secs`, as the database writes a time.
fn ts(secs: i64) -> String {
    let s = 10 * 3600 + secs;
    format!(
        "2026-11-24T{:02}:{:02}:{:02}.000Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

fn event(w: &StateWriter, task: &str, kind: &str, at: i64, payload: Value) {
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(&payload),
        occurred_at: Some(&ts(at)),
        run_task_id: Some(task),
        ..Default::default()
    })
    .unwrap();
}

/// A run `task` launched at 0 with `spawn` (resuming `c1` with `prompt` `resume` when `resumed`),
/// working at 5 and done at 10, with `turns` turn ends of its terminal at 10 (turn 1, 2, ...) and
/// no report.
fn run(w: &StateWriter, task: &str, resumed: bool, turns: u64) {
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    let spawn = format!("spawn-{task}");
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, conversation_id,
                                     stage, created_at)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'fake', 'b', '/w', ?3,
                     'verified', ?4)",
            rusqlite::params![task, spawn, resumed.then_some("c1"), ts(0)],
        )
        .unwrap();
    let prompt = if resumed { "resume" } else { "brief" };
    let spawned = json!({"task_id": task, "spawn_id": spawn, "prompt": prompt});
    event(w, task, "worker_spawned", 0, spawned);
    for (at, state) in [(5, "working"), (10, "done")] {
        let p = json!({"task_id": task, "role": "worker", "spawn_id": spawn,
                       "state": state, "kind": null, "msg": null});
        event(w, task, "terminal_status", at, p);
    }
    for n in 1..=turns {
        let p = json!({"task_id": task, "role": "worker", "spawn_id": spawn, "turn": n});
        event(w, task, "terminal_turn", 10, p);
    }
}

/// The tasks one pass at `now` over the scene `build` writes raise `done_no_report` for.
fn raised(build: impl Fn(&StateWriter), now: i64) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    build(&w);
    Anomalies::new().pass_at(&w, &ts(now)).unwrap();
    let mut q = w
        .conn()
        .prepare(
            "SELECT json_extract(payload_json, '$.task_id') FROM events
             WHERE kind = 'anomaly_raised'
               AND json_extract(payload_json, '$.rule') = 'done_no_report'
             ORDER BY 1",
        )
        .unwrap();
    q.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn a_resumed_run_with_no_report_and_no_turn_raises_and_stops_nothing() {
    let scene = |w: &StateWriter| {
        run(w, "r", true, 0);
        run(w, "o", false, 1);
    };
    assert_eq!(raised(scene, 10 + 120), ["o", "r"]);
}

#[test]
fn a_resumed_run_with_no_report_and_turn_one_raises_and_stops_nothing() {
    let scene = |w: &StateWriter| {
        run(w, "r", true, 1);
        run(w, "o", false, 1);
    };
    assert_eq!(raised(scene, 10 + 120), ["o", "r"]);
}
