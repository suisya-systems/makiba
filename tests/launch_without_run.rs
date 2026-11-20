//! Issue 166: a launch that belongs to no run (the curator, the secretary).
//!
//! The daemon runs in this process on `Local` (as tests/stage_guard.rs) with the test agent, whose
//! steps are read from `steps.json` in the folder it starts in. A launch without a run is added by
//! `daemon::launch::add_launch`, the function `makibad`'s own code uses; a worker launch is a
//! queued run with a planned document launch, as the door leaves it. `max_concurrent_workers` is 1
//! for every test here, and a launch has `TIMEOUT_MS` to be verified.

mod common;

use common::local::Local;
use makiba::base::Base;
use makiba::daemon;
use makiba::daemon::launch::add_launch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const TIMEOUT_MS: u64 = 8000;
const WAIT: Duration = Duration::from_secs(40);

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_FAKE_AGENT", agent);
            std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", TIMEOUT_MS.to_string());
            std::env::set_var("MAKIBA_MAX_CONCURRENT_WORKERS", "1");
        }
    });
}

/// A running daemon: its stop flag and its thread.
type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    root: PathBuf,
    dir: PathBuf,
    base: Local,
    daemon: Option<Daemon>,
    _tmp: tempfile::TempDir,
}

/// The daemon is stopped and every terminal ended when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        if let Some((stop, daemon)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
        }
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

fn org() -> Org {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    drop(StateWriter::open(&dir.join("state.db")).unwrap());
    Org {
        root,
        dir,
        base: Local::new(),
        daemon: None,
        _tmp: tmp,
    }
}

fn steps(folder: &Path, steps: Value) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("steps.json"), steps.to_string()).unwrap();
}

impl Org {
    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.dir.join("state.db")).unwrap()
    }

    /// A launch of `role` without a run, in `<root>/<name>`, whose test agent runs `s`.
    fn without_run(&self, role: Role, name: &str, s: Value) -> String {
        let folder = self.root.join(name);
        steps(&folder, s);
        add_launch(&self.writer(), role, &folder, "the brief", None, "fake").unwrap()
    }

    /// A queued run `task` with a planned document launch whose test agent runs `s`.
    fn worker(&self, task: &str, s: Value) -> String {
        let work = self.root.join(task);
        steps(&work, s);
        let w = self.writer();
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
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'document', 'fake',
                         'the brief', ?3)",
                [task, &spawn, &work.to_string_lossy()],
            )
            .unwrap();
        spawn
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.dir.clone(), self.base.clone(), Arc::clone(&stop));
        let daemon =
            std::thread::spawn(move || daemon::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, daemon));
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    }

    fn one<T: rusqlite::types::FromSql>(&self, sql: &str, p: impl rusqlite::Params) -> T {
        self.db().query_row(sql, p, |r| r.get(0)).unwrap()
    }

    fn stage(&self, spawn: &str) -> String {
        self.one("SELECT stage FROM run_launch WHERE spawn_id = ?1", [spawn])
    }

    /// Wait until the launch `spawn` reaches `stage`.
    fn reaches(&self, spawn: &str, stage: &str) {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if self.stage(spawn) == stage {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(self.stage(spawn), stage, "launch {spawn}");
    }

    /// The id of the first event of `kind` whose payload names `spawn`, if any.
    fn event(&self, kind: &str, spawn: &str) -> Option<i64> {
        self.one(
            "SELECT min(id) FROM events
             WHERE kind = ?1 AND json_extract(payload_json, '$.spawn_id') = ?2",
            [kind, spawn],
        )
    }
}

#[test]
fn a_curator_without_a_run_goes_to_verified_with_its_own_terminal_and_key() {
    let mut org = org();
    let spawn = org.without_run(Role::Curator, "knowledge", json!(["working", "hang"]));
    org.start();
    org.reaches(&spawn, "verified");

    // Every stage was passed: launched, then verified by an observed first turn.
    assert!(org.event("worker_spawned", &spawn).is_some());
    assert!(org.event("worker_spawn_verified", &spawn).is_some());
    assert_eq!(org.one::<i64>("SELECT count(*) FROM runs", []), 0);
    assert_eq!(
        org.one::<Option<i64>>(
            "SELECT run_id FROM run_launch WHERE spawn_id = ?1",
            [&spawn]
        ),
        None
    );

    // One terminal named `curator`, tagged as the curator of this launch, with no task.
    let ours: Vec<_> = org
        .base
        .list()
        .unwrap()
        .into_iter()
        .filter(|t| t.tags.get("makiba.spawn") == Some(&spawn))
        .collect();
    assert_eq!(ours.len(), 1, "{ours:?}");
    let t = &ours[0];
    assert_eq!(t.name, "curator");
    assert_eq!(
        t.tags.get("makiba.role").map(String::as_str),
        Some("curator")
    );
    assert_eq!(t.tags.get("makiba.task_id"), None);
    let curators = org
        .base
        .list()
        .unwrap()
        .into_iter()
        .filter(|t| t.tags.get("makiba.role").map(String::as_str) == Some("curator"))
        .count();
    assert_eq!(curators, 1);

    // Its own key: the curator's, issued for this launch, the one the terminal carries.
    let (agent_id, role, run_id, live): (String, String, Option<i64>, i64) = org
        .db()
        .query_row(
            "SELECT agent_id, role, run_id, count(*) FROM agents
             WHERE spawn_id = ?1 AND revoked_at IS NULL",
            [&spawn],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(live, 1);
    assert_eq!((agent_id.as_str(), role.as_str()), ("curator", "curator"));
    assert_eq!(run_id, None);
    let hash: String = org.one(
        "SELECT key_sha256 FROM agents WHERE spawn_id = ?1 AND revoked_at IS NULL",
        [&spawn],
    );
    assert_eq!(t.tags.get("makiba.key"), Some(&hash));
}

#[test]
fn a_launch_without_a_run_not_verified_in_time_fails_with_a_notice_and_changes_no_run() {
    let mut org = org();
    // A run that is no launch's: nothing may change it.
    org.writer()
        .upsert_run(&RunUpsert {
            task_id: "bystander",
            project_slug: "p",
            status: Some("review"),
            ..Default::default()
        })
        .unwrap();
    let before: String = org.one(
        "SELECT json_array(status, completed_at, outcome_note) FROM runs",
        [],
    );
    let spawn = org.without_run(Role::Curator, "knowledge", json!(["hang"]));
    org.start();
    org.reaches(&spawn, "failed");

    assert!(org.event("worker_spawn_failed", &spawn).is_some());
    assert!(org.event("worker_spawn_verified", &spawn).is_none());
    let after: String = org.one(
        "SELECT json_array(status, completed_at, outcome_note) FROM runs",
        [],
    );
    assert_eq!(after, before);
    assert_eq!(org.one::<i64>("SELECT count(*) FROM runs", []), 1);
    // A notice to the secretary, about no run, queued for delivery.
    let notices: i64 = org.one(
        "SELECT count(*) FROM messages m
         JOIN event_deliveries d ON d.source_event_id = m.event_id
         WHERE m.kind = 'notice' AND m.recipient = 'secretary' AND m.run_id IS NULL
           AND d.recipient = 'secretary'",
        [],
    );
    assert_eq!(notices, 1);
}

#[test]
fn a_worker_launch_still_ends_with_its_run_in_use() {
    let mut org = org();
    let spawn = org.worker("t1", json!(["working", "hang"]));
    org.start();
    org.reaches(&spawn, "verified");
    assert_eq!(
        org.one::<String>("SELECT status FROM runs WHERE task_id = 't1'", []),
        "in_use"
    );
    assert_eq!(
        org.one::<String>("SELECT role FROM run_launch WHERE spawn_id = ?1", [&spawn]),
        "worker"
    );
    let ours: Vec<_> = org
        .base
        .list()
        .unwrap()
        .into_iter()
        .filter(|t| t.tags.get("makiba.spawn") == Some(&spawn))
        .collect();
    assert_eq!(ours.len(), 1, "{ours:?}");
    assert_eq!(ours[0].name, "w-t1");
    assert_eq!(
        ours[0].tags.get("makiba.role").map(String::as_str),
        Some("worker")
    );
    assert_eq!(
        ours[0].tags.get("makiba.task_id").map(String::as_str),
        Some("t1")
    );
}

#[test]
fn a_launch_without_a_run_is_not_held_to_capacity_and_does_not_count_against_it() {
    let mut org = org();
    // Capacity 1. The curator's launch is added first; worker `a` hangs at verification for the
    // timeout and holds the one place, so worker `b` waits for it.
    let curator = org.without_run(Role::Curator, "knowledge", json!(["working", "hang"]));
    let a = org.worker("a", json!(["hang"]));
    std::thread::sleep(Duration::from_millis(20));
    let b = org.worker("b", json!(["hang"]));
    org.start();
    org.reaches(&curator, "verified");
    let verified = org.event("worker_spawn_verified", &curator).unwrap();

    // Not counted: `a` was taken while the curator's launch was under way.
    let taken_a = org.event("run_taken", &a).expect("worker a is taken");
    assert!(
        taken_a < verified,
        "worker a was taken only after the curator's launch was verified"
    );
    // Not held: the curator went through while `a` held the only place and `b` waited.
    let a_done: Option<i64> = org.one(
        "SELECT min(id) FROM events WHERE kind IN ('worker_spawn_verified', 'worker_spawn_failed')
         AND json_extract(payload_json, '$.spawn_id') = ?1",
        [&a],
    );
    assert!(
        a_done.is_none_or(|d| d > verified),
        "worker a was done before the curator was verified"
    );
    assert!(
        org.event("run_taken", &b).is_none_or(|t| t > verified),
        "worker b was taken while worker a held the only place"
    );
    assert_eq!(
        org.one::<i64>(
            "SELECT count(*) FROM events WHERE kind = 'run_taken'
             AND json_extract(payload_json, '$.spawn_id') = ?1",
            [&curator]
        ),
        0
    );
}
