//! Issue 257, the belt step 1: a range as a record, and `makibad` dispatches the next task inside
//! it (docs/design/belt.md, section 8, step 1; Issue 248's three conditions).
//!
//! - With a range approved for two tasks and one slot, the second task is dispatched when the
//!   first closes, with no call by the person; a task outside the range is never dispatched.
//! - A stop for the person leaves a pending entry and the belt goes on with the next task, in the
//!   transaction of the escalation.
//! - An agent key cannot approve or resume a range.
//! - The read surface lists the whole range, each item with its place, in a stable order.
//!
//! The daemon runs in this process (`daemon::run_until`) on the stand-in base, with the test agent
//! named and not given (`MAKIBA_LAUNCH_AGENT=fake` without `MAKIBA_FAKE_AGENT`): a launch stops at
//! `settings_ready` and holds its slot. `max_concurrent_workers` is 1. The door is called in this
//! process with the person's, the secretary's and the worker's keys. `gh` is a stand-in script
//! (`MAKIBA_GH`) that answers from files in a folder per repository and refuses anything else, so
//! no test reaches the network. Each test has a project of its own, with a real clone (a code
//! launch prepares its workplace from it) and a GitHub name for the reads.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SPEC: &str = include_str!("../docs/design/spec.md");
const WAIT: Duration = Duration::from_secs(60);
const HEAD: &str = "Brief head of the range: design first, then code; stop and ask before anything outside the issue.";

const STAND_IN: &str = r#"#!/bin/sh
# gh issue|pr list --repo github.com/<o>/<n> --state <s> ...: the file <what>-<state>.json, else [].
# gh issue|pr view <n> --repo github.com/<o>/<n> ...: the file <what>-<n>.json, else a failure.
what="$1 $2"; shift 2
num=
case "$what" in
  "issue list"|"pr list") ;;
  "issue view"|"pr view") num=$1; shift ;;
  *) echo "unexpected call: $what" >&2; exit 2 ;;
esac
repo=; state=
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --state) state=$2; shift 2 ;;
    --limit|--json|--search|--base) shift 2 ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done
case "$repo" in github.com/*/*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
r=${repo#github.com/}
dir="$FAKE_GH_DIR/${r%%/*}_${r#*/}"
mkdir -p "$dir"
echo "$what $num $state" >> "$dir/calls"
if [ -n "$num" ]; then
  f="$dir/${what%% *}-$num.json"
  if [ -e "$f" ]; then cat "$f"; else echo "not found" >&2; exit 1; fi
else
  f="$dir/${what%% *}-$state.json"
  if [ -e "$f" ]; then cat "$f"; else echo '[]'; fi
fi
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The folder the tests share: the `gh` stand-in and its answers, the repositories, the
/// workplaces. Each test uses a project of its own in it.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        for d in [
            "answers", "goals", "repos", "origins", "workers", "docs", "share", "config",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_GOALS_DIR", root.join("goals"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
            std::env::remove_var("MAKIBA_FAKE_AGENT");
            std::env::set_var("MAKIBA_MAX_CONCURRENT_WORKERS", "1");
            std::env::set_var("MAKIBA_MERGE_APPROVAL_MODE", "human");
            std::env::set_var("MAKIBA_PRWATCH_BUDGET_MS", "1000");
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "200");
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn issue(number: i64, title: &str, body: &str, comment: &str) -> Value {
    json!({"number": number, "title": title, "body": body, "labels": [], "milestone": null,
           "createdAt": "2026-11-01T00:00:00Z", "updatedAt": "2026-11-01T00:00:00Z",
           "comments": [{"author": {"login": "someone"}, "body": comment,
                         "createdAt": "2026-11-02T00:00:00Z"}]})
}

/// One organization with one project `slug` (GitHub `o/<slug>`), its door and its daemon.
struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    answers: PathBuf,
    door: Door,
    person: String,
    secretary: String,
    slug: String,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl Org {
    /// `issues`: the open Issues the stand-in answers. The daemon is started when `run` is true.
    fn new(slug: &str, issues: &[Value], run: bool) -> Org {
        let home = init();
        // A clone with a GitHub name (`projects.origin_url`) and a local origin for git.
        let repo = home.join("repos").join(slug);
        let origin = home.join("origins").join(format!("{slug}.git"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let answers = home.join("answers").join(format!("o_{slug}"));
        std::fs::create_dir_all(&answers).unwrap();
        std::fs::write(
            answers.join("issue-open.json"),
            Value::Array(issues.to_vec()).to_string(),
        )
        .unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.ensure_project(slug, None, Some(&format!("https://github.com/o/{slug}")))
            .unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        let stop = Arc::new(AtomicBool::new(false));
        let daemon = run.then(|| {
            let (dir, flag) = (dir.clone(), Arc::clone(&stop));
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(Stub::new()), flag))
        });
        Org {
            _tmp: tmp,
            db,
            answers,
            door,
            person,
            secretary,
            slug: slug.into(),
            stop,
            daemon,
        }
    }

    fn handle(&self, key: &str, verb: &str, args: Value) -> Response {
        self.door.handle(Some(key), verb, args, None, true)
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Value {
        self.handle(key, verb, args.clone())
            .unwrap_or_else(|e| panic!("{verb} {args} refused: {e:?}"))
    }

    fn conn(&self) -> Connection {
        Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    /// A worker key for the run of `task`, as its launch would issue it.
    fn worker(&self, task: &str) -> String {
        let w = StateWriter::open(&self.db).unwrap();
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap();
        issue_key(&w, Role::Worker, &format!("w-{task}"), Some(run), None).unwrap()
    }

    fn run_exists(&self, task: &str) -> bool {
        self.conn()
            .query_row("SELECT 1 FROM runs WHERE task_id = ?1", [task], |_| Ok(()))
            .optional()
            .unwrap()
            .is_some()
    }

    /// The task ids of every run, by id.
    fn tasks(&self) -> Vec<String> {
        let c = self.conn();
        let mut q = c.prepare("SELECT task_id FROM runs ORDER BY id").unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The payloads of the events of `kind`, oldest first, with their actor.
    fn events(&self, kind: &str) -> Vec<(Option<String>, Value)> {
        let c = self.conn();
        let mut q = c
            .prepare("SELECT actor, payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| {
            let p: String = r.get(1)?;
            Ok((r.get(0)?, serde_json::from_str(&p).unwrap()))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn stage(&self, task: &str) -> Option<String> {
        self.conn()
            .query_row(
                "SELECT l.stage FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE r.task_id = ?1 ORDER BY l.id DESC LIMIT 1",
                [task],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
    }

    /// The person's `status --json`, through the door.
    fn status(&self) -> Value {
        self.call(&self.person, "status", json!({}))
    }

    /// The range `id` on the read surface.
    fn range(&self, id: i64) -> Value {
        let s = self.status();
        s["ranges"]
            .as_array()
            .unwrap_or_else(|| panic!("status has no ranges[]: {s}"))
            .iter()
            .find(|r| r["id"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("range {id} not listed: {s}"))
    }

    /// The read of range `id` has been recorded, complete.
    fn read_done(&self, id: i64) -> bool {
        self.events("range_read")
            .iter()
            .any(|(_, p)| p["range_id"] == json!(id) && p["complete"] == json!(true))
    }

    /// Propose a range of `issues` with `at_once` 1 and `budget_tasks` `budget` as the secretary;
    /// returns its id.
    fn propose(&self, issues: &[i64], budget: i64) -> i64 {
        let r = self.call(
            &self.secretary,
            "range",
            json!({"action": "propose", "project": self.slug, "issues": issues, "at_once": 1,
                   "budget_tasks": budget, "brief_head": HEAD}),
        );
        r["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("no range id: {r}"))
    }

    /// Approve range `id` with the person key, on the read the read surface shows.
    fn approve(&self, id: i64) {
        wait("the read of the draft", || self.read_done(id));
        let read = self.range(id)["read_id"].clone();
        self.call(
            &self.person,
            "range",
            json!({"action": "approve", "id": id, "read": read}),
        );
    }

    /// The worker of `task` reports done, then the secretary closes it.
    fn finish(&self, task: &str) {
        wait(&format!("{task} prepared"), || {
            self.stage(task).as_deref() == Some("settings_ready")
        });
        let worker = self.worker(task);
        self.call(
            &worker,
            "report",
            json!({"state": "done", "task": task, "text": "Done; ran the tests."}),
        );
        self.call(&self.secretary, "close", json!({"task_id": task}));
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(d) = self.daemon.take() {
            let _ = d.join();
        }
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The items of a range as (issue, place).
fn places(range: &Value) -> Vec<(i64, String)> {
    range["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items[]: {range}"))
        .iter()
        .map(|i| {
            (
                i["issue"].as_i64().unwrap(),
                i["place"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn item(range: &Value, issue: i64) -> Value {
    range["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["issue"] == json!(issue))
        .cloned()
        .unwrap_or_else(|| panic!("no item {issue}: {range}"))
}

// ---- the definition of done ----------------------------------------------------------------

/// A range of two tasks and one slot: the first goes in on approval, the second when the first
/// closes, by `makibad` and with no call by the person; an open Issue outside the range never
/// goes in. The read surface lists the whole range with each item's place, in a stable order,
/// and a landed item with its pull request.
#[test]
fn two_tasks_one_slot_the_second_goes_in_when_the_first_closes_and_the_list_shows_each_place() {
    let org = Org::new(
        "alpha",
        &[
            issue(11, "Eleven", "Body of eleven.", "A comment on eleven."),
            issue(12, "Twelve", "Body of twelve.", "A comment on twelve."),
            issue(13, "Thirteen", "Outside the range.", "-"),
            issue(14, "Fourteen", "Waits.\n\nDepends on #99\n", "-"),
            issue(15, "Fifteen", "Skipped.", "-"),
        ],
        true,
    );
    std::fs::write(
        org.answers.join("issue-99.json"),
        json!({"number": 99, "title": "Open dependency", "state": "OPEN"}).to_string(),
    )
    .unwrap();
    let id = org.propose(&[11, 12, 14, 15], 2);
    org.call(
        &org.secretary,
        "range",
        json!({"action": "skip", "id": id, "issue": 15, "reason": "the person said later"}),
    );
    wait("the read of the draft", || org.read_done(id));
    // The draft: what it would admit now, and why the rest is left out.
    let draft = org.range(id);
    assert_eq!(draft["status"], json!("proposed"), "{draft}");
    let cand = |n: i64| {
        draft["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["issue"] == json!(n))
            .cloned()
    };
    assert_eq!(cand(11).unwrap()["admitted"], json!(true), "{draft}");
    let fourteen = cand(14).unwrap();
    assert_eq!(fourteen["admitted"], json!(false), "{draft}");
    assert!(
        fourteen["left_out_why"]
            .as_str()
            .is_some_and(|w| w.contains("#99")),
        "{draft}"
    );
    assert!(
        cand(13).is_none(),
        "an Issue outside the range is no candidate: {draft}"
    );
    assert!(org.tasks().is_empty(), "a draft dispatches nothing");

    org.approve(id);
    let first = format!("r{id}-11");
    let second = format!("r{id}-12");
    wait("the first task", || org.run_exists(&first));
    wait("the first task prepared", || {
        org.stage(&first).as_deref() == Some("settings_ready")
    });
    assert!(
        !org.run_exists(&second),
        "one slot: the second waits while the first runs"
    );
    let dispatched = org.events("belt_dispatched");
    assert_eq!(dispatched.len(), 1, "{dispatched:?}");
    assert_eq!(dispatched[0].0.as_deref(), Some("makibad"));
    assert_eq!(dispatched[0].1["task_id"], json!(first));
    assert_eq!(dispatched[0].1["range_id"], json!(id));

    // The first lands: its pull request is recorded and merged, the worker reports, it is closed.
    org.call(
        &org.secretary,
        "pr",
        json!({"state": "opened", "repo": format!("o/{}", org.slug), "pr": 101,
               "head": "a".repeat(40), "task": first}),
    );
    std::fs::write(
        org.answers.join("pr-101.json"),
        json!({"number": 101, "state": "MERGED", "mergedAt": "2026-11-22T10:00:00Z",
               "title": "Land eleven"})
        .to_string(),
    )
    .unwrap();
    org.finish(&first);
    assert!(
        org.run_exists(&second),
        "the second is dispatched by the time the first is closed"
    );
    let brief: String = org
        .conn()
        .query_row(
            "SELECT l.brief FROM run_launch l JOIN runs r ON r.id = l.run_id
             WHERE r.task_id = ?1 ORDER BY l.id LIMIT 1",
            [&second],
            |r| r.get(0),
        )
        .unwrap();
    assert!(brief.starts_with(HEAD), "{brief}");
    for part in ["Twelve", "Body of twelve.", "A comment on twelve."] {
        assert!(brief.contains(part), "{part:?} not in the brief: {brief}");
    }
    // No call by the person dispatched anything: no dispatch command, every dispatch by makibad.
    let commands: i64 = org
        .conn()
        .query_row(
            "SELECT count(*) FROM commands WHERE verb = 'dispatch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(commands, 0);
    assert!(
        org.events("delegate_sent")
            .iter()
            .all(|(a, _)| a.as_deref() == Some("makibad"))
    );

    // The whole range as a list: the skip, the landed first, the second at work, the one waiting.
    wait("the landing of the first", || {
        item(&org.range(id), 11)["place"] == json!("landed")
    });
    let range = org.range(id);
    assert_eq!(range["status"], json!("active"), "{range}");
    assert_eq!(range["approved_by"], json!("person"), "{range}");
    assert_eq!(
        places(&range),
        vec![
            (15, "skipped".to_string()),
            (11, "landed".to_string()),
            (12, "in_work".to_string()),
            (14, "not_in".to_string()),
        ],
        "{range}"
    );
    let landed = item(&range, 11);
    assert_eq!(landed["pr"], json!(101), "{range}");
    assert_eq!(landed["pr_title"], json!("Land eleven"), "{range}");
    assert_eq!(landed["task_id"], json!(first), "{range}");
    assert!(
        item(&range, 14)["why"]
            .as_str()
            .is_some_and(|w| w.contains("#99")),
        "{range}"
    );
    for (place, n) in [("skipped", 1), ("landed", 1), ("in_work", 1), ("not_in", 1)] {
        assert_eq!(range["places"][place], json!(n), "{place}: {range}");
    }

    // The budget is two tasks: once the second closes, nothing more goes in and the range ends.
    org.finish(&second);
    wait("the range to end", || {
        org.range(id)["status"] == json!("ended")
    });
    assert_eq!(org.range(id)["ended_reason"], json!("budget_tasks"));
    assert_eq!(org.events("belt_dispatched").len(), 2);
    assert_eq!(org.tasks(), vec![first, second]);
    assert!(
        org.tasks().iter().all(|t| !t.ends_with("-13")),
        "the Issue outside the range was never dispatched"
    );
}

/// A stop for the person: the escalated question leaves a pending entry that records the stop
/// (line, range, addressee, kind, recommendation), and in the escalation's own transaction the
/// next candidate goes in; the stopped run no longer holds the one slot. The answer is recorded
/// against the recommendation.
#[test]
fn a_stop_for_the_person_leaves_a_pending_entry_and_the_belt_goes_on() {
    let org = Org::new(
        "beta",
        &[
            issue(21, "Twenty-one", "Body of 21.", "c"),
            issue(22, "Twenty-two", "Body of 22.", "c"),
            issue(23, "Twenty-three", "Body of 23.", "c"),
        ],
        true,
    );
    let id = org.propose(&[21, 22, 23], 2);
    org.call(
        &org.secretary,
        "range",
        json!({"action": "skip", "id": id, "issue": 23, "reason": "not this time"}),
    );
    org.approve(id);
    let first = format!("r{id}-21");
    let second = format!("r{id}-22");
    wait("the first task taken", || {
        org.stage(&first).as_deref() == Some("settings_ready")
    });
    assert!(!org.run_exists(&second));

    let entry = org.call(
        &org.secretary,
        "pending",
        json!({"action": "add", "task": first, "question": "Which of the two designs?",
               "background": "The worker wrote two designs.", "if_yes": "The first is built.",
               "if_no": "The second is built.", "recommend": "Yes: the first is smaller.",
               "why": "It touches one file.", "stop_kind": "design", "recommend_answer": "yes"}),
    );
    let pending = entry["id"].as_i64().unwrap();
    org.call(
        &org.secretary,
        "pending",
        json!({"action": "escalate", "id": pending}),
    );
    // Same transaction: the next task is there as soon as the escalation has returned.
    assert!(
        org.run_exists(&second),
        "the next candidate goes in with the escalation"
    );
    let (status, context): (String, String) = org
        .conn()
        .query_row(
            "SELECT status, context_json FROM pending WHERE id = ?1",
            [pending],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "escalated");
    let context: Value = serde_json::from_str(&context).unwrap();
    assert_eq!(context["stop_line"], json!("decision"), "{context}");
    assert_eq!(context["range_id"], json!(id), "{context}");
    assert_eq!(context["to"], json!("person"), "{context}");
    assert_eq!(context["kind"], json!("design"), "{context}");
    assert_eq!(context["recommend_answer"], json!("yes"), "{context}");
    let stopped = org.events("belt_stopped");
    assert_eq!(stopped.len(), 1, "{stopped:?}");
    assert_eq!(stopped[0].1["pending_id"], json!(pending));
    assert_eq!(stopped[0].1["task_id"], json!(first));
    // The stopped run holds no slot: with one slot in all, the next one's launch is taken.
    wait("the next task taken", || {
        org.events("run_taken")
            .iter()
            .any(|(_, p)| p["task_id"] == json!(second))
    });

    let range = org.range(id);
    assert_eq!(
        places(&range),
        vec![
            (23, "skipped".to_string()),
            (21, "waiting_person".to_string()),
            (22, "in_work".to_string()),
        ],
        "{range}"
    );
    let stops = range["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 1, "{range}");
    assert_eq!(stops[0]["id"], json!(pending));
    assert_eq!(stops[0]["kind"], json!("design"));
    assert_eq!(stops[0]["recommend_answer"], json!("yes"));
    assert_eq!(stops[0]["answer"], Value::Null);

    org.call(
        &org.person,
        "answer",
        json!({"id": pending, "answer": "yes"}),
    );
    let range = org.range(id);
    assert_eq!(range["stops"][0]["answer"], json!("yes"), "{range}");
    assert_eq!(
        range["stops_taken"]["design"],
        json!({"stops": 1, "as_recommended": 1}),
        "{range}"
    );
    assert_eq!(item(&range, 21)["place"], json!("in_work"), "{range}");

    // The read fields printed are the ones spec section 12 documents in its table "Range field".
    assert_eq!(documented_range_fields(), printed_range_fields(&range));
}

/// Approving and resuming a range need the person key: an agent's key is refused and changes
/// nothing; the person's key passes the check (and meets the state of the range instead).
#[test]
fn an_agent_key_cannot_approve_or_resume_a_range() {
    let org = Org::new("gamma", &[issue(31, "Thirty-one", "b", "c")], false);
    let id = org.propose(&[31], 1);
    for action in ["approve", "resume"] {
        let r = org.handle(
            &org.secretary,
            "range",
            json!({"action": action, "id": id, "read": 1}),
        );
        let e = r.expect_err(action);
        assert_eq!(e.kind.as_str(), "denied", "{action}: {e:?}");
    }
    let worker_run = {
        let w = StateWriter::open(&org.db).unwrap();
        w.upsert_run(&makiba::db::writer::RunUpsert {
            task_id: "t1",
            project_slug: "gamma",
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap()
    };
    let worker = {
        let w = StateWriter::open(&org.db).unwrap();
        issue_key(&w, Role::Worker, "w-t1", Some(worker_run), None).unwrap()
    };
    for action in ["propose", "approve", "pause", "end", "skip"] {
        let r = org.handle(&worker, "range", json!({"action": action, "id": id}));
        assert_eq!(r.expect_err(action).kind.as_str(), "denied", "{action}");
    }
    assert!(org.events("range_approved").is_empty());
    assert!(org.events("range_resumed").is_empty());
    let status: String = org
        .conn()
        .query_row("SELECT status FROM ranges WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "proposed");
    // The person's key is not refused for who it is: with no read yet the approval is a conflict.
    let r = org.handle(
        &org.person,
        "range",
        json!({"action": "approve", "id": id, "read": 1}),
    );
    let e = r.expect_err("approve without a read");
    assert_ne!(e.kind.as_str(), "denied", "{e:?}");
}

// ---- the documented read fields ------------------------------------------------------------

/// The first backticked word of each row of spec section 12's table "Range field".
fn documented_range_fields() -> BTreeSet<String> {
    let start = SPEC.find("\n## 12. ").expect("spec.md has no section 12");
    let end = start + SPEC[start..].find("\n## 13. ").expect("no section 13");
    let mut out = BTreeSet::new();
    let mut inside = false;
    for line in SPEC[start..end].lines().map(str::trim) {
        if !line.starts_with('|') {
            inside = false;
            continue;
        }
        let cell = line.trim_matches('|').split('|').next().unwrap().trim();
        if cell == "Range field" {
            inside = true;
            continue;
        }
        if !inside || cell.chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        out.insert(cell.split('`').nth(1).unwrap_or(cell).to_string());
    }
    assert!(
        !out.is_empty(),
        "spec section 12 has no table \"Range field\""
    );
    out
}

/// The field paths of one printed range, as `ranges[].a.b`, `ranges[].a[].b`; `places` and
/// `stops_taken` (keyed by place and by kind) are one field each.
fn printed_range_fields(range: &Value) -> BTreeSet<String> {
    fn paths(v: &Value, prefix: &str, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let p = format!("{prefix}.{k}");
                    if ["places", "stops_taken"].contains(&k.as_str())
                        || !(x.is_object() || x.is_array())
                    {
                        out.insert(p);
                    } else {
                        paths(x, &p, out);
                    }
                }
            }
            Value::Array(a) => {
                for x in a {
                    paths(x, &format!("{prefix}[]"), out);
                }
            }
            _ => {
                out.insert(prefix.to_string());
            }
        }
    }
    let mut out = BTreeSet::new();
    paths(range, "ranges[]", &mut out);
    out
}
