//! Issue 181: a terminal event observed while another connection holds the write lock on
//! `state.db` is kept and written once the lock is released; one whose lock outlasts the retry
//! window is recorded as lost (`observation_lost`, and the log).
//!
//! The daemon runs in this process on `Local` with the test agent. The test takes the write lock
//! (`BEGIN IMMEDIATE` on its own connection) BEFORE it makes the agent report `working`, so the
//! monitor's write always meets the held lock; the results are waited for, not slept for.
//!
//! Issue 571: when the lock must outlast the retry window, the test does not hold it for a fixed
//! time, which a slow tick can outwait (each write of a pass waits the 5 s busy timeout, so the
//! monitor's attempt may start only after the release and succeed). It holds the lock until the
//! monitor says it dropped the event, through the seam `MAKIBA_WRITE_LOST_MARK` (a file the monitor
//! appends one line to per record of a dropped event), and only then releases it. A later
//! terminal's recorded status is the barrier after which the counts are read.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, LaunchSpec};
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
/// How long the lock is held at most while the monitor's drop is waited for: a watchdog for a
/// hang, not an expected time (the drop comes within a locked tick, about 20 s).
const LOCKED_WAIT: Duration = Duration::from_secs(120);
/// The monitor's test seam: the file it appends a line to for each record of a dropped event.
const MARK_ENV: &str = "MAKIBA_WRITE_LOST_MARK";

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    dir: PathBuf,
    mark: PathBuf,
    base: Local,
    daemon: Option<Daemon>,
    _tmp: tempfile::TempDir,
    _knowledge: makibad::Knowledge,
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
    let mark = root.join("lost.mark");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&knowledge).unwrap();
    drop(StateWriter::open(&dir.join("state.db")).unwrap());
    let held_knowledge = makibad::use_knowledge(&knowledge);
    // SAFETY: the lock above is held for the whole test; no daemon of an earlier test is running.
    unsafe {
        match retry_ms {
            Some(ms) => std::env::set_var("MAKIBA_WRITE_RETRY_MS", ms.to_string()),
            None => std::env::remove_var("MAKIBA_WRITE_RETRY_MS"),
        }
        std::env::set_var(MARK_ENV, &mark);
    }
    let mut org = Org {
        dir,
        mark,
        base: Local::new(),
        daemon: None,
        _tmp: tmp,
        _knowledge: held_knowledge,
        _env: env,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (d, base, flag) = (org.dir.clone(), org.base.clone(), Arc::clone(&stop));
    let daemon =
        std::thread::spawn(move || makibad::run_until(&d, &|| Box::new(base.clone()), flag));
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
        self.until_within(what, WAIT, f)
    }

    fn until_within(&self, what: &str, wait: Duration, f: impl Fn(&Org) -> bool) {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for: {what}");
    }

    /// The lines of the monitor's drop mark that name `spawn`.
    fn marked(&self, spawn: &str) -> usize {
        let text = std::fs::read_to_string(&self.mark).unwrap_or_default();
        text.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["spawn_id"] == spawn)
            .count()
    }

    /// A worker terminal of this organization, `spawn` of task `task`, whose agent runs `steps`.
    fn launch(&self, spawn: &str, task: &str, steps: &[&str]) -> makiba::base::TerminalRef {
        let record = self.dir.join(format!("{spawn}.record"));
        let mut argv = vec![
            AGENT.to_string(),
            "--record".into(),
            record.to_string_lossy().into_owned(),
        ];
        argv.extend(steps.iter().map(|s| s.to_string()));
        let id = self
            .base
            .launch(&LaunchSpec {
                argv,
                cwd: self.dir.clone(),
                name: spawn.into(),
                tags: BTreeMap::from([
                    ("makiba.role".to_string(), "worker".to_string()),
                    ("makiba.task_id".to_string(), task.to_string()),
                    ("makiba.spawn".to_string(), spawn.to_string()),
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
        self.working_under_lock_until(t, || std::thread::sleep(hold));
    }

    /// Take the write lock, make the agent report `working`, run `hold`, then release the lock.
    fn working_under_lock_until(&self, t: &makiba::base::TerminalRef, hold: impl FnOnce()) {
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
        hold();
        lock.execute_batch("COMMIT").unwrap();
    }

    fn alive(&self) -> bool {
        self.daemon.as_ref().is_some_and(|(_, d)| !d.is_finished())
    }
}

const WORKING: &str = "SELECT count(*) FROM events WHERE kind = 'terminal_status'
     AND json_extract(payload_json, '$.spawn_id') = 't1-s1'
     AND json_extract(payload_json, '$.state') = 'working'";
const LATER: &str = "SELECT count(*) FROM events WHERE kind = 'terminal_status'
     AND json_extract(payload_json, '$.spawn_id') = 't2-s1'
     AND json_extract(payload_json, '$.state') = 'working'";
const LOST: &str = "SELECT count(*) FROM events WHERE kind = 'observation_lost'
     AND json_extract(payload_json, '$.spawn_id') = 't1-s1'
     AND json_extract(payload_json, '$.kind') = 'terminal_status'";

/// Definition of done, first point: a lock held longer than the busy timeout, but shorter than
/// the retry window, loses nothing; the event is written once, after the release.
#[test]
fn a_terminal_event_met_by_a_held_lock_is_recorded_once_the_lock_is_released() {
    let org = org(None);
    let t = org.launch(SPAWN, "t1", &["wait-line", "working", "hang"]);
    org.working_under_lock(&t, BUSY + Duration::from_secs(2));
    org.until("the working record", |o| o.count(WORKING) >= 1);
    // Let a few more ticks pass: the record is not written twice and nothing is lost.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(org.count(WORKING), 1);
    assert_eq!(org.count(LOST), 0);
    assert!(org.alive(), "makibad ended while the database was locked");
}

/// Definition of done, second point: a lock that outlasts the retry window (0: one attempt)
/// loses the event, and the loss is recorded once the database takes writes again. The lock is
/// held until the monitor has dropped the event (Issue 571), not for a fixed time.
#[test]
fn a_lock_that_outlasts_the_retry_records_the_loss() {
    let org = org(Some(0));
    let t = org.launch(SPAWN, "t1", &["wait-line", "working", "hang"]);
    org.working_under_lock_until(&t, || {
        org.until_within("the monitor dropped the event", LOCKED_WAIT, |o| {
            o.marked(SPAWN) >= 1
        })
    });
    org.until("the loss recorded", |o| o.count(LOST) >= 1);
    // The barrier: the monitor writes in arrival order and writes its pending losses first, so
    // once a later terminal's status is recorded, a second loss or a late write of the lost event
    // would be recorded too.
    org.launch("t2-s1", "t2", &["working", "hang"]);
    org.until("a later terminal's status recorded", |o| {
        o.count(LATER) >= 1
    });
    assert_eq!(org.count(LOST), 1);
    assert_eq!(org.count(WORKING), 0, "a lost event was written after all");
    assert_eq!(org.marked(SPAWN), 1, "the event was dropped more than once");
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
