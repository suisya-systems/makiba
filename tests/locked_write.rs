//! Issue 181: a terminal event observed while another connection holds the write lock on
//! `state.db` is kept and written once the lock is released; one whose lock outlasts the retry
//! window is recorded as lost (`observation_lost`, and the log).
//!
//! The daemon runs in this process on `Local` with the test agent. The test takes the write lock
//! (`BEGIN IMMEDIATE` on its own connection) BEFORE it makes the agent report `working`, so the
//! monitor's write always meets the held lock; the results are waited for, not slept for.

mod common;

use common::local::Local;
use makiba::base::{Base, LaunchSpec};
use makiba::daemon;
use makiba::db::writer::{NewEvent, StateWriter};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(60);
/// The busy timeout of every connection of makibad (src/db/mod.rs) is 5 s.
const BUSY: Duration = Duration::from_secs(5);
const SPAWN: &str = "t1-s1";

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    dir: PathBuf,
    base: Local,
    daemon: Option<Daemon>,
    _tmp: tempfile::TempDir,
    _env: MutexGuard<'static, ()>,
}

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

/// A started daemon whose first `Resync` is recorded. `retry_ms` is the monitor's retry window
/// (`MAKIBA_WRITE_RETRY_MS`), unset for the default.
fn org(retry_ms: Option<u64>) -> Org {
    let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (dir, knowledge) = (root.join("state"), root.join("knowledge"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&knowledge).unwrap();
    drop(StateWriter::open(&dir.join("state.db")).unwrap());
    // SAFETY: the lock above is held for the whole test; no daemon of an earlier test is running.
    unsafe {
        std::env::set_var("MAKIBA_KNOWLEDGE_DIR", &knowledge);
        match retry_ms {
            Some(ms) => std::env::set_var("MAKIBA_WRITE_RETRY_MS", ms.to_string()),
            None => std::env::remove_var("MAKIBA_WRITE_RETRY_MS"),
        }
    }
    let mut org = Org {
        dir,
        base: Local::new(),
        daemon: None,
        _tmp: tmp,
        _env: env,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (d, base, flag) = (org.dir.clone(), org.base.clone(), Arc::clone(&stop));
    let daemon =
        std::thread::spawn(move || daemon::run_until(&d, &|| Box::new(base.clone()), flag));
    org.daemon = Some((stop, daemon));
    org.until("the first resync", |o| {
        o.count("SELECT count(*) FROM events WHERE kind = 'base_resynced'") >= 1
    });
    org
}

impl Org {
    fn db(&self) -> Connection {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
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

    /// A worker terminal of this organization whose agent reports `working` on the first line it
    /// is sent, and then hangs.
    fn launch(&self) -> makiba::base::TerminalRef {
        let record = self.dir.join("agent.record");
        let id = self
            .base
            .launch(&LaunchSpec {
                argv: vec![
                    AGENT.into(),
                    "--record".into(),
                    record.to_string_lossy().into_owned(),
                    "wait-line".into(),
                    "working".into(),
                    "hang".into(),
                ],
                cwd: self.dir.clone(),
                name: SPAWN.into(),
                tags: BTreeMap::from([
                    ("makiba.role".to_string(), "worker".to_string()),
                    ("makiba.task_id".to_string(), "t1".to_string()),
                    ("makiba.spawn".to_string(), SPAWN.to_string()),
                    (
                        "makiba.org".to_string(),
                        self.dir.to_string_lossy().into_owned(),
                    ),
                ]),
                env: BTreeMap::new(),
                unset: vec![],
            })
            .unwrap();
        // The agent is waiting for its line once it has started.
        std::thread::sleep(Duration::from_millis(200));
        id
    }

    /// Hold the write lock for `hold`, with the agent made to report `working` once the lock is
    /// taken; then release it.
    fn working_under_lock(&self, t: &makiba::base::TerminalRef, hold: Duration) {
        let lock = makiba::db::open(&self.dir.join("state.db")).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        self.base.send(t, "go on", false).unwrap();
        self.until("the agent reported working", |o| {
            o.base
                .get(t)
                .ok()
                .and_then(|t| t.status)
                .is_some_and(|s| s.state == makiba::base::State::Working)
        });
        std::thread::sleep(hold);
        lock.execute_batch("COMMIT").unwrap();
    }

    fn alive(&self) -> bool {
        self.daemon.as_ref().is_some_and(|(_, d)| !d.is_finished())
    }
}

const WORKING: &str = "SELECT count(*) FROM events WHERE kind = 'terminal_status'
     AND json_extract(payload_json, '$.spawn_id') = 't1-s1'
     AND json_extract(payload_json, '$.state') = 'working'";
const LOST: &str = "SELECT count(*) FROM events WHERE kind = 'observation_lost'
     AND json_extract(payload_json, '$.spawn_id') = 't1-s1'
     AND json_extract(payload_json, '$.kind') = 'terminal_status'";

/// Definition of done, first point: a lock held longer than the busy timeout, but shorter than
/// the retry window, loses nothing; the event is written once, after the release.
#[test]
fn a_terminal_event_met_by_a_held_lock_is_recorded_once_the_lock_is_released() {
    let org = org(None);
    let t = org.launch();
    org.working_under_lock(&t, BUSY + Duration::from_secs(2));
    org.until("the working record", |o| o.count(WORKING) >= 1);
    // Let a few more ticks pass: the record is not written twice and nothing is lost.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(org.count(WORKING), 1);
    assert_eq!(org.count(LOST), 0);
    assert!(org.alive(), "makibad ended while the database was locked");
}

/// Definition of done, second point: a lock that outlasts the retry window (0: one attempt)
/// loses the event, and the loss is recorded once the database takes writes again.
#[test]
fn a_lock_that_outlasts_the_retry_records_the_loss() {
    let org = org(Some(0));
    let t = org.launch();
    org.working_under_lock(&t, Duration::from_secs(20));
    org.until("the loss recorded", |o| o.count(LOST) >= 1);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(org.count(LOST), 1);
    assert_eq!(org.count(WORKING), 0, "a lost event was written after all");
    assert!(org.alive(), "makibad ended while the database was locked");
}

/// The cause: a transaction of the writer that reads before it writes waits for a held lock (the
/// busy timeout) instead of failing at once.
#[test]
fn a_transaction_that_reads_then_writes_waits_for_a_held_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let w = StateWriter::open(&path).unwrap();
    let lock = makiba::db::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        lock.execute_batch("COMMIT").unwrap();
    });
    let r = w.transaction(|w| {
        let _: i64 = w
            .conn()
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
        w.append_event(&NewEvent {
            kind: "x",
            ..Default::default()
        })
    });
    release.join().unwrap();
    assert!(r.is_ok(), "the transaction failed: {:?}", r.err());
}
