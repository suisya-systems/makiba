//! Issue 491: `makiba status` reads a pull request's close from any run.
//!
//! - A pull request two open runs name reads `closed` when the newest of its `pr_closed`,
//!   `pr_reopened`, `pr_opened` and `fix_pushed` (repository in any case, and number), of any
//!   run, is a `pr_closed`, also when only the run listed second recorded that close.
//! - A reopen by any run after that close reads it `open` again.
//!
//! The state database is built in this process with the writer, and the read is
//! `status_local::read` of it, the object `makiba status --json` prints.

use makiba::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};

/// A state database of its own: project `p`, runs `ta` (`failed`) and `tb` (`review`), both of
/// which registered `o/q#9`, and a `pr_closed` of it on `tb` only.
struct Org {
    tmp: tempfile::TempDir,
    w: StateWriter,
}

impl Org {
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
        let project = w
            .ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        for (task, status) in [("ta", "failed"), ("tb", "review")] {
            w.conn()
                .execute(
                    "INSERT INTO runs (task_id, project_id, pattern, title, status)
                     VALUES (?1, ?2, 'B', ?1, ?3)",
                    rusqlite::params![task, project, status],
                )
                .unwrap();
        }
        let org = Org { tmp, w };
        let head = "a".repeat(40);
        for task in ["ta", "tb"] {
            org.event(
                "pr_opened",
                task,
                json!({"repo": "o/q", "pr": 9, "head_oid": head}),
            );
        }
        org.event(
            "pr_closed",
            "tb",
            json!({"repo": "o/q", "pr": 9, "title": "T", "task_id": "tb"}),
        );
        org
    }

    fn event(&self, kind: &str, task: &str, payload: Value) {
        self.w
            .append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(&payload),
                run_task_id: Some(task),
                ..Default::default()
            })
            .unwrap();
    }

    /// The one entry of `pull_requests[]` of `o/q#9` (repository in any case).
    fn entry(&self) -> Value {
        let v = makiba::status_local::read(&self.tmp.path().join("state.db")).unwrap();
        let listed: Vec<Value> = v["pull_requests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| {
                e["repo"]
                    .as_str()
                    .is_some_and(|r| r.eq_ignore_ascii_case("o/q"))
                    && e["pr"] == json!(9)
            })
            .cloned()
            .collect();
        assert_eq!(listed.len(), 1, "{}", v["pull_requests"]);
        listed.into_iter().next().unwrap()
    }
}

#[test]
fn a_close_recorded_by_another_run_reads_closed() {
    let org = Org::new();
    let e = org.entry();
    assert_eq!(e["state"], json!("closed"), "{e}");
}

#[test]
fn a_reopen_by_any_run_after_the_close_reads_open() {
    let org = Org::new();
    org.event(
        "pr_reopened",
        "ta",
        json!({"repo": "O/Q", "pr": 9, "task_id": "ta"}),
    );
    let e = org.entry();
    assert_eq!(e["state"], json!("open"), "{e}");
}
