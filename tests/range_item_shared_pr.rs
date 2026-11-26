//! Issue 569: every run of a range not closed is linked on the read surface to its current pull
//! request (`ranges[].items[].current_repo`, `.current_pr`, from `belt::current_pr_sql`), so the
//! screen model gives each item of a shared open pull request its CI detail (spec section 12,
//! table "Range field"; docs/design/screen.md section 11).
//!
//! - Two runs on one open pull request, the second having pushed a new head: the one row of
//!   `pull_requests[]` is of the newest registered head, both items are linked to it, both are at
//!   stage `ci` (not the older run's passed head), and both show "checks running", then "checks
//!   failed".
//! - A second run that registers the head another run registered reads the shared watch: stage
//!   `ci`, not `no_pr`.
//! - A closed run has no current pull request (its landing is `pr`); a link keeps the case of the
//!   repository as registered.
//!
//! No daemon: the range is proposed through the door in this process, registrations go through
//! `prwatch::register` as `makiba pr` writes them, CI results are the watch rows and
//! `ci_completed` events the PR watch writes, and the status is `status_local::read` of the same
//! state.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::prwatch;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::screen::{self, Words};
use makiba::spec::Role;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

/// 2026-11-24T10:00:00Z in milliseconds.
const NOW_MS: i64 = 1_795_514_400_000;

const TASKS: [&str; 4] = ["ta", "tb", "tc", "td"];

struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
}

impl Org {
    /// A range of project `p` (repository `o/p`) for Issues 1 to 4, and the runs `ta`, `tb`,
    /// `tc`, `td` (in that order) at `review`, each dispatched for its Issue.
    fn new() -> Org {
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
                json!({"action": "propose", "project": "p", "issues": [1, 2, 3, 4],
                       "at_once": 4, "budget_tasks": 4, "brief_head": "h"}),
                None,
                true,
            )
            .unwrap_or_else(|e| panic!("range propose refused: {e:?}"))["id"]
            .as_i64()
            .unwrap();
        let w = StateWriter::open(&db).unwrap();
        for (i, task) in TASKS.iter().enumerate() {
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                title: Some(task),
                status: Some("review"),
                ..Default::default()
            })
            .unwrap();
            w.conn()
                .execute(
                    "UPDATE runs SET range_id = ?1, dispatched_at = '2026-11-24T08:00:00.000Z'
                     WHERE task_id = ?2",
                    rusqlite::params![range, task],
                )
                .unwrap();
            w.append_event(&NewEvent {
                kind: "belt_dispatched",
                actor: Some("makibad"),
                payload: Some(&json!({"range_id": range, "issue": i as i64 + 1,
                                      "task_id": task, "design_only": false})),
                run_task_id: Some(task),
                ..Default::default()
            })
            .unwrap();
        }
        Org { _tmp: tmp, db }
    }

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.db).unwrap()
    }

    /// `kind` (`pr_opened` or `fix_pushed`) of `repo#pr` at `head` by `task`, as `makiba pr`
    /// registers it.
    fn register(&self, kind: &str, task: &str, repo: &str, pr: i64, head: &str) {
        prwatch::register(&self.writer(), kind, task, repo, pr, head).unwrap();
    }

    /// The watch of `head` concluded with `conclusion` (`passed` or `failed`), and the
    /// `ci_completed` the PR watch writes for it.
    fn concluded(&self, task: &str, repo: &str, pr: i64, head: &str, conclusion: &str) {
        let w = self.writer();
        let n = w
            .conn()
            .execute(
                "UPDATE pr_watches SET status = 'completed', conclusion = ?2,
                   finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE head_oid = ?1",
                rusqlite::params![head, conclusion],
            )
            .unwrap();
        assert_eq!(n, 1, "one watch row of {head}");
        w.append_event(&NewEvent {
            kind: "ci_completed",
            actor: Some("makibad"),
            payload: Some(&json!({"repo": repo, "pr": pr, "head_oid": head,
                                  "status": conclusion, "fail_count": 0, "pending_count": 0,
                                  "total_checks": 1, "probe_attempts": 1})),
            run_task_id: Some(task),
            ..Default::default()
        })
        .unwrap();
    }

    /// The status of watch rows of `head`.
    fn watch_status(&self, head: &str) -> Vec<String> {
        let w = self.writer();
        let mut q = w
            .conn()
            .prepare("SELECT status FROM pr_watches WHERE head_oid = ?1 ORDER BY id")
            .unwrap();
        q.query_map([head], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn status(&self) -> Value {
        makiba::status_local::read(&self.db).unwrap()
    }
}

fn head(c: char) -> String {
    c.to_string().repeat(40)
}

fn en(key: &str) -> String {
    Words::new("en").get(key)
}

/// The item of `task` in the one range of `v` (a status, or the screen's model).
fn item<'a>(v: &'a Value, task: &str) -> &'a Value {
    let ranges = v["ranges"].as_array().expect("ranges");
    assert_eq!(ranges.len(), 1, "{v}");
    ranges[0]["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["task_id"] == json!(task))
        .unwrap_or_else(|| panic!("no item of {task}: {}", ranges[0]))
}

/// The rows of `pull_requests[]` of `repo` (any case) and `pr`.
fn rows<'a>(v: &'a Value, repo: &str, pr: i64) -> Vec<&'a Value> {
    v["pull_requests"]
        .as_array()
        .expect("pull_requests")
        .iter()
        .filter(|p| {
            p["repo"]
                .as_str()
                .is_some_and(|r| r.eq_ignore_ascii_case(repo))
                && p["pr"] == json!(pr)
        })
        .collect()
}

fn model(status: &Value) -> Value {
    screen::model(status, NOW_MS, &Words::new("en"))
}

#[test]
fn two_runs_on_one_open_pull_request_each_show_its_checks() {
    let org = Org::new();
    let (h1, h2) = (head('1'), head('2'));
    org.register("pr_opened", "ta", "o/p", 7, &h1);
    org.concluded("ta", "o/p", 7, &h1, "passed");
    // The replacement run pushes a new head on the same branch.
    org.register("fix_pushed", "tb", "o/p", 7, &h2);
    assert_eq!(org.watch_status(&h1), vec!["completed".to_string()]);
    assert_eq!(org.watch_status(&h2), vec!["watching".to_string()]);

    let s = org.status();
    let listed = rows(&s, "o/p", 7);
    assert_eq!(listed.len(), 1, "{}", s["pull_requests"]);
    let row = listed[0];
    assert_eq!(row["task_id"], json!("ta"), "{row}");
    assert_eq!(row["state"], json!("open"), "{row}");
    assert_eq!(row["head_oid"], json!(h2), "{row}");
    assert_eq!(row["ci"], json!("running"), "{row}");
    for task in ["ta", "tb"] {
        let it = item(&s, task);
        assert_eq!(it["current_repo"], json!("o/p"), "{it}");
        assert_eq!(it["current_pr"], json!(7), "{it}");
        assert_eq!(it["pr"], Value::Null, "{it}");
        assert_eq!(it["stage"], json!("ci"), "{it}");
    }
    let it = item(&s, "tc");
    assert_eq!(it["current_repo"], Value::Null, "{it}");
    assert_eq!(it["current_pr"], Value::Null, "{it}");
    let m = model(&s);
    for task in ["ta", "tb"] {
        let it = item(&m, task);
        assert_eq!(it["detail"], json!(en("stage.checks_running")), "{it}");
    }

    org.concluded("tb", "o/p", 7, &h2, "failed");
    let s = org.status();
    let listed = rows(&s, "o/p", 7);
    assert_eq!(listed.len(), 1, "{}", s["pull_requests"]);
    assert_eq!(listed[0]["ci"], json!("failed"), "{}", listed[0]);
    for task in ["ta", "tb"] {
        let it = item(&s, task);
        assert_eq!(it["stage"], json!("ci"), "{it}");
    }
    let m = model(&s);
    for task in ["ta", "tb"] {
        let it = item(&m, task);
        assert_eq!(it["detail"], json!(en("stage.checks_failed")), "{it}");
    }
}

#[test]
fn a_second_run_at_the_same_head_reads_the_shared_watch() {
    let org = Org::new();
    let h1 = head('3');
    org.register("pr_opened", "ta", "o/p", 7, &h1);
    org.register("pr_opened", "tb", "o/p", 7, &h1);
    assert_eq!(org.watch_status(&h1), vec!["watching".to_string()]);

    let s = org.status();
    assert_eq!(rows(&s, "o/p", 7).len(), 1, "{}", s["pull_requests"]);
    for task in ["ta", "tb"] {
        let it = item(&s, task);
        assert_eq!(it["current_pr"], json!(7), "{it}");
        assert_eq!(it["stage"], json!("ci"), "{it}");
    }
    let m = model(&s);
    for task in ["ta", "tb"] {
        let it = item(&m, task);
        assert_eq!(it["detail"], json!(en("stage.checks_running")), "{it}");
    }
}

#[test]
fn a_closed_run_has_no_current_pull_request_and_a_repository_keeps_its_case() {
    let org = Org::new();
    let h1 = head('4');
    org.register("pr_opened", "ta", "o/p", 7, &h1);
    let w = org.writer();
    w.append_event(&NewEvent {
        kind: "pr_merged",
        actor: Some("makibad"),
        payload: Some(
            &json!({"repo": "o/p", "pr": 7, "title": "T", "task_id": "ta",
                              "head_oid": h1}),
        ),
        run_task_id: Some("ta"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "UPDATE runs SET status = 'completed', completed_at = '2026-11-24T09:00:00.000Z'
             WHERE task_id = 'ta'",
            [],
        )
        .unwrap();
    org.register("pr_opened", "td", "O/P", 8, &head('5'));

    let s = org.status();
    let it = item(&s, "ta");
    assert_eq!(it["place"], json!("landed"), "{it}");
    assert_eq!(it["pr"], json!(7), "{it}");
    assert_eq!(it["current_repo"], Value::Null, "{it}");
    assert_eq!(it["current_pr"], Value::Null, "{it}");
    let it = item(&s, "td");
    assert_eq!(it["current_repo"], json!("O/P"), "{it}");
    assert_eq!(it["current_pr"], json!(8), "{it}");
}
