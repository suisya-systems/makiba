//! M5-1 (Issue 36): `close` is refused before the done report.
//!
//! The door runs in this process on a temporary `state.db` with the stand-in base. The run has a
//! worker terminal on the base and a worker key, so "nothing is ended" can be looked at: the
//! terminal is still listed, the key is not revoked, the run's status is unchanged and no
//! `run_closed` is written.

use makiba::base::stub::Stub;
use makiba::base::{Base, LaunchSpec};
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    base: Arc<Stub>,
    door: Door,
    person: String,
    secretary: String,
}

impl Org {
    /// One `in_use` run per task, each with a worker key and a tagged terminal on the base.
    fn new(tasks: &[&str]) -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        let w = StateWriter::open(&db).unwrap();
        let base = Arc::new(Stub::new());
        for task in tasks {
            let run = w
                .upsert_run(&RunUpsert {
                    task_id: task,
                    project_slug: "p",
                    status: Some("in_use"),
                    ..Default::default()
                })
                .unwrap();
            issue_key(&w, Role::Worker, &format!("w-{task}"), Some(run), None).unwrap();
            base.launch(&LaunchSpec {
                argv: vec!["sleep".into(), "60".into()],
                cwd: "/".into(),
                name: format!("w-{task}"),
                tags: BTreeMap::from([
                    ("makiba.role".to_string(), "worker".to_string()),
                    ("makiba.task_id".to_string(), task.to_string()),
                ]),
                env: BTreeMap::new(),
                unset: vec![],
            })
            .unwrap();
        }
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, base.clone(), "fake".into());
        Org {
            _tmp: tmp,
            db,
            base,
            door,
            person,
            secretary,
        }
    }

    fn ask(&self, key: &str, verb: &str, args: Value) -> Response {
        self.door.handle(Some(key), verb, args, None, false)
    }

    fn close(&self, key: &str, task: &str, abandon: bool) -> Response {
        self.ask(key, "close", json!({"task_id": task, "abandon": abandon}))
    }

    fn report_done(&self, task: &str) {
        let key = issue_key_of(self, &format!("w-{task}"));
        self.ask(
            &key,
            "report",
            json!({"state": "done", "task": task, "text": "finished"}),
        )
        .unwrap();
    }

    fn conn(&self) -> Connection {
        Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    fn status(&self, task: &str) -> String {
        self.conn()
            .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn closed_events(&self, task: &str) -> Vec<Option<String>> {
        let conn = self.conn();
        let mut q = conn
            .prepare(
                "SELECT e.actor FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'run_closed' AND r.task_id = ?1 ORDER BY e.id",
            )
            .unwrap();
        q.query_map([task], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn terminal_listed(&self, task: &str) -> bool {
        self.base
            .list()
            .unwrap()
            .iter()
            .any(|t| t.tags.get("makiba.task_id").map(String::as_str) == Some(task))
    }

    fn key_revoked(&self, task: &str) -> bool {
        self.conn()
            .query_row(
                "SELECT revoked_at IS NOT NULL FROM agents WHERE agent_id = ?1",
                [format!("w-{task}")],
                |r| r.get(0),
            )
            .unwrap()
    }
}

/// The worker's key cannot be read back from its hash, so the test issues the worker a second one.
fn issue_key_of(org: &Org, agent: &str) -> String {
    let w = StateWriter::open(&org.db).unwrap();
    let run: i64 = w
        .conn()
        .query_row(
            "SELECT id FROM runs WHERE task_id = ?1",
            [agent.trim_start_matches("w-")],
            |r| r.get(0),
        )
        .unwrap();
    issue_key(&w, Role::Worker, agent, Some(run), None).unwrap()
}

#[test]
fn close_before_any_report_is_a_conflict_and_nothing_is_ended() {
    let org = Org::new(&["t1"]);
    for key in [&org.person, &org.secretary] {
        let e = org.close(key, "t1", false).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
    }
    assert_eq!(org.status("t1"), "in_use");
    assert!(org.closed_events("t1").is_empty());
    assert!(org.terminal_listed("t1"));
    assert!(!org.key_revoked("t1"));
    let rejected: i64 = org
        .conn()
        .query_row(
            "SELECT count(*) FROM commands
             WHERE verb = 'close' AND status = 'rejected' AND error_kind = 'conflict'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rejected, 2);
}

#[test]
fn close_after_report_done_is_accepted_and_a_second_close_is_done_again() {
    let org = Org::new(&["t2"]);
    org.report_done("t2");
    org.close(&org.secretary, "t2", false).unwrap();
    assert_eq!(org.status("t2"), "completed");
    assert_eq!(org.closed_events("t2"), [Some("secretary".to_string())]);

    org.close(&org.person, "t2", false).unwrap();
    org.close(&org.person, "t2", true).unwrap();
    assert_eq!(org.status("t2"), "completed");
    assert_eq!(org.closed_events("t2").len(), 1);
}

#[test]
fn close_abandon_before_a_report_is_accepted_and_the_run_becomes_abandoned() {
    let org = Org::new(&["t3"]);
    org.close(&org.person, "t3", true).unwrap();
    assert_eq!(org.status("t3"), "abandoned");
    assert_eq!(org.closed_events("t3"), [Some("person".to_string())]);

    org.close(&org.secretary, "t3", false).unwrap();
    assert_eq!(org.status("t3"), "abandoned");
}

#[test]
fn close_of_an_unknown_task_is_not_found_and_a_worker_may_not_close() {
    let org = Org::new(&["t4"]);
    let e = org.close(&org.person, "nope", true).unwrap_err();
    assert_eq!(e.kind, ErrorKind::NotFound, "{e:?}");
    let worker = issue_key_of(&org, "w-t4");
    let e = org.close(&worker, "t4", true).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
    assert_eq!(org.status("t4"), "in_use");
}
