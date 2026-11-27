//! Issue 578: the holders of the write lock on `state.db` that the timing of Issue 561 names.
//!
//! - The door's `peek` and its `answer` of a prompt call the base (list, read, keys) with no
//!   write transaction open, so a slow base never holds the lock past another writer's busy
//!   timeout. The answer reserves its entry (`answered`, `prompt_answered`, the command
//!   `accepted`) in a transaction committed before the key is pressed: the prompts pass, which
//!   withdraws only open or escalated entries, leaves it alone while the key goes in.
//! - The tick's belt pass (settle and fill) and dispatch pass take the write lock only when they
//!   write (`StateWriter::transaction_read_first`); one with nothing to write passes while another
//!   connection holds the lock, and one with work waits for the lock and writes.
//!
//! No timing: a probe inside each base call opens its own connection with a busy timeout of 0
//! and asks for the lock (`BEGIN IMMEDIATE`), which says whether it is held at that moment. The
//! passes' writer has a busy timeout of 0 too, or a busy handler that releases the holder, so
//! nothing waits for a clock. The records are written by hand; no `makibad` runs.

use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, BlockKind, Detail, EventStream, LaunchSpec, State,
    Status, Terminal, TerminalRef,
};
use makiba::daemon::belt::Belt;
use makiba::daemon::dispatch::Dispatch;
use makiba::daemon::door::{Door, issue_key, sha256_hex};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::Connection;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ORG: &str = "/org";
/// The worker's screen: the test agent's prompt starts at its question.
const SCREEN: [&str; 5] = [
    "Bash command",
    "  rm build.log",
    "Do you want to proceed?",
    "> 1. Yes",
    "  2. No",
];

/// Whether another connection can take the write lock now, without waiting.
fn lock_free(db: &Path) -> bool {
    let c = Connection::open(db).unwrap();
    c.busy_timeout(Duration::ZERO).unwrap();
    let free = c.execute_batch("BEGIN IMMEDIATE").is_ok();
    if free {
        c.execute_batch("ROLLBACK").unwrap();
    }
    free
}

type Hook = Mutex<Option<Box<dyn FnOnce(&Path) + Send>>>;

/// A base with one worker terminal of task `t` (spawn `s1`), blocked on a permission prompt and
/// showing `SCREEN`. Each list, read and keys call first probes the lock (`probes`) and runs its
/// hook once; keys records what it was given, or refuses when `keys_refused`.
struct Probed {
    db: PathBuf,
    stub: Stub,
    probes: Mutex<Vec<(&'static str, bool)>>,
    keys: Mutex<Vec<String>>,
    keys_refused: bool,
    on_read: Hook,
    on_keys: Hook,
}

impl Probed {
    fn new(db: &Path) -> Probed {
        Probed {
            db: db.to_path_buf(),
            stub: Stub::new(),
            probes: Mutex::new(Vec::new()),
            keys: Mutex::new(Vec::new()),
            keys_refused: false,
            on_read: Mutex::new(None),
            on_keys: Mutex::new(None),
        }
    }

    fn probe(&self, call: &'static str) {
        let free = lock_free(&self.db);
        self.probes.lock().unwrap().push((call, free));
    }

    fn hook(&self, h: &Hook) {
        let f = h.lock().unwrap().take();
        if let Some(f) = f {
            f(&self.db);
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.probes
            .lock()
            .unwrap()
            .iter()
            .map(|(c, _)| *c)
            .collect()
    }

    fn all_free(&self) -> bool {
        self.probes.lock().unwrap().iter().all(|(_, free)| *free)
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

impl Base for Probed {
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
        self.probe("list");
        Ok(vec![Probed::terminal()])
    }
    fn get(&self, _t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.probe("get");
        Ok(Probed::terminal())
    }
    fn read(&self, _t: &TerminalRef, _lines: usize) -> Result<Vec<String>, BaseError> {
        self.probe("read");
        self.hook(&self.on_read);
        Ok(SCREEN.iter().map(|l| l.to_string()).collect())
    }
    fn keys(&self, _t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.probe("keys");
        self.hook(&self.on_keys);
        if self.keys_refused {
            return Err(BaseError::Finished(Detail {
                kind: "finished".into(),
                message: "the terminal has exited".into(),
                hint: None,
            }));
        }
        let mut got = self.keys.lock().unwrap();
        got.extend(keys.iter().map(|k| k.to_string()));
        Ok(())
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

/// The records of task `t` at work: its verified launch `s1` of the test agent, blocked on a
/// permission prompt since 5 seconds ago, and an open entry of that prompt. Returns the folder,
/// the database, the entry's id and the secretary's key.
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
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
             VALUES (1, 's1', 'fake', 'b', '/w', 'verified', 'c1')",
            [],
        )
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO events (kind, run_id, occurred_at, payload_json)
             VALUES ('terminal_status', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds'),
                     json_object('spawn_id', 's1', 'state', 'blocked', 'kind', 'permission'))",
            [],
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

fn door(db: &Path, base: &Arc<Probed>) -> Door {
    let base: Arc<dyn Base> = base.clone();
    Door::new(StateWriter::open(db).unwrap(), base, "claude_code".into())
}

fn one<T: rusqlite::types::FromSql>(db: &Path, sql: &str) -> T {
    Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

fn strings(db: &Path, sql: &str) -> Vec<String> {
    let c = Connection::open(db).unwrap();
    let mut st = c.prepare(sql).unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn peek_reads_the_base_with_the_write_lock_free() {
    let (_dir, db, _, key) = org();
    let base = Arc::new(Probed::new(&db));
    let r = door(&db, &base).handle(Some(&key), "peek", json!({"task_id": "t"}), None, true);
    let v = r.expect("peek answers");
    assert_eq!(v["lines"], json!(SCREEN));
    assert_eq!(base.calls(), ["list", "read"]);
    assert!(
        base.all_free(),
        "the write lock was held during a base call: {:?}",
        base.probes.lock().unwrap()
    );
    assert_eq!(
        strings(&db, "SELECT status FROM commands WHERE verb = 'peek'"),
        ["done"]
    );
}

#[test]
fn a_prompt_answer_reads_and_presses_with_the_write_lock_free() {
    let (_dir, db, id, key) = org();
    let mut probed = Probed::new(&db);
    let during = Arc::new(Mutex::new(None::<String>));
    let seen = during.clone();
    probed.on_keys = Mutex::new(Some(Box::new(move |db: &Path| {
        let status: String = one(db, &format!("SELECT status FROM pending WHERE id = {id}"));
        *seen.lock().unwrap() = Some(status);
    })));
    let base = Arc::new(probed);
    let r = door(&db, &base).handle(
        Some(&key),
        "answer",
        json!({"id": id, "answer": "yes"}),
        None,
        true,
    );
    let v = r.expect("the answer is pressed");
    assert_eq!(v, json!({"id": id, "status": "answered", "key": "enter"}));
    assert_eq!(base.calls(), ["list", "read", "keys"]);
    assert!(
        base.all_free(),
        "the write lock was held during a base call: {:?}",
        base.probes.lock().unwrap()
    );
    assert_eq!(
        during.lock().unwrap().as_deref(),
        Some("answered"),
        "the entry is reserved before the key is pressed"
    );
    assert_eq!(*base.keys.lock().unwrap(), ["enter"]);
    assert_eq!(
        strings(
            &db,
            &format!(
                "SELECT status || ' ' || answer || ' ' || answered_by FROM pending WHERE id = {id}"
            )
        ),
        ["answered yes secretary"]
    );
    assert_eq!(
        one::<i64>(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'prompt_answered'"
        ),
        1
    );
    assert_eq!(
        strings(&db, "SELECT status FROM commands WHERE verb = 'answer'"),
        ["done"]
    );
}

#[test]
fn a_prompt_withdrawn_before_the_reservation_presses_nothing() {
    let (_dir, db, id, key) = org();
    let mut probed = Probed::new(&db);
    // What the prompts pass writes when the episode is over, while the door reads the screen.
    probed.on_read = Mutex::new(Some(Box::new(move |db: &Path| {
        let c = Connection::open(db).unwrap();
        c.busy_timeout(Duration::ZERO).unwrap();
        c.execute(
            "UPDATE pending SET status = 'withdrawn' WHERE id = ?1",
            [id],
        )
        .expect("the prompts pass can write while the door reads the base");
    })));
    let base = Arc::new(probed);
    let r = door(&db, &base).handle(
        Some(&key),
        "answer",
        json!({"id": id, "answer": "yes"}),
        None,
        true,
    );
    let e = r.expect_err("a withdrawn entry is not answered");
    assert_eq!(e.kind, ErrorKind::Stale, "{e:?}");
    assert!(base.keys.lock().unwrap().is_empty(), "no key is pressed");
    assert_eq!(
        one::<i64>(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'prompt_answered'"
        ),
        0
    );
}

#[test]
fn a_refused_key_press_reopens_the_entry() {
    let (_dir, db, id, key) = org();
    let mut probed = Probed::new(&db);
    probed.keys_refused = true;
    let base = Arc::new(probed);
    let r = door(&db, &base).handle(
        Some(&key),
        "answer",
        json!({"id": id, "answer": "yes"}),
        None,
        true,
    );
    let e = r.expect_err("the base refused the key");
    assert_eq!(e.kind, ErrorKind::Unavailable, "{e:?}");
    assert!(base.all_free(), "{:?}", base.probes.lock().unwrap());
    assert_eq!(
        strings(
            &db,
            &format!("SELECT status FROM pending WHERE id = {id} AND answer IS NULL")
        ),
        ["open"]
    );
    assert_eq!(
        one::<i64>(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'prompt_answered'"
        ),
        0
    );
    assert_eq!(
        strings(&db, "SELECT status FROM commands WHERE verb = 'answer'"),
        ["failed"]
    );
}

/// Another connection holding the write lock until it is dropped.
fn hold(db: &Path) -> Connection {
    let c = Connection::open(db).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    c
}

/// The tick's writer, which does not wait for the lock.
fn impatient(db: &Path) -> StateWriter {
    let w = StateWriter::open(db).unwrap();
    w.conn().busy_timeout(Duration::ZERO).unwrap();
    w
}

#[test]
fn a_tick_pass_with_nothing_to_write_takes_no_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    drop(StateWriter::open(&db).unwrap());
    let w = impatient(&db);
    let _held = hold(&db);
    let mut belt = Belt::from_env(dir.path().join("repos"));
    belt.pass(&w).expect("an idle belt pass takes no lock");
    let taken = Dispatch::new(4)
        .pass(&w)
        .expect("an idle dispatch pass takes no lock");
    assert!(taken.is_empty());
}

#[test]
fn an_idle_belt_pass_with_a_landed_dependency_takes_no_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    {
        let w = StateWriter::open(&db).unwrap();
        for (task, status) in [("w", "in_use"), ("a", "completed"), ("b", "in_use")] {
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                status: Some(status),
                ..Default::default()
            })
            .unwrap();
        }
        w.conn()
            .execute_batch(
                "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                     proposed_by, status, approved_by, approved_at)
                 VALUES ((SELECT id FROM projects WHERE slug = 'p'), '[10,11,12]', 1, 5, 'h',
                         'secretary', 'paused', 'person', strftime('%Y-%m-%dT%H:%M:%fZ','now'));
                 UPDATE runs SET range_id = 1 WHERE task_id = 'w';
                 INSERT INTO events (kind, run_id, payload_json)
                 VALUES ('belt_dispatched', (SELECT id FROM runs WHERE task_id = 'w'),
                         json_object('range_id', 1, 'task_id', 'w', 'issue', 12,
                                     'design_only', 1, 'waits_for', json('[10,11]'),
                                     'waits_tasks', json('[\"a\",\"b\"]')));
                 INSERT INTO events (kind, run_id, payload_json)
                 VALUES ('pr_opened', (SELECT id FROM runs WHERE task_id = 'a'),
                         json_object('repo', 'o/r', 'pr', 1, 'head_oid', 'h1'));
                 INSERT INTO events (kind, run_id, payload_json)
                 VALUES ('pr_merged', (SELECT id FROM runs WHERE task_id = 'a'),
                         json_object('repo', 'o/r', 'pr', 1, 'merge_commit', 'm1'));
                 INSERT INTO events (kind, run_id, payload_json)
                 VALUES ('landed_in_clone', (SELECT id FROM runs WHERE task_id = 'a'),
                         json_object('repo', 'o/r', 'pr', 1, 'merge_commit', 'm1'));",
            )
            .unwrap();
    }
    let rows = |db: &Path| -> (i64, i64) {
        (
            one(db, "SELECT count(*) FROM pending"),
            one(db, "SELECT count(*) FROM events"),
        )
    };
    let before = rows(&db);
    let w = impatient(&db);
    let held = hold(&db);
    let mut belt = Belt::from_env(dir.path().join("repos"));
    belt.pass(&w)
        .expect("a wait with one task landed and one not has nothing to write");
    drop(held);
    assert_eq!(rows(&db), before, "the pass wrote nothing");
}

/// The connection holding the lock in `a_pass_with_work_waits_for_the_lock_and_writes`, dropped
/// (its transaction rolled back) by the tick writer's busy handler: the only release.
static HOLDER: Mutex<Option<Connection>> = Mutex::new(None);

extern "C" fn release(_: *mut std::ffi::c_void, _tries: std::ffi::c_int) -> std::ffi::c_int {
    HOLDER.lock().unwrap_or_else(|e| e.into_inner()).take();
    1
}

#[test]
fn a_pass_with_work_waits_for_the_lock_and_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    {
        let w = StateWriter::open(&db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                 VALUES (1, 's1', 'fake', 'b', '/w', 'planned')",
                [],
            )
            .unwrap();
    }
    let w = StateWriter::open(&db).unwrap();
    // The writer's own connection gets the handler (the writer set a busy timeout, which it
    // replaces): SQLite calls it only while a statement waits for the lock.
    // SAFETY: the handle is the writer's open connection, used on this thread only; `release`
    // touches no SQLite state.
    let set = unsafe {
        rusqlite::ffi::sqlite3_busy_handler(w.conn().handle(), Some(release), std::ptr::null_mut())
    };
    assert_eq!(set, rusqlite::ffi::SQLITE_OK);
    *HOLDER.lock().unwrap() = Some(hold(&db));
    let taken = Dispatch::new(4)
        .pass(&w)
        .expect("the pass waits for the lock");
    assert_eq!(taken, ["t"]);
    assert!(
        HOLDER.lock().unwrap().is_none(),
        "the pass waited for the lock"
    );
    assert_eq!(
        one::<i64>(&db, "SELECT count(*) FROM events WHERE kind = 'run_taken'"),
        1
    );
}
