//! Issue 578: the answer of a prompt reserves its entry before it presses the key, with no write
//! transaction open (src/daemon/door.rs, `answer_prompt`). What the reservation guarantees:
//!
//! - a key whose outcome is not known keeps the entry answered, and the command answers that
//!   the outcome is not known;
//! - a request interrupted after the reservation (here a panic inside the key press, as a crash
//!   would leave it) never presses again on a retry of the same idem;
//! - a key the base refused reopens the entry even when the first try to reopen it meets the
//!   write lock held by another connection.
//!
//! No timing: the base is written in this file and records every key; the door's writer gets a
//! busy handler that releases the other connection's lock and refuses the first wait, so the
//! first try to reopen fails at once and the next one succeeds.

use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, BlockKind, Detail, EventStream, LaunchSpec, State,
    Status, Terminal, TerminalRef,
};
use makiba::daemon::door::{Door, Response, issue_key, sha256_hex};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::Connection;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const ORG: &str = "/org";
const SCREEN: [&str; 5] = [
    "Bash command",
    "  rm build.log",
    "Do you want to proceed?",
    "> 1. Yes",
    "  2. No",
];

#[derive(Clone, Copy, PartialEq)]
enum Keys {
    /// The base says nothing was pressed.
    Refused,
    /// The base gives no answer: the key may have gone in.
    Unknown,
    /// The request ends inside the press, as a crash would end it.
    Panics,
}

/// A base with one worker terminal of task `t` (spawn `s1`) blocked on a permission prompt.
struct Prompted {
    stub: Stub,
    keys: Keys,
    pressed: AtomicUsize,
    on_keys: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Prompted {
    fn new(keys: Keys) -> Prompted {
        Prompted {
            stub: Stub::new(),
            keys,
            pressed: AtomicUsize::new(0),
            on_keys: Mutex::new(None),
        }
    }

    fn terminal() -> Terminal {
        Terminal {
            id: TerminalRef("term-1".into()),
            name: "w-t".into(),
            tags: BTreeMap::from([
                ("makiba.spawn".to_string(), "s1".to_string()),
                ("makiba.org".to_string(), ORG.to_string()),
                ("makiba.task_id".to_string(), "t".to_string()),
                ("makiba.role".to_string(), "worker".to_string()),
            ]),
            status: Some(Status {
                state: State::Blocked,
                kind: Some(BlockKind::Permission),
                msg: None,
            }),
            turn: Some(1),
            alive: true,
            exit_code: None,
        }
    }
}

fn detail(kind: &str) -> Detail {
    Detail {
        kind: kind.into(),
        message: format!("the base says {kind}"),
        hint: None,
    }
}

impl Base for Prompted {
    fn name(&self) -> &'static str {
        "stub"
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.stub.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.stub.launch(spec)
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        Ok(vec![Prompted::terminal()])
    }
    fn get(&self, _t: &TerminalRef) -> Result<Terminal, BaseError> {
        Ok(Prompted::terminal())
    }
    fn read(&self, _t: &TerminalRef, _lines: usize) -> Result<Vec<String>, BaseError> {
        Ok(SCREEN.iter().map(|l| l.to_string()).collect())
    }
    fn keys(&self, _t: &TerminalRef, _keys: &[&str]) -> Result<(), BaseError> {
        self.pressed.fetch_add(1, Ordering::SeqCst);
        let hook = self.on_keys.lock().unwrap().take();
        if let Some(f) = hook {
            f();
        }
        match self.keys {
            Keys::Refused => Err(BaseError::Finished(detail("finished"))),
            Keys::Unknown => Err(BaseError::Unavailable(detail("unavailable"))),
            Keys::Panics => panic!("the request ends inside the key press"),
        }
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        self.stub.send(t, text, only_if_idle)
    }
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        self.stub.end(t)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.stub.events()
    }
    fn start(&self) -> Result<(), BaseError> {
        self.stub.start()
    }
    fn worker_denies(&self) -> BaseDenies {
        self.stub.worker_denies()
    }
}

/// Task `t` at work, its verified launch `s1` blocked on a permission prompt since 5 seconds ago,
/// and an open entry of that prompt. Returns the folder, the database, the entry and the
/// secretary's key.
fn org() -> (tempfile::TempDir, PathBuf, i64, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let w = StateWriter::open(&db).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t",
        project_slug: "p",
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    let text = SCREEN[2..].join("\n");
    w.conn()
        .execute_batch(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
             VALUES (1, 's1', 'fake', 'b', '/w', 'verified', 'c1');
             INSERT INTO events (kind, run_id, occurred_at, payload_json)
             VALUES ('terminal_status', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds'),
                     json_object('spawn_id', 's1', 'state', 'blocked', 'kind', 'permission'));",
        )
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO pending (run_id, kind, question, prompt_text, prompt_sum)
             VALUES (1, 'prompt', 'w-t asks for permission', ?1, ?2)",
            rusqlite::params![text, sha256_hex(&text)],
        )
        .unwrap();
    let id = w.conn().last_insert_rowid();
    issue_key(&w, Role::Worker, "w-t", Some(1), Some("s1")).unwrap();
    let key = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    (dir, db, id, key)
}

fn answer(door: &Door, key: &str, id: i64, idem: &str) -> Response {
    door.handle(
        Some(key),
        "answer",
        json!({"id": id, "answer": "yes"}),
        Some(idem),
        true,
    )
}

fn one<T: rusqlite::types::FromSql>(db: &Path, sql: &str) -> T {
    Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn entry(db: &Path, id: i64) -> String {
    one(db, &format!("SELECT status FROM pending WHERE id = {id}"))
}

fn answered_events(db: &Path) -> i64 {
    one(
        db,
        "SELECT count(*) FROM events WHERE kind = 'prompt_answered'",
    )
}

fn command(db: &Path) -> String {
    one(db, "SELECT status FROM commands WHERE verb = 'answer'")
}

#[test]
fn a_key_whose_outcome_is_not_known_keeps_the_reservation() {
    let (_dir, db, id, key) = org();
    let base = Arc::new(Prompted::new(Keys::Unknown));
    let as_base: Arc<dyn Base> = base.clone();
    let door = Door::new(
        StateWriter::open(&db).unwrap(),
        as_base,
        "claude_code".into(),
    );
    let e = answer(&door, &key, id, "a1").expect_err("the outcome is not known");
    assert_eq!(e.kind, ErrorKind::Internal, "{e:?}");
    assert!(e.message.contains("not known"), "{e:?}");
    assert_eq!(entry(&db, id), "answered");
    assert_eq!(answered_events(&db), 1);
    assert_eq!(command(&db), "failed");
    // The same idem again: the first answer, and no second press.
    let again = answer(&door, &key, id, "a1").expect_err("the first answer again");
    assert_eq!(again.message, e.message);
    assert_eq!(base.pressed.load(Ordering::SeqCst), 1);
}

#[test]
fn a_request_interrupted_after_the_reservation_never_presses_again() {
    let (_dir, db, id, key) = org();
    let base = Arc::new(Prompted::new(Keys::Panics));
    let as_base: Arc<dyn Base> = base.clone();
    let door = Door::new(
        StateWriter::open(&db).unwrap(),
        as_base,
        "claude_code".into(),
    );
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        answer(&door, &key, id, "a1")
    }));
    assert!(ended.is_err(), "the request ended inside the press");
    assert_eq!(entry(&db, id), "answered");
    assert_eq!(command(&db), "accepted");
    let e = answer(&door, &key, id, "a1").expect_err("the outcome is not known");
    assert_eq!(e.kind, ErrorKind::Internal, "{e:?}");
    assert!(e.message.contains("not known"), "{e:?}");
    assert_eq!(base.pressed.load(Ordering::SeqCst), 1, "no second press");
}

/// The connection holding the lock while the refused key is undone; the door's busy handler
/// drops it (rolling its transaction back) and refuses that first wait.
static HOLDER: Mutex<Option<Connection>> = Mutex::new(None);
static WAITS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn release(_: *mut std::ffi::c_void, _tries: std::ffi::c_int) -> std::ffi::c_int {
    HOLDER.lock().unwrap_or_else(|e| e.into_inner()).take();
    // The first wait fails at once; any later one is waited for.
    if WAITS.fetch_add(1, Ordering::SeqCst) == 0 {
        0
    } else {
        1
    }
}

#[test]
fn a_refused_key_reopens_the_entry_past_a_held_lock() {
    let (_dir, db, id, key) = org();
    let mut prompted = Prompted::new(Keys::Refused);
    let hold_at = db.clone();
    prompted.on_keys = Mutex::new(Some(Box::new(move || {
        let c = Connection::open(&hold_at).unwrap();
        c.execute_batch("BEGIN IMMEDIATE").unwrap();
        *HOLDER.lock().unwrap() = Some(c);
    })));
    let base: Arc<dyn Base> = Arc::new(prompted);
    let w = StateWriter::open(&db).unwrap();
    // SAFETY: the handle is the writer's open connection; `release` touches no SQLite state.
    let set = unsafe {
        rusqlite::ffi::sqlite3_busy_handler(w.conn().handle(), Some(release), std::ptr::null_mut())
    };
    assert_eq!(set, rusqlite::ffi::SQLITE_OK);
    let door = Door::new(w, base, "claude_code".into());
    let e = answer(&door, &key, id, "a1").expect_err("the base refused the key");
    assert_eq!(e.kind, ErrorKind::Unavailable, "{e:?}");
    assert!(
        WAITS.load(Ordering::SeqCst) >= 1,
        "the reopening met the lock"
    );
    assert_eq!(entry(&db, id), "open");
    assert_eq!(answered_events(&db), 0);
    assert_eq!(command(&db), "failed");
}
