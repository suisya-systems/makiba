//! M2-6: the notify gate (Issue 21). The rules carried from claude-org-ja `tools/notify_gate.py`
//! and their cases from `tools/test_notify_gate.py`, then the gate where the anomaly pass queues
//! a notice.
//!
//! The observable contract fixed here (the spec prescribes no metadata for a suppression):
//! - `decide_*` return a `Decision { send, reason }`; the reason strings are the carried ones;
//! - `decide(kind)` is the gate for what makiba queues, by the anomaly rule name (or notice kind);
//!   `secretary_gone` and `secretary_still` are not sent (reason `person_only`), and a kind the gate
//!   does not know is sent (reason `unknown_kind`), as in the carried code (fail loud);
//! - a suppressed anomaly is still raised: its `anomaly_raised` payload gains `"suppressed"`, the
//!   reason. A sent one has no such field. No `message_sent` event, no `messages` row and no
//!   `event_deliveries` row exist for a suppressed one.
//! - the three carried rules have no producer in makiba yet: they are kept as pure rules only.

use makiba::daemon::anomalies::Anomalies;
use makiba::daemon::notify_gate::{
    Decision, decide, decide_curator_vanished, decide_event_cursor_reset, decide_retro_recorded,
};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use serde_json::{Value, json};
use tempfile::TempDir;

fn d(send: bool, reason: &'static str) -> Decision {
    Decision { send, reason }
}

// ---- the carried cases (test_notify_gate.py), ported ----

#[test]
fn cursor_reset_absent_state_suppresses() {
    assert_eq!(
        decide_event_cursor_reset(None),
        d(false, "no_background_worker")
    );
}

#[test]
fn cursor_reset_empty_and_same_tab_only_suppress() {
    assert!(!decide_event_cursor_reset(Some(&json!({}))).send);
    let st = json!({"worker-a": {"placement": "same_tab"}, "worker-b": {"idle_streak_cycles": 1}});
    assert!(!decide_event_cursor_reset(Some(&st)).send);
}

#[test]
fn cursor_reset_one_background_worker_sends() {
    let st = json!({
        "worker-a": {"placement": "same_tab"},
        "worker-b": {"placement": "background_tab"},
    });
    assert_eq!(
        decide_event_cursor_reset(Some(&st)),
        d(true, "background_worker_present")
    );
}

#[test]
fn cursor_reset_unknown_placement_and_garbage_send() {
    assert_eq!(
        decide_event_cursor_reset(Some(&json!({"w": {"placement": "x"}}))),
        d(true, "unknown_placement")
    );
    assert_eq!(
        decide_event_cursor_reset(Some(&json!({"w": 3}))),
        d(true, "unreadable_state")
    );
    assert_eq!(
        decide_event_cursor_reset(Some(&json!([]))),
        d(true, "unreadable_state")
    );
    // JSON null is not an absent file: it is unreadable, and sends.
    assert_eq!(
        decide_event_cursor_reset(Some(&Value::Null)),
        d(true, "unreadable_state")
    );
    // A background worker wins over a record that cannot be read after it.
    assert!(decide_event_cursor_reset(Some(&json!({"a": {"placement": "background_tab"}}))).send);
}

#[test]
fn curator_vanished_table() {
    assert_eq!(
        decide_curator_vanished(Some("done")),
        d(false, "result_already_received")
    );
    assert!(!decide_curator_vanished(Some("skipped")).send);
    for v in [Some("null"), None, Some("error"), Some("weird")] {
        assert_eq!(decide_curator_vanished(v), d(true, "no_result_received"));
    }
}

#[test]
fn retro_recorded_never_sends() {
    assert_eq!(decide_retro_recorded(), d(false, "info_only"));
}

// ---- the gate for what makiba queues ----

#[test]
fn the_person_rules_are_not_sent_and_every_other_kind_is() {
    assert_eq!(decide("secretary_gone"), d(false, "person_only"));
    assert_eq!(decide("secretary_still"), d(false, "person_only"));
    for k in [
        "blocked_question",
        "blocked_permission",
        "blocked_auth",
        "error",
        "gone",
        "done_no_report",
        "no_status",
        "unknown_terminal",
    ] {
        assert!(decide(k).send, "{k}");
    }
    // Fail loud: what the gate does not know is sent.
    assert_eq!(decide("something_new"), d(true, "unknown_kind"));
    assert_eq!(decide(""), d(true, "unknown_kind"));
}

// ---- the gate where the anomaly pass queues a notice ----

struct Db {
    w: StateWriter,
    _dir: TempDir,
}

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    Db { w, _dir: dir }
}

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

fn count(w: &StateWriter, sql: &str) -> i64 {
    w.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A secretary launched at T0 that never reports a status: `secretary_gone` holds from 180 s.
fn silent_secretary(w: &StateWriter) {
    w.conn()
        .execute(
            "INSERT INTO agents (agent_id, role, key_sha256, created_at)
             VALUES ('secretary', 'secretary', 'k', ?1)",
            [ts(0)],
        )
        .unwrap();
}

/// A run launched at T0 whose terminal reports a permission prompt at 20 s.
fn blocked_run(w: &StateWriter) {
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: "p",
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, created_at)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 's', 'fake', 'b', '/w', ?1)",
            [ts(0)],
        )
        .unwrap();
    let p = json!({"task_id": "t1", "role": "worker", "state": "blocked", "kind": "permission",
                   "msg": null});
    w.append_event(&NewEvent {
        kind: "terminal_status",
        actor: Some("makibad"),
        payload: Some(&p),
        occurred_at: Some(&ts(20)),
        run_task_id: Some("t1"),
        ..Default::default()
    })
    .unwrap();
}

fn raised(w: &StateWriter, rule: &str) -> Vec<Value> {
    let mut q = w
        .conn()
        .prepare("SELECT payload_json FROM events WHERE kind = 'anomaly_raised' ORDER BY id")
        .unwrap();
    q.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|p| serde_json::from_str::<Value>(&p.unwrap()).unwrap())
        .filter(|p| p["rule"] == rule)
        .collect()
}

fn queue_artifacts(w: &StateWriter) -> (i64, i64, i64) {
    (
        count(w, "SELECT COUNT(*) FROM events WHERE kind = 'message_sent'"),
        count(w, "SELECT COUNT(*) FROM messages"),
        count(w, "SELECT COUNT(*) FROM event_deliveries"),
    )
}

#[test]
fn a_suppressed_anomaly_writes_anomaly_raised_and_queues_no_message() {
    let d = db();
    silent_secretary(&d.w);
    let mut a = Anomalies::new();
    a.pass_at(&d.w, &ts(180)).unwrap();

    let got = raised(&d.w, "secretary_gone");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["suppressed"], "person_only");
    assert_eq!(queue_artifacts(&d.w), (0, 0, 0));

    // The same scene again, in this process and in a new one: still one raise, still no message.
    a.pass_at(&d.w, &ts(181)).unwrap();
    Anomalies::new().pass_at(&d.w, &ts(182)).unwrap();
    assert_eq!(raised(&d.w, "secretary_gone").len(), 1);
    assert_eq!(queue_artifacts(&d.w), (0, 0, 0));
}

#[test]
fn an_anomaly_the_secretary_can_act_on_is_raised_and_queued_with_no_suppression_mark() {
    let d = db();
    blocked_run(&d.w);
    Anomalies::new().pass_at(&d.w, &ts(30)).unwrap();

    let got = raised(&d.w, "blocked_permission");
    assert_eq!(got.len(), 1);
    assert!(got[0].get("suppressed").is_none(), "{}", got[0]);
    assert_eq!(queue_artifacts(&d.w), (1, 1, 1));
}

#[test]
fn a_suppressed_and_a_sent_anomaly_in_one_pass_queue_only_the_sent_one() {
    let d = db();
    blocked_run(&d.w);
    silent_secretary(&d.w);
    Anomalies::new().pass_at(&d.w, &ts(190)).unwrap();

    assert_eq!(raised(&d.w, "secretary_gone").len(), 1);
    assert_eq!(raised(&d.w, "blocked_permission").len(), 1);
    assert_eq!(queue_artifacts(&d.w), (1, 1, 1));
    let body: String = d
        .w
        .conn()
        .query_row("SELECT body FROM messages", [], |r| r.get(0))
        .unwrap();
    assert!(body.contains("blocked_permission"), "{body}");
}

#[test]
fn a_suppressed_raise_is_rolled_back_with_its_event_when_a_write_fails() {
    // `anomaly_raised` is written in the same transaction as everything else of the raise: a
    // failing event write leaves nothing, and the next pass raises it once.
    let d = db();
    silent_secretary(&d.w);
    d.w.conn()
        .execute_batch(
            "CREATE TRIGGER no_raise BEFORE INSERT ON events WHEN NEW.kind = 'anomaly_raised'
             BEGIN SELECT RAISE(ABORT, 'no'); END;",
        )
        .unwrap();
    let mut a = Anomalies::new();
    assert!(a.pass_at(&d.w, &ts(180)).is_err());
    assert_eq!(raised(&d.w, "secretary_gone").len(), 0);
    d.w.conn().execute_batch("DROP TRIGGER no_raise").unwrap();
    a.pass_at(&d.w, &ts(181)).unwrap();
    a.pass_at(&d.w, &ts(182)).unwrap();
    assert_eq!(raised(&d.w, "secretary_gone").len(), 1);
    assert_eq!(queue_artifacts(&d.w), (0, 0, 0));
}
