//! Issue 357: a range item reads its run's current pull request (`belt::current_pr_sql`, Issue
//! 345: the repository and number of its newest `pr_opened` or `fix_pushed`), not its first
//! `pr_merged` nor its newest watch row (docs/design/belt.md 7.1, spec section 12 "Range field").
//!
//! - A completed run is `landed`, with `pr` and `pr_title`, only by a merge of its current pull
//!   request: a merged A replaced by an unmerged B is `closed`; a pull request registered again
//!   shows as the item's pull request; another repository with the same number is another pull
//!   request; the repository matches in any case.
//! - A run in review takes its stage from the watch of its current pull request, also when it
//!   returns to one registered before (`prwatch::register` leaves that row older than another's).
//!
//! No daemon: the range is proposed through the door in this process, registrations go through
//! `prwatch::register` as `makiba pr` writes them, merges are `pr_merged` events, and the range is
//! read with `belt::read_ranges` on the same state.

use makiba::base::stub::Stub;
use makiba::daemon::belt::read_ranges;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::prwatch;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    task: String,
}

impl Org {
    /// A range of project `p` and its run `task` for `issue` at `status`, with its
    /// `belt_dispatched`.
    fn new(task: &str, issue: i64, status: &str) -> Org {
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
            status: Some(status),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET range_id = ?1, dispatched_at = '2026-01-01T00:00:00.000Z',
                   completed_at = CASE WHEN status = 'completed'
                                       THEN '2026-01-02T00:00:00.000Z' END
                 WHERE task_id = ?2",
                rusqlite::params![range, task],
            )
            .unwrap();
        w.append_event(&NewEvent {
            kind: "belt_dispatched",
            actor: Some("makibad"),
            payload: Some(&json!({"range_id": range, "issue": issue, "task_id": task,
                                  "design_only": false})),
            run_task_id: Some(task),
            ..Default::default()
        })
        .unwrap();
        Org {
            _tmp: tmp,
            db,
            task: task.into(),
        }
    }

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.db).unwrap()
    }

    /// `kind` (`pr_opened` or `fix_pushed`) of `repo#pr` at its head, as `makiba pr` registers it.
    fn register(&self, kind: &str, repo: &str, pr: i64) {
        let head = format!("{pr:0>40}");
        prwatch::register(&self.writer(), kind, &self.task, repo, pr, &head).unwrap();
    }

    /// The PR watch read `repo#pr` merged, with `title`.
    fn merged(&self, repo: &str, pr: i64, title: &str) {
        self.writer()
            .append_event(&NewEvent {
                kind: "pr_merged",
                actor: Some("makibad"),
                payload: Some(&json!({"repo": repo, "pr": pr, "title": title,
                                      "task_id": self.task})),
                run_task_id: Some(&self.task),
                ..Default::default()
            })
            .unwrap();
    }

    /// The watch rows of `repo#pr` decided with `conclusion`.
    fn ci(&self, repo: &str, pr: i64, conclusion: &str) {
        let n = self
            .writer()
            .conn()
            .execute(
                "UPDATE pr_watches SET status = 'completed', conclusion = ?3
                 WHERE repo = ?1 AND pr = ?2",
                rusqlite::params![repo, pr, conclusion],
            )
            .unwrap();
        assert_eq!(n, 1, "one watch row of {repo}#{pr}");
    }

    /// The range item of the run.
    fn item(&self) -> Value {
        let c = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let ranges = read_ranges(&c).unwrap();
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        ranges[0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["task_id"] == json!(self.task))
            .unwrap_or_else(|| panic!("no item of {}: {}", self.task, ranges[0]))
            .clone()
    }
}

#[test]
fn a_completed_task_is_landed_only_by_a_merge_of_its_current_pull_request() {
    let org = Org::new("r1-7", 7, "completed");

    // a. A merged, then replaced by an unmerged B: not landed.
    org.register("pr_opened", "o/p", 11);
    org.merged("o/p", 11, "A");
    org.register("pr_opened", "o/p", 12);
    let it = org.item();
    assert_eq!(it["place"], json!("closed"), "{it}");
    assert_eq!(it["why"], json!("no merge recorded"), "{it}");
    assert_eq!(it["pr"], Value::Null, "{it}");
    assert_eq!(it["pr_title"], Value::Null, "{it}");

    // b. A registered again: landed with A.
    org.register("fix_pushed", "o/p", 11);
    let it = org.item();
    assert_eq!(it["place"], json!("landed"), "{it}");
    assert_eq!(it["pr"], json!(11), "{it}");
    assert_eq!(it["pr_title"], json!("A"), "{it}");

    // c. B registered again and merged: landed with B, not with the first merge.
    org.register("pr_opened", "o/p", 12);
    org.merged("o/p", 12, "B");
    let it = org.item();
    assert_eq!(it["place"], json!("landed"), "{it}");
    assert_eq!(it["pr"], json!(12), "{it}");
    assert_eq!(it["pr_title"], json!("B"), "{it}");

    // d. Another repository with the same number: another pull request, not merged.
    org.register("pr_opened", "o2/p", 12);
    let it = org.item();
    assert_eq!(it["place"], json!("closed"), "{it}");
    assert_eq!(it["pr"], Value::Null, "{it}");

    // e. B's repository in another case: B again.
    org.register("pr_opened", "O/P", 12);
    let it = org.item();
    assert_eq!(it["place"], json!("landed"), "{it}");
    assert_eq!(it["pr"], json!(12), "{it}");
    assert_eq!(it["pr_title"], json!("B"), "{it}");
}

#[test]
fn the_review_stage_is_that_of_the_current_pull_requests_watch() {
    let org = Org::new("r1-8", 8, "review");

    // a. No pull request registered.
    let it = org.item();
    assert_eq!(it["place"], json!("review"), "{it}");
    assert_eq!(it["stage"], json!("no_pr"), "{it}");

    // b. A registered, its CI running.
    org.register("pr_opened", "o/p", 11);
    assert_eq!(org.item()["stage"], json!("ci"));

    // c. A's CI passed.
    org.ci("o/p", 11, "passed");
    assert_eq!(org.item()["stage"], json!("to_land"));

    // d. B registered, its own watch running.
    org.register("pr_opened", "o/p", 12);
    assert_eq!(org.item()["stage"], json!("ci"));

    // e. A registered again at its head: its decided row is kept, older than B's.
    org.register("fix_pushed", "o/p", 11);
    let it = org.item();
    assert_eq!(it["place"], json!("review"), "{it}");
    assert_eq!(it["stage"], json!("to_land"), "{it}");

    // f. B passed and A failed: A's own watch decides.
    org.ci("o/p", 12, "passed");
    org.writer()
        .conn()
        .execute(
            "UPDATE pr_watches SET conclusion = 'failed' WHERE repo = 'o/p' AND pr = 11",
            [],
        )
        .unwrap();
    assert_eq!(org.item()["stage"], json!("ci"));
}
