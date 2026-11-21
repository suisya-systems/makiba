//! Issue 218: `done_no_report` counts a report made inside the turn that just ended.
//!
//! A worker reports as the last act of its turn, so its report (`message_sent` and its `messages`
//! row) is recorded before the `terminal_status done` and the `terminal_turn` of that same turn.
//! That report answers the rule; a report of an earlier turn does not answer a later turn that ends
//! with none. Same in-database style as `tests/anomalies.rs`: explicit times (seconds after `T0`)
//! and `Anomalies::pass_at` at an explicit `now`.

use makiba::daemon::anomalies::Anomalies;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use serde_json::{Value, json};

/// `T0 + secs`, as the database writes a time.
fn ts(secs: i64) -> String {
    let s = 10 * 3600 + secs;
    format!(
        "2026-11-18T{:02}:{:02}:{:02}.000Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

fn event(w: &StateWriter, kind: &str, task: Option<&str>, at: i64, payload: Value) -> i64 {
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(&payload),
        occurred_at: Some(&ts(at)),
        run_task_id: task,
        ..Default::default()
    })
    .unwrap()
}

/// A run in review (as in the host run of the Issue), launched at `T0`.
fn run(w: &StateWriter) {
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: "p",
        status: Some("review"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, created_at)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'fake', 'b', '/w', ?1)",
            [ts(0)],
        )
        .unwrap();
}

fn status(w: &StateWriter, at: i64, state: &str) {
    let p = json!({"task_id": "t1", "role": "worker", "spawn_id": "spawn-t1",
                   "state": state, "kind": null, "msg": null});
    event(w, "terminal_status", Some("t1"), at, p);
}

/// The end of turn `n` of the worker's terminal.
fn turn_end(w: &StateWriter, at: i64, n: u64) {
    let p = json!({"task_id": "t1", "role": "worker", "spawn_id": "spawn-t1", "turn": n});
    event(w, "terminal_turn", Some("t1"), at, p);
}

/// A message of `kind` from `sender` to `recipient` about the run: the event and its row.
fn message(w: &StateWriter, at: i64, sender: &str, recipient: &str, kind: &str) {
    let id = event(w, "message_sent", Some("t1"), at, json!({}));
    w.conn()
        .execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
             VALUES (?1, ?2, ?3, ?4, (SELECT id FROM runs WHERE task_id = 't1'), 'r')",
            rusqlite::params![id, sender, recipient, kind],
        )
        .unwrap();
}

fn report(w: &StateWriter, at: i64, kind: &str) {
    message(w, at, "w-t1", "secretary", kind);
}

/// Whether one pass at `now` over the scene `build` writes raises `done_no_report` for `t1`.
fn raises(build: impl Fn(&StateWriter), now: i64) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    build(&w);
    Anomalies::new().pass_at(&w, &ts(now)).unwrap();
    let n: i64 = w
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind = 'anomaly_raised'
               AND json_extract(payload_json, '$.task_id') = 't1'
               AND json_extract(payload_json, '$.rule') = 'done_no_report'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    n > 0
}

#[test]
fn a_report_inside_the_turn_that_just_ended_raises_no_done_no_report() {
    for kind in ["report_done", "report_blocked"] {
        // The order of the host run: the report, then done and the end of the same turn.
        let scene = |w: &StateWriter| {
            run(w);
            status(w, 5, "working");
            report(w, 20, kind);
            status(w, 21, "done");
            turn_end(w, 21, 1);
        };
        assert!(!raises(scene, 21 + 120), "{kind} at +120");
        assert!(!raises(scene, 21 + 600), "{kind} at +600");
    }
}

#[test]
fn a_report_in_turn_one_does_not_answer_turn_two() {
    let scene = |w: &StateWriter| {
        run(w);
        status(w, 5, "working");
        report(w, 20, "report_done");
        status(w, 21, "done");
        turn_end(w, 21, 1);
        // The secretary writes to the worker; turn 2 ends with no report.
        message(w, 100, "secretary", "w-t1", "message");
        status(w, 101, "working");
        status(w, 110, "done");
        turn_end(w, 110, 2);
    };
    assert!(!raises(scene, 110 + 119), "before the wait");
    assert!(raises(scene, 110 + 120), "after the wait");
}

#[test]
fn turn_two_waits_its_own_120_seconds_when_done_did_not_change() {
    // The `working` of turn 2 was not observed: the status stays the `done` of turn 1.
    let scene = |w: &StateWriter| {
        run(w);
        status(w, 5, "working");
        report(w, 20, "report_done");
        status(w, 21, "done");
        turn_end(w, 21, 1);
        turn_end(w, 300, 2);
    };
    assert!(!raises(scene, 300 + 119), "before the wait");
    assert!(raises(scene, 300 + 120), "after the wait");
}

#[test]
fn a_report_after_turn_two_ends_answers_it() {
    let scene = |w: &StateWriter| {
        run(w);
        status(w, 5, "working");
        report(w, 20, "report_done");
        status(w, 21, "done");
        turn_end(w, 21, 1);
        status(w, 101, "working");
        status(w, 110, "done");
        turn_end(w, 110, 2);
        report(w, 115, "report_done");
    };
    assert!(!raises(scene, 110 + 300));
}
