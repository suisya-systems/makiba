//! Issue 654: a run reworking its pull request after its done report counts against its range's
//! `at_once` and against `max_concurrent_workers` until its next done report (docs/design/belt.md
//! 3.2).
//!
//! A range of `at_once` 1 has two candidates; its first task reports done with its test commit,
//! which is pushed alone, and is sent on to its fix by the secretary. Its slot stays taken (the
//! second candidate is not dispatched, and `dispatch::busy` counts the run) through the done
//! report, the push, the message and a blocked report, and is freed by the done report of the fix,
//! in whose transaction the second candidate goes in.
//!
//! No daemon: the door is called in this process on the stand-in base with the secretary's and
//! the worker's keys, and the range, its read and the first run are written as `makibad` would
//! write them. The push is accepted by the door and finished as push.rs finishes one that
//! succeeded (the command `done` and `branch_pushed`), with no git.

use makiba::base::stub::Stub;
use makiba::daemon::belt::fill;
use makiba::daemon::dispatch::busy;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    door: Door,
    range: i64,
    first: String,
    second: String,
    secretary: String,
    worker: String,
}

impl Org {
    /// An active range of project `p` with `at_once` 1 and two admitted candidates, #1 and #2, in
    /// a complete read started after its approval; its first task `r<range>-1` at work, with a
    /// code worktree.
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        let w = StateWriter::open(&db).unwrap();
        let project = w
            .ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                     proposed_by, status, approved_by, approved_at)
                 VALUES (?1, '[1, 2]', 1, 2, 'Do it.', 'secretary', 'active', 'person',
                         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [project],
            )
            .unwrap();
        let range = w.conn().last_insert_rowid();
        let event = |kind: &str, task: Option<&str>, payload: Value| {
            w.append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(&payload),
                run_task_id: task,
                ..Default::default()
            })
            .unwrap()
        };
        let approved = event("range_approved", None, json!({"range_id": range}));
        let read = event(
            "range_read",
            None,
            json!({"range_id": range, "complete": true, "activation": approved}),
        );
        for (position, issue) in [(0, 1), (1, 2)] {
            w.conn()
                .execute(
                    "INSERT INTO range_candidates (range_id, read_id, position, issue, title,
                                                   body, admitted)
                     VALUES (?1, ?2, ?3, ?4, 'An Issue', 'Its body.', 1)",
                    params![range, read, position, issue],
                )
                .unwrap();
        }
        let first = format!("r{range}-1");
        let second = format!("r{range}-2");
        w.upsert_run(&RunUpsert {
            task_id: &first,
            project_slug: "p",
            title: Some("An Issue"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET range_id = ?1, issue_refs = 'p#1',
                   dispatched_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE task_id = ?2",
                params![range, first],
            )
            .unwrap();
        event(
            "belt_dispatched",
            Some(&first),
            json!({"range_id": range, "issue": 1, "task_id": first, "design_only": false}),
        );
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [&first], |r| {
                r.get(0)
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, 'spawn-1', 'code', 'fake', 'Do it.', ?2, 'verified')",
                params![run, tmp.path().join("work").to_string_lossy()],
            )
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let worker = issue_key(&w, Role::Worker, &format!("w-{first}"), Some(run), None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        Org {
            _tmp: tmp,
            db,
            door,
            range,
            first,
            second,
            secretary,
            worker,
        }
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Value {
        self.door
            .handle(Some(key), verb, args.clone(), None, true)
            .unwrap_or_else(|e| panic!("{verb} {args} refused: {e:?}"))
    }

    fn report(&self, state: &str) {
        self.call(
            &self.worker,
            "report",
            json!({"state": state, "task": self.first, "text": format!("{state} report")}),
        );
    }

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.db).unwrap()
    }

    fn status(&self, task: &str) -> Option<String> {
        self.writer()
            .conn()
            .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .optional()
            .unwrap()
    }

    /// The runs `max_concurrent_workers` counts.
    fn busy(&self) -> usize {
        busy(&self.writer()).unwrap()
    }

    /// A pass of the belt, as `makibad`'s: the tasks it dispatched.
    fn fill(&self) -> Vec<String> {
        self.writer().transaction(|w| fill(w, "fake")).unwrap()
    }

    /// The push of the test commit alone, accepted by the door, finished as push.rs finishes a
    /// push that succeeded: the command `done` and `branch_pushed` naming the head.
    fn push_test_commit(&self) {
        let r = self.call(
            &self.secretary,
            "push",
            json!({"task": self.first, "head": TEST_COMMIT}),
        );
        assert_eq!(r["status"], json!("accepted"), "{r}");
        let w = self.writer();
        let n = w
            .conn()
            .execute(
                "UPDATE commands SET status = 'done',
                   finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE verb = 'push' AND status = 'accepted'",
                [],
            )
            .unwrap();
        assert_eq!(n, 1, "one push accepted");
        w.append_event(&NewEvent {
            kind: "branch_pushed",
            actor: Some("makibad"),
            payload: Some(
                &json!({"task_id": self.first, "branch": format!("makiba/{}", self.first),
                                  "head": TEST_COMMIT}),
            ),
            run_task_id: Some(&self.first),
            ..Default::default()
        })
        .unwrap();
    }
}

#[test]
fn a_run_sent_back_to_its_fix_keeps_its_slot_until_its_next_done() {
    let org = Org::new();
    assert_eq!(org.busy(), 1);

    // 1. Done with the test commit: in review, its fix due; the slot stays taken.
    org.report("done");
    assert_eq!(org.status(&org.first).as_deref(), Some("review"));
    assert_eq!(
        org.status(&org.second),
        None,
        "dispatched at the done of the test commit"
    );
    assert_eq!(
        org.busy(),
        1,
        "the run in review with its fix due is counted"
    );

    // 2. The test commit pushed alone: still due.
    org.push_test_commit();
    assert_eq!(org.fill(), Vec::<String>::new());
    assert_eq!(org.status(&org.second), None);
    assert_eq!(org.busy(), 1);

    // 3. CI failed there: the secretary sends the worker on to its fix, at work again.
    org.call(
        &org.secretary,
        "send",
        json!({"to": format!("w-{}", org.first),
               "text": "CI failed at your test commit: write the fix."}),
    );
    assert_eq!(org.status(&org.first).as_deref(), Some("in_use"));
    assert_eq!(org.fill(), Vec::<String>::new());
    assert_eq!(org.status(&org.second), None);
    assert_eq!(org.busy(), 1);

    // 4. Blocked on a question in the fix: still at work.
    org.report("blocked");
    assert_eq!(org.status(&org.first).as_deref(), Some("in_use"));
    assert_eq!(org.fill(), Vec::<String>::new());
    assert_eq!(org.status(&org.second), None);

    // 5. Done with the fix: in review waiting for its push and CI, counted no more; the second
    //    candidate goes in, in the transaction of that report.
    org.report("done");
    assert_eq!(org.status(&org.first).as_deref(), Some("review"));
    assert_eq!(org.status(&org.second).as_deref(), Some("queued"));
    let range: i64 = org
        .writer()
        .conn()
        .query_row(
            "SELECT range_id FROM runs WHERE task_id = ?1",
            [&org.second],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(range, org.range);
    assert_eq!(
        org.busy(),
        0,
        "neither the run in review nor the untaken one"
    );
}
