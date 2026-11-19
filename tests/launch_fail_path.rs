//! Issue 124: a launch that fails at preparation, or that the base refuses with no terminal,
//! fails its run and tells the secretary, by the one failure path of M3-6. The daemon runs in
//! this process on `Local` (no door, so no Unix socket is needed); the test agent is a program
//! that does not exist, so the base refuses every launch that gets as far as the base.

mod common;

use common::local::Local;
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

/// One scratch root for the places of spec section 2, kept for the life of the process.
fn setup() -> PathBuf {
    static ONCE: Once = Once::new();
    let root = std::env::temp_dir().join(format!("makiba-124-{}", std::process::id()));
    ONCE.call_once(|| {
        std::fs::create_dir_all(root.join("repos")).unwrap();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var(
                "MAKIBA_FAKE_AGENT",
                serde_json::json!(["/nonexistent/makiba-agent", "record"]).to_string(),
            );
        }
    });
    root
}

/// A daemon with one queued run `t1` and its launch of `kind` at `stage`; the state folder.
fn org(kind: &str, stage: &str) -> (tempfile::TempDir, PathBuf) {
    let root = setup();
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let work = root.join(format!("work-{kind}-{stage}"));
    std::fs::create_dir_all(&work).unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', ?1,
                     'fake', 'the brief', ?2, ?3)",
            [kind, &work.to_string_lossy(), stage],
        )
        .unwrap();
    drop(w);
    let local = Local::new();
    let daemon_dir = dir.clone();
    std::thread::spawn(move || daemon::run(&daemon_dir, &|| Box::new(local.clone())));
    (tmp, dir)
}

fn db(dir: &std::path::Path) -> Connection {
    Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// Wait up to 20 seconds for the run `t1` to be `failed`.
fn failed(dir: &std::path::Path) {
    let until = Instant::now() + Duration::from_secs(20);
    let status = || {
        db(dir)
            .query_row("SELECT status FROM runs WHERE task_id = 't1'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
    };
    while Instant::now() < until && status().as_deref() != Some("failed") {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(status().as_deref(), Some("failed"), "the run did not fail");
}

/// The run failed, its launch failed at `stage` with a reason, and the secretary has exactly one
/// notice pending that names the stage.
fn told_the_secretary(dir: &std::path::Path, stage: &str) {
    let conn = db(dir);
    let (launch, why): (String, String) = conn
        .query_row("SELECT stage, failed_reason FROM run_launch", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(launch, "failed");
    assert!(!why.is_empty());
    let p: String = conn
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'worker_spawn_failed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let p: Value = serde_json::from_str(&p).unwrap();
    assert_eq!(p["stage"], stage);
    assert_eq!(p["reason"], why.as_str());
    std::thread::sleep(Duration::from_millis(1500));
    let notices: Vec<(String, String)> = {
        let mut q = conn
            .prepare(
                "SELECT m.body, d.status FROM messages m JOIN event_deliveries d
                 ON d.source_event_id = m.event_id
                 WHERE m.kind = 'notice' AND m.recipient = 'secretary'",
            )
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].0.contains(stage), "{notices:?}");
    assert_eq!(notices[0].1, "pending");
}

/// A `code` launch whose project has no repository fails its run, at `planned`.
#[test]
fn a_code_launch_with_no_repository_fails_its_run_and_tells_the_secretary() {
    let (_tmp, dir) = org("code", "planned");
    failed(&dir);
    told_the_secretary(&dir, "planned");
}

/// A launch the base refuses, with no terminal of it, fails its run, at `settings_ready`.
#[test]
fn a_launch_the_base_refuses_with_no_terminal_fails_its_run_and_tells_the_secretary() {
    let (_tmp, dir) = org("document", "settings_ready");
    failed(&dir);
    told_the_secretary(&dir, "settings_ready");
}
