//! Issue 412: a failed run on `makiba status --json` carries why its newest launch failed
//! (`runs[].failed_reason`, the newest launch's `run_launch.failed_reason`), so the reason is not
//! derived by tying `worker_spawn_failed.reason` to a launch by `spawn_id`.
//!
//! The state is written straight into a scratch `state.db`; no `makibad` runs, so the person's
//! `makiba status --json` reads it by itself and exits non-zero.

use makiba::db::writer::{RunUpsert, StateWriter};
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Stdio};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// A run `task` of status `status` with one launch per `(spawn_id, stage, failed_reason)`, oldest
/// first.
fn run(w: &StateWriter, task: &str, status: &str, launches: &[(&str, &str, Option<&str>)]) {
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "acme",
        status: Some(status),
        ..Default::default()
    })
    .unwrap();
    let id: i64 = w
        .conn()
        .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
            r.get(0)
        })
        .unwrap();
    for (spawn, stage, why) in launches {
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, role, spawn_id, kind, agent, brief, worktree,
                                         stage, failed_reason)
                 VALUES (?1, 'worker', ?2, 'code', 'fake', 'the brief', '/w', ?3, ?4)",
                rusqlite::params![id, spawn, stage, why],
            )
            .unwrap();
    }
}

fn status_json(state: &Path) -> Value {
    let out = Command::new(MAKIBA)
        .args(["status", "--json"])
        .env("MAKIBA_STATE_DIR", state)
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE", "stub")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "makibad is not running, yet exit 0");
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn entry<'a>(v: &'a Value, task: &str) -> &'a Value {
    v["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("no runs[]: {v}"))
        .iter()
        .find(|r| r["task_id"] == json!(task))
        .unwrap_or_else(|| panic!("no run {task}: {v}"))
}

#[test]
fn a_failed_run_carries_the_reason_of_its_newest_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    w.ensure_project("acme", None, Some("https://github.com/acme/widget"))
        .unwrap();
    run(
        &w,
        "f1",
        "failed",
        &[
            ("spawn-f1-a", "failed", Some("the first launch's reason")),
            ("spawn-f1-b", "failed", Some("the second launch's reason")),
        ],
    );
    run(
        &w,
        "ok1",
        "in_use",
        &[
            ("spawn-ok1-a", "failed", Some("an old reason")),
            ("spawn-ok1-b", "verified", None),
        ],
    );
    drop(w);

    let v = status_json(&state);
    let f1 = entry(&v, "f1");
    assert_eq!(f1["status"], json!("failed"), "{v}");
    assert_eq!(f1["stage"], json!("failed"), "{v}");
    assert_eq!(
        f1["failed_reason"],
        json!("the second launch's reason"),
        "{v}"
    );

    let ok1 = entry(&v, "ok1");
    assert!(
        ok1.as_object().unwrap().contains_key("failed_reason"),
        "no failed_reason on ok1: {v}"
    );
    assert_eq!(ok1["failed_reason"], Value::Null, "{v}");
}
