//! Issue 177: the curator's terminal is ended when its cycle is reported.
//!
//! The harness is tests/curator.rs's: the daemon in this process on `Local` with the test agent,
//! a temporary knowledge folder and a time limit set per test. The curator's report is the rows
//! the door writes for its `send` to the secretary (the `message_sent` event naming the message,
//! the message, the pending delivery), as a close is the `run_closed` event. Other passes notice
//! the secretary after a close (the triage scan), so only the rows the test names are counted.

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
/// A limit no test reaches: a curator ended before it was ended for its report.
const LONG_MS: u64 = 120_000;
/// How long a reported curator may take to be ended (the pass runs once a second).
const PROMPT: Duration = Duration::from_secs(15);

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
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

/// An organization with 5 raw notes (the threshold) and the curator time limit `limit_ms`.
fn org(limit_ms: u64) -> Org {
    let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (dir, knowledge) = (root.join("state"), root.join("knowledge"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(knowledge.join("raw")).unwrap();
    for n in 0..5 {
        std::fs::write(knowledge.join(format!("raw/note-{n}.md")), "a note").unwrap();
    }
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
        std::env::set_var("MAKIBA_CURATOR_TIMEOUT_MS", limit_ms.to_string());
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "20000");
    }
    Org {
        dir,
        base: Local::new(),
        daemon: None,
        closes: 0,
        _tmp: tmp,
        _knowledge: held_knowledge,
        _env: env,
    }
}

impl Org {
    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.dir.clone(), self.base.clone(), Arc::clone(&stop));
        let daemon =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, daemon));
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

    fn until(&self, what: &str, within: Duration, f: impl Fn(&Org) -> bool) {
        let until = Instant::now() + within;
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
const STARTED: &str = "SELECT count(*) FROM events WHERE kind = 'curator_started'";
const TIME_LIMIT_NOTICES: &str = "SELECT count(*) FROM messages
     WHERE sender = 'makibad' AND recipient = 'secretary' AND kind = 'notice'
       AND body LIKE 'The curator was ended after its time limit%'";
const LIVE_KEYS: &str = "SELECT count(*) FROM agents WHERE role = 'curator' AND revoked_at IS NULL";
const REPORTS: &str = "SELECT count(*) FROM messages m
     JOIN event_deliveries d ON d.source_event_id = m.event_id
     WHERE m.sender = 'curator' AND m.recipient = 'secretary' AND m.kind = 'message'
       AND d.recipient = 'secretary'";

#[test]
fn a_curator_that_reported_is_ended_and_another_launches_at_the_next_close() {
    let mut org = org(LONG_MS);
    org.start();
    org.close();
    org.until("the first curator verified", WAIT, |o| {
        o.count(VERIFIED) == 1 && o.curators() == 1
    });
    let first_verified = Instant::now();

    // Reported: ended long before its limit, its key revoked, no notice of the time limit.
    org.report();
    org.until("the reported curator ended", PROMPT, |o| o.curators() == 0);
    org.until("its key revoked", PROMPT, |o| o.count(LIVE_KEYS) == 0);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(org.curators(), 0);
    assert_eq!(org.count(TIME_LIMIT_NOTICES), 0);
    // The report itself is kept for the secretary.
    assert_eq!(org.count(REPORTS), 1);
    // Ended, not relaunched: nothing closed since.
    assert_eq!(org.count(LAUNCHES), 1);

    // The notes are still at the threshold: the next close launches a curator again, without
    // waiting for the time limit of the first.
    org.close();
    org.until("a second curator launched", WAIT, |o| {
        o.count(LAUNCHES) == 2 && o.count(STARTED) == 2 && o.curators() == 1
    });
    assert!(first_verified.elapsed() < Duration::from_millis(LONG_MS));
    assert_eq!(org.count(TIME_LIMIT_NOTICES), 0);
}

#[test]
fn a_curator_that_never_reports_is_ended_at_its_time_limit_with_the_notice() {
    let mut org = org(5000);
    org.start();
    org.close();
    org.until("the curator verified", WAIT, |o| o.count(VERIFIED) == 1);
    assert_eq!(org.count(TIME_LIMIT_NOTICES), 0);

    org.until("the curator ended", WAIT, |o| o.curators() == 0);
    org.until("the notice", WAIT, |o| o.count(TIME_LIMIT_NOTICES) >= 1);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(org.count(TIME_LIMIT_NOTICES), 1);
    assert_eq!(org.count(LIVE_KEYS), 0);
    assert_eq!(org.count(REPORTS), 0);
    assert_eq!(org.count(LAUNCHES), 1);
}
