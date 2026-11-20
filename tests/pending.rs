//! M6-4 (Issue 45): pending decisions in the ledger.
//!
//! The cases of claude-org-ja `tests/test_pending_decisions.py`, ported to the `pending` table
//! (spec 5.5): the door's half through `Door::handle` as the secretary's tool, on a scratch
//! `state.db` with the stand-in base; the relay-gap half (`list_pending_older_than`) as the
//! `secretary_still` rule of spec section 11, through `Anomalies::pass_at` with explicit times.
//! Cases with no counterpart (the JSON file, the `user_replied_at` marker, the LLM dispatcher's
//! stall judgement) are not carried. Plus the Issue's two points on `answer`: an answered entry is
//! `conflict`, and one answer queues exactly one message to the run's worker.

use makiba::base::stub::Stub;
use makiba::daemon::anomalies::Anomalies;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

// ---- the door ----------------------------------------------------------------------------------

struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    door: Door,
    key: String,
}

impl Org {
    /// Runs `t1` and `t2` in use, and the secretary's key.
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        let w = StateWriter::open(&db).unwrap();
        for task in ["t1", "t2"] {
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                status: Some("in_use"),
                ..Default::default()
            })
            .unwrap();
        }
        let key = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        Org {
            _tmp: tmp,
            db,
            door,
            key,
        }
    }

    fn call(&self, verb: &str, args: Value) -> Response {
        self.door.handle(Some(&self.key), verb, args, None, true)
    }

    fn add(&self, task: &str, q: &str) -> Value {
        self.call(
            "pending",
            json!({"action": "add", "task": task, "question": q}),
        )
        .unwrap()
    }

    fn act(&self, action: &str, id: &Value) -> Response {
        self.call("pending", json!({"action": action, "id": id}))
    }

    fn answer(&self, id: &Value, answer: &str) -> Response {
        self.call("answer", json!({"id": id, "answer": answer}))
    }

    fn list(&self) -> Vec<Value> {
        let r = self.call("pending", json!({"action": "list"})).unwrap();
        r["entries"].as_array().unwrap().clone()
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    /// `(status, answer, answered_by, answered_at is set)` of an entry.
    fn row(&self, id: &Value) -> (String, Option<String>, Option<String>, bool) {
        self.db()
            .query_row(
                "SELECT status, answer, answered_by, answered_at IS NOT NULL FROM pending
                 WHERE id = ?1",
                [id.as_i64().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    fn entries(&self) -> i64 {
        self.db()
            .query_row("SELECT COUNT(*) FROM pending", [], |r| r.get(0))
            .unwrap()
    }

    /// Every pending row, as text, to show that a request changed nothing.
    fn snapshot(&self) -> Vec<String> {
        let c = self.db();
        let mut q = c
            .prepare(
                "SELECT id, run_id, kind, question, status, answer, answered_by, answered_at
                 FROM pending ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| {
            Ok(format!(
                "{:?}",
                (
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                )
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    /// Messages of kind `message` from the secretary to `to`, of run `task`, whose body names
    /// pending entry `id`.
    fn answers_to(&self, to: &str, task: &str, id: &Value) -> Vec<String> {
        let c = self.db();
        let mut q = c
            .prepare(
                "SELECT m.body FROM messages m JOIN runs r ON r.id = m.run_id
                 WHERE m.sender = 'secretary' AND m.recipient = ?1 AND r.task_id = ?2
                   AND m.kind = 'message' AND m.body LIKE ?3 ORDER BY m.id",
            )
            .unwrap();
        let like = format!("Answer to pending entry {} %", id.as_i64().unwrap());
        q.query_map(rusqlite::params![to, task, like], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}

fn kind_of(r: &Response) -> Option<ErrorKind> {
    r.as_ref().err().map(|e| e.kind)
}

/// append_to_empty_store_creates_file_with_pending_entry, missing_file_returns_empty (and
/// ListStale missing_store_is_empty), cli_append_and_list.
#[test]
fn add_to_an_empty_table_makes_one_open_entry_and_list_shows_it() {
    let org = Org::new();
    assert_eq!(org.list(), Vec::<Value>::new());
    let added = org.add("t1", "judgment please");
    assert_eq!(added["status"], json!("open"), "{added}");
    assert_eq!(org.entries(), 1);
    assert_eq!(org.row(&added["id"]), ("open".into(), None, None, false));
    let listed = org.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], added["id"]);
    assert_eq!(listed[0]["task_id"], json!("t1"));
    assert_eq!(listed[0]["question"], json!("judgment please"));
}

/// append_same_task_id_is_idempotent, and the bounds of that: the key is the task and the
/// question, and only an open entry is matched (append_after_resolve_creates_new_pending).
#[test]
fn add_of_the_same_open_question_returns_the_same_entry() {
    let org = Org::new();
    let first = org.add("t1", "Use the old API?");
    let second = org.add("t1", "Use the old API?");
    assert_eq!(second["id"], first["id"], "{second}");
    assert_eq!(second["status"], json!("open"));
    assert_eq!(org.entries(), 1);
    // Another question of the same task, or the same question of another task: new entries.
    let other_q = org.add("t1", "Keep the tests?");
    let other_t = org.add("t2", "Use the old API?");
    assert_ne!(other_q["id"], first["id"]);
    assert_ne!(other_t["id"], first["id"]);
    assert_eq!(org.entries(), 3);
    // Once escalated, withdrawn or answered, the same question opens a new entry.
    org.act("escalate", &first["id"]).unwrap();
    let after_escalate = org.add("t1", "Use the old API?");
    assert_ne!(after_escalate["id"], first["id"]);
    org.act("withdraw", &after_escalate["id"]).unwrap();
    let after_withdraw = org.add("t1", "Use the old API?");
    assert_ne!(after_withdraw["id"], after_escalate["id"]);
    org.answer(&after_withdraw["id"], "no").unwrap();
    let after_answer = org.add("t1", "Use the old API?");
    assert_ne!(after_answer["id"], after_withdraw["id"]);
    assert_eq!(after_answer["status"], json!("open"));
    assert_eq!(org.entries(), 6);
}

/// resolve_to_user_marks_escalated, resolve_to_user_after_escalated_is_noop.
#[test]
fn escalate_moves_open_to_escalated_once() {
    let org = Org::new();
    let e = org.add("t1", "ask");
    let r = org.act("escalate", &e["id"]).unwrap();
    assert_eq!(r["status"], json!("escalated"));
    assert_eq!(org.row(&e["id"]).0, "escalated");
    let again = org.act("escalate", &e["id"]);
    assert_eq!(kind_of(&again), Some(ErrorKind::Conflict), "{again:?}");
    assert_eq!(org.row(&e["id"]).0, "escalated");
}

/// resolve_to_worker_marks_resolved: from open and from escalated.
#[test]
fn answer_marks_the_entry_answered_by_whom_and_when() {
    let org = Org::new();
    let open = org.add("t1", "ask one");
    let r = org.answer(&open["id"], "yes").unwrap();
    assert_eq!(r["status"], json!("answered"), "{r}");
    assert_eq!(
        org.row(&open["id"]),
        (
            "answered".into(),
            Some("yes".into()),
            Some("secretary".into()),
            true
        )
    );
    let esc = org.add("t1", "ask two");
    org.act("escalate", &esc["id"]).unwrap();
    org.answer(&esc["id"], "no").unwrap();
    assert_eq!(
        org.row(&esc["id"]),
        (
            "answered".into(),
            Some("no".into()),
            Some("secretary".into()),
            true
        )
    );
}

/// resolve_unknown_task_id_returns_none, resolve_unknown_kind_raises.
#[test]
fn an_unknown_entry_is_not_found_and_a_bad_answer_is_usage_and_nothing_changes() {
    let org = Org::new();
    let e = org.add("t1", "ask");
    let before = org.snapshot();
    for r in [
        org.act("escalate", &json!(999)),
        org.act("withdraw", &json!(999)),
        org.answer(&json!(999), "yes"),
    ] {
        assert_eq!(kind_of(&r), Some(ErrorKind::NotFound), "{r:?}");
    }
    let bad = org.answer(&e["id"], "maybe");
    assert_eq!(kind_of(&bad), Some(ErrorKind::Usage), "{bad:?}");
    assert_eq!(org.snapshot(), before);
    assert!(org.answers_to("w-t1", "t1", &e["id"]).is_empty());
}

/// canonical_lifecycle_pending_escalated_resolved.
#[test]
fn add_escalate_answer_leaves_nothing_listed() {
    let org = Org::new();
    let e = org.add("t1", "ask");
    org.act("escalate", &e["id"]).unwrap();
    assert_eq!(org.list()[0]["status"], json!("escalated"));
    org.answer(&e["id"], "yes").unwrap();
    assert_eq!(org.row(&e["id"]).0, "answered");
    assert_eq!(org.list(), Vec::<Value>::new());
}

/// ListStale boundary_resolved_and_escalated (the statuses; the age is not carried) and
/// read_only.
#[test]
fn list_shows_open_and_escalated_only_and_changes_nothing() {
    let org = Org::new();
    let open = org.add("t1", "open one");
    let esc = org.add("t1", "escalated one");
    let answered = org.add("t2", "answered one");
    let withdrawn = org.add("t2", "withdrawn one");
    org.act("escalate", &esc["id"]).unwrap();
    org.answer(&answered["id"], "yes").unwrap();
    org.act("withdraw", &withdrawn["id"]).unwrap();
    let before = org.snapshot();
    let ids: Vec<Value> = org.list().iter().map(|e| e["id"].clone()).collect();
    assert_eq!(ids, vec![open["id"].clone(), esc["id"].clone()]);
    assert_eq!(org.snapshot(), before);
}

/// latest_for_task_discriminates_across_tasks: an answer goes to its own run's worker only.
#[test]
fn an_answer_reaches_its_own_runs_worker_only() {
    let org = Org::new();
    let one = org.add("t1", "t1 question");
    let two = org.add("t2", "t2 question");
    org.answer(&one["id"], "yes").unwrap();
    assert_eq!(org.answers_to("w-t1", "t1", &one["id"]).len(), 1);
    assert!(org.answers_to("w-t2", "t2", &one["id"]).is_empty());
    assert_eq!(org.row(&two["id"]).0, "open");
}

/// The Issue: `answer` on an entry already answered is `conflict`, and one answer queues exactly
/// one message to the run's worker, which names the question it answers.
#[test]
fn a_second_answer_is_conflict_and_one_answer_queues_exactly_one_message() {
    let org = Org::new();
    let first = org.add("t1", "Use the old API?");
    let second = org.add("t1", "Keep the tests?");
    // Answered in reverse order: each message says which question it answers.
    org.answer(&second["id"], "no").unwrap();
    org.answer(&first["id"], "yes").unwrap();
    let again = org.answer(&first["id"], "no");
    assert_eq!(kind_of(&again), Some(ErrorKind::Conflict), "{again:?}");
    assert_eq!(org.row(&first["id"]).1.as_deref(), Some("yes"));
    let to_first = org.answers_to("w-t1", "t1", &first["id"]);
    assert_eq!(to_first.len(), 1, "{to_first:?}");
    assert!(to_first[0].contains("Use the old API?"), "{to_first:?}");
    assert!(to_first[0].contains("yes"), "{to_first:?}");
    let to_second = org.answers_to("w-t1", "t1", &second["id"]);
    assert_eq!(to_second.len(), 1, "{to_second:?}");
    assert!(to_second[0].contains("Keep the tests?"), "{to_second:?}");
}

// ---- secretary_still reads open entries --------------------------------------------------------

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

/// A scene: run `t1` in use with no anomaly, a secretary launched at `T0` whose status changes
/// every 100 s up to `now` (it is not still), and one entry of `status` created at `created`.
/// Returns the rules raised by one pass at `now`.
fn still_at(status: &str, created: i64, now: i64) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: "p",
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    let c = w.conn();
    c.execute(
        "INSERT INTO agents (agent_id, role, key_sha256, created_at)
         VALUES ('secretary', 'secretary', 'k', ?1)",
        [ts(0)],
    )
    .unwrap();
    let mut at = 10;
    while at <= now {
        let state = if (at / 100) % 2 == 0 {
            "idle"
        } else {
            "working"
        };
        let p = json!({"task_id": null, "role": "secretary", "state": state, "kind": null, "msg": null});
        w.append_event(&makiba::db::writer::NewEvent {
            kind: "terminal_status",
            actor: Some("makibad"),
            payload: Some(&p),
            occurred_at: Some(&ts(at)),
            ..Default::default()
        })
        .unwrap();
        at += 100;
    }
    c.execute(
        "INSERT INTO pending (created_at, run_id, kind, question, status)
         VALUES (?1, (SELECT id FROM runs WHERE task_id = 't1'), 'question', 'q', ?2)",
        rusqlite::params![ts(created), status],
    )
    .unwrap();
    Anomalies::new().pass_at(&w, &ts(now)).unwrap();
    let mut q = c
        .prepare("SELECT payload_json FROM events WHERE kind = 'anomaly_raised' ORDER BY id")
        .unwrap();
    q.query_map([], |r| r.get::<_, String>(0))
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

/// list_pending_older_than_filters_by_received_at: an open entry 600 s old raises
/// `secretary_still` even while the secretary's status moves; a younger one does not; an
/// answered or withdrawn one never does.
#[test]
fn an_open_entry_600_seconds_old_raises_secretary_still() {
    assert_eq!(still_at("open", 1000, 1000 + 599), Vec::<String>::new());
    assert_eq!(still_at("open", 1000, 1000 + 600), ["-:secretary_still"]);
    for closed in ["answered", "withdrawn"] {
        assert_eq!(still_at(closed, 0, 2000), Vec::<String>::new(), "{closed}");
    }
}

/// list_pending_excludes_escalated: an escalated entry waits on the person, not the secretary.
#[test]
fn an_escalated_entry_does_not_raise_secretary_still() {
    assert_eq!(still_at("escalated", 0, 2000), Vec::<String>::new());
}
