//! M2-3: the nine anomaly rules of spec section 11, run over what `state.db` holds.
//!
//! No daemon, no base and no real clock: each scene is written into a fresh database with explicit
//! times (seconds after `T0`), and `Anomalies::pass_at` is called with an explicit `now`. A rule's
//! scene is checked twice, one second (or one step) before it holds and when it holds.
//!
//! What the scenes rely on, all of it named by the spec:
//! - a run's records: `terminal_status`, `terminal_turn`, `terminal_gone` events of the run (section
//!   10), the launch time `run_launch.created_at` (5.3), and the reports as `messages` rows of kind
//!   `report_done` / `report_blocked` joined to their `message_sent` event (5.4);
//! - the secretary has no run: its records carry `role = "secretary"` and no task id, and its
//!   launch time is `created_at` of its newest `agents` row (5.1), one row per terminal generation;
//! - a renewal under way is a `commands` row with verb `renew` that is `accepted`, or `done` with
//!   the gone inside the time it took (5.2).

use makiba::daemon::anomalies::Anomalies;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Db {
    w: StateWriter,
    _dir: TempDir,
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

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    Db { w, _dir: dir }
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

/// A run in use, launched at `T0` (a `run_launch` row).
fn run(w: &StateWriter, task: &str) {
    run_with(w, task, "in_use");
}

fn run_with(w: &StateWriter, task: &str, status: &str) {
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some(status),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, created_at)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'fake', 'b', '/w', ?3)",
            rusqlite::params![task, format!("spawn-{task}"), ts(0)],
        )
        .unwrap();
}

fn status(w: &StateWriter, task: &str, at: i64, state: &str, kind: Option<&str>) {
    let p = json!({"task_id": task, "role": "worker", "state": state, "kind": kind, "msg": null});
    event(w, "terminal_status", Some(task), at, p);
}

fn turn(w: &StateWriter, task: &str, at: i64) {
    let p = json!({"task_id": task, "role": "worker", "turn": 1});
    event(w, "terminal_turn", Some(task), at, p);
}

fn gone(w: &StateWriter, task: &str, at: i64) {
    let p = json!({"task_id": task, "role": "worker", "reason": "removed", "exit_code": null});
    event(w, "terminal_gone", Some(task), at, p);
}

/// A report to the secretary: the `message_sent` event and its `messages` row.
fn report(w: &StateWriter, task: &str, kind: &str, at: i64) {
    let id = event(w, "message_sent", Some(task), at, json!({}));
    w.conn()
        .execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
             VALUES (?1, ?2, 'secretary', ?3, (SELECT id FROM runs WHERE task_id = ?4), 'r')",
            rusqlite::params![id, format!("w-{task}"), kind, task],
        )
        .unwrap();
}

/// A secretary terminal generation launched at `at`: its key row.
fn secretary(w: &StateWriter, at: i64) {
    w.conn()
        .execute(
            "INSERT INTO agents (agent_id, role, key_sha256, created_at)
             VALUES ('secretary', 'secretary', ?1, ?2)",
            rusqlite::params![format!("key-{at}"), ts(at)],
        )
        .unwrap();
}

fn sec_status(w: &StateWriter, at: i64, state: &str) {
    let p =
        json!({"task_id": null, "role": "secretary", "state": state, "kind": null, "msg": null});
    event(w, "terminal_status", None, at, p);
}

/// A `terminal_gone` of a terminal with no task id and the given role tag.
fn gone_of(w: &StateWriter, role: Option<&str>, at: i64) {
    let p = json!({"task_id": null, "role": role, "reason": "removed", "exit_code": null});
    event(w, "terminal_gone", None, at, p);
}

fn renew(w: &StateWriter, received: i64, finished: Option<i64>, status: &str) {
    w.conn()
        .execute(
            "INSERT INTO commands (received_at, actor, verb, status, finished_at)
             VALUES (?1, 'secretary', 'renew', ?2, ?3)",
            rusqlite::params![ts(received), status, finished.map(ts)],
        )
        .unwrap();
}

fn pass(a: &mut Anomalies, w: &StateWriter, at: i64) {
    a.pass_at(w, &ts(at)).unwrap();
}

/// `<task or ->:<rule>` of every event of `kind` (`anomaly_raised` or `anomaly_cleared`), in order.
fn anomalies(w: &StateWriter, kind: &str) -> Vec<String> {
    let mut q = w
        .conn()
        .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
        .unwrap();
    q.query_map([kind], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|p| {
            let p: Value = serde_json::from_str(&p.unwrap()).unwrap();
            format!(
                "{}:{}",
                p["task_id"].as_str().unwrap_or("-"),
                p["rule"].as_str().unwrap()
            )
        })
        .collect()
}

fn count(w: &StateWriter, sql: &str) -> i64 {
    w.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// The rules raised by one pass at `now` over the scene `build` writes, as `<task or ->:<rule>`.
fn raised_at(build: impl Fn(&StateWriter), now: i64) -> Vec<String> {
    let d = db();
    build(&d.w);
    let mut a = Anomalies::new();
    pass(&mut a, &d.w, now);
    let mut got = anomalies(&d.w, "anomaly_raised");
    got.sort();
    got
}

fn none() -> Vec<String> {
    Vec::new()
}

#[test]
fn blocked_question_after_60_seconds_and_a_blocked_without_a_kind_too() {
    for kind in [Some("question"), None] {
        let scene = |w: &StateWriter| {
            run(w, "t1");
            status(w, "t1", 10, "working", None);
            status(w, "t1", 20, "blocked", kind);
        };
        assert_eq!(raised_at(scene, 20 + 59), none(), "kind {kind:?}");
        assert_eq!(
            raised_at(scene, 20 + 60),
            ["t1:blocked_question"],
            "kind {kind:?}"
        );
    }
}

#[test]
fn blocked_permission_blocked_auth_and_error_raise_at_once() {
    let cases = [
        ("blocked_permission", "blocked", Some("permission")),
        ("blocked_auth", "blocked", Some("auth")),
        ("error", "error", None),
    ];
    for (rule, state, kind) in cases {
        let before = |w: &StateWriter| {
            run(w, "t1");
            status(w, "t1", 10, "working", None);
        };
        let after = |w: &StateWriter| {
            before(w);
            status(w, "t1", 20, state, kind);
        };
        assert_eq!(raised_at(before, 21), none(), "{rule}");
        assert_eq!(raised_at(after, 21), [format!("t1:{rule}")], "{rule}");
    }
}

#[test]
fn done_no_report_after_120_seconds_unless_a_report_follows_the_last_turn() {
    let done = |w: &StateWriter| {
        run(w, "t1");
        status(w, "t1", 5, "working", None);
        turn(w, "t1", 10);
        status(w, "t1", 20, "done", None);
    };
    assert_eq!(raised_at(done, 20 + 119), none());
    assert_eq!(raised_at(done, 20 + 120), ["t1:done_no_report"]);

    // Either kind of report after the last turn answers it.
    for kind in ["report_done", "report_blocked"] {
        let reported = |w: &StateWriter| {
            done(w);
            report(w, "t1", kind, 30);
        };
        assert_eq!(raised_at(reported, 20 + 300), none(), "{kind}");
    }
    // A report from before the last turn does not: it was about an earlier turn.
    for kind in ["report_done", "report_blocked"] {
        let stale = |w: &StateWriter| {
            run(w, "t1");
            report(w, "t1", kind, 5);
            turn(w, "t1", 10);
            status(w, "t1", 20, "done", None);
        };
        let want = if kind == "report_done" {
            none()
        } else {
            vec!["t1:done_no_report".into()]
        };
        assert_eq!(raised_at(stale, 20 + 120), want, "{kind}");
    }
    // A report that is not of these kinds does not answer it.
    let other = |w: &StateWriter| {
        done(w);
        report(w, "t1", "report_start", 30);
    };
    assert_eq!(raised_at(other, 20 + 120), ["t1:done_no_report"]);
    // No turn recorded at all: a report anywhere since the launch answers it.
    let no_turn = |w: &StateWriter| {
        run(w, "t1");
        status(w, "t1", 20, "done", None);
        report(w, "t1", "report_done", 15);
    };
    assert_eq!(raised_at(no_turn, 20 + 120), none());
}

#[test]
fn no_status_180_seconds_after_the_launch() {
    let launched = |w: &StateWriter| run(w, "t1");
    assert_eq!(raised_at(launched, 179), none());
    assert_eq!(raised_at(launched, 180), ["t1:no_status"]);
    let with_status = |w: &StateWriter| {
        run(w, "t1");
        status(w, "t1", 100, "working", None);
    };
    assert_eq!(raised_at(with_status, 400), none());
}

#[test]
fn gone_raises_at_once_without_a_report_done_and_the_run_fails() {
    let before = |w: &StateWriter| {
        run(w, "t1");
        status(w, "t1", 5, "working", None);
    };
    let after = |w: &StateWriter| {
        before(w);
        gone(w, "t1", 30);
    };
    assert_eq!(raised_at(before, 31), none());
    assert_eq!(raised_at(after, 31), ["t1:gone"]);
    let reported = |w: &StateWriter| {
        after(w);
        report(w, "t1", "report_done", 20);
    };
    assert_eq!(raised_at(reported, 31), none());

    let d = db();
    after(&d.w);
    let mut a = Anomalies::new();
    pass(&mut a, &d.w, 31);
    let run_status = |w: &StateWriter| {
        w.conn()
            .query_row("SELECT status FROM runs WHERE task_id = 't1'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
    };
    assert_eq!(run_status(&d.w), "failed");
    // The run is no longer active, and the anomaly stays open: no clear, no second raise.
    for at in 32..40 {
        pass(&mut a, &d.w, at);
    }
    assert_eq!(anomalies(&d.w, "anomaly_raised"), ["t1:gone"]);
    assert!(anomalies(&d.w, "anomaly_cleared").is_empty());
}

#[test]
fn a_run_that_is_not_active_raises_nothing() {
    for state in ["completed", "abandoned", "queued"] {
        let scene = |w: &StateWriter| {
            run_with(w, "t1", state);
            status(w, "t1", 20, "blocked", Some("permission"));
        };
        assert_eq!(raised_at(scene, 500), none(), "{state}");
    }
    let closed = |w: &StateWriter| {
        run_with(w, "t1", "completed");
        gone(w, "t1", 30);
    };
    assert_eq!(raised_at(closed, 31), none());
}

#[test]
fn secretary_gone_by_a_gone_terminal_of_the_secretary_and_by_no_status() {
    // The records of a terminal that is not the secretary's do not count.
    for role in [None, Some("worker"), Some("curator")] {
        let other = |w: &StateWriter| {
            secretary(w, 0);
            sec_status(w, 5, "idle");
            gone_of(w, role, 50);
        };
        assert_eq!(raised_at(other, 51), none(), "role {role:?}");
    }
    let before = |w: &StateWriter| {
        secretary(w, 0);
        sec_status(w, 5, "idle");
    };
    let after = |w: &StateWriter| {
        before(w);
        gone_of(w, Some("secretary"), 50);
    };
    assert_eq!(raised_at(before, 51), none());
    assert_eq!(raised_at(after, 51), ["-:secretary_gone"]);

    // No status 180 s after the launch; a status at all, however old, stops it.
    let launched = |w: &StateWriter| secretary(w, 0);
    assert_eq!(raised_at(launched, 179), none());
    assert_eq!(raised_at(launched, 180), ["-:secretary_gone"]);
    assert_eq!(raised_at(before, 1000), none());
}

#[test]
fn secretary_gone_is_not_raised_by_a_renewal_and_a_later_secretary_clears_it() {
    let base = |w: &StateWriter| {
        secretary(w, 0);
        sec_status(w, 5, "idle");
    };
    // Under way: accepted, not finished.
    let under_way = |w: &StateWriter| {
        base(w);
        renew(w, 40, None, "accepted");
        gone_of(w, Some("secretary"), 50);
    };
    assert_eq!(raised_at(under_way, 51), none());
    // Finished, and the removal came in while it took.
    let finished = |w: &StateWriter| {
        base(w);
        renew(w, 40, Some(60), "done");
        gone_of(w, Some("secretary"), 50);
    };
    assert_eq!(raised_at(finished, 100), none());
    // A removal after it was over, or a renewal that failed, is a gone secretary.
    let late = |w: &StateWriter| {
        base(w);
        renew(w, 40, Some(60), "done");
        gone_of(w, Some("secretary"), 70);
    };
    assert_eq!(raised_at(late, 100), ["-:secretary_gone"]);
    let failed = |w: &StateWriter| {
        base(w);
        renew(w, 40, Some(60), "failed");
        gone_of(w, Some("secretary"), 50);
    };
    assert_eq!(raised_at(failed, 100), ["-:secretary_gone"]);

    // Raised, then a new terminal generation: cleared, and no status for it yet is not a gone.
    let d = db();
    after_gone(&d.w);
    let mut a = Anomalies::new();
    for at in 51..61 {
        pass(&mut a, &d.w, at);
    }
    assert_eq!(anomalies(&d.w, "anomaly_raised"), ["-:secretary_gone"]);
    secretary(&d.w, 100);
    pass(&mut a, &d.w, 110);
    assert_eq!(anomalies(&d.w, "anomaly_cleared"), ["-:secretary_gone"]);
    pass(&mut a, &d.w, 279);
    assert_eq!(anomalies(&d.w, "anomaly_raised"), ["-:secretary_gone"]);
    // The new generation can raise again: no status 180 s after its launch.
    pass(&mut a, &d.w, 280);
    assert_eq!(
        anomalies(&d.w, "anomaly_raised"),
        ["-:secretary_gone", "-:secretary_gone"]
    );
}

fn after_gone(w: &StateWriter) {
    secretary(w, 0);
    sec_status(w, 5, "idle");
    gone_of(w, Some("secretary"), 50);
}

#[test]
fn secretary_still_after_600_seconds_with_a_worker_anomaly_open() {
    let blocked = |w: &StateWriter| {
        run(w, "t1");
        status(w, "t1", 20, "blocked", Some("permission"));
        secretary(w, 0);
        sec_status(w, 30, "idle");
    };
    assert_eq!(raised_at(blocked, 30 + 599), ["t1:blocked_permission"]);
    assert_eq!(
        raised_at(blocked, 30 + 600),
        ["-:secretary_still", "t1:blocked_permission"]
    );
    // A change of the secretary's status starts the time again.
    let moved = |w: &StateWriter| {
        blocked(w);
        sec_status(w, 400, "working");
    };
    assert_eq!(raised_at(moved, 400 + 599), ["t1:blocked_permission"]);
    // No worker anomaly open: nothing to be still about.
    let calm = |w: &StateWriter| {
        run(w, "t1");
        status(w, "t1", 20, "working", None);
        secretary(w, 0);
        sec_status(w, 30, "idle");
    };
    assert_eq!(raised_at(calm, 30 + 600), none());
}

#[test]
fn a_scene_held_for_ten_passes_raises_once_and_queues_one_notice() {
    let d = db();
    run(&d.w, "t1");
    status(&d.w, "t1", 10, "working", None);
    status(&d.w, "t1", 20, "blocked", Some("question"));
    let mut a = Anomalies::new();
    for at in 80..90 {
        pass(&mut a, &d.w, at);
    }
    assert_eq!(anomalies(&d.w, "anomaly_raised"), ["t1:blocked_question"]);
    assert_eq!(count(&d.w, "SELECT COUNT(*) FROM messages"), 1);

    // The notice is a message: its `message_sent` event, its row, and a delivery that is pending
    // with no attempt.
    let (sender, recipient, kind, body, run_id, run_of_t1, event_id): (
        String,
        String,
        String,
        String,
        i64,
        i64,
        i64,
    ) = d
        .w
        .conn()
        .query_row(
            "SELECT sender, recipient, kind, body, run_id, (SELECT id FROM runs WHERE task_id = 't1'),
                    event_id FROM messages",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap();
    assert_eq!(
        (sender.as_str(), recipient.as_str(), kind.as_str()),
        ("makibad", "secretary", "notice")
    );
    assert_eq!(run_id, run_of_t1);
    assert!(
        body.contains("blocked_question") && body.contains("t1"),
        "{body}"
    );
    let (ekind, payload): (String, String) =
        d.w.conn()
            .query_row(
                "SELECT kind, payload_json FROM events WHERE id = ?1",
                [event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(ekind, "message_sent");
    let message_id: i64 =
        d.w.conn()
            .query_row("SELECT id FROM messages", [], |r| r.get(0))
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["message_id"],
        message_id
    );
    let (rows, dstatus, attempt): (i64, String, i64) =
        d.w.conn()
            .query_row(
                "SELECT COUNT(*), MIN(status), MAX(attempt) FROM event_deliveries
             WHERE source_event_id = ?1 AND recipient = 'secretary'",
                [event_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
    assert_eq!((rows, dstatus.as_str(), attempt), (1, "pending", 0));

    // It stops being true: cleared once. Then it may raise again, with a second notice.
    status(&d.w, "t1", 95, "working", None);
    for at in 96..100 {
        pass(&mut a, &d.w, at);
    }
    assert_eq!(anomalies(&d.w, "anomaly_cleared"), ["t1:blocked_question"]);
    status(&d.w, "t1", 100, "blocked", Some("question"));
    for at in 160..165 {
        pass(&mut a, &d.w, at);
    }
    assert_eq!(
        anomalies(&d.w, "anomaly_raised"),
        ["t1:blocked_question", "t1:blocked_question"]
    );
    assert_eq!(count(&d.w, "SELECT COUNT(*) FROM messages"), 2);
}

#[test]
fn the_notice_marks_what_the_secretary_must_know_and_the_person_rules_queue_none() {
    let d = db();
    run(&d.w, "t1");
    status(&d.w, "t1", 20, "blocked", Some("auth"));
    secretary(&d.w, 0);
    let mut a = Anomalies::new();
    pass(&mut a, &d.w, 30);
    let body: String =
        d.w.conn()
            .query_row("SELECT body FROM messages", [], |r| r.get(0))
            .unwrap();
    assert!(
        body.contains("blocked_auth") && body.contains("needs the person"),
        "{body}"
    );

    // `secretary_gone` and `secretary_still` are told to the person (the log and `makiba status`),
    // not to the secretary: raised, and no message for them.
    pass(&mut a, &d.w, 190);
    pass(&mut a, &d.w, 700);
    assert_eq!(
        anomalies(&d.w, "anomaly_raised"),
        ["t1:blocked_auth", "-:secretary_gone", "-:secretary_still"]
    );
    assert_eq!(count(&d.w, "SELECT COUNT(*) FROM messages"), 1);
    assert_eq!(count(&d.w, "SELECT COUNT(*) FROM event_deliveries"), 1);
}

#[test]
fn what_is_open_is_read_from_the_events_so_a_restart_neither_repeats_nor_forgets() {
    let d = db();
    run(&d.w, "t1");
    status(&d.w, "t1", 20, "blocked", Some("permission"));
    pass(&mut Anomalies::new(), &d.w, 30);
    // A new process: same scene, still one; then it clears, and the clear is seen by a third.
    pass(&mut Anomalies::new(), &d.w, 31);
    assert_eq!(anomalies(&d.w, "anomaly_raised"), ["t1:blocked_permission"]);
    status(&d.w, "t1", 40, "working", None);
    pass(&mut Anomalies::new(), &d.w, 41);
    pass(&mut Anomalies::new(), &d.w, 42);
    assert_eq!(
        anomalies(&d.w, "anomaly_cleared"),
        ["t1:blocked_permission"]
    );
    assert_eq!(count(&d.w, "SELECT COUNT(*) FROM messages"), 1);
}

#[test]
fn a_failed_write_leaves_nothing_behind_and_the_next_pass_tries_again() {
    let d = db();
    run(&d.w, "t1");
    status(&d.w, "t1", 5, "working", None);
    gone(&d.w, "t1", 30);
    d.w.conn()
        .execute_batch(
            "CREATE TRIGGER no_delivery BEFORE INSERT ON event_deliveries
             BEGIN SELECT RAISE(ABORT, 'no'); END;",
        )
        .unwrap();
    let mut a = Anomalies::new();
    assert!(a.pass_at(&d.w, &ts(31)).is_err());
    // The raise, the notice and the run's failure go together: none of them happened.
    assert_eq!(
        count(
            &d.w,
            "SELECT COUNT(*) FROM events WHERE kind IN ('anomaly_raised', 'message_sent')"
        ),
        0
    );
    assert_eq!(count(&d.w, "SELECT COUNT(*) FROM messages"), 0);
    assert_eq!(
        count(&d.w, "SELECT COUNT(*) FROM runs WHERE status = 'in_use'"),
        1
    );

    d.w.conn()
        .execute_batch("DROP TRIGGER no_delivery")
        .unwrap();
    pass(&mut a, &d.w, 32);
    pass(&mut a, &d.w, 33);
    assert_eq!(anomalies(&d.w, "anomaly_raised"), ["t1:gone"]);
    assert_eq!(count(&d.w, "SELECT COUNT(*) FROM messages"), 1);
    assert_eq!(
        count(&d.w, "SELECT COUNT(*) FROM runs WHERE status = 'failed'"),
        1
    );
}
