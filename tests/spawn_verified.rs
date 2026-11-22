//! M3-6 (Issue 29): a launch is verified only by an observed first turn. The daemon runs in this
//! process on `Local` (as tests/startup_dialog.rs); a queued run's launch waits at
//! `settings_ready` and the launch (M3-4) and the dialogs (M3-5) take it on. The test agent is
//! given by `MAKIBA_FAKE_AGENT`: it runs the steps of `steps.json` in its working folder (the
//! launch's workplace). The time a launch has to be verified is `MAKIBA_VERIFY_TIMEOUT_MS`
//! (default 180 s), set here to `TIMEOUT_MS` so the test does not wait three minutes.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, TerminalRef};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const TIMEOUT_MS: i64 = 5000;

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_FAKE_AGENT", agent);
            std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", TIMEOUT_MS.to_string());
        }
    });
}

/// Ends the terminal when the test ends, passed or failed.
struct Ended(Local, TerminalRef);

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.end(&self.1);
    }
}

struct Org {
    dir: PathBuf,
    _ended: Ended,
    _tmp: tempfile::TempDir,
}

/// A daemon on `Local` with one launch of `task`, at `settings_ready`, whose agent runs `steps`.
/// `seed` writes to the database before the daemon starts.
fn org(task: &str, steps: &[&str], seed: impl FnOnce(&StateWriter)) -> Org {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let work = root.join(task);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("steps.json"), json!(steps).to_string()).unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        // Not `in_use`: an `in_use` run with no terminal is recorded as lost (M2-5).
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    seed(&w);
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'spawn-' || ?1, 'document',
                     'fake', 'the brief', ?2, 'settings_ready')",
            [task.to_string(), work.to_string_lossy().into_owned()],
        )
        .unwrap();
    drop(w);
    let local = Local::new();
    let ended = Ended(local.clone(), TerminalRef(format!("w-{task}")));
    let daemon_dir = dir.clone();
    std::thread::spawn(move || makibad::run(&daemon_dir, &|| Box::new(local.clone())));
    Org {
        dir,
        _ended: ended,
        _tmp: tmp,
    }
}

impl Org {
    fn db(&self) -> Option<Connection> {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
    }

    fn one<T: rusqlite::types::FromSql>(&self, sql: &str) -> Option<T> {
        self.db()?.query_row(sql, [], |r| r.get(0)).ok()
    }

    /// Wait up to `TIMEOUT_MS` plus 20 seconds for the newest launch to reach `stage`.
    fn reaches(&self, stage: &str) {
        let sql = "SELECT stage FROM run_launch ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + Duration::from_millis(TIMEOUT_MS as u64 + 20_000);
        while Instant::now() < until {
            if self.one::<String>(sql).as_deref() == Some(stage) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(self.one::<String>(sql).as_deref(), Some(stage));
    }

    /// The payloads of the events of `kind`, oldest first.
    fn payloads(&self, kind: &str) -> Vec<Value> {
        let conn = self.db().unwrap();
        let mut q = conn
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }
}

#[test]
fn an_agent_that_reaches_working_is_verified_and_its_run_is_in_use() {
    // Records of other terminals of the same task and role, before this launch: an earlier
    // launch's working status, and one without a launch identity.
    let org = org("t1", &["working", "hang"], |w| {
        for spawn in [json!("spawn-old"), Value::Null] {
            w.append_event(&NewEvent {
                kind: "terminal_status",
                actor: Some("makibad"),
                payload: Some(&json!({
                    "task_id": "t1", "role": "worker", "spawn_id": spawn,
                    "state": "working", "kind": null, "msg": null,
                })),
                run_task_id: Some("t1"),
                ..Default::default()
            })
            .unwrap();
        }
    });
    org.reaches("verified");
    assert_eq!(
        org.one::<String>("SELECT status FROM runs WHERE task_id = 't1'")
            .as_deref(),
        Some("in_use")
    );
    let verified = org.payloads("worker_spawn_verified");
    assert_eq!(verified.len(), 1, "{verified:?}");
    assert_eq!(verified[0]["spawn_id"], "spawn-t1");
    // The event that verified it is the working status of this launch's own terminal.
    let id = verified[0]["event_id"].as_i64().expect("an event id");
    let (kind, payload): (String, String) = org
        .db()
        .unwrap()
        .query_row(
            "SELECT kind, payload_json FROM events WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(kind, "terminal_status");
    assert_eq!(payload["state"], "working");
    assert_eq!(payload["spawn_id"], "spawn-t1");
}

#[test]
fn an_agent_that_launches_and_never_reports_is_failed_after_the_timeout() {
    let org = org("t2", &["hang"], |_| {});
    org.reaches("failed");
    let failed = org.payloads("worker_spawn_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["spawn_id"], "spawn-t2");
    assert!(org.payloads("worker_spawn_verified").is_empty());
    // Not before the timeout: measured from the launch's `worker_spawned`.
    let waited: i64 = org
        .one(
            "SELECT CAST(ROUND((julianday(f.occurred_at) - julianday(s.occurred_at)) * 86400000)
                    AS INTEGER)
             FROM events f, events s
             WHERE f.kind = 'worker_spawn_failed' AND s.kind = 'worker_spawned'",
        )
        .unwrap();
    assert!(waited >= TIMEOUT_MS, "failed {waited} ms after the launch");
    assert_ne!(
        org.one::<String>("SELECT status FROM runs WHERE task_id = 't2'")
            .as_deref(),
        Some("in_use")
    );
    // A notice to the secretary about the run, queued for delivery.
    let queued: i64 = org
        .one(
            "SELECT count(*) FROM messages m
             JOIN event_deliveries d ON d.source_event_id = m.event_id
             WHERE m.kind = 'notice' AND m.recipient = 'secretary'
               AND m.run_id = (SELECT id FROM runs WHERE task_id = 't2')
               AND d.recipient = 'secretary' AND d.status = 'pending'",
        )
        .unwrap();
    assert_eq!(queued, 1);
}

#[test]
fn no_code_path_writes_worker_spawn_verified_without_a_status_or_turn_event_id() {
    let tmp = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t3",
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    let record = |kind: &str, spawn: &str, extra: Value| {
        let mut p = json!({"task_id": "t3", "role": "worker", "spawn_id": spawn});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&p),
            run_task_id: Some("t3"),
            ..Default::default()
        })
        .unwrap()
    };
    let idle = record("terminal_status", "s3", json!({"state": "idle"}));
    let other = record("terminal_status", "s-other", json!({"state": "working"}));
    let spawned = record("worker_spawned", "s3", json!({}));
    let working = record("terminal_status", "s3", json!({"state": "working"}));
    let turn = record("terminal_turn", "s3", json!({"turn": 1}));
    let count = || -> i64 {
        w.conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'worker_spawn_verified'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };

    let bad = [
        json!({"task_id": "t3", "spawn_id": "s3"}),
        json!({"task_id": "t3", "spawn_id": "s3", "event_id": null}),
        json!({"task_id": "t3", "spawn_id": "s3", "event_id": idle}),
        json!({"task_id": "t3", "spawn_id": "s3", "event_id": other}),
        json!({"task_id": "t3", "spawn_id": "s3", "event_id": spawned}),
        json!({"task_id": "t3", "spawn_id": "s3", "event_id": 999_999}),
        json!({"task_id": "t3", "event_id": working}),
    ];
    for payload in &bad {
        // Through the writer every daemon pass uses.
        let by_writer = w.append_event(&NewEvent {
            kind: "worker_spawn_verified",
            actor: Some("makibad"),
            payload: Some(payload),
            run_task_id: Some("t3"),
            ..Default::default()
        });
        assert!(by_writer.is_err(), "written by the writer: {payload}");
        // Through raw SQL on the same connection, as a second writer could.
        let by_sql = w.conn().execute(
            "INSERT INTO events (kind, payload_json) VALUES ('worker_spawn_verified', ?1)",
            [payload.to_string()],
        );
        assert!(by_sql.is_err(), "written by SQL: {payload}");
        // By turning another event into one.
        let by_update = w.conn().execute(
            "UPDATE events SET kind = 'worker_spawn_verified', payload_json = ?1 WHERE id = ?2",
            rusqlite::params![payload.to_string(), spawned],
        );
        assert!(by_update.is_err(), "written by an update: {payload}");
    }
    assert_eq!(count(), 0);

    // A working status or a turn of the same terminal is what it takes.
    for evidence in [working, turn] {
        w.append_event(&NewEvent {
            kind: "worker_spawn_verified",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": "t3", "spawn_id": "s3", "event_id": evidence})),
            run_task_id: Some("t3"),
            ..Default::default()
        })
        .unwrap();
    }
    assert_eq!(count(), 2);
}
