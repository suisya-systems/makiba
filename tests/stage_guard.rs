//! Issue 163: a pass does not overwrite a launch that was failed or suspended underneath it.
//!
//! The daemon runs in this process on `Local` (as tests/launch_idempotent.rs), one document launch
//! per test. `<state>/makibad.hold-at` names the stage update a pass must stop before: a debug
//! build of `makibad` writes `<state>/makibad.held` there and waits until it is removed. While the
//! pass holds what it read, the test closes the run (`close --abandon`, through the door) or
//! suspends it (`down::suspend`, as `makiba up` does after a stop of the base); then it sets the
//! stop flag and releases the pass, so the daemon ends right after the tick that held. Nothing
//! later can hide or repair what that tick wrote.

mod common;

use common::local::Local;
use makiba::base::Base;
use makiba::daemon;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(30);

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_FAKE_AGENT", agent);
            std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "3000");
        }
    });
}

struct Org {
    dir: PathBuf,
    task: String,
    spawn: String,
    base: Local,
    _tmp: tempfile::TempDir,
}

/// Ends every terminal when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

/// A queued run with one planned document launch whose test agent runs `steps`.
fn org(task: &str, steps: serde_json::Value) -> Org {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let work = root.join(task);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("steps.json"), steps.to_string()).unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    let spawn = format!("spawn-{task}");
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'document', 'fake',
                     'the brief', ?3, 'planned')",
            [task, &spawn, &work.to_string_lossy()],
        )
        .unwrap();
    Org {
        dir,
        task: task.to_string(),
        spawn,
        base: Local::new(),
        _tmp: tmp,
    }
}

/// What a pass may write for a launch: its events, the messages, the launch rows.
type Written = (i64, i64, i64);

impl Org {
    fn one<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        let conn = Connection::open_with_flags(
            self.dir.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn written(&self) -> Written {
        (
            self.one(
                "SELECT count(*) FROM events WHERE kind IN
                 ('worker_spawned', 'worker_spawn_verified', 'worker_spawn_failed')",
            ),
            self.one("SELECT count(*) FROM messages"),
            self.one("SELECT count(*) FROM run_launch"),
        )
    }

    /// The live terminals carrying the launch's `makiba.spawn` tag.
    fn tagged(&self) -> usize {
        self.base
            .list()
            .unwrap()
            .iter()
            .filter(|t| t.alive && t.tags.get("makiba.spawn") == Some(&self.spawn))
            .count()
    }

    /// Start a daemon that holds before `step`, and wait until it holds there.
    fn hold_at(&self, step: &str) -> (Arc<AtomicBool>, JoinHandle<Result<(), String>>) {
        std::fs::write(self.dir.join("makibad.hold-at"), step).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.dir.clone(), self.base.clone(), Arc::clone(&stop));
        let daemon =
            std::thread::spawn(move || daemon::run_until(&dir, &|| Box::new(base.clone()), flag));
        let held = self.dir.join("makibad.held");
        let until = Instant::now() + WAIT;
        while !held.exists() && !daemon.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !held.exists() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
            panic!("makibad did not hold before {step}");
        }
        (stop, daemon)
    }

    /// Stop the daemon after the held tick, release the pass and wait for the daemon to end.
    fn release(&self, (stop, daemon): (Arc<AtomicBool>, JoinHandle<Result<(), String>>)) {
        stop.store(true, Ordering::SeqCst);
        std::fs::remove_file(self.dir.join("makibad.held")).unwrap();
        daemon.join().unwrap().unwrap();
    }

    /// `close --abandon`, as the person asks it through the door.
    fn close(&self) {
        let w = StateWriter::open(&self.dir.join("state.db")).unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let door = Door::new(w, Arc::new(self.base.clone()), "fake".into());
        door.handle(
            Some(&person),
            "close",
            json!({"task_id": self.task, "abandon": true}),
            None,
            false,
        )
        .unwrap();
    }
}

/// Held before `step`, the run closed: the launch stays `failed` with the close's reason, no
/// terminal with its spawn tag is left, and nothing more is written for it.
fn closed_while_held(step: &str, steps: serde_json::Value) -> Org {
    let org = org(&format!("t-{}", step.replace('_', "-")), steps);
    let held = org.hold_at(step);
    org.close();
    let before = org.written();
    org.release(held);
    assert_eq!(
        org.one::<String>("SELECT stage || ' ' || failed_reason FROM run_launch"),
        "failed makiba close"
    );
    assert_eq!(org.one::<String>("SELECT status FROM runs"), "abandoned");
    assert_eq!(org.written(), before, "nothing more is written for it");
    assert_eq!(org.tagged(), 0, "no terminal carries the spawn tag");
    org
}

#[test]
fn closed_while_its_worktree_was_prepared() {
    closed_while_held("worktree_ready", json!(["working", "hang"]));
}

#[test]
fn closed_while_its_settings_were_prepared() {
    closed_while_held("settings_ready", json!(["working", "hang"]));
}

#[test]
fn closed_while_it_was_launched() {
    let org = closed_while_held("launched", json!(["working", "hang"]));
    assert_eq!(
        org.one::<i64>("SELECT count(*) FROM agents WHERE role = 'worker' AND revoked_at IS NULL"),
        0,
        "the key issued for it is revoked"
    );
}

#[test]
fn closed_while_its_dialog_was_passed() {
    closed_while_held("dialog_passed", json!(["working", "hang"]));
}

#[test]
fn closed_while_it_was_verified() {
    closed_while_held("verified", json!(["working", "hang"]));
}

/// The failure path is a stage update too: a launch timed out by verify while it was closed.
#[test]
fn closed_while_it_was_failed_by_the_timeout() {
    closed_while_held("failed", json!(["hang"]));
}

#[test]
fn suspended_while_it_was_verified() {
    let org = org("t-suspended", json!(["working", "hang"]));
    let held = org.hold_at("verified");
    let w = StateWriter::open(&org.dir.join("state.db")).unwrap();
    assert_eq!(makiba::down::suspend(&w).unwrap(), vec![org.task.clone()]);
    let before = org.written();
    org.release(held);
    assert_eq!(org.one::<String>("SELECT status FROM runs"), "suspended");
    assert_eq!(
        org.one::<String>("SELECT stage || ' ' || failed_reason FROM run_launch"),
        "failed makiba down"
    );
    assert_eq!(org.written(), before, "nothing more is written for it");
    assert_eq!(org.tagged(), 0, "no terminal carries the spawn tag");
}
