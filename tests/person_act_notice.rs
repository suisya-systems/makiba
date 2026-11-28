//! Issue 612: an act the person does through their key reaches the secretary as a notice.
//!
//! The person deals with the secretary only (goal G1), so each act of the person's key that
//! changes what the organization does writes one notice from `makibad` to the secretary, in the
//! act's own transaction: a `messages` row of kind `notice` with a pending delivery. The
//! secretary's own act writes none, nor does a refused or stale request.
//!
//! Each test seeds, before the door opens the state: a project `p` (GitHub `o/p`), a range A
//! `active` (approved by the person), a range B `proposed` that replaces A, one complete read of B,
//! and a run `t1` of project `p` with its verified launch `s1`. The door runs on a base written in
//! this file: one worker terminal of `t1`, idle or blocked on a permission prompt, whose key
//! presses succeed, are refused, or have an outcome that is not known. The person's requests come
//! as the `makiba` command sends them (no tool); the secretary's as its tool calls.

use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, BlockKind, Detail, EventStream, LaunchSpec, State,
    Status, Terminal, TerminalRef,
};
use makiba::daemon::door::{Door, issue_key, sha256_hex};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::{DoorError, ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCREEN: [&str; 5] = [
    "Bash command",
    "  rm build.log",
    "Do you want to proceed?",
    "> 1. Yes",
    "  2. No",
];

/// What a key press of the base does.
#[derive(Clone, Copy)]
enum Keys {
    Pressed,
    /// The base says nothing was pressed.
    Refused,
    /// The request was written and no answer came back.
    Unknown,
}

/// A base with one live worker terminal of task `t1` (spawn `s1`).
struct OneWorker {
    stub: Stub,
    blocked: bool,
    keys: Keys,
}

impl OneWorker {
    fn terminal(&self) -> Terminal {
        let status = if self.blocked {
            Status {
                state: State::Blocked,
                kind: Some(BlockKind::Permission),
                msg: None,
            }
        } else {
            Status {
                state: State::Idle,
                kind: None,
                msg: None,
            }
        };
        Terminal {
            id: TerminalRef("term-1".into()),
            name: "w-t1".into(),
            tags: BTreeMap::from([
                ("makiba.spawn".to_string(), "s1".to_string()),
                ("makiba.task_id".to_string(), "t1".to_string()),
                ("makiba.role".to_string(), "worker".to_string()),
            ]),
            status: Some(status),
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

impl Base for OneWorker {
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
        Ok(vec![self.terminal()])
    }
    fn get(&self, _t: &TerminalRef) -> Result<Terminal, BaseError> {
        Ok(self.terminal())
    }
    fn read(&self, _t: &TerminalRef, _lines: usize) -> Result<Vec<String>, BaseError> {
        Ok(SCREEN.iter().map(|l| l.to_string()).collect())
    }
    fn keys(&self, _t: &TerminalRef, _keys: &[&str]) -> Result<(), BaseError> {
        match self.keys {
            Keys::Pressed => Ok(()),
            Keys::Refused => Err(BaseError::Finished(detail("finished"))),
            Keys::Unknown => Err(BaseError::Unavailable(detail("read"))),
        }
    }
    fn send(&self, _t: &TerminalRef, _text: &str, _only_if_idle: bool) -> Result<(), BaseError> {
        Ok(())
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

/// The seeded state and the door with the person's key and the secretary's.
struct Case {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    door: Door,
    person: String,
    secretary: String,
    a: i64,
    b: i64,
    read: i64,
    /// An open entry of the prompt on `t1`'s screen.
    prompt: i64,
}

impl Case {
    fn new() -> Case {
        Case::on(false, Keys::Pressed)
    }

    fn on(blocked: bool, keys: Keys) -> Case {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        let w = StateWriter::open(&db).unwrap();
        let project = w
            .ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                     proposed_by, status, approval_mode, approved_by, approved_at)
                 VALUES (?1, '[5]', 1, 1, 'Do it.', 'secretary', 'active', 'human', 'person',
                         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [project],
            )
            .unwrap();
        let a = w.conn().last_insert_rowid();
        w.conn()
            .execute(
                "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                     proposed_by, replaces)
                 VALUES (?1, '[7]', 2, 1, 'Do it again.', 'secretary', ?2)",
                [project, a],
            )
            .unwrap();
        let b = w.conn().last_insert_rowid();
        let read = w
            .append_event(&NewEvent {
                kind: "range_read",
                actor: Some("makibad"),
                payload: Some(&json!({"range_id": b, "complete": true, "through": 0})),
                ..Default::default()
            })
            .unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            title: Some("A task"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        let text = SCREEN[2..].join("\n");
        w.conn()
            .execute_batch(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 's1', 'fake', 'b', '/w',
                         'verified', 'c1');
                 INSERT INTO events (kind, run_id, occurred_at, payload_json)
                 VALUES ('terminal_status', (SELECT id FROM runs WHERE task_id = 't1'),
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds'),
                         json_object('spawn_id', 's1', 'state', 'blocked', 'kind', 'permission'));",
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO pending (run_id, kind, question, prompt_text, prompt_sum)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'prompt',
                         'w-t1 asks for permission', ?1, ?2)",
                rusqlite::params![text, sha256_hex(&text)],
            )
            .unwrap();
        let prompt = w.conn().last_insert_rowid();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let base = OneWorker {
            stub: Stub::new(),
            blocked,
            keys,
        };
        let door = Door::new(w, Arc::new(base), "claude_code".into());
        Case {
            _tmp: tmp,
            db,
            door,
            person,
            secretary,
            a,
            b,
            read,
            prompt,
        }
    }

    fn person(&self, verb: &str, args: Value) -> Result<Value, DoorError> {
        self.door
            .handle(Some(&self.person), verb, args, None, false)
    }

    fn secretary(&self, verb: &str, args: Value) -> Result<Value, DoorError> {
        self.door
            .handle(Some(&self.secretary), verb, args, None, true)
    }

    fn approve(&self, read: i64) -> Result<Value, DoorError> {
        self.person(
            "range",
            json!({"action": "approve", "id": self.b, "read": read,
                   "approval_mode": makiba::daemon::gate::merge_approval_mode()}),
        )
    }

    /// An open question of `t1`, added by the secretary.
    fn question(&self, q: &str) -> i64 {
        let added = self
            .secretary(
                "pending",
                json!({"action": "add", "task": "t1", "question": q}),
            )
            .unwrap_or_else(|e| panic!("pending add: {e:?}"));
        added["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("no id: {added}"))
    }

    /// Every message's `(sender, recipient, kind, body)`, oldest first.
    fn messages(&self) -> Vec<(String, String, String, String)> {
        let c = read_only(&self.db);
        let mut q = c
            .prepare("SELECT sender, recipient, kind, body FROM messages ORDER BY id")
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The bodies of makibad's notices to the secretary, oldest first; each must have its
    /// delivery to the secretary `pending`.
    fn notices(&self) -> Vec<String> {
        let c = read_only(&self.db);
        let mut q = c
            .prepare(
                "SELECT m.body, (SELECT count(*) FROM event_deliveries d
                                 WHERE d.source_event_id = m.event_id AND d.recipient = 'secretary'
                                   AND d.status = 'pending')
                 FROM messages m WHERE m.sender = 'makibad' AND m.recipient = 'secretary'
                   AND m.kind = 'notice' ORDER BY m.id",
            )
            .unwrap();
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .map(|r| {
                let (body, pending) = r.unwrap();
                assert_eq!(pending, 1, "the notice has no pending delivery: {body}");
                body
            })
            .collect()
    }

    fn entry(&self, id: i64) -> String {
        read_only(&self.db)
            .query_row("SELECT status FROM pending WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }
}

fn read_only(db: &Path) -> Connection {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn refused_as(r: Result<Value, DoorError>, kind: ErrorKind) {
    match r {
        Err(e) => assert_eq!(e.kind, kind, "{e:?}"),
        Ok(v) => panic!("expected {kind:?}, got {v}"),
    }
}

fn names(body: &str, parts: &[String]) {
    for part in parts {
        assert!(body.contains(part.as_str()), "{part:?} not in {body:?}");
    }
}

/// The person approves B: exactly one message, makibad's notice to the secretary, naming B and the
/// range A it replaced and ended, `at_once`, the mode, and that a read follows.
#[test]
fn the_persons_approval_is_one_notice_naming_both_ranges() {
    let c = Case::new();
    let v = c
        .approve(c.read)
        .unwrap_or_else(|e| panic!("approve: {e:?}"));
    assert_eq!(v["status"], json!("active"), "{v}");
    let all = c.messages();
    assert_eq!(all.len(), 1, "{all:?}");
    let (sender, recipient, kind, body) = &all[0];
    assert_eq!(
        (sender.as_str(), recipient.as_str(), kind.as_str()),
        ("makibad", "secretary", "notice")
    );
    assert_eq!(c.notices(), vec![body.clone()]);
    let mode = makiba::daemon::gate::merge_approval_mode();
    names(
        body,
        &[
            format!("range {}", c.b),
            format!("replaced range {}", c.a),
            "at_once 2".to_string(),
            format!("mode {mode}"),
            "reads its candidates".to_string(),
        ],
    );
}

/// An approval of a read that is not the newest is `stale` and writes no message.
#[test]
fn a_stale_approval_writes_no_message() {
    let c = Case::new();
    refused_as(c.approve(c.read + 1), ErrorKind::Stale);
    assert_eq!(c.messages(), vec![]);
}

/// The secretary pauses B and the person resumes it: one notice, for the resume only.
#[test]
fn a_secretary_pause_is_not_told_and_the_persons_resume_is() {
    let c = Case::new();
    c.approve(c.read)
        .unwrap_or_else(|e| panic!("approve: {e:?}"));
    let before = c.notices().len();
    c.secretary(
        "range",
        json!({"action": "pause", "id": c.b, "reason": "hold"}),
    )
    .unwrap_or_else(|e| panic!("pause: {e:?}"));
    assert_eq!(c.notices().len(), before, "{:?}", c.notices());
    c.person("range", json!({"action": "resume", "id": c.b}))
        .unwrap_or_else(|e| panic!("resume: {e:?}"));
    let after = c.notices();
    assert_eq!(after.len(), before + 1, "{after:?}");
    names(
        after.last().unwrap(),
        &["resumed".to_string(), format!("range {}", c.b)],
    );
    assert_eq!(c.messages().len(), before + 1, "{:?}", c.messages());
}

/// The person answers a worker's question: the worker's message and one secretary notice naming
/// the entry, its task and the answer. The secretary's own answer writes no notice.
#[test]
fn the_persons_answer_to_a_worker_is_its_message_and_one_notice() {
    let c = Case::new();
    let id = c.question("Use the cache?");
    c.person("answer", json!({"id": id, "answer": "yes"}))
        .unwrap_or_else(|e| panic!("answer: {e:?}"));
    let all = c.messages();
    assert_eq!(all.len(), 2, "{all:?}");
    let to_worker = all
        .iter()
        .filter(|(s, r, k, _)| s == "person" && r == "w-t1" && k == "message")
        .count();
    assert_eq!(to_worker, 1, "{all:?}");
    let notices = c.notices();
    assert_eq!(notices.len(), 1, "{all:?}");
    names(
        &notices[0],
        &[
            format!("entry {id}"),
            "task t1".to_string(),
            "yes".to_string(),
        ],
    );

    let other = c.question("Drop the cache?");
    c.secretary("answer", json!({"id": other, "answer": "no"}))
        .unwrap_or_else(|e| panic!("secretary answer: {e:?}"));
    assert_eq!(c.messages().len(), 3, "{:?}", c.messages());
    assert_eq!(c.notices().len(), 1, "{:?}", c.notices());
}

/// The person's typed answer: the entry answered and one notice, as for a stored one.
#[test]
fn the_persons_typed_answer_is_one_notice() {
    let c = Case::new();
    let id = c.question("Use the cache?");
    c.person("answer", json!({"id": id, "answer": "no", "typed": true}))
        .unwrap_or_else(|e| panic!("typed answer: {e:?}"));
    assert_eq!(c.entry(id), "answered");
    let notices = c.notices();
    assert_eq!(notices.len(), 1, "{:?}", c.messages());
    names(
        &notices[0],
        &[
            format!("entry {id}"),
            "task t1".to_string(),
            "no".to_string(),
        ],
    );
}

/// The person answers the permission prompt on `t1`'s screen: one notice once the key was
/// pressed; none when the base refused the press (the entry opens again) or its outcome is not
/// known.
#[test]
fn the_persons_prompt_answer_is_told_only_once_pressed() {
    let c = Case::on(true, Keys::Pressed);
    c.person("answer", json!({"id": c.prompt, "answer": "yes"}))
        .unwrap_or_else(|e| panic!("prompt answer: {e:?}"));
    assert_eq!(c.entry(c.prompt), "answered");
    let notices = c.notices();
    assert_eq!(notices.len(), 1, "{:?}", c.messages());
    names(
        &notices[0],
        &[
            "permission prompt".to_string(),
            format!("entry {}", c.prompt),
            "task t1".to_string(),
        ],
    );

    let refused = Case::on(true, Keys::Refused);
    refused_as(
        refused.person("answer", json!({"id": refused.prompt, "answer": "yes"})),
        ErrorKind::Unavailable,
    );
    assert_eq!(refused.entry(refused.prompt), "open");
    assert_eq!(refused.messages(), vec![]);

    let unknown = Case::on(true, Keys::Unknown);
    refused_as(
        unknown.person("answer", json!({"id": unknown.prompt, "answer": "yes"})),
        ErrorKind::Internal,
    );
    assert_eq!(unknown.messages(), vec![]);
}

/// The person's merge order: one notice; the same order again none; the secretary's order none.
#[test]
fn the_persons_merge_order_is_one_notice_and_the_secretarys_none() {
    let c = Case::new();
    let order = json!({"action": "order", "repo": "o/p", "pr": 2, "after_pr": 1,
                       "reason": "one after the other"});
    c.person("merge", order.clone())
        .unwrap_or_else(|e| panic!("order: {e:?}"));
    let notices = c.notices();
    assert_eq!(notices.len(), 1, "{:?}", c.messages());
    names(
        &notices[0],
        &[
            "o/p#2".to_string(),
            "#1".to_string(),
            "one after the other".to_string(),
        ],
    );
    c.person("merge", order)
        .unwrap_or_else(|e| panic!("order again: {e:?}"));
    c.secretary(
        "merge",
        json!({"action": "order", "repo": "o/p", "pr": 3, "after_pr": 1,
               "reason": "relayed"}),
    )
    .unwrap_or_else(|e| panic!("secretary order: {e:?}"));
    assert_eq!(c.messages().len(), 1, "{:?}", c.messages());
}

/// The person sets a project's `network_hosts`: one notice naming the key and the project. A set
/// that is refused writes none.
#[test]
fn the_persons_config_set_is_one_notice() {
    let c = Case::new();
    refused_as(
        c.person(
            "config",
            json!({"action": "set", "key": "network_hosts", "project": "p",
                   "value": "github.com"}),
        ),
        ErrorKind::Usage,
    );
    assert_eq!(c.messages(), vec![]);
    c.person(
        "config",
        json!({"action": "set", "key": "network_hosts", "project": "p",
               "value": "example.com"}),
    )
    .unwrap_or_else(|e| panic!("config set: {e:?}"));
    let all = c.messages();
    assert_eq!(all.len(), 1, "{all:?}");
    let notices = c.notices();
    assert_eq!(notices.len(), 1, "{all:?}");
    names(
        &notices[0],
        &["network_hosts".to_string(), "project p".to_string()],
    );
}
