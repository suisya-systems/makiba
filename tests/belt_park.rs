//! Issue 283, the belt step 3: a run that waits only for the person lets its terminal go
//! (docs/design/belt.md, sections 4.1 and 8, step 3).
//!
//! - A run at work whose worker waits on an escalated question, and whose terminal has been idle
//!   for the wait (`MAKIBA_PARK_AFTER_MS`), is `parked`: no terminal of it is left, its worker's
//!   keys are revoked, and `run_parked` is written.
//! - The person's `answer` brings it back on its conversation: `run_unparked` with reason
//!   `answer`, a new launch resuming the conversation of its verified launch, verified, a live
//!   terminal, the run `in_use` again, and the answer stored for the worker.
//! - `makiba down` and `makiba up` leave a parked run alone: it stays `parked` with no launch
//!   added and no terminal.
//!
//! The daemon runs in this process on `Local` with the test agent (as tests/down_up.rs), stopped
//! by a flag; the secretary and the person act through a door of their own on the same records
//! (as tests/belt_stops.rs). The test agent reports `working` (its launch is verified), then
//! `idle`, then waits; a resume runs the steps file its conversation id names in the workplace.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, Terminal};
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const PARK_AFTER_MS: u64 = 300;
const WAIT: u64 = 30;

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        makibad::process_knowledge();
        let agent = json!([AGENT, "record", "working", "idle", "hang"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_FAKE_AGENT", agent);
            std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "3000");
            std::env::set_var("MAKIBA_PARK_AFTER_MS", PARK_AFTER_MS.to_string());
        }
    });
}

/// Poll until `f` holds, for up to `secs` seconds.
fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Ends every terminal of the base when the test ends, passed or failed.
struct EndAll(Local);

impl Drop for EndAll {
    fn drop(&mut self) {
        for t in self.0.list().unwrap_or_default() {
            let _ = self.0.end(&t.id);
        }
    }
}

/// One organization: its state folder, the run `task` in its workplace, the daemon on `local`,
/// and a door for the secretary and the person.
struct Org {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    task: String,
    local: Local,
    _end_all: EndAll,
    door: Door,
    secretary: String,
    person: String,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl Org {
    /// A queued document run `task` whose launch waits at `settings_ready`; its agent names its
    /// conversation `conv-<task>`, which can be resumed (its steps file exists and is empty).
    fn new(task: &str) -> Org {
        setup();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dir = root.join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let work = root.join(task);
        std::fs::create_dir_all(&work).unwrap();
        let conversation = format!("conv-{task}");
        std::fs::write(work.join(".fake-conversation"), &conversation).unwrap();
        std::fs::write(work.join(&conversation), "[]").unwrap();
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'spawn-' || ?1, 'document',
                         'fake', 'the brief of ' || ?1, ?2, 'settings_ready')",
                [task.to_string(), work.to_string_lossy().into_owned()],
            )
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let local = Local::new();
        let door = Door::new(w, Arc::new(local.clone()), "fake".into());
        let mut org = Org {
            _tmp: tmp,
            dir,
            task: task.into(),
            _end_all: EndAll(local.clone()),
            local,
            door,
            secretary,
            person,
            stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
        };
        org.start();
        org
    }

    fn start(&mut self) {
        self.stop = Arc::new(AtomicBool::new(false));
        let (dir, local, flag) = (self.dir.clone(), self.local.clone(), Arc::clone(&self.stop));
        self.daemon = Some(std::thread::spawn(move || {
            makibad::run_until(&dir, &|| Box::new(local.clone()), flag)
        }));
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.daemon.take() {
            t.join().unwrap().unwrap();
        }
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Value {
        self.door
            .handle(Some(key), verb, args.clone(), None, true)
            .unwrap_or_else(|e| panic!("{verb} {args} refused: {e:?}"))
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    }

    fn status(&self) -> String {
        self.db()
            .query_row(
                "SELECT status FROM runs WHERE task_id = ?1",
                [&self.task],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// (stage, conversation_id) of every launch of the run, oldest first.
    fn launches(&self) -> Vec<(String, Option<String>)> {
        let c = self.db();
        let mut q = c
            .prepare(
                "SELECT l.stage, l.conversation_id FROM run_launch l
                 JOIN runs r ON r.id = l.run_id WHERE r.task_id = ?1 ORDER BY l.id",
            )
            .unwrap();
        q.query_map([&self.task], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The payloads of the events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let c = self.db();
        let mut q = c
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap().unwrap_or_default()).unwrap_or(Value::Null))
            .collect()
    }

    /// (keys issued, keys not revoked) for the run's worker.
    fn keys(&self) -> (i64, i64) {
        self.db()
            .query_row(
                "SELECT count(*), count(*) FILTER (WHERE revoked_at IS NULL) FROM agents
                 WHERE role = 'worker' AND run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                [&self.task],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    /// The live terminals carrying the run's `makiba.task_id`.
    fn terminals(&self) -> Vec<Terminal> {
        self.local
            .list()
            .unwrap()
            .into_iter()
            .filter(|t| t.alive && t.tags.get("makiba.task_id") == Some(&self.task))
            .collect()
    }

    /// The run is at work, its launch verified, and its terminal has said `idle`.
    fn at_work_and_idle(&self) {
        let ok = wait_for(WAIT, || {
            self.status() == "in_use"
                && self
                    .db()
                    .query_row(
                        "SELECT count(*) FROM events WHERE kind = 'terminal_status'
                       AND json_extract(payload_json, '$.state') = 'idle'
                       AND run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                        [&self.task],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                    >= 1
        });
        assert!(
            ok,
            "not at work and idle: {} {:?}",
            self.status(),
            self.launches()
        );
    }

    /// The secretary records the worker's question for the person and escalates it; its id.
    fn escalate(&self) -> i64 {
        let added = self.call(
            &self.secretary,
            "pending",
            json!({"action": "add", "task": self.task,
                   "question": "May the worker rename the module?",
                   "background": "The module's name hides what it does.",
                   "if_yes": "The worker renames it in this task.",
                   "if_no": "The worker keeps the name.",
                   "recommend": "yes", "why": "The new name says what it does."}),
        );
        let id = added["id"].as_i64().unwrap();
        self.call(
            &self.secretary,
            "pending",
            json!({"action": "escalate", "id": id}),
        );
        id
    }

    /// The run is parked once the wait is over.
    fn parked(&self) {
        let ok = wait_for(WAIT, || self.status() == "parked");
        assert!(
            ok,
            "not parked after the wait: {} {:?} terminals {}",
            self.status(),
            self.launches(),
            self.terminals().len()
        );
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.daemon.take() {
            let _ = t.join();
        }
    }
}

/// A run waiting on an escalated question has no terminal after the wait, and `answer` brings it
/// back on its conversation.
#[test]
fn a_run_waiting_on_an_escalated_question_is_parked_and_an_answer_brings_it_back() {
    let mut org = Org::new("p1");
    org.at_work_and_idle();
    assert_eq!(org.terminals().len(), 1);
    let entry = org.escalate();

    org.parked();
    assert!(
        wait_for(WAIT, || org.terminals().is_empty()),
        "a parked run kept its terminal: {:?}",
        org.terminals()
    );
    let (issued, live) = org.keys();
    assert!(issued >= 1, "no worker key was issued");
    assert_eq!(live, 0, "a parked run's worker key is not revoked");
    let parked = org.events("run_parked");
    assert_eq!(parked.len(), 1, "{parked:?}");
    assert_eq!(parked[0]["task_id"], json!("p1"), "{parked:?}");
    // Kept: one launch, verified, with its conversation.
    let before = org.launches();
    assert_eq!(
        before,
        vec![("verified".to_string(), Some("conv-p1".to_string()))]
    );

    org.call(
        &org.person,
        "answer",
        json!({"id": entry, "answer": "yes", "note": "rename it"}),
    );
    let unparked = org.events("run_unparked");
    assert_eq!(unparked.len(), 1, "{unparked:?}");
    assert_eq!(unparked[0]["task_id"], json!("p1"), "{unparked:?}");
    assert_eq!(unparked[0]["reason"], json!("answer"), "{unparked:?}");
    assert_eq!(unparked[0]["detail"], json!(entry), "{unparked:?}");

    let back = wait_for(WAIT, || {
        org.status() == "in_use"
            && org.launches().len() == 2
            && org.launches()[1].0 == "verified"
            && org.terminals().len() == 1
    });
    let launches = org.launches();
    org.halt();
    assert!(
        back,
        "not back after the answer: {} {launches:?}",
        org.status()
    );
    // On its conversation: the new launch resumed the verified one's.
    assert_eq!(launches[1].1.as_deref(), Some("conv-p1"), "{launches:?}");
    let (issued_after, live_after) = org.keys();
    assert!(issued_after > issued, "the relaunch issued no key");
    assert_eq!(live_after, 1, "the woken worker has one live key");
    // The answer waits for the woken worker; it is not parked again (nothing is escalated).
    let stored: i64 = org
        .db()
        .query_row(
            "SELECT count(*) FROM messages WHERE recipient = 'w-p1' AND body LIKE ?1",
            [format!("Answer to pending entry {entry} %")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, 1);
    assert_eq!(org.events("run_parked").len(), 1);
    assert_eq!(org.status(), "in_use");
}

/// `makiba down` and `makiba up` leave a parked run alone.
#[test]
fn down_and_up_leave_a_parked_run_alone() {
    let mut org = Org::new("p2");
    org.at_work_and_idle();
    org.escalate();
    org.parked();
    assert!(wait_for(WAIT, || org.terminals().is_empty()));
    org.halt();
    let before = org.launches();

    makiba::down::run(&org.dir, &org.local, makiba::down::Options::default()).unwrap();
    assert_eq!(org.status(), "parked");
    let w = StateWriter::open(&org.dir.join("state.db")).unwrap();
    let resumed = makiba::up::resume(&w).unwrap();
    assert!(resumed.is_empty(), "up relaunched {resumed:?}");
    drop(w);

    org.start();
    std::thread::sleep(Duration::from_millis(PARK_AFTER_MS * 4));
    org.halt();
    assert_eq!(org.status(), "parked");
    assert_eq!(org.launches(), before);
    assert!(org.terminals().is_empty(), "{:?}", org.terminals());
    assert!(org.events("run_unparked").is_empty());
}
