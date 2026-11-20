//! M6-8 (Issue 49): relay a permission prompt. The daemon runs in this process on `Local` (as
//! tests/startup_dialog.rs) and launches each task's test agent, which reports `working` (so the
//! launch is verified) and then shows a prompt. The secretary answers through a `Door` in this
//! process on the same `state.db`, as the door's thread does in `makibad`. The test agent records
//! every key it receives in `record` in its workplace; "no key" is read there.
//!
//! The test writes to `state.db` only before the daemon starts; afterwards only the door writes
//! (`BEGIN IMMEDIATE`, as in production), and every wait reads what makibad wrote, read-only.

mod common;

use common::local::Local;
use makiba::base::{Base, State, TerminalRef};
use makiba::daemon;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets it, once.
        unsafe { std::env::set_var("MAKIBA_FAKE_AGENT", agent) };
    });
}

/// Ends the terminals when the test ends, passed or failed.
struct Ended(Local, Vec<TerminalRef>);

impl Drop for Ended {
    fn drop(&mut self) {
        for t in &self.1 {
            let _ = self.0.end(t);
        }
    }
}

struct Org {
    dir: PathBuf,
    work: PathBuf,
    local: Local,
    door: Door,
    secretary: String,
    _ended: Ended,
    _tmp: tempfile::TempDir,
}

/// The steps before a prompt: a status, so the launch is verified, then a pause.
const BEFORE: [&str; 2] = ["working", "sleep:1000"];

/// A daemon on `Local` with one launch of `task` at `settings_ready` running `steps`, and the
/// secretary's door.
fn org(task: &str, steps: &[&str]) -> Org {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("state.db");
    let w = StateWriter::open(&db).unwrap();
    let work = root.join(task);
    std::fs::create_dir_all(&work).unwrap();
    let all: Vec<&str> = BEFORE.iter().chain(steps).copied().collect();
    std::fs::write(work.join("steps.json"), json!(all).to_string()).unwrap();
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
                     'fake', 'the brief', ?2, 'settings_ready')",
            [task.to_string(), work.to_string_lossy().into_owned()],
        )
        .unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    drop(w);
    let local = Local::new();
    let ended = Ended(local.clone(), vec![TerminalRef(format!("w-{task}"))]);
    let door = Door::new(
        StateWriter::open(&db).unwrap(),
        Arc::new(local.clone()),
        "fake".into(),
    );
    let for_daemon = local.clone();
    let daemon_dir = dir.clone();
    std::thread::spawn(move || daemon::run(&daemon_dir, &|| Box::new(for_daemon.clone())));
    Org {
        dir,
        work: root,
        local,
        door,
        secretary,
        _ended: ended,
        _tmp: tmp,
    }
}

impl Org {
    fn db(&self) -> Option<Connection> {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
    }

    /// What the test agent of `task` received, one row per key.
    fn record(&self, task: &str) -> Vec<String> {
        std::fs::read_to_string(self.work.join(task).join("record"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn screen(&self, task: &str) -> Vec<String> {
        self.local
            .read(&TerminalRef(format!("w-{task}")), 0)
            .unwrap_or_default()
    }

    /// Wait up to 20 seconds for `sql` (one parameter, the task id) to give a row.
    fn wait_for<T: rusqlite::types::FromSql>(&self, task: &str, sql: &str) -> T {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let got = self.db().and_then(|c| {
                c.query_row(sql, [task], |r| r.get::<_, T>(0))
                    .optional()
                    .ok()
                    .flatten()
            });
            if let Some(v) = got {
                return v;
            }
            assert!(
                Instant::now() < until,
                "{task}: nothing for {sql}; screen {:?}, record {:?}",
                self.screen(task),
                self.record(task)
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The id of `task`'s open pending entry of `kind`, once makibad has recorded it with its
    /// `prompt_seen` event.
    fn entry(&self, task: &str, kind: &str) -> i64 {
        let sql = format!(
            "SELECT p.id FROM pending p JOIN runs r ON r.id = p.run_id
             WHERE r.task_id = ?1 AND p.kind = '{kind}' AND p.status = 'open'
               AND EXISTS (SELECT 1 FROM events e WHERE e.kind = 'prompt_seen'
                           AND json_extract(e.payload_json, '$.pending_id') = p.id)
             ORDER BY p.id LIMIT 1"
        );
        self.wait_for(task, &sql)
    }

    fn answer(&self, id: i64, answer: &str) -> Result<Value, makiba::spec::DoorError> {
        self.door.handle(
            Some(&self.secretary),
            "answer",
            json!({"id": id, "answer": answer}),
            None,
            true,
        )
    }

    fn one<T: rusqlite::types::FromSql>(&self, sql: &str, id: i64) -> T {
        self.db()
            .unwrap()
            .query_row(sql, [id], |r| r.get(0))
            .unwrap()
    }

    /// The `prompt_answered` events of pending entry `id`.
    fn answered_events(&self, id: i64) -> Vec<Value> {
        let c = self.db().unwrap();
        let mut q = c
            .prepare(
                "SELECT payload_json FROM events WHERE kind = 'prompt_answered'
                 AND json_extract(payload_json, '$.pending_id') = ?1",
            )
            .unwrap();
        q.query_map([id], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }
}

/// Long enough for several passes of the daemon, each of which could press a key.
fn a_while() {
    std::thread::sleep(Duration::from_millis(1500));
}

#[test]
fn a_prompt_answered_yes_receives_exactly_one_key_and_the_agent_continues() {
    let org = org("t1", &["prompt", "working", "hang"]);
    let id = org.entry("t1", "prompt");
    let text: String = org.one("SELECT prompt_text FROM pending WHERE id = ?1", id);
    assert!(text.contains("Do you want to proceed?"), "{text:?}");
    let sum: String = org.one("SELECT prompt_sum FROM pending WHERE id = ?1", id);
    assert_eq!(sum, makiba::daemon::door::sha256_hex(&text));
    assert!(org.record("t1").is_empty(), "a key before the answer");

    let r = org.answer(id, "yes");
    assert!(r.is_ok(), "{r:?}");
    let until = Instant::now() + Duration::from_secs(20);
    let t = TerminalRef("w-t1".into());
    while org
        .local
        .get(&t)
        .ok()
        .and_then(|t| t.status)
        .map(|s| s.state)
        != Some(State::Working)
    {
        assert!(
            Instant::now() < until,
            "the agent did not go on: {:?}",
            org.screen("t1")
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    a_while();
    // Exactly one key, the one-time yes, and the agent went on to its next step.
    assert_eq!(org.record("t1"), ["key enter"]);
    let status: String = org.one("SELECT status FROM pending WHERE id = ?1", id);
    assert_eq!(status, "answered");
    let events = org.answered_events(id);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["key"], "enter");
    assert_eq!(events[0]["prompt_sum"], sum.as_str());
}

#[test]
fn a_prompt_whose_text_changed_since_it_was_recorded_is_stale_and_gets_no_key() {
    let org = org("t2", &["prompt", "working", "hang"]);
    let id = org.entry("t2", "prompt");
    // The prompt changes on its own screen: the cursor moves to its second line (this key is the
    // test's, the agent records it).
    org.local
        .keys(&TerminalRef("w-t2".into()), &["down"])
        .unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    while !org
        .screen("t2")
        .iter()
        .any(|l| l.contains("\u{276f} 2. No"))
    {
        assert!(
            Instant::now() < until,
            "not redrawn: {:?}",
            org.screen("t2")
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let r = org.answer(id, "yes");
    assert_eq!(
        r.as_ref().err().map(|e| e.kind),
        Some(ErrorKind::Stale),
        "{r:?}"
    );
    a_while();
    assert_eq!(org.record("t2"), ["key down"], "makibad sent a key");
    let status: String = org.one("SELECT status FROM pending WHERE id = ?1", id);
    assert_eq!(status, "open");
    assert!(org.answered_events(id).is_empty());
}

#[test]
fn a_prompt_the_module_does_not_recognize_is_a_question_with_the_screen_and_gets_no_key() {
    let menu = json!({
        "above": ["", " Allow Bash: touch made-by-t3.txt here?", ""],
        "options": ["1. Sure", "2. Not now"],
        "at": 0,
        "indent": " ",
        "below": ["", " Enter to confirm"],
    });
    let menu = format!("menu:{menu}");
    let org = org("t3", &["blocked:permission", &menu, "hang"]);
    let id = org.entry("t3", "question");
    let question: String = org.one("SELECT question FROM pending WHERE id = ?1", id);
    assert!(
        question.contains("Allow Bash: touch made-by-t3.txt here?")
            && question.contains("2. Not now"),
        "the screen is not in the question: {question:?}"
    );
    let prompts: i64 = org
        .db()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM pending p JOIN runs r ON r.id = p.run_id
             WHERE r.task_id = 't3' AND p.kind = 'prompt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(prompts, 0, "an unrecognized prompt became a prompt entry");
    // Its answer is an answer to a question: a message to the worker, never a key.
    let r = org.answer(id, "yes");
    assert!(r.is_ok(), "{r:?}");
    let to: String = org.one(
        "SELECT recipient FROM messages WHERE id = ?1",
        r.unwrap()["message"].as_i64().unwrap(),
    );
    assert_eq!(to, "w-t3");
    a_while();
    a_while();
    assert!(
        org.record("t3").is_empty(),
        "a key was sent: {:?}",
        org.record("t3")
    );
    assert!(org.answered_events(id).is_empty());
}
