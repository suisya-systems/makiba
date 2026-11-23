//! Issue 332: a typed send to a worker and the parking of its run cannot interleave so that the
//! text is lost (spec section 5.8; docs/design/belt.md section 4.1).
//!
//! - A typed send that arrives after the park pass read the base and before its transaction keeps
//!   the run unparked: the worker's terminal is not ended, and the text stays on it.
//!   The same holds when the monitor writes, after the send, an idle status it saw before it
//!   (its queue keeps the arrival time): only a later turn of the terminal releases the run.
//! - A typed send to a run that the pass parks after the door checked it and before the send is
//!   recorded is not typed: it is stored for the worker, which wakes the run (reason `message`),
//!   whether the pass could end the terminal (the door's lookup then finds none) or not.
//!
//! The interleavings are made with a base that runs the other side inside one of its calls: the
//! park pass's base read for the first, the door's reads of the terminal for the second. The
//! records are written by hand, as `src/daemon/park.rs`'s own tests write them; no `makibad` runs.

use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, Detail, EventStream, LaunchSpec, State, Terminal,
    TerminalRef,
};
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::daemon::park::Park;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ORG: &str = "/org";
const TEXT: &str = "typed-332: go on with the second part";

/// The records of a run `t` at work whose worker waits on an escalated question, with an idle
/// status of its verified launch `s1` written 5 seconds ago; the stub holds that launch's one
/// terminal, alive and idle. Returns the folder, the database path, the stub, the terminal and
/// the secretary's key.
fn org() -> (tempfile::TempDir, PathBuf, Arc<Stub>, TerminalRef, String) {
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
    w.conn()
        .execute_batch(
            "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
             VALUES (1, 's1', 'fake', 'b', '/w', 'verified', 'c1');
             INSERT INTO events (kind, run_id, occurred_at, payload_json)
             VALUES ('terminal_status', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds'),
                     json_object('spawn_id', 's1', 'state', 'idle'));
             INSERT INTO pending (run_id, kind, question, status)
             VALUES (1, 'question', 'which way?', 'escalated');",
        )
        .unwrap();
    issue_key(&w, Role::Worker, "w-t", Some(1), Some("s1")).unwrap();
    let key = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let stub = Arc::new(Stub::new());
    let t = stub
        .launch(&LaunchSpec {
            argv: vec![],
            cwd: "/".into(),
            name: "w-t".into(),
            tags: BTreeMap::from([
                ("makiba.spawn".to_string(), "s1".to_string()),
                ("makiba.org".to_string(), ORG.to_string()),
                ("makiba.task_id".to_string(), "t".to_string()),
                ("makiba.role".to_string(), "worker".to_string()),
            ]),
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
    stub.set_state(&t, State::Idle);
    (dir, db, stub, t, key)
}

/// The secretary's `send` of `TEXT` to `w-t`, typed.
fn send_typed(door: &Door, key: &str) -> Response {
    door.handle(
        Some(key),
        "send",
        json!({"to": "w-t", "text": TEXT, "typed": true}),
        None,
        true,
    )
}

fn pass(w: &StateWriter, base: &dyn Base) -> Vec<String> {
    Park::new(ORG, Duration::from_millis(1000))
        .pass(w, base)
        .unwrap()
}

fn status(db: &Path) -> String {
    StateWriter::open(db)
        .unwrap()
        .conn()
        .query_row("SELECT status FROM runs WHERE task_id = 't'", [], |r| {
            r.get(0)
        })
        .unwrap()
}

/// Whether the stub's terminal `t` is still there, and whether `TEXT` was typed on it.
fn screen(stub: &Stub, t: &TerminalRef) -> (bool, bool) {
    match stub.read(t, 1000) {
        Ok(lines) => (true, lines.iter().any(|l| l.contains(TEXT))),
        Err(_) => (false, false),
    }
}

type Hook = Mutex<Option<Box<dyn FnOnce() + Send>>>;

/// The stub, with a hook run once inside one of its calls, `end` refused when asked, and a turn
/// counter reported for every terminal when set (the stub reports none).
struct Hooked {
    stub: Arc<Stub>,
    on_list: Hook,
    /// The list hook runs before the list is read instead of after it.
    list_hook_first: bool,
    on_get: Hook,
    end_refused: bool,
    turn: Option<u64>,
}

impl Hooked {
    fn new(stub: &Arc<Stub>) -> Hooked {
        Hooked {
            stub: stub.clone(),
            on_list: Mutex::new(None),
            list_hook_first: false,
            on_get: Mutex::new(None),
            end_refused: false,
            turn: None,
        }
    }

    fn turned(&self, mut t: Terminal) -> Terminal {
        if self.turn.is_some() {
            t.turn = self.turn;
        }
        t
    }

    fn hook(h: &Hook) {
        let f = h.lock().unwrap().take();
        if let Some(f) = f {
            f();
        }
    }
}

impl Base for Hooked {
    fn name(&self) -> &'static str {
        "stub"
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.stub.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.stub.launch(spec)
    }
    /// The list is read first and the hook runs after it, so what it changes is not in the list;
    /// with `list_hook_first`, the other way round.
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        if self.list_hook_first {
            Hooked::hook(&self.on_list);
        }
        let read = self.stub.list();
        Hooked::hook(&self.on_list);
        Ok(read?.into_iter().map(|t| self.turned(t)).collect())
    }
    /// The hook runs first: the terminal is read after it.
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        Hooked::hook(&self.on_get);
        self.stub.get(t).map(|t| self.turned(t))
    }
    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.stub.read(t, lines)
    }
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.stub.keys(t, keys)
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        self.stub.send(t, text, only_if_idle)
    }
    /// Refused when asked: a base that cannot end the terminal now (the pass ends it later).
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        if self.end_refused {
            return Err(BaseError::Unavailable(Detail {
                kind: "unavailable".into(),
                message: "the base cannot end the terminal now".into(),
                hint: None,
            }));
        }
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

/// One pass on a base reporting the turn `turn`, during whose base read (after it) the secretary
/// sends `TEXT` typed through a door on the same base, then `after_send` runs. Returns what the
/// pass parked and the send's answer.
fn pass_with_send_inside(
    db: &Path,
    stub: &Arc<Stub>,
    key: &str,
    turn: Option<u64>,
    after_send: impl FnOnce() + Send + 'static,
) -> (Vec<String>, Response) {
    let mut door_base = Hooked::new(stub);
    door_base.turn = turn;
    let door = Door::new(
        StateWriter::open(db).unwrap(),
        Arc::new(door_base),
        "fake".into(),
    );
    let sent: Arc<Mutex<Option<Response>>> = Arc::default();
    let mut base = Hooked::new(stub);
    base.turn = turn;
    {
        let (sent, key) = (sent.clone(), key.to_string());
        *base.on_list.lock().unwrap() = Some(Box::new(move || {
            *sent.lock().unwrap() = Some(send_typed(&door, &key));
            after_send();
        }));
    }
    let parked = pass(&StateWriter::open(db).unwrap(), &base);
    let sent = sent
        .lock()
        .unwrap()
        .take()
        .expect("the send ran inside the pass");
    (parked, sent)
}

/// The run was not parked: still `in_use`, no `run_parked`, its worker's key live, and its
/// terminal kept with the text typed on it.
fn kept(db: &Path, stub: &Stub, t: &TerminalRef, parked: &[String], sent: Response) {
    let id = sent.expect("the typed send is accepted")["id"].as_i64();
    assert!(id.is_some(), "the typed send answers with its message id");
    assert!(parked.is_empty(), "the pass parked {parked:?}");
    assert_eq!(status(db), "in_use");
    let w = StateWriter::open(db).unwrap();
    let count = |sql: &str| -> i64 { w.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count("SELECT count(*) FROM events WHERE kind = 'run_parked'"),
        0,
        "no run_parked is written"
    );
    assert_eq!(
        count("SELECT count(*) FROM agents WHERE agent_id = 'w-t' AND revoked_at IS NULL"),
        1,
        "the worker's key is not revoked"
    );
    assert_eq!(
        screen(stub, t),
        (true, true),
        "the terminal is kept, with the text typed on it"
    );
}

/// The send was stored, not typed: its message `id` is to `w-t` with `TEXT`, not yet delivered,
/// and it woke the run (`suspended`, newest `run_unparked` of reason `message`).
fn stored(db: &Path, sent: Response) {
    let id = sent.expect("the send is accepted and stored")["id"]
        .as_i64()
        .expect("the send answers with its message id");
    let w = StateWriter::open(db).unwrap();
    let (recipient, body, delivery): (String, String, String) = w
        .conn()
        .query_row(
            "SELECT m.recipient, m.body, d.status FROM messages m
             JOIN event_deliveries d ON d.source_event_id = m.event_id AND d.recipient = m.recipient
             WHERE m.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (recipient.as_str(), body.as_str(), delivery.as_str()),
        ("w-t", TEXT, "pending"),
        "the text is stored for the worker, not yet delivered"
    );
    assert_eq!(status(db), "suspended", "the stored message woke the run");
    let reason: String = w
        .conn()
        .query_row(
            "SELECT json_extract(payload_json, '$.reason') FROM events
             WHERE kind = 'run_unparked' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "message");
}

/// The send lands between the pass's read of the base and its transaction: the run is not parked
/// and its terminal, with the text typed on it, is not ended.
#[test]
fn a_typed_send_after_the_pass_read_the_base_keeps_the_run_unparked() {
    let (_dir, db, stub, t, key) = org();
    let (parked, sent) = pass_with_send_inside(&db, &stub, &key, None, || {});
    kept(&db, &stub, &t, &parked, sent);
}

/// As above on a base that counts turns (the terminal at turn 3, recorded), and the monitor then
/// writes an idle status of that terminal it saw before the send, with its arrival time 5 seconds
/// ago: a record newer than the message, but no later turn, so the run is still not parked.
#[test]
fn an_older_observation_written_after_a_typed_send_does_not_let_the_run_park() {
    let (_dir, db, stub, t, key) = org();
    let observed = |w: &StateWriter, kind: &str, payload: &str| {
        w.conn()
            .execute(
                &format!(
                    "INSERT INTO events (kind, run_id, occurred_at, payload_json)
                     VALUES ('{kind}', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds'),
                             {payload})"
                ),
                [],
            )
            .unwrap();
    };
    let w = StateWriter::open(&db).unwrap();
    observed(
        &w,
        "terminal_turn",
        "json_object('spawn_id', 's1', 'task_id', 't', 'role', 'worker', 'turn', 3)",
    );
    let late = db.clone();
    let (parked, sent) = pass_with_send_inside(&db, &stub, &key, Some(3), move || {
        observed(
            &StateWriter::open(&late).unwrap(),
            "terminal_status",
            "json_object('spawn_id', 's1', 'task_id', 't', 'role', 'worker', 'state', 'idle')",
        );
    });
    kept(&db, &stub, &t, &parked, sent);
}

/// The pass parks the run after the door checked it and before the send is recorded, and the
/// base cannot end the terminal at once: nothing is typed on it, the text is stored for the
/// worker, and the run is woken by it.
#[test]
fn a_typed_send_to_a_run_parked_meanwhile_is_stored_for_the_woken_worker() {
    let (_dir, db, stub, t, key) = org();
    let parked: Arc<Mutex<Option<Vec<String>>>> = Arc::default();
    let door_base = Arc::new(Hooked::new(&stub));
    {
        let (db, stub, parked) = (db.clone(), stub.clone(), parked.clone());
        *door_base.on_get.lock().unwrap() = Some(Box::new(move || {
            let mut pass_base = Hooked::new(&stub);
            pass_base.end_refused = true;
            let w = StateWriter::open(&db).unwrap();
            *parked.lock().unwrap() = Some(pass(&w, &pass_base));
        }));
    }
    let door = Door::new(StateWriter::open(&db).unwrap(), door_base, "fake".into());
    let sent = send_typed(&door, &key);

    assert_eq!(
        parked.lock().unwrap().take().as_deref(),
        Some(&["t".to_string()][..]),
        "the pass inside the send parked the run"
    );
    assert_eq!(
        screen(&stub, &t),
        (true, false),
        "nothing is typed on the terminal of the parked run"
    );
    stored(&db, sent);
}

/// The pass parks the run and ends its terminal after the door checked it, before the door's
/// list of the terminals, or between that list and its read of the terminal: the door finds no
/// terminal, and the text is stored for the woken worker instead of refused.
#[test]
fn a_typed_send_whose_terminal_was_ended_by_parking_is_stored_for_the_woken_worker() {
    for at_get in [false, true] {
        let (_dir, db, stub, t, key) = org();
        let parked: Arc<Mutex<Option<Vec<String>>>> = Arc::default();
        let mut door_base = Hooked::new(&stub);
        door_base.list_hook_first = true;
        let hook: Box<dyn FnOnce() + Send> = {
            let (db, stub, parked) = (db.clone(), stub.clone(), parked.clone());
            Box::new(move || {
                let w = StateWriter::open(&db).unwrap();
                *parked.lock().unwrap() = Some(pass(&w, &Hooked::new(&stub)));
            })
        };
        if at_get {
            *door_base.on_get.lock().unwrap() = Some(hook);
        } else {
            *door_base.on_list.lock().unwrap() = Some(hook);
        }
        let door = Door::new(
            StateWriter::open(&db).unwrap(),
            Arc::new(door_base),
            "fake".into(),
        );
        let sent = send_typed(&door, &key);

        assert_eq!(
            parked.lock().unwrap().take().as_deref(),
            Some(&["t".to_string()][..]),
            "the pass inside the send parked the run (at_get {at_get})"
        );
        assert_eq!(
            screen(&stub, &t),
            (false, false),
            "the pass ended the terminal (at_get {at_get})"
        );
        stored(&db, sent);
    }
}
