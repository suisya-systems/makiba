//! Issue 414: a range item in review carries the time its run went into review, not the dispatch
//! (docs/design/belt.md 7.1, spec section 12 `ranges[].items[].since`).
//!
//! - A run moved into review by a done report: `since` is that report's time. A second done while
//!   in review does not move it again; a rework (`start`, then `done`) and a done after `makiba
//!   down` (review, then suspended) each move it again and give the new time.
//! - A design-only run waiting for its landing: `since` is its design report.
//!
//! No daemon: the door is called in this process with the worker's key, and the range is read with
//! `belt::read_ranges` on the same state.

use makiba::base::stub::Stub;
use makiba::daemon::belt::read_ranges;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const DISPATCHED: &str = "2026-01-01T00:00:00.000Z";

struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    door: Door,
    task: String,
    worker: String,
}

impl Org {
    /// A range of project `p` and its run `task` for `issue`, at work, dispatched at
    /// `DISPATCHED`, with its `belt_dispatched` (extra fields from `dispatch`).
    fn new(task: &str, issue: i64, dispatch: Value) -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        let range = door
            .handle(
                Some(&secretary),
                "range",
                json!({"action": "propose", "project": "p", "issues": [issue], "at_once": 1,
                       "budget_tasks": 1, "brief_head": "h"}),
                None,
                true,
            )
            .unwrap_or_else(|e| panic!("range propose refused: {e:?}"))["id"]
            .as_i64()
            .unwrap();
        let w = StateWriter::open(&db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            title: Some("the task"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET range_id = ?1, dispatched_at = ?2 WHERE task_id = ?3",
                rusqlite::params![range, DISPATCHED, task],
            )
            .unwrap();
        let mut payload = json!({"range_id": range, "issue": issue, "task_id": task,
                                 "design_only": false});
        for (k, v) in dispatch.as_object().unwrap() {
            payload[k] = v.clone();
        }
        w.append_event(&NewEvent {
            kind: "belt_dispatched",
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: Some(task),
            ..Default::default()
        })
        .unwrap();
        let mut org = Org {
            _tmp: tmp,
            db,
            door,
            task: task.into(),
            worker: String::new(),
        };
        org.worker = org.new_worker_key();
        org
    }

    /// A worker key for the run, as its launch (or a relaunch) issues it.
    fn new_worker_key(&self) -> String {
        let w = StateWriter::open(&self.db).unwrap();
        let run: i64 = w
            .conn()
            .query_row(
                "SELECT id FROM runs WHERE task_id = ?1",
                [&self.task],
                |r| r.get(0),
            )
            .unwrap();
        issue_key(
            &w,
            Role::Worker,
            &format!("w-{}", self.task),
            Some(run),
            None,
        )
        .unwrap()
    }

    fn report(&self, state: &str) {
        std::thread::sleep(Duration::from_millis(5));
        self.door
            .handle(
                Some(&self.worker),
                "report",
                json!({"state": state, "task": self.task, "text": format!("{state} report")}),
                None,
                true,
            )
            .unwrap_or_else(|e| panic!("report {state} refused: {e:?}"));
    }

    fn conn(&self) -> Connection {
        Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    fn status(&self) -> String {
        self.conn()
            .query_row(
                "SELECT status FROM runs WHERE task_id = ?1",
                [&self.task],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The range item of the run.
    fn item(&self) -> Value {
        let ranges = read_ranges(&self.conn()).unwrap();
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        ranges[0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["task_id"] == json!(self.task))
            .unwrap_or_else(|| panic!("no item of {}: {}", self.task, ranges[0]))
            .clone()
    }

    /// The time of the event of the run's `n`-th done report (1 is the first).
    fn done_at(&self, n: usize) -> String {
        let c = self.conn();
        let mut q = c
            .prepare(
                "SELECT e.occurred_at FROM messages m JOIN events e ON e.id = m.event_id
                 JOIN runs r ON r.id = m.run_id
                 WHERE r.task_id = ?1 AND m.kind = 'report_done' ORDER BY m.id",
            )
            .unwrap();
        let all: Vec<String> = q
            .query_map([&self.task], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        all.get(n - 1)
            .unwrap_or_else(|| panic!("no done report {n}: {all:?}"))
            .clone()
    }
}

#[test]
fn a_range_item_in_review_carries_the_time_of_the_done_that_moved_it() {
    let mut org = Org::new("r1-5", 5, json!({}));

    // a. At work: since the dispatch.
    let it = org.item();
    assert_eq!(it["place"], json!("in_work"), "{it}");
    assert_eq!(it["since"], json!(DISPATCHED), "{it}");

    // b. Start, then done: in review since that done.
    org.report("start");
    org.report("done");
    assert_eq!(org.status(), "review");
    let it = org.item();
    assert_eq!(it["place"], json!("review"), "{it}");
    assert_ne!(it["since"], json!(DISPATCHED), "{it}");
    assert_eq!(it["since"], json!(org.done_at(1)), "{it}");

    // c. A second done while in review does not move it again.
    org.report("done");
    let it = org.item();
    assert_eq!(it["place"], json!("review"), "{it}");
    assert_eq!(it["since"], json!(org.done_at(1)), "{it}");

    // d. Rework: at work again, then in review since the new done.
    org.report("start");
    let it = org.item();
    assert_eq!(it["place"], json!("in_work"), "{it}");
    assert_eq!(it["since"], json!(DISPATCHED), "{it}");
    org.report("done");
    let it = org.item();
    assert_eq!(it["place"], json!("review"), "{it}");
    assert_eq!(it["since"], json!(org.done_at(3)), "{it}");

    // e. makiba down suspends it; relaunched, a done with no start moves it into review again.
    {
        let w = StateWriter::open(&org.db).unwrap();
        makiba::down::suspend(&w).unwrap();
    }
    assert_eq!(org.status(), "suspended");
    org.worker = org.new_worker_key();
    org.report("done");
    assert_eq!(org.status(), "review");
    let it = org.item();
    assert_eq!(it["place"], json!("review"), "{it}");
    assert_eq!(it["since"], json!(org.done_at(4)), "{it}");
}

#[test]
fn a_design_only_item_waiting_for_its_landing_carries_the_time_of_its_design_report() {
    let org = Org::new(
        "r1-8",
        8,
        json!({"design_only": true, "waits_for": [7], "waits_tasks": ["r1-7"]}),
    );
    let it = org.item();
    assert_eq!(it["since"], json!(DISPATCHED), "{it}");

    org.report("blocked");
    let reported: String = org
        .conn()
        .query_row(
            "SELECT e.occurred_at FROM events e JOIN runs r ON r.id = e.run_id
             WHERE r.task_id = ?1 AND e.kind = 'design_reported' ORDER BY e.id DESC LIMIT 1",
            [&org.task],
            |r| r.get(0),
        )
        .unwrap();
    let it = org.item();
    assert_eq!(it["place"], json!("review"), "{it}");
    assert_eq!(it["stage"], json!("waits_landing #7"), "{it}");
    assert_ne!(it["since"], json!(DISPATCHED), "{it}");
    assert_eq!(it["since"], json!(reported), "{it}");
}
