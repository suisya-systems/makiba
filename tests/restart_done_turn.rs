//! Issue 426: a worker resumed after a restart whose last report still stands (`report_done` or
//! `report_blocked`) obeys `RESUME_PROMPT` ("do nothing and do not report again"), so its first
//! turn after the resume raises no `done_no_report`.
//!
//! Only that turn is exempt (G7: any other turn that ends with no report still shows): the newest
//! launch's `worker_spawned` says it was given the resume line (`prompt` `resume`), the only turn
//! recorded since that launch is turn 1 of its own terminal, and the run's newest report is
//! `report_done` or `report_blocked`. The scenes use the in-database style of
//! `tests/sync_turn_report.rs`: explicit times (seconds after `T0`) and `Anomalies::pass_at`.

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

fn event(w: &StateWriter, kind: &str, at: i64, payload: Value) -> i64 {
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(&payload),
        occurred_at: Some(&ts(at)),
        run_task_id: Some("t1"),
        ..Default::default()
    })
    .unwrap()
}

/// A launch of `t1` at `at` with `spawn`, resuming `conversation` when given, and its
/// `worker_spawned` carrying `prompt` (no field when `None`).
fn launch(w: &StateWriter, at: i64, spawn: &str, conversation: Option<&str>, prompt: Option<&str>) {
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, conversation_id,
                                     stage, created_at)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), ?1, 'fake', 'b', '/w', ?2,
                     'verified', ?3)",
            rusqlite::params![spawn, conversation, ts(at)],
        )
        .unwrap();
    let mut p = json!({"task_id": "t1", "spawn_id": spawn});
    if let Some(prompt) = prompt {
        p["prompt"] = prompt.into();
    }
    event(w, "worker_spawned", at, p);
}

fn status(w: &StateWriter, at: i64, spawn: &str, state: &str) {
    let p = json!({"task_id": "t1", "role": "worker", "spawn_id": spawn,
                   "state": state, "kind": null, "msg": null});
    event(w, "terminal_status", at, p);
}

/// The end of turn `n` of the terminal of `spawn`.
fn turn_end(w: &StateWriter, at: i64, spawn: &str, n: u64) {
    let p = json!({"task_id": "t1", "role": "worker", "spawn_id": spawn, "turn": n});
    event(w, "terminal_turn", at, p);
}

/// A message of `kind` from `sender` to `recipient` about the run: the event and its row.
fn message(w: &StateWriter, at: i64, sender: &str, recipient: &str, kind: &str) {
    let id = event(w, "message_sent", at, json!({}));
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

/// How the relaunch at 100 was made.
#[derive(Clone, Copy)]
enum How {
    /// A resume given `RESUME_PROMPT`.
    Resume,
    /// A resume that wakes a parked run, given `UNPARK_PROMPT`.
    Unpark,
    /// A fresh launch given the brief, verified (so its conversation id is set).
    Fresh,
    /// A resume whose `worker_spawned` was recorded with no `prompt` (before Issue 426).
    Unrecorded,
}

/// Launch 1 does the task and makes the report `before` at 20, its turn 1 ends at 21; the
/// organization is restarted and the run relaunched at 100 (`how`); the new terminal works and
/// its turn `first_turn` ends at 110 with no report.
fn restart(before: &'static str, how: How, first_turn: u64) -> impl Fn(&StateWriter) {
    move |w: &StateWriter| {
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("review"),
            ..Default::default()
        })
        .unwrap();
        launch(w, 0, "spawn-1", None, Some("brief"));
        status(w, 5, "spawn-1", "working");
        report(w, 20, before);
        status(w, 21, "spawn-1", "done");
        turn_end(w, 21, "spawn-1", 1);
        match how {
            How::Resume => launch(w, 100, "spawn-2", Some("c1"), Some("resume")),
            How::Unpark => {
                launch(w, 100, "spawn-2", Some("c1"), Some("unpark"));
                event(
                    w,
                    "run_unparked",
                    100,
                    json!({"task_id": "t1", "spawn_id": "spawn-2", "reason": "answer"}),
                );
            }
            How::Fresh => launch(w, 100, "spawn-2", Some("c2"), Some("brief")),
            How::Unrecorded => launch(w, 100, "spawn-2", Some("c1"), None),
        }
        status(w, 105, "spawn-2", "working");
        status(w, 110, "spawn-2", "done");
        turn_end(w, 110, "spawn-2", first_turn);
    }
}

#[test]
fn a_resumed_run_that_reported_done_raises_nothing_for_a_silent_first_turn() {
    let scene = restart("report_done", How::Resume, 1);
    assert!(!raises(&scene, 110 + 120), "at +120");
    assert!(!raises(&scene, 110 + 600), "at +600");
}

#[test]
fn a_resumed_run_that_reported_blocked_raises_nothing_for_a_silent_first_turn() {
    let scene = restart("report_blocked", How::Resume, 1);
    assert!(!raises(&scene, 110 + 120), "at +120");
    assert!(!raises(&scene, 110 + 600), "at +600");
}

#[test]
fn a_resumed_run_whose_last_report_is_start_still_raises() {
    let scene = restart("report_start", How::Resume, 1);
    assert!(!raises(&scene, 110 + 119), "before the wait");
    assert!(raises(&scene, 110 + 120), "after the wait");
}

#[test]
fn a_start_report_after_the_resume_ends_the_exemption() {
    let base = restart("report_done", How::Resume, 1);
    let scene = move |w: &StateWriter| {
        base(w);
        report(w, 107, "report_start");
    };
    assert!(raises(scene, 110 + 120));
}

#[test]
fn a_silent_second_turn_after_the_resume_still_raises() {
    let base = restart("report_done", How::Resume, 1);
    let scene = move |w: &StateWriter| {
        base(w);
        message(w, 200, "secretary", "w-t1", "message");
        status(w, 201, "spawn-2", "working");
        status(w, 210, "spawn-2", "done");
        turn_end(w, 210, "spawn-2", 2);
    };
    assert!(!raises(&scene, 210 + 119), "before the wait");
    assert!(raises(&scene, 210 + 120), "after the wait");
}

#[test]
fn a_turn_counter_that_jumped_after_the_resume_still_raises() {
    assert!(raises(restart("report_done", How::Resume, 2), 110 + 120));
}

#[test]
fn an_unparked_run_still_raises_for_a_silent_first_turn() {
    assert!(raises(restart("report_blocked", How::Unpark, 1), 110 + 120));
}

#[test]
fn a_verified_fresh_relaunch_still_raises_for_a_silent_first_turn() {
    assert!(raises(restart("report_done", How::Fresh, 1), 110 + 120));
}

#[test]
fn a_resume_recorded_without_a_prompt_still_raises() {
    assert!(raises(
        restart("report_done", How::Unrecorded, 1),
        110 + 120
    ));
}
