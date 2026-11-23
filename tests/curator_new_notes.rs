//! Issue 335: the curator is launched on the raw notes new since the last reported cycle, not on
//! every note in `raw/`.
//!
//! The harness is tests/curator_end.rs's: the daemon in this process on `Local` with the test
//! agent, a temporary knowledge folder and a time limit set per test. A close is the `run_closed`
//! event the door writes; the curator's report is the rows the door writes for its `send` to the
//! secretary. A cycle that reported moves what counts as new; one ended at its time limit or whose
//! launch failed does not.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::Base;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(40);
/// A limit no test reaches.
const LONG_MS: u64 = 120_000;
/// Past the pass that answers a close (one a second) and the first one after a start.
const SETTLE: Duration = Duration::from_millis(2500);

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    dir: PathBuf,
    knowledge: PathBuf,
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
        self.stop();
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

/// An organization with `notes` raw notes, the curator time limit `limit_ms` and the test agent
/// run with `script` (`steps:steps.json` follows `["working", "hang"]`: it verifies and runs on).
fn org(notes: usize, limit_ms: u64, script: &str, verify_ms: u64) -> Org {
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
    let agent = json!([AGENT, "record", script]).to_string();
    let held_knowledge = makibad::use_knowledge(&knowledge);
    // SAFETY: the lock above is held for the whole test; no daemon of an earlier test is running.
    unsafe {
        std::env::set_var("MAKIBA_FAKE_AGENT", agent);
        std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        std::env::set_var("MAKIBA_CURATOR_TIMEOUT_MS", limit_ms.to_string());
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", verify_ms.to_string());
    }
    let org = Org {
        dir,
        knowledge,
        base: Local::new(),
        daemon: None,
        closes: 0,
        _tmp: tmp,
        _knowledge: held_knowledge,
        _env: env,
    };
    for n in 0..notes {
        org.note(&format!("old-{n}"));
    }
    org
}

impl Org {
    /// A raw note named `<name>.md`.
    fn note(&self, name: &str) {
        std::fs::write(self.knowledge.join(format!("raw/{name}.md")), "a note").unwrap();
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.dir.clone(), self.base.clone(), Arc::clone(&stop));
        let daemon =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, daemon));
    }

    fn stop(&mut self) {
        if let Some((stop, daemon)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
        }
    }

    /// Writes with the daemon writing too: a busy database is tried again.
    fn write(&self, what: &str, f: impl Fn(&StateWriter) -> Result<(), makiba::db::Error>) {
        let once = || -> Result<(), makiba::db::Error> {
            let w = StateWriter::open(&self.dir.join("state.db"))?;
            w.transaction(|w| f(w))
        };
        let until = Instant::now() + WAIT;
        while let Err(e) = once() {
            assert!(Instant::now() < until, "cannot record {what}: {e}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A close: the event the door writes when it accepts one.
    fn close(&mut self) {
        self.closes += 1;
        let task = format!("t{}", self.closes);
        self.write("a close", |w| {
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
        });
    }

    /// The curator's report: what the door writes for `send` from `curator` to `secretary`.
    fn report(&self) {
        self.write("the curator's report", |w| {
            let event = w.append_event(&NewEvent {
                kind: "message_sent",
                actor: Some("curator"),
                ..Default::default()
            })?;
            w.conn().execute(
                "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
                 VALUES (?1, 'curator', 'secretary', 'message', NULL, 'Curation cycle done.')",
                [event],
            )?;
            w.conn().execute(
                "UPDATE events SET payload_json = json_object('message_id', ?1) WHERE id = ?2",
                params![w.conn().last_insert_rowid(), event],
            )?;
            w.conn().execute(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt)
                 VALUES (?1, 'secretary', 'pending', 0)",
                [event],
            )?;
            Ok(())
        });
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
const VERIFIED: &str =
    "SELECT count(*) FROM run_launch WHERE role = 'curator' AND stage = 'verified'";
const FAILED: &str = "SELECT count(*) FROM run_launch WHERE role = 'curator' AND stage = 'failed'";
const TIME_LIMIT_NOTICES: &str = "SELECT count(*) FROM messages
     WHERE sender = 'makibad' AND recipient = 'secretary' AND kind = 'notice'
       AND body LIKE 'The curator was ended after its time limit%'";

#[test]
fn a_close_with_no_note_new_since_the_reported_cycle_launches_no_curator_and_five_new_ones_do() {
    let mut org = org(7, LONG_MS, "steps:steps.json", 20_000);
    org.start();
    org.close();
    org.until("the first curator verified", |o| {
        o.count(LAUNCHES) == 1 && o.count(VERIFIED) == 1 && o.curators() == 1
    });
    // Two notes saved while the cycle runs: they are new for the next one.
    org.note("new-0");
    org.note("new-1");
    org.report();
    org.until("the reported curator ended", |o| o.curators() == 0);

    // Nine notes in raw/, two of them new since the reported cycle: no curator.
    org.close();
    std::thread::sleep(SETTLE);
    assert_eq!(org.count(LAUNCHES), 1);
    assert_eq!(org.curators(), 0);

    // Four new: still none.
    org.note("new-2");
    org.note("new-3");
    org.close();
    std::thread::sleep(SETTLE);
    assert_eq!(org.count(LAUNCHES), 1);

    // A restarted makibad counts once at its start: still four new, still none.
    org.stop();
    org.start();
    std::thread::sleep(SETTLE);
    assert_eq!(org.count(LAUNCHES), 1);
    assert_eq!(org.curators(), 0);

    // The fifth new note: the next close launches the curator.
    org.note("new-4");
    org.close();
    org.until("a second curator launched", |o| {
        o.count(LAUNCHES) == 2 && o.curators() == 1
    });
}

#[test]
fn a_cycle_ended_at_its_time_limit_does_not_reset_the_count() {
    let mut org = org(5, 5000, "steps:steps.json", 20_000);
    org.start();
    org.close();
    org.until("the first curator verified", |o| o.count(VERIFIED) == 1);
    org.report();
    org.until("the reported curator ended", |o| o.curators() == 0);

    // Five notes new since the reported cycle: a curator, which never reports.
    for n in 0..5 {
        org.note(&format!("new-{n}"));
    }
    org.close();
    org.until("the second curator verified", |o| {
        o.count(LAUNCHES) == 2 && o.count(VERIFIED) == 2
    });
    org.until("the second curator ended at its time limit", |o| {
        o.curators() == 0 && o.count(TIME_LIMIT_NOTICES) == 1
    });

    // Its cycle did not move the count: the five notes are still new.
    org.close();
    org.until("a third curator launched", |o| o.count(LAUNCHES) == 3);
}

#[test]
fn a_failed_cycle_does_not_reset_the_count() {
    let mut org = org(5, LONG_MS, "hang", 2000);
    org.start();
    org.close();
    org.until("the curator launch failed", |o| o.count(FAILED) == 1);
    // Even a message from its curator does not make a failed launch a reported cycle.
    org.report();
    org.until("the failed curator ended", |o| o.curators() == 0);

    org.close();
    org.until("a second curator launched", |o| o.count(LAUNCHES) == 2);
}
