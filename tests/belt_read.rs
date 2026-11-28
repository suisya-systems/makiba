//! Issue 248, the belt step 5: the read fields `belt[]` and `pending[].stop_line`, `.range_id`,
//! `.to` (docs/design/belt.md, sections 7 and 8, step 5).
//!
//! - `belt[]` holds one row per open run of a range, in run order: its place and stage as the
//!   range's list gives them, whether it counts against the range's `at_once`, the stop it waits
//!   on, and the Issues a design-only run still waits for. A run with no range is not on it, and a
//!   closed run leaves it.
//! - A pending entry that is a stop of the belt names its line, its range and whom it is put to.
//! - The fields printed are the ones spec section 12 documents in its table "Belt field".
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
use rusqlite::{Connection, OpenFlags};
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
# gh issue|pr list --repo [github.com/]<o>/<n> --state <s> ...: the file <what>-<state>.json, else [].
# gh issue|pr view <n> --repo [github.com/]<o>/<n> ...: the file <what>-<n>.json, else a failure.
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
case "$repo" in */*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
r=${repo#github.com/}
dir="$FAKE_GH_DIR/${r%%/*}_${r#*/}"
mkdir -p "$dir"
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
            std::env::set_var("MAKIBA_RANGE_READ_INTERVAL_MS", "500");
            std::env::set_var("MAKIBA_LANDING_RETRY_MS", "300");
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

fn issue(number: i64, title: &str, body: &str) -> Value {
    json!({"number": number, "title": title, "body": body, "labels": [], "milestone": null,
           "createdAt": "2026-11-01T00:00:00Z", "updatedAt": "2026-11-01T00:00:00Z",
           "comments": []})
}

/// One organization with one project `slug` (GitHub `o/<slug>`), its door and its daemon.
struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    door: Door,
    person: String,
    secretary: String,
    slug: String,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl Org {
    /// `issues`: the open Issues the stand-in answers.
    fn new(slug: &str, issues: &[Value]) -> Org {
        let home = init();
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
        let daemon = {
            let (dir, flag) = (dir.clone(), Arc::clone(&stop));
            Some(std::thread::spawn(move || {
                makibad::run_until(&dir, &|| Box::new(Stub::new()), flag)
            }))
        };
        Org {
            _tmp: tmp,
            db,
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

    /// The payloads of the events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let c = self.conn();
        let mut q = c
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| {
            let p: String = r.get(0)?;
            Ok(serde_json::from_str(&p).unwrap())
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
            .ok()
    }

    /// The person's `status --json`, through the door.
    fn status(&self) -> Value {
        self.call(&self.person, "status", json!({}))
    }

    /// `belt[]` of the person's status.
    fn belt(&self) -> Vec<Value> {
        let s = self.status();
        s["belt"]
            .as_array()
            .unwrap_or_else(|| panic!("status has no belt[]: {s}"))
            .clone()
    }

    /// The pending entry `id` on the read surface.
    fn pending(&self, id: i64) -> Option<Value> {
        let s = self.status();
        s["pending"]
            .as_array()
            .unwrap_or_else(|| panic!("status has no pending[]: {s}"))
            .iter()
            .find(|p| p["id"] == json!(id))
            .cloned()
    }

    fn read_done(&self, id: i64) -> bool {
        self.events("range_read")
            .iter()
            .any(|p| p["range_id"] == json!(id) && p["complete"] == json!(true))
    }

    /// Propose a range of `issues` as the secretary, with `more` fields; returns its id.
    fn propose(&self, issues: &[i64], at_once: i64, budget: i64, more: Value) -> i64 {
        let mut args = json!({"action": "propose", "project": self.slug, "issues": issues,
                              "at_once": at_once, "budget_tasks": budget, "brief_head": HEAD});
        if let (Value::Object(a), Value::Object(m)) = (&mut args, more) {
            a.extend(m);
        }
        let r = self.call(&self.secretary, "range", args);
        r["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("no range id: {r}"))
    }

    /// Approve range `id` with the person key, on its newest complete read.
    fn approve(&self, id: i64) {
        wait("the read of the draft", || self.read_done(id));
        let s = self.status();
        let range = s["ranges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == json!(id))
            .unwrap_or_else(|| panic!("range {id} not listed: {s}"));
        // The mode the read surface shows is the one the approval binds (Issue 326).
        let (read, mode) = (range["read_id"].clone(), range["approves_under"].clone());
        self.call(
            &self.person,
            "range",
            json!({"action": "approve", "id": id, "read": read, "approval_mode": mode}),
        );
    }

    /// The launch of `task` has been prepared (it holds there, on the stand-in base).
    fn prepared(&self, task: &str) {
        wait(&format!("{task} prepared"), || {
            self.stage(task).as_deref() == Some("settings_ready")
        });
    }

    /// The worker of `task` reports `state` with `text`.
    fn report(&self, task: &str, state: &str, text: &str) {
        let worker = self.worker(task);
        self.call(
            &worker,
            "report",
            json!({"state": state, "task": task, "text": text}),
        );
    }

    /// The secretary records the pull request `pr` of `task`.
    fn pr(&self, task: &str, pr: i64) {
        self.call(
            &self.secretary,
            "pr",
            json!({"state": "opened", "repo": format!("o/{}", self.slug), "pr": pr,
                   "head": "a".repeat(40), "task": task}),
        );
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

/// The row of `task` in `belt`.
fn row<'a>(belt: &'a [Value], task: &str) -> &'a Value {
    belt.iter()
        .find(|r| r["task_id"] == json!(task))
        .unwrap_or_else(|| panic!("no belt row of {task}: {belt:?}"))
}

/// The first backticked word of each row of spec section 12's table "Belt field".
fn documented_belt_fields() -> BTreeSet<String> {
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
        if cell == "Belt field" {
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
        "spec section 12 has no table \"Belt field\""
    );
    out
}

/// The field paths of the printed rows, as `belt[].a`; an array field, empty or not, as
/// `belt[].a[]`.
fn printed_belt_fields(belt: &[Value]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for r in belt {
        for (k, v) in r.as_object().expect("a belt row is an object") {
            out.insert(if v.is_array() {
                format!("belt[].{k}[]")
            } else {
                format!("belt[].{k}")
            });
        }
    }
    out
}

// ---- the read fields -----------------------------------------------------------------------

/// A range of two tasks and one slot: each open run of the range is on the belt with its place,
/// its count and its stop; a one-off run and an Issue outside the range are not; a stop for the
/// person shows on the belt and on its pending entry, and goes when answered; a run in review
/// counts no more, and a closed one leaves the belt.
#[test]
fn the_belt_lists_each_open_range_task_with_its_place_its_count_and_its_stop() {
    let org = Org::new(
        "delta",
        &[
            issue(41, "Forty-one", "Body of 41."),
            issue(42, "Forty-two", "Body of 42."),
            issue(43, "Forty-three", "Outside the range."),
        ],
    );
    {
        let w = StateWriter::open(&org.db).unwrap();
        w.upsert_run(&makiba::db::writer::RunUpsert {
            task_id: "solo",
            project_slug: "delta",
            title: Some("A one-off task"),
            status: Some("review"),
            ..Default::default()
        })
        .unwrap();
    }
    let id = org.propose(&[41, 42], 1, 2, json!({}));
    org.approve(id);
    let first = format!("r{id}-41");
    let second = format!("r{id}-42");
    org.prepared(&first);

    // 1. One task on the belt, at work, counting; nothing outside the range.
    let belt = org.belt();
    assert_eq!(belt.len(), 1, "{belt:?}");
    let r = &belt[0];
    assert_eq!(r["range_id"], json!(id), "{r}");
    assert_eq!(r["task_id"], json!(first), "{r}");
    assert_eq!(r["issue"], json!(41), "{r}");
    assert_eq!(r["place"], json!("in_work"), "{r}");
    assert_eq!(r["stage"], json!("launching"), "{r}");
    assert_eq!(r["counts"], json!(true), "{r}");
    assert_eq!(r["stop_line"], Value::Null, "{r}");
    assert_eq!(r["stop_reason"], Value::Null, "{r}");
    assert_eq!(r["pending_id"], Value::Null, "{r}");
    assert_eq!(r["waits_for"], json!([]), "{r}");
    assert!(r["since"].is_string(), "{r}");
    assert!(
        !belt
            .iter()
            .any(|r| r["task_id"] == json!("solo") || r["issue"] == json!(43)),
        "{belt:?}"
    );

    // 2. A question on the one-off task is no stop of the belt.
    let plain = org.call(
        &org.secretary,
        "pending",
        json!({"action": "add", "task": "solo", "question": "Is the one-off still wanted?",
               "background": "It has waited a day.", "if_yes": "It goes on.",
               "if_no": "It is closed.", "recommend": "Yes: it is nearly done.",
               "why": "One commit is left."}),
    )["id"]
        .as_i64()
        .unwrap();
    let p = org
        .pending(plain)
        .unwrap_or_else(|| panic!("no pending entry {plain}"));
    assert_eq!(p["stop_line"], Value::Null, "{p}");
    assert_eq!(p["range_id"], Value::Null, "{p}");
    assert_eq!(p["to"], Value::Null, "{p}");

    // 3. A stop of the first task: it waits for the person and counts no more; the second goes in.
    let stop = org.call(
        &org.secretary,
        "pending",
        json!({"action": "add", "task": first, "question": "Which of the two designs?",
               "background": "The worker wrote two designs.", "if_yes": "The first is built.",
               "if_no": "The second is built.", "recommend": "Yes: the first is smaller.",
               "why": "It touches one file.", "stop_kind": "design", "recommend_answer": "yes"}),
    )["id"]
        .as_i64()
        .unwrap();
    org.call(
        &org.secretary,
        "pending",
        json!({"action": "escalate", "id": stop}),
    );
    let belt = org.belt();
    let tasks: Vec<&Value> = belt.iter().map(|r| &r["task_id"]).collect();
    assert_eq!(tasks, vec![&json!(first), &json!(second)], "{belt:?}");
    let r = row(&belt, &first);
    assert_eq!(r["place"], json!("waiting_person"), "{r}");
    assert_eq!(r["counts"], json!(false), "{r}");
    assert_eq!(r["stop_line"], json!("decision"), "{r}");
    assert_eq!(r["stop_reason"], json!("Which of the two designs?"), "{r}");
    assert_eq!(r["pending_id"], json!(stop), "{r}");
    let r = row(&belt, &second);
    assert_eq!(r["place"], json!("in_work"), "{r}");
    assert_eq!(r["counts"], json!(true), "{r}");
    assert_eq!(r["stop_line"], Value::Null, "{r}");
    let p = org
        .pending(stop)
        .unwrap_or_else(|| panic!("no pending entry {stop}"));
    assert_eq!(p["stop_line"], json!("decision"), "{p}");
    assert_eq!(p["range_id"], json!(id), "{p}");
    assert_eq!(p["to"], json!("person"), "{p}");

    // The read fields printed are the ones spec section 12 documents in its table "Belt field".
    assert_eq!(documented_belt_fields(), printed_belt_fields(&belt));

    // 4. Answered: the first task is at work again and counts; its entry is no longer pending.
    org.call(&org.person, "answer", json!({"id": stop, "answer": "yes"}));
    let belt = org.belt();
    let r = row(&belt, &first);
    assert_eq!(r["place"], json!("in_work"), "{r}");
    assert_eq!(r["counts"], json!(true), "{r}");
    assert_eq!(r["stop_line"], Value::Null, "{r}");
    assert_eq!(r["pending_id"], Value::Null, "{r}");
    assert!(org.pending(stop).is_none(), "entry {stop} still pending");

    // 5. Done with nothing pushed: in review, and still counting (its fix may be due, Issue 654).
    org.report(&first, "done", "Done; ran the tests.");
    let belt = org.belt();
    let r = row(&belt, &first);
    assert_eq!(r["place"], json!("review"), "{r}");
    assert_eq!(r["stage"], json!("no_pr"), "{r}");
    assert_eq!(r["counts"], json!(true), "{r}");

    // 6. Closed: off the belt; the second is still on it.
    org.call(&org.secretary, "close", json!({"task_id": first}));
    let belt = org.belt();
    assert!(
        !belt.iter().any(|r| r["task_id"] == json!(first)),
        "{belt:?}"
    );
    row(&belt, &second);
}

/// A design-only task waiting for two tasks of its range: the belt names both while both are
/// open, keeps naming both while the question about an abandoned one waits for the person, and
/// names only the other once the person lets the abandoned one go.
#[test]
fn waits_for_names_only_the_dependencies_not_yet_landed_or_let_go() {
    let org = Org::new(
        "epsilon",
        &[
            issue(51, "Base A", "The first part."),
            issue(52, "Base B", "The second part."),
            issue(
                53,
                "On both",
                "Builds on both.\n\nDepends on #51\nDepends on #52",
            ),
        ],
    );
    let id = org.propose(&[51, 52, 53], 1, 3, json!({"design_ahead": true}));
    org.approve(id);
    let (a, b, top) = (
        format!("r{id}-51"),
        format!("r{id}-52"),
        format!("r{id}-53"),
    );

    // Both dependencies go in, one at a time, each with its pull request.
    org.prepared(&a);
    org.pr(&a, 11);
    org.report(&a, "done", "Done; ran the tests.");
    org.prepared(&b);
    org.pr(&b, 12);
    org.report(&b, "done", "Done; ran the tests.");

    // The dependent goes in for its design only and reports it: it waits for both.
    org.prepared(&top);
    org.report(
        &top,
        "blocked",
        "The design: build on both new types; no code yet.",
    );
    let belt = org.belt();
    let r = row(&belt, &top);
    assert_eq!(r["place"], json!("review"), "{r}");
    assert_eq!(r["stage"], json!("waits_landing #51 #52"), "{r}");
    assert_eq!(r["counts"], json!(false), "{r}");
    assert_eq!(r["waits_for"], json!([51, 52]), "{r}");
    assert_eq!(r["stop_line"], Value::Null, "{r}");

    // One dependency is abandoned: a question to the person; the wait is not settled yet.
    org.call(
        &org.secretary,
        "close",
        json!({"task_id": a, "abandon": true}),
    );
    let mut asked = None;
    wait("the question about the abandoned dependency", || {
        asked = org
            .events("belt_stopped")
            .iter()
            .find(|p| p["task_id"] == json!(top))
            .and_then(|p| p["pending_id"].as_i64());
        asked.is_some()
    });
    let asked = asked.unwrap();
    let belt = org.belt();
    let r = row(&belt, &top);
    assert_eq!(r["place"], json!("waiting_person"), "{r}");
    assert_eq!(r["stop_line"], json!("decision"), "{r}");
    assert_eq!(r["pending_id"], json!(asked), "{r}");
    assert_eq!(r["waits_for"], json!([51, 52]), "{r}");

    // The person lets it go: the belt names only the other.
    org.call(&org.person, "answer", json!({"id": asked, "answer": "yes"}));
    let belt = org.belt();
    let r = row(&belt, &top);
    assert_eq!(r["waits_for"], json!([52]), "{r}");
    assert_eq!(r["stop_line"], Value::Null, "{r}");
    assert_eq!(r["pending_id"], Value::Null, "{r}");
}
