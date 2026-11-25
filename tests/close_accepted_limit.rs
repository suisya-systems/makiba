//! Issue 320: a run whose pull request landed closes as completed with a known limit the person
//! accepted, recorded in the payload of its `run_closed`; a close with `--abandon` carries its
//! reason there.
//!
//! The door runs in this process on a temporary `state.db` with the stand-in base, as
//! `tests/close_gate.rs` does. The worker's last diff review is a `commands` row as the door's
//! `review` records it, a merge is a `pr_opened` and a `pr_merged` event of the run as the PR watch
//! writes them, and the person's word is a pending question answered `yes` by the person's key.

use makiba::base::stub::Stub;
use makiba::base::{Base, LaunchSpec};
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

const REPO: &str = "o/n";
const HEAD: &str = "1111111111111111111111111111111111111111";
const MERGE: &str = "2222222222222222222222222222222222222222";
const FINDING: &str = "tests/common/fixture.rs: set_var is lazy in the protected helper";
const REASON: &str = "the helper is fixed by the test commit; the person accepted it";
/// A review received long before any question: the question is asked after it.
const EARLY: &str = "2000-01-01T00:00:00.000Z";

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

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.db).unwrap()
    }

    fn conn(&self) -> Connection {
        Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    /// The worker's key cannot be read back from its hash, so the test issues it a second one.
    fn worker_key(&self, task: &str) -> String {
        let w = self.writer();
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap();
        issue_key(&w, Role::Worker, &format!("w-{task}"), Some(run), None).unwrap()
    }

    /// The worker's last diff review, as the door records it, with `blocker` blockers; its id.
    fn review(&self, task: &str, blocker: i64, received_at: &str) -> i64 {
        let w = self.writer();
        let result = json!({"result": {
            "verdict": "passed", "kind": "diff", "reader": "codex", "round": 1,
            "head": HEAD, "findings": {"blocker": blocker, "major": 0, "minor": 0},
        }});
        w.conn()
            .execute(
                "INSERT INTO commands (received_at, actor, verb, args_json, status, result_json,
                                       finished_at)
                 VALUES (?1, ?2, 'review', '{\"kind\":\"diff\"}', 'done', ?3, ?1)",
                params![received_at, format!("w-{task}"), result.to_string()],
            )
            .unwrap();
        w.conn().last_insert_rowid()
    }

    fn event(&self, kind: &str, task: &str, payload: Value) -> i64 {
        self.writer()
            .append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(&payload),
                run_task_id: Some(task),
                ..Default::default()
            })
            .unwrap()
    }

    /// `pr` registered for `task` (its current pull request from then on).
    fn opened(&self, task: &str, pr: i64) {
        self.event(
            "pr_opened",
            task,
            json!({"repo": REPO, "pr": pr, "head_oid": HEAD}),
        );
    }

    /// `pr` read merged for `task`; the event's id.
    fn merged(&self, task: &str, pr: i64) -> i64 {
        self.event(
            "pr_merged",
            task,
            json!({"repo": REPO, "pr": pr, "title": "t", "merged_at": EARLY,
                   "merge_commit": MERGE, "task_id": task, "merged_by": null,
                   "head_oid": HEAD, "merged_via": "gate"}),
        )
    }

    /// A merged run whose worker's last diff review found a blocker: the review's id and the
    /// `pr_merged` id.
    fn landed_with_blocker(&self, task: &str) -> (i64, i64) {
        let review = self.review(task, 1, EARLY);
        self.opened(task, 7);
        (review, self.merged(task, 7))
    }

    /// A pending question of `task` added by the secretary; its id.
    fn ask_person(&self, task: &str, question: &str, why: &str) -> i64 {
        let v = self
            .ask(
                &self.secretary,
                "pending",
                json!({"action": "add", "task": task, "question": question,
                       "background": "the last diff review found a blocker",
                       "if_yes": "the run closes as completed with the limit",
                       "if_no": "the run stays open",
                       "recommend": "yes", "why": why}),
            )
            .unwrap();
        v["id"].as_i64().unwrap()
    }

    fn answer(&self, key: &str, id: i64, answer: &str) {
        self.ask(key, "answer", json!({"id": id, "answer": answer}))
            .unwrap();
    }

    /// The question the secretary asks for the limit `finding` of `task`, answered `answer` by
    /// `key`; the entry's id.
    fn accepted(&self, key: &str, task: &str, finding: &str, why: &str, answer: &str) -> i64 {
        let id = self.ask_person(task, &question(task, finding), why);
        self.answer(key, id, answer);
        id
    }

    fn close(&self, key: &str, args: Value) -> Response {
        self.ask(key, "close", args)
    }

    fn status(&self, task: &str) -> String {
        self.conn()
            .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// Each `run_closed` of `task`: its id, actor and payload.
    fn closed(&self, task: &str) -> Vec<(i64, Option<String>, Value)> {
        let conn = self.conn();
        let mut q = conn
            .prepare(
                "SELECT e.id, e.actor, e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'run_closed' AND r.task_id = ?1 ORDER BY e.id",
            )
            .unwrap();
        q.query_map([task], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap(),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// The only `run_closed` of `task`, its payload.
    fn closed_once(&self, task: &str) -> Value {
        let all = self.closed(task);
        assert_eq!(all.len(), 1, "{all:?}");
        all[0].2.clone()
    }

    fn terminal_listed(&self, task: &str) -> bool {
        self.base
            .list()
            .unwrap()
            .iter()
            .any(|t| t.tags.get("makiba.task_id").map(String::as_str) == Some(task))
    }

    /// `task` is still open: status `in_use`, no `run_closed`, its terminal listed.
    fn untouched(&self, task: &str) {
        assert_eq!(self.status(task), "in_use");
        assert!(self.closed(task).is_empty(), "{:?}", self.closed(task));
        assert!(self.terminal_listed(task));
    }
}

/// The question the secretary asks the person for a limit, word for word.
fn question(task: &str, finding: &str) -> String {
    format!("Accept as a known limit of {task} and close it as completed: {finding}")
}

fn limit_close(task: &str, pending: Option<i64>) -> Value {
    let mut v = json!({"task_id": task, "limit": FINDING, "reason": REASON});
    if let Some(p) = pending {
        v["pending"] = json!(p);
    }
    v
}

fn conflict(r: Response) {
    let e = r.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
}

fn usage(r: Response) {
    let e = r.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Usage, "{e:?}");
}

#[test]
fn a_merged_run_with_an_accepted_blocker_closes_completed_and_shows_the_limit() {
    let org = Org::new(&["test-knowledge-dir", "t2"]);
    let task = "test-knowledge-dir";
    let (review, merge) = org.landed_with_blocker(task);

    // The premise: the worker's done is refused by the blocker, so no plain close is possible.
    conflict(org.ask(
        &org.worker_key(task),
        "report",
        json!({"state": "done", "task": task, "text": "finished"}),
    ));
    conflict(org.close(&org.secretary, json!({"task_id": task})));

    let pending = org.accepted(&org.person, task, FINDING, REASON, "yes");
    org.close(&org.secretary, limit_close(task, Some(pending)))
        .unwrap();
    assert_eq!(org.status(task), "completed");
    let all = org.closed(task);
    assert_eq!(all.len(), 1, "{all:?}");
    let (event_id, actor, payload) = all[0].clone();
    assert_eq!(actor.as_deref(), Some("secretary"));
    assert_eq!(payload["task_id"], task);
    assert_eq!(payload["status"], "completed");
    assert!(payload["reason"].is_null(), "{payload}");
    let limit = &payload["limit"];
    assert_eq!(limit["finding"], FINDING);
    assert_eq!(limit["reason"], REASON);
    assert_eq!(limit["accepted_by"], "person");
    assert_eq!(limit["pending_id"], pending);
    assert!(limit["answered_at"].is_string(), "{limit}");
    assert_eq!(limit["review"]["command_id"], review);
    assert_eq!(limit["review"]["head"], HEAD);
    assert_eq!(limit["review"]["findings"]["blocker"], 1);
    assert_eq!(limit["merge"]["event_id"], merge);
    assert_eq!(limit["merge"]["repo"], REPO);
    assert_eq!(limit["merge"]["pr"], 7);
    assert_eq!(limit["merge"]["merge_commit"], MERGE);

    // The read surface: `makiba events` shows the same record.
    let read = org
        .ask(&org.person, "events", json!({"after": 0, "limit": 1000}))
        .unwrap();
    let shown = read["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == event_id)
        .cloned()
        .expect("the run_closed is read");
    assert_eq!(shown["kind"], "run_closed");
    assert_eq!(shown["task_id"], task);
    assert_eq!(shown["payload"]["limit"], *limit);

    // The person's own key is the person's word: no pending entry is needed.
    org.landed_with_blocker("t2");
    org.close(&org.person, limit_close("t2", None)).unwrap();
    assert_eq!(org.status("t2"), "completed");
    let payload = org.closed_once("t2");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["limit"]["accepted_by"], "person");
    assert!(payload["limit"]["pending_id"].is_null(), "{payload}");
    assert!(payload["limit"]["answered_at"].is_null(), "{payload}");
    assert_eq!(payload["limit"]["finding"], FINDING);
}

#[test]
fn without_the_persons_acceptance_the_close_is_refused() {
    let org = Org::new(&["t", "other"]);
    org.landed_with_blocker("t");

    // No done report and no limit.
    conflict(org.close(&org.secretary, json!({"task_id": "t"})));
    // A limit with no word of the person.
    conflict(org.close(&org.secretary, limit_close("t", None)));
    // The secretary's own yes.
    let by_secretary = org.accepted(&org.secretary, "t", FINDING, REASON, "yes");
    conflict(org.close(&org.secretary, limit_close("t", Some(by_secretary))));
    // The person's no.
    let no = org.ask_person("t", &question("t", FINDING), REASON);
    org.answer(&org.person, no, "no");
    conflict(org.close(&org.secretary, limit_close("t", Some(no))));
    // A question not answered yet.
    let open = org.ask_person("t", &question("t", FINDING), REASON);
    conflict(org.close(&org.secretary, limit_close("t", Some(open))));
    // The person's yes for another task.
    let elsewhere = org.accepted(&org.person, "other", FINDING, REASON, "yes");
    conflict(org.close(&org.secretary, limit_close("t", Some(elsewhere))));
    // The person's yes to another question of the task.
    let unrelated = org.ask_person("t", "May the worker rebase its branch?", REASON);
    org.answer(&org.person, unrelated, "yes");
    conflict(org.close(&org.secretary, limit_close("t", Some(unrelated))));
    // The person's yes to another limit.
    let other_limit = org.accepted(&org.person, "t", "another finding", REASON, "yes");
    conflict(org.close(&org.secretary, limit_close("t", Some(other_limit))));
    // The person's yes with another reason.
    let other_reason = org.ask_person("t", &question("t", FINDING), "another reason");
    org.answer(&org.person, other_reason, "yes");
    conflict(org.close(&org.secretary, limit_close("t", Some(other_reason))));
    // The person's yes given before the worker's last diff review.
    let before = org.ask_person("t", &question("t", FINDING), REASON);
    org.answer(&org.person, before, "yes");
    org.review("t", 1, "2999-01-01T00:00:00.000Z");
    conflict(org.close(&org.secretary, limit_close("t", Some(before))));

    org.untouched("t");
    org.untouched("other");
}

#[test]
fn an_unmerged_run_is_refused() {
    let org = Org::new(&["t", "lander", "moved", "own"]);

    // No merge recorded.
    org.review("t", 1, EARLY);
    org.opened("t", 7);
    let p = org.accepted(&org.person, "t", FINDING, REASON, "yes");
    conflict(org.close(&org.secretary, limit_close("t", Some(p))));
    // Another run's merge of the same pull request is not this run's.
    org.opened("lander", 7);
    org.merged("lander", 7);
    conflict(org.close(&org.secretary, limit_close("t", Some(p))));
    org.untouched("t");

    // A merge of an older pull request, the run's current one being another, still open.
    org.review("moved", 1, EARLY);
    org.opened("moved", 7);
    org.merged("moved", 7);
    org.opened("moved", 8);
    let p = org.accepted(&org.person, "moved", FINDING, REASON, "yes");
    conflict(org.close(&org.secretary, limit_close("moved", Some(p))));
    org.untouched("moved");

    // The person's key does not stand for a merge either.
    org.review("own", 1, EARLY);
    org.opened("own", 9);
    conflict(org.close(&org.person, limit_close("own", None)));
    org.untouched("own");
}

#[test]
fn an_abandon_carries_its_reason() {
    let org = Org::new(&["a1", "a2", "u", "done", "retry"]);

    org.close(
        &org.secretary,
        json!({"task_id": "a1", "abandon": true, "reason": "superseded by 999"}),
    )
    .unwrap();
    assert_eq!(org.status("a1"), "abandoned");
    let payload = org.closed_once("a1");
    assert_eq!(payload["status"], "abandoned");
    assert_eq!(payload["reason"], "superseded by 999");
    assert!(payload["limit"].is_null(), "{payload}");

    org.close(&org.person, json!({"task_id": "a2", "abandon": true}))
        .unwrap();
    assert_eq!(org.status("a2"), "abandoned");
    let payload = org.closed_once("a2");
    assert_eq!(payload["status"], "abandoned");
    assert!(payload["reason"].is_null(), "{payload}");

    // What does not make sense together is refused before anything is read.
    org.landed_with_blocker("u");
    let p = org.accepted(&org.person, "u", FINDING, REASON, "yes");
    for args in [
        json!({"task_id": "u", "abandon": true, "limit": FINDING, "reason": REASON}),
        json!({"task_id": "u", "limit": FINDING, "pending": p}),
        json!({"task_id": "u", "reason": REASON}),
        json!({"task_id": "u", "limit": "two\nlines", "reason": REASON, "pending": p}),
        json!({"task_id": "u", "limit": FINDING, "reason": "two\nlines", "pending": p}),
        json!({"task_id": "u", "limit": " ", "reason": REASON, "pending": p}),
        json!({"task_id": "u", "abandon": true, "pending": p}),
    ] {
        usage(org.close(&org.secretary, args));
    }
    org.untouched("u");

    // A close after the done report records a plain completion.
    org.ask(
        &org.worker_key("done"),
        "report",
        json!({"state": "done", "task": "done", "text": "finished"}),
    )
    .unwrap();
    org.close(&org.secretary, json!({"task_id": "done"}))
        .unwrap();
    assert_eq!(org.status("done"), "completed");
    let payload = org.closed_once("done");
    assert_eq!(payload["status"], "completed");
    assert!(payload["reason"].is_null(), "{payload}");
    assert!(payload["limit"].is_null(), "{payload}");

    // A retry after a failed teardown keeps the outcome the close recorded.
    org.ask(
        &org.worker_key("retry"),
        "report",
        json!({"state": "done", "task": "retry", "text": "finished"}),
    )
    .unwrap();
    org.close(&org.secretary, json!({"task_id": "retry"}))
        .unwrap();
    org.writer()
        .conn()
        .execute(
            "UPDATE commands SET status = 'failed', error_kind = 'conflict'
             WHERE verb = 'close' AND status = 'accepted'
               AND json_extract(args_json, '$.task_id') = 'retry'",
            [],
        )
        .unwrap();
    org.close(
        &org.secretary,
        json!({"task_id": "retry", "abandon": true, "reason": "drop the unsaved work"}),
    )
    .unwrap();
    assert_eq!(org.status("retry"), "completed");
    let payload = org.closed_once("retry");
    assert_eq!(payload["status"], "completed");
}
