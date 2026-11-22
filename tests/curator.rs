//! Issue 39: the curator is launched at the threshold.
//!
//! The daemon runs in this process on `Local` with the test agent (as tests/launch_without_run.rs),
//! its knowledge folder a temporary one (`MAKIBA_KNOWLEDGE_DIR`) and a short time limit
//! (`MAKIBA_CURATOR_TIMEOUT_MS`). A close is the `run_closed` event `door::close` writes. The test
//! agent reads `steps.json` from the folder it starts in, which is the knowledge folder (outside
//! `raw/`, so it is no note). Other passes may notice the secretary after a close (the triage
//! scan), so only the notices that name the curator are counted.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::Base;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const LIMIT_MS: u64 = 6000;
const WAIT: Duration = Duration::from_secs(40);

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    knowledge: PathBuf,
    dir: PathBuf,
    base: Local,
    daemon: Option<Daemon>,
    closes: i64,
    _tmp: tempfile::TempDir,
    _knowledge: makibad::Knowledge,
    _env: MutexGuard<'static, ()>,
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

/// An organization whose knowledge folder holds `notes` raw notes.
fn org(notes: usize) -> Org {
    let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (dir, knowledge) = (root.join("state"), root.join("knowledge"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(knowledge.join("raw")).unwrap();
    std::fs::write(
        knowledge.join("steps.json"),
        json!(["working", "hang"]).to_string(),
    )
    .unwrap();
    drop(StateWriter::open(&dir.join("state.db")).unwrap());
    let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
    let held_knowledge = makibad::use_knowledge(&knowledge);
    // SAFETY: the lock above is held for the whole test; no daemon of an earlier test is running.
    unsafe {
        std::env::set_var("MAKIBA_FAKE_AGENT", agent);
        std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        std::env::set_var("MAKIBA_CURATOR_TIMEOUT_MS", LIMIT_MS.to_string());
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "20000");
    }
    let mut org = Org {
        knowledge,
        dir,
        base: Local::new(),
        daemon: None,
        closes: 0,
        _tmp: tmp,
        _knowledge: held_knowledge,
        _env: env,
    };
    for _ in 0..notes {
        org.note();
    }
    org
}

impl Org {
    fn note(&mut self) {
        let n = std::fs::read_dir(self.knowledge.join("raw"))
            .unwrap()
            .count();
        std::fs::write(self.knowledge.join(format!("raw/note-{n}.md")), "a note").unwrap();
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.dir.clone(), self.base.clone(), Arc::clone(&stop));
        let daemon =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, daemon));
    }

    /// A close: the event the door writes when it accepts one. The daemon writes too, so a busy
    /// database is tried again.
    fn close(&mut self) {
        self.closes += 1;
        let task = format!("t{}", self.closes);
        let write = || -> Result<(), makiba::db::Error> {
            let w = StateWriter::open(&self.dir.join("state.db"))?;
            w.transaction(|w| {
                w.upsert_run(&RunUpsert {
                    task_id: &task,
                    project_slug: "p",
                    status: Some("completed"),
                    ..Default::default()
                })?;
                w.append_event(&NewEvent {
                    kind: "run_closed",
                    actor: Some("person"),
                    payload: Some(&json!({"task_id": task})),
                    run_task_id: Some(&task),
                    ..Default::default()
                })
                .map(|_| ())
            })
        };
        let until = Instant::now() + WAIT;
        while let Err(e) = write() {
            assert!(Instant::now() < until, "cannot record a close: {e}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn curators(&self) -> usize {
        self.base
            .list()
            .unwrap()
            .iter()
            .filter(|t| t.tags.get("makiba.role").map(String::as_str) == Some("curator"))
            .count()
    }

    fn until(&self, what: &str, f: impl Fn(&Org) -> bool) {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for: {what}");
    }
}

const LAUNCHES: &str = "SELECT count(*) FROM run_launch WHERE role = 'curator'";
const STARTED: &str = "SELECT count(*) FROM events WHERE kind = 'curator_started'";

#[test]
fn below_the_threshold_nothing_is_launched() {
    let mut org = org(4);
    org.start();
    org.close();
    org.close();
    // Past the pass that answers a close (one a second) and the first one after the start.
    std::thread::sleep(Duration::from_millis(3000));
    assert_eq!(org.count(LAUNCHES), 0);
    assert_eq!(org.count(STARTED), 0);
    assert_eq!(org.curators(), 0);
}

#[test]
fn at_the_threshold_one_curator_is_launched_and_a_second_close_launches_none() {
    // Four notes at the start: the count of the start finds nothing, so what launches is the
    // close that follows the fifth note.
    let mut org = org(4);
    org.start();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(org.count(LAUNCHES), 0);
    org.note();
    org.close();
    org.until("a curator launched and recorded", |o| {
        o.count(STARTED) == 1 && o.curators() == 1
    });
    org.until("the launch verified", |o| {
        o.count("SELECT count(*) FROM run_launch WHERE role = 'curator' AND stage = 'verified'")
            == 1
    });
    // Its own launch: no run, role curator.
    assert_eq!(
        org.count("SELECT count(*) FROM run_launch WHERE role = 'curator' AND run_id IS NULL"),
        1
    );

    org.close();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(org.count(LAUNCHES), 1);
    assert_eq!(org.count(STARTED), 1);
    assert_eq!(org.curators(), 1);
}

#[test]
fn a_curator_past_its_time_limit_is_ended_and_a_notice_queued() {
    let mut org = org(5);
    org.start();
    org.close();
    org.until("the curator verified", |o| {
        o.count("SELECT count(*) FROM run_launch WHERE role = 'curator' AND stage = 'verified'")
            == 1
    });
    assert_eq!(org.curators(), 1);
    assert_eq!(
        org.count("SELECT count(*) FROM messages WHERE kind = 'notice' AND body LIKE '%curator%'"),
        0
    );

    org.until("the curator ended", |o| o.curators() == 0);
    // Once: one notice to the secretary about no run, queued for delivery; the key is revoked.
    org.until("the notice", |o| {
        o.count("SELECT count(*) FROM messages WHERE kind = 'notice' AND body LIKE '%curator%'")
            >= 1
    });
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        org.count(
            "SELECT count(*) FROM messages m
             JOIN event_deliveries d ON d.source_event_id = m.event_id
             WHERE m.kind = 'notice' AND m.body LIKE '%curator%' AND m.recipient = 'secretary' AND m.run_id IS NULL
               AND d.recipient = 'secretary'"
        ),
        1
    );
    assert_eq!(
        org.count("SELECT count(*) FROM messages WHERE kind = 'notice' AND body LIKE '%curator%'"),
        1
    );
    assert_eq!(
        org.count("SELECT count(*) FROM agents WHERE role = 'curator' AND revoked_at IS NULL"),
        0
    );
    // Ended, not relaunched: nothing closed since.
    assert_eq!(org.count(LAUNCHES), 1);
}
