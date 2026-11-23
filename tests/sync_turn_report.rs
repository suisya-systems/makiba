//! Issue 386: a worker turn that only brings in a branch updated at origin ends with a one-line
//! report, so it raises no `done_no_report`.
//!
//! The anomaly rule is unchanged (G7: a turn that ends with no report still shows). What changes
//! is the text the agents read: the worker's rules (every turn ends in a report; a sync-only turn
//! ends with one line), the update-branch skill (what the secretary sends) and the update notice
//! of `makibad` (`sync_request`). The scenes use the in-database style of
//! `tests/done_no_report_turn.rs`: explicit times (seconds after `T0`) and `Anomalies::pass_at`.

use makiba::daemon::anomalies::Anomalies;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use serde_json::{Value, json};
use std::path::Path;

/// The file at `rel` in the repository, with every run of whitespace collapsed to one space, so
/// that line wrapping does not matter.
fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let text =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_carries(what: &str, text: &str, phrases: &[&str]) {
    for phrase in phrases {
        assert!(
            text.contains(phrase),
            "{what} does not ask for the one-line report: {phrase:?} is missing"
        );
    }
}

#[test]
fn the_worker_rules_end_every_turn_in_a_report() {
    let rel = "roles/worker/rules.md";
    assert_carries(
        rel,
        &read(rel),
        &[
            "Every turn ends in a report",
            "also when the message asked for no change",
            "commits nothing ends with that one line alone",
            "git merge --ff-only origin/<your branch>`, and `report` `done` in one line",
            "fast-forwarded to <sha>, nothing committed",
        ],
    );
}

#[test]
fn the_update_branch_skill_asks_for_a_one_line_report() {
    let rel = "skills/update-branch/SKILL.md";
    assert_carries(
        rel,
        &read(rel),
        &[
            "git merge --ff-only origin/<branch>, and report done in one line",
            "fast-forwarded to <head>, nothing committed",
        ],
    );
}

#[test]
fn the_update_notice_asks_for_a_one_line_report() {
    let text = makiba::daemon::sync_request("b1");
    assert_carries(
        "the update notice",
        &text,
        &[
            "git merge --ff-only origin/b1",
            "report done in one line",
            "fast-forwarded to <head>, nothing committed",
        ],
    );
}

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

/// A run in review, launched at `T0`.
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

/// Turn 1 does the task and reports; the secretary then asks for a fast-forward (turn 2), which
/// ends with the one-line report when `one_line` holds.
fn sync_scene(one_line: bool) -> impl Fn(&StateWriter) {
    move |w: &StateWriter| {
        run(w);
        status(w, 5, "working");
        report(w, 20, "report_done");
        status(w, 21, "done");
        turn_end(w, 21, 1);
        message(w, 100, "secretary", "w-t1", "message");
        status(w, 101, "working");
        if one_line {
            report(w, 109, "report_done");
        }
        status(w, 110, "done");
        turn_end(w, 110, 2);
    }
}

#[test]
fn a_sync_turn_with_its_one_line_report_raises_no_done_no_report() {
    assert!(!raises(sync_scene(true), 110 + 120), "at +120");
    assert!(!raises(sync_scene(true), 110 + 600), "at +600");
}

#[test]
fn a_silent_sync_turn_still_raises_done_no_report() {
    assert!(!raises(sync_scene(false), 110 + 119), "before the wait");
    assert!(raises(sync_scene(false), 110 + 120), "after the wait");
}
