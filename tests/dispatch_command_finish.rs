//! Issue 145: an accepted `dispatch` command is finished when its launch is settled.
//!
//! The person's `dispatch` goes through `Door::handle` with the person key (the handler of the
//! person's socket) in this process, and the daemon runs in this process too
//! (`daemon::run_until`), with the test agent. The base is `Local` (tests/common/local.rs), which
//! runs the agent: the in-process `Stub` runs no program, so no first turn of the launch is ever
//! observed on it and nothing is verified.
//!
//! One test, as the places of a dispatch (`MAKIBA_DOCS_DIR`, `MAKIBA_WORKERS_DIR`,
//! `MAKIBA_REPOS_DIR`) are read from the process's environment.

mod common;

use common::local::Local;
use makiba::base::Base;
use makiba::daemon;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(40);

fn one<T: rusqlite::types::FromSql>(
    db: &std::path::Path,
    sql: &str,
    p: impl rusqlite::Params,
) -> T {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row(sql, p, |r| r.get(0))
        .unwrap()
}

/// Wait until the launch of `task` is at `stage`.
fn reaches(db: &std::path::Path, task: &str, stage: &str) {
    let sql = "SELECT l.stage FROM run_launch l JOIN runs r ON r.id = l.run_id
               WHERE r.task_id = ?1 ORDER BY l.id DESC LIMIT 1";
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if one::<String>(db, sql, [task]) == stage {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(one::<String>(db, sql, [task]), stage, "launch of {task}");
}

/// The status and the result of the dispatch command of `task`.
fn command(db: &std::path::Path, task: &str) -> (String, Value) {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.query_row(
        "SELECT status, result_json FROM commands
         WHERE verb = 'dispatch' AND json_extract(args_json, '$.task_id') = ?1",
        [task],
        |r| {
            let j: String = r.get(1)?;
            Ok((r.get(0)?, serde_json::from_str(&j).unwrap()))
        },
    )
    .unwrap()
}

#[test]
fn a_dispatch_command_is_finished_by_its_verified_launch_and_failed_by_its_failed_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    for d in ["state", "docs", "workers", "repos"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // SAFETY: set before any thread of this test binary starts; the only test here.
    unsafe {
        std::env::set_var(
            "MAKIBA_FAKE_AGENT",
            json!([AGENT, "record", "steps:steps.json"]).to_string(),
        );
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "8000");
        std::env::set_var("MAKIBA_MAX_CONCURRENT_WORKERS", "4");
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        // Empty: no project has a repository there.
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
    }
    let db = dir.join("state.db");
    let w = StateWriter::open(&db).unwrap();
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let base = Local::new();
    let door = Door::new(w, Arc::new(base.clone()), "fake".into());

    // The document task's agent starts working (its first turn) and stays.
    let doc = root.join("docs/doc-1");
    std::fs::create_dir_all(&doc).unwrap();
    std::fs::write(
        doc.join("steps.json"),
        json!(["working", "hang"]).to_string(),
    )
    .unwrap();
    let dispatch = |args: Value| {
        let r = door.handle(Some(&person), "dispatch", args, None, true);
        assert!(r.is_ok(), "{r:?}");
    };
    dispatch(json!({"kind": "document", "task_id": "doc-1", "title": "Notes", "brief": "Write."}));
    dispatch(
        json!({"kind": "code", "task_id": "code-1", "title": "Fix", "brief": "Fix it.",
                    "project": "norepo"}),
    );
    assert_eq!(command(&db, "doc-1").0, "accepted");
    assert_eq!(command(&db, "code-1").0, "accepted");

    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (dir, base, flag) = (dir.clone(), base.clone(), Arc::clone(&stop));
        std::thread::spawn(move || daemon::run_until(&dir, &|| Box::new(base.clone()), flag))
    };
    let ended = || {
        stop.store(true, Ordering::SeqCst);
        for t in base.list().unwrap_or_default() {
            let _ = base.end(&t.id);
        }
    };
    struct Guard<F: Fn()>(F);
    impl<F: Fn()> Drop for Guard<F> {
        fn drop(&mut self) {
            (self.0)()
        }
    }
    let guard = Guard(ended);

    // Verified: the command is no longer accepted.
    reaches(&db, "doc-1", "verified");
    let (status, _) = command(&db, "doc-1");
    assert_ne!(
        status, "accepted",
        "the dispatch of doc-1 after its launch was verified"
    );
    assert_eq!(status, "done");

    // Failed (no repository for a code run): the command is failed, with the launch's reason.
    reaches(&db, "code-1", "failed");
    let reason: String = one(
        &db,
        "SELECT l.failed_reason FROM run_launch l JOIN runs r ON r.id = l.run_id
         WHERE r.task_id = 'code-1'",
        [],
    );
    assert!(reason.contains("no git repository"), "{reason}");
    let (status, result) = command(&db, "code-1");
    assert_eq!(status, "failed", "{result}");
    let message = result["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains(&reason), "{result}");

    drop(guard);
    let _ = daemon.join();
}
