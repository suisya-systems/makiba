//! Issue 406: a failure on the read surface says whether its notice was delivered and whether the
//! secretary acted (docs/design/screen.md section 6, row D).
//!
//! `makiba status --json` gives an open anomaly, and a failed run, the notice that told the
//! secretary of it (`notice`: to whom, `queued` or `delivered`, when), and every run the
//! secretary's newest request about its task (`secretary_acted_at`, `secretary_acted_verb`).
//!
//! No `makibad` runs here: each scene is written by the real code through its public entry points
//! (`Anomalies::pass_at`, `Prepare::pass`, `Door::handle`) into a fresh `state.db`, and read back
//! with `status_local::read`.

use makiba::base::BaseDenies;
use makiba::base::stub::Stub;
use makiba::daemon::anomalies::Anomalies;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::prepare::{Places, Prepare};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

struct Org {
    dir: TempDir,
    w: StateWriter,
}

impl Org {
    fn new() -> Org {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        Org { dir, w }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("state.db")
    }

    fn status(&self) -> Value {
        makiba::status_local::read(&self.path()).unwrap()
    }

    /// The entry of `runs[]` of `task`.
    fn run_row(&self, task: &str) -> Value {
        let s = self.status();
        s["runs"]
            .as_array()
            .unwrap_or_else(|| panic!("no runs[]: {s}"))
            .iter()
            .find(|r| r["task_id"] == json!(task))
            .cloned()
            .unwrap_or_else(|| panic!("run {task} not listed: {s}"))
    }

    /// The open anomaly of `rule` on `task`.
    fn anomaly(&self, task: &str, rule: &str) -> Value {
        let s = self.status();
        s["anomalies"]
            .as_array()
            .unwrap_or_else(|| panic!("no anomalies[]: {s}"))
            .iter()
            .find(|a| a["task_id"] == json!(task) && a["rule"] == json!(rule))
            .cloned()
            .unwrap_or_else(|| panic!("no open {rule} on {task}: {s}"))
    }

    /// A run of `status` with a launch at `T0`.
    fn run(&self, task: &str, status: &str) {
        self.w
            .upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                status: Some(status),
                ..Default::default()
            })
            .unwrap();
        self.w
            .conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, created_at)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'fake', 'b', '/w',
                         'verified', ?3)",
                rusqlite::params![task, format!("spawn-{task}"), ts(0)],
            )
            .unwrap();
    }

    fn event(&self, kind: &str, task: Option<&str>, at: i64, payload: Value) -> i64 {
        self.w
            .append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(&payload),
                occurred_at: Some(&ts(at)),
                run_task_id: task,
                ..Default::default()
            })
            .unwrap()
    }

    fn pass(&self, at: i64) {
        Anomalies::new().pass_at(&self.w, &ts(at)).unwrap();
    }

    /// A door over the same `state.db`, and a key of `role` for it.
    fn door(&self) -> Door {
        let w = StateWriter::open(&self.path()).unwrap();
        Door::new(w, Arc::new(Stub::new()), "fake".into())
    }

    fn key(&self, role: Role, agent: &str) -> String {
        issue_key(&self.w, role, agent, None, None).unwrap()
    }

    /// `received_at` of the newest `commands` row of `actor`.
    fn last_command_at(&self, actor: &str) -> String {
        self.w
            .conn()
            .query_row(
                "SELECT received_at FROM commands WHERE actor = ?1 ORDER BY id DESC LIMIT 1",
                [actor],
                |r| r.get(0),
            )
            .unwrap()
    }
}

/// `T0 + secs`, as the database writes a time.
fn ts(secs: i64) -> String {
    let s = 10 * 3600 + secs;
    format!(
        "2026-11-18T{:02}:{:02}:{:02}.000Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// The secretary reads its inbox through the door; the messages it was given.
fn read_inbox(door: &Door, secretary: &str) -> Vec<Value> {
    let v = door
        .handle(Some(secretary), "inbox", json!({}), None, false)
        .unwrap_or_else(|e| panic!("inbox refused: {e:?}"));
    v["messages"].as_array().cloned().unwrap_or_default()
}

#[test]
fn an_anomaly_notice_is_queued_until_the_secretary_reads_its_inbox() {
    let org = Org::new();
    org.run("t1", "in_use");
    let p = json!({"task_id": "t1", "role": "worker", "state": "error", "kind": null, "msg": null});
    org.event("terminal_status", Some("t1"), 1, p);
    org.pass(2);

    let a = org.anomaly("t1", "error");
    let notice = &a["notice"];
    assert_eq!(notice["to"], json!("secretary"), "{a}");
    assert_eq!(notice["state"], json!("queued"), "{a}");
    assert_eq!(notice["written_at"], a["since"], "{a}");
    assert_eq!(notice["delivered_at"], Value::Null, "{a}");
    let run = org.run_row("t1");
    assert_eq!(
        run["notice"],
        Value::Null,
        "an in_use run carries no notice: {run}"
    );

    let secretary = org.key(Role::Secretary, "secretary");
    let door = org.door();
    assert_eq!(read_inbox(&door, &secretary).len(), 1);

    let after = org.anomaly("t1", "error");
    assert_eq!(after["notice"]["state"], json!("delivered"), "{after}");
    assert!(after["notice"]["delivered_at"].is_string(), "{after}");
    assert_eq!(
        after["notice"]["written_at"], notice["written_at"],
        "{after}"
    );
}

#[test]
fn a_suppressed_raise_has_no_notice_and_does_not_take_a_later_one() {
    let org = Org::new();
    org.run("t1", "in_use");
    org.event(
        "anomaly_raised",
        Some("t1"),
        1,
        json!({"task_id": "t1", "rule": "error", "suppressed": "person_only"}),
    );
    // A later notice to the secretary about t1, written as `notify_secretary` writes one.
    let sent = org.event("message_sent", Some("t1"), 2, json!({}));
    org.w
        .conn()
        .execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
             VALUES (?1, 'makibad', 'secretary', 'notice',
                     (SELECT id FROM runs WHERE task_id = 't1'), 'another notice')",
            [sent],
        )
        .unwrap();
    org.w
        .conn()
        .execute(
            "INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?1, 'secretary')",
            [sent],
        )
        .unwrap();

    let a = org.anomaly("t1", "error");
    assert_eq!(a["notice"], Value::Null, "{a}");
}

#[test]
fn a_failed_launch_carries_its_notice_and_its_delivery() {
    let org = Org::new();
    org.w
        .upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
    let work = org.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    org.w
        .conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'code', 'fake',
                     'the brief', ?1, 'planned')",
            [work.to_string_lossy()],
        )
        .unwrap();
    org.w
        .append_event(&NewEvent {
            kind: "run_taken",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": "t1", "spawn_id": "spawn-t1"})),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap();
    let root = org.dir.path();
    let places = Places {
        workers: root.join("workers"),
        docs: root.join("docs"),
        repos: root.join("repos"),
        settings: root.join("settings"),
        state_db: org.path(),
    };
    // A code launch whose project has no repository fails at `planned` (Issue 124).
    Prepare::new(places, BaseDenies::default())
        .pass(&org.w)
        .unwrap();

    let run = org.run_row("t1");
    assert_eq!(run["status"], json!("failed"), "{run}");
    assert_eq!(run["stage"], json!("failed"), "{run}");
    let notice = &run["notice"];
    assert_eq!(notice["to"], json!("secretary"), "{run}");
    assert_eq!(notice["state"], json!("queued"), "{run}");
    assert!(notice["written_at"].is_string(), "{run}");
    assert_eq!(notice["delivered_at"], Value::Null, "{run}");

    let secretary = org.key(Role::Secretary, "secretary");
    let door = org.door();
    assert_eq!(read_inbox(&door, &secretary).len(), 1);
    let run = org.run_row("t1");
    assert_eq!(run["notice"]["state"], json!("delivered"), "{run}");
    assert!(run["notice"]["delivered_at"].is_string(), "{run}");
}

#[test]
fn a_run_failed_by_gone_carries_the_gone_notice() {
    let org = Org::new();
    org.run("t1", "in_use");
    let p = json!({"task_id": "t1", "role": "worker", "reason": "removed", "exit_code": null});
    org.event("terminal_gone", Some("t1"), 1, p);
    org.pass(2);

    let run = org.run_row("t1");
    assert_eq!(run["status"], json!("failed"), "{run}");
    let a = org.anomaly("t1", "gone");
    assert!(a["notice"].is_object(), "{a}");
    assert_eq!(a["notice"]["state"], json!("queued"), "{a}");
    assert_eq!(run["notice"], a["notice"], "{run} {a}");
}

#[test]
fn the_secretarys_request_about_a_task_is_its_act() {
    let org = Org::new();
    org.run("t1", "in_use");
    org.run("t2", "in_use");
    let secretary = org.key(Role::Secretary, "secretary");
    let person = org.key(Role::Person, "person");
    let door = org.door();
    let ask = |key: &str, verb: &str, args: Value| {
        door.handle(Some(key), verb, args, None, false)
            .unwrap_or_else(|e| panic!("{verb} refused: {e:?}"))
    };

    let run = org.run_row("t1");
    assert_eq!(run["secretary_acted_at"], Value::Null, "{run}");
    assert_eq!(run["secretary_acted_verb"], Value::Null, "{run}");

    let added = ask(
        &secretary,
        "pending",
        json!({"action": "add", "task": "t1", "question": "Q?"}),
    );
    let run = org.run_row("t1");
    assert_eq!(run["secretary_acted_verb"], json!("pending"), "{run}");
    assert_eq!(
        run["secretary_acted_at"],
        json!(org.last_command_at("secretary")),
        "{run}"
    );

    ask(
        &secretary,
        "send",
        json!({"to": "w-t1", "text": "look again"}),
    );
    let run = org.run_row("t1");
    assert_eq!(run["secretary_acted_verb"], json!("send"), "{run}");
    assert_eq!(
        run["secretary_acted_at"],
        json!(org.last_command_at("secretary")),
        "{run}"
    );

    ask(
        &secretary,
        "pending",
        json!({"action": "withdraw", "id": added["id"]}),
    );
    let run = org.run_row("t1");
    assert_eq!(run["secretary_acted_verb"], json!("pending"), "{run}");
    let acted = json!(org.last_command_at("secretary"));
    assert_eq!(run["secretary_acted_at"], acted, "{run}");

    // None of these is the secretary acting on t1.
    ask(
        &person,
        "send",
        json!({"to": "w-t1", "text": "from the person"}),
    );
    ask(
        &secretary,
        "pending",
        json!({"action": "add", "task": "t2", "question": "Q2?"}),
    );
    ask(&secretary, "status", json!({"task_id": "t1"}));
    let recorded: i64 = org
        .w
        .conn()
        .query_row(
            "SELECT count(*) FROM commands WHERE actor = 'secretary' AND verb = 'status'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 1, "the secretary's status is a commands row");
    let run = org.run_row("t1");
    assert_eq!(run["secretary_acted_verb"], json!("pending"), "{run}");
    assert_eq!(run["secretary_acted_at"], acted, "{run}");
    let other = org.run_row("t2");
    assert_eq!(other["secretary_acted_verb"], json!("pending"), "{other}");
}
