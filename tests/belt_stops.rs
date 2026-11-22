//! Issue 282, the belt step 2: the stops `makibad` writes itself (docs/design/belt.md, section 8,
//! step 2).
//!
//! - A refused gate verdict on a range task leaves a pending entry with stop line `gate`, written
//!   by `makibad`, and the next task is dispatched in the gate's own transaction.
//! - Two refusals in a row of different pull requests pause the range, and the refusal that pauses
//!   dispatches nothing; the same pull request refused twice is one stop.
//! - `budget_fixes`: the head that reaches it is a stop of the task, never a refusal of the push.
//! - `budget_hours`: once past, nothing new goes in, the budget is a stop of the range, and the
//!   range ends `budget_hours` when its last task closes.
//! - `escalate` refuses a range task's entry without the stop's kind and the recommended answer.
//!
//! The setting is that of `tests/belt.rs`: the daemon in this process on the stand-in base, a
//! launch that stops at `settings_ready` and holds its slot, `max_concurrent_workers` 1, and a
//! `gh` stand-in that answers from files. The gate is asked through the door with the merge
//! gate's key; it refuses before any git check, since no passed CI is recorded for the head.

use makiba::base::stub::Stub;
use makiba::daemon;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
const HEAD: &str = "Brief head of the range: design first, then code; stop and ask before anything outside the issue.";

const STAND_IN: &str = r#"#!/bin/sh
# gh issue|pr list --repo github.com/<o>/<n> --state <s> ...: the file <what>-<state>.json, else [].
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
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        door.set_gate_key(GATE_KEY);
        let stop = Arc::new(AtomicBool::new(false));
        let daemon = run.then(|| {
            let (dir, flag) = (dir.clone(), Arc::clone(&stop));
            std::thread::spawn(move || daemon::run_until(&dir, &|| Box::new(Stub::new()), flag))
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

    /// Propose a range of `issues` with `at_once` 1 and `budget_tasks` `budget` as the secretary,
    /// with `more` fields beside them; returns its id.
    fn propose(&self, issues: &[i64], budget: i64, more: Value) -> i64 {
        let mut args = json!({"action": "propose", "project": self.slug, "issues": issues,
                              "at_once": 1, "budget_tasks": budget, "brief_head": HEAD});
        for (k, v) in more.as_object().into_iter().flatten() {
            args[k] = v.clone();
        }
        let r = self.call(&self.secretary, "range", args);
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

fn item(range: &Value, issue: i64) -> Value {
    range["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["issue"] == json!(issue))
        .cloned()
        .unwrap_or_else(|| panic!("no item {issue}: {range}"))
}

const GATE_KEY: &str = "9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e";

impl Org {
    fn repo(&self) -> String {
        format!("o/{}", self.slug)
    }

    /// Record pull request `pr` of `task` at `head` (`state`: `opened` or `pushed`), and let the
    /// stand-in show it open at that head.
    fn record_pr(&self, state: &str, task: &str, pr: i64, head: &str) {
        std::fs::write(
            self.answers.join(format!("pr-{pr}.json")),
            json!({"number": pr, "url": format!("https://github.com/{}/pull/{pr}", self.repo()),
                   "baseRefName": "main", "headRefOid": head, "state": "OPEN",
                   "mergedAt": null, "title": "A change"})
            .to_string(),
        )
        .unwrap();
        self.call(
            &self.secretary,
            "pr",
            json!({"state": state, "repo": self.repo(), "pr": pr, "head": head, "task": task}),
        );
    }

    /// The gate's answer for `pr` at `head`, asked with the merge gate's key.
    fn gate(&self, pr: i64, head: &str) -> Value {
        self.door
            .handle(
                Some(GATE_KEY),
                "gate",
                json!({"repo": self.repo(), "pr": pr, "head": head}),
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("gate: {e:?}"))
    }

    /// The pending entries that are stops of line `line`: (id, task id, status, context).
    fn stops(&self, line: &str) -> Vec<(i64, Option<String>, String, Value)> {
        let c = self.conn();
        let mut q = c
            .prepare(
                "SELECT p.id, r.task_id, p.status, p.context_json FROM pending p
                 LEFT JOIN runs r ON r.id = p.run_id
                 WHERE json_extract(p.context_json, '$.stop_line') = ?1 ORDER BY p.id",
            )
            .unwrap();
        q.query_map([line], |r| {
            let context: String = r.get(3)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                serde_json::from_str(&context).unwrap(),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn range_status(&self, id: i64) -> String {
        self.conn()
            .query_row("SELECT status FROM ranges WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }
}

fn head(c: char) -> String {
    c.to_string().repeat(40)
}

// ---- the definition of done ----------------------------------------------------------------

/// A range of two tasks and one slot. The gate refuses the first task's pull request: `makibad`
/// leaves a pending entry for the person with stop line `gate`, the range and a recommended
/// answer, and a `belt_stopped` event of its own; the refusal is recorded; and the second task
/// is dispatched by the time the gate has answered.
#[test]
fn a_refused_gate_verdict_leaves_a_stop_of_line_gate_and_the_next_task_goes_in() {
    let org = Org::new(
        "stop-gate",
        &[
            issue(11, "Eleven", "Body of eleven.", "c"),
            issue(12, "Twelve", "Body of twelve.", "c"),
        ],
        true,
    );
    let id = org.propose(&[11, 12], 2, json!({}));
    org.approve(id);
    let first = format!("r{id}-11");
    let second = format!("r{id}-12");
    wait("the first task prepared", || {
        org.stage(&first).as_deref() == Some("settings_ready")
    });
    org.record_pr("opened", &first, 101, &head('a'));
    assert!(
        !org.run_exists(&second),
        "one slot: the second waits while the first runs"
    );
    assert!(org.stops("gate").is_empty());

    let answer = org.gate(101, &head('a'));
    assert_eq!(answer["allow"], json!(false), "{answer}");

    // Same transaction: all of it is there once the gate has answered.
    let stops = org.stops("gate");
    assert_eq!(stops.len(), 1, "{stops:?}");
    let (pending, task, status, context) = &stops[0];
    assert_eq!(task.as_deref(), Some(first.as_str()));
    assert_eq!(status, "escalated");
    assert_eq!(context["range_id"], json!(id), "{context}");
    assert_eq!(context["to"], json!("person"), "{context}");
    assert!(
        ["yes", "no", "none"].contains(&context["recommend_answer"].as_str().unwrap_or("")),
        "the stop carries a recommended answer: {context}"
    );
    for part in ["background", "if_yes", "if_no", "recommend", "why"] {
        assert!(
            context[part].as_str().is_some_and(|t| !t.trim().is_empty()),
            "{part} is missing: {context}"
        );
    }
    assert_eq!(answer["pending_id"], json!(pending), "{answer}");
    let stopped = org.events("belt_stopped");
    assert_eq!(stopped.len(), 1, "{stopped:?}");
    assert_eq!(stopped[0].0.as_deref(), Some("makibad"));
    assert_eq!(stopped[0].1["line"], json!("gate"));
    assert_eq!(stopped[0].1["range_id"], json!(id));
    assert_eq!(stopped[0].1["task_id"], json!(first));
    assert_eq!(stopped[0].1["pending_id"], json!(pending));
    // The refusal itself is a record, though it was decided before the git checks.
    let verdicts = org.events("acceptance_gate");
    assert_eq!(verdicts.len(), 1, "{verdicts:?}");
    assert_eq!(verdicts[0].1["allow"], json!(false));
    assert_eq!(verdicts[0].1["pr"], json!(101));
    assert_eq!(verdicts[0].1["head"], json!(head('a')));
    assert!(
        org.run_exists(&second),
        "the next task is dispatched with the stop"
    );
    wait("the next task taken", || {
        org.events("run_taken")
            .iter()
            .any(|(_, p)| p["task_id"] == json!(second))
    });
    let range = org.range(id);
    assert_eq!(
        item(&range, 11)["place"],
        json!("waiting_person"),
        "{range}"
    );
    assert!(
        item(&range, 11)["why"]
            .as_str()
            .is_some_and(|w| w.starts_with("gate")),
        "{range}"
    );
    assert_eq!(item(&range, 12)["place"], json!("in_work"), "{range}");
    assert_eq!(range["status"], json!("active"), "{range}");

    // The person's answer is recorded and goes to the secretary, who acts on it.
    org.call(
        &org.person,
        "answer",
        json!({"id": pending, "answer": "no"}),
    );
    let to: String = org
        .conn()
        .query_row(
            "SELECT recipient FROM messages WHERE body LIKE ?1 ORDER BY id DESC LIMIT 1",
            [format!("Answer to pending entry {pending} %")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(to, "secretary");
}

/// The same pull request refused twice at one head is one stop and no pause. A second pull
/// request refused next pauses the range, by `makibad`, and that refusal dispatches nothing.
#[test]
fn two_refusals_in_a_row_of_different_pull_requests_pause_the_range() {
    let org = Org::new(
        "stop-pause",
        &[
            issue(41, "Forty-one", "b", "c"),
            issue(42, "Forty-two", "b", "c"),
            issue(43, "Forty-three", "b", "c"),
        ],
        true,
    );
    let id = org.propose(&[41, 42, 43], 3, json!({}));
    org.approve(id);
    let task = |n: i64| format!("r{id}-{n}");
    wait("the first task prepared", || {
        org.stage(&task(41)).as_deref() == Some("settings_ready")
    });
    org.record_pr("opened", &task(41), 201, &head('a'));
    assert_eq!(org.gate(201, &head('a'))["allow"], json!(false));
    assert_eq!(org.gate(201, &head('a'))["allow"], json!(false));
    assert_eq!(
        org.stops("gate").len(),
        1,
        "one stop per pull request and head"
    );
    assert_eq!(org.range_status(id), "active");
    assert!(org.run_exists(&task(42)));

    wait("the second task prepared", || {
        org.stage(&task(42)).as_deref() == Some("settings_ready")
    });
    org.record_pr("opened", &task(42), 202, &head('b'));
    assert_eq!(org.gate(202, &head('b'))["allow"], json!(false));
    assert_eq!(org.stops("gate").len(), 2);
    assert_eq!(org.range_status(id), "paused");
    let paused = org.events("range_paused");
    assert_eq!(paused.len(), 1, "{paused:?}");
    assert_eq!(paused[0].0.as_deref(), Some("makibad"));
    assert_eq!(paused[0].1["range_id"], json!(id));
    assert!(
        paused[0].1["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "{paused:?}"
    );
    // The paused range puts nothing in, now or on a later pass.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !org.run_exists(&task(43)),
        "the refusal that pauses dispatches nothing"
    );
}

/// `budget_fixes` 1: the first head recorded after the pull request was opened is recorded as
/// any push, and is a stop of the task, line `budget`; the belt goes on.
#[test]
fn the_head_that_reaches_budget_fixes_is_a_stop_and_the_push_is_recorded() {
    let org = Org::new(
        "stop-fixes",
        &[
            issue(51, "Fifty-one", "b", "c"),
            issue(52, "Fifty-two", "b", "c"),
        ],
        true,
    );
    let id = org.propose(&[51, 52], 2, json!({"budget_fixes": 1}));
    org.approve(id);
    let first = format!("r{id}-51");
    wait("the first task prepared", || {
        org.stage(&first).as_deref() == Some("settings_ready")
    });
    assert_eq!(org.range(id)["budget"]["fixes"], json!(1));
    org.record_pr("opened", &first, 301, &head('a'));
    assert!(org.stops("budget").is_empty());
    org.record_pr("pushed", &first, 301, &head('b'));
    let pushed = org.events("fix_pushed");
    assert_eq!(pushed.len(), 1, "the push is recorded: {pushed:?}");
    assert_eq!(pushed[0].1["head_oid"], json!(head('b')));
    let stops = org.stops("budget");
    assert_eq!(stops.len(), 1, "{stops:?}");
    assert_eq!(stops[0].1.as_deref(), Some(first.as_str()));
    assert_eq!(stops[0].2, "escalated");
    assert_eq!(stops[0].3["range_id"], json!(id));
    assert!(
        org.run_exists(&format!("r{id}-52")),
        "the next task goes in with the stop"
    );
}

/// `budget_hours`: once the hours are past nothing new goes in; the budget that keeps a candidate
/// out is a stop of the range (no task), and the range ends `budget_hours` when its task closes.
#[test]
fn past_budget_hours_nothing_goes_in_and_the_range_ends_when_its_task_closes() {
    let org = Org::new(
        "stop-hours",
        &[
            issue(71, "Seventy-one", "b", "c"),
            issue(72, "Seventy-two", "b", "c"),
        ],
        true,
    );
    // 0.003 hours: 10.8 seconds.
    let id = org.propose(&[71, 72], 2, json!({"budget_hours": 0.003}));
    org.approve(id);
    let approved = Instant::now();
    let first = format!("r{id}-71");
    wait("the first task prepared", || {
        org.stage(&first).as_deref() == Some("settings_ready")
    });
    let range = org.range(id);
    assert_eq!(range["budget"]["hours"], json!(0.003), "{range}");
    assert_eq!(range["budget"]["fixes"], json!(3), "the default: {range}");
    std::thread::sleep(Duration::from_secs(12).saturating_sub(approved.elapsed()));
    org.finish(&first);
    assert!(
        !org.run_exists(&format!("r{id}-72")),
        "past the hours nothing new goes in"
    );
    let stops = org.stops("budget");
    assert_eq!(stops.len(), 1, "{stops:?}");
    assert_eq!(stops[0].1, None, "a stop of the range, not of a task");
    assert_eq!(stops[0].2, "escalated");
    assert_eq!(stops[0].3["range_id"], json!(id));
    wait("the range to end", || org.range_status(id) == "ended");
    assert_eq!(org.range(id)["ended_reason"], json!("budget_hours"));
    assert_eq!(org.tasks(), vec![first]);
}

/// A range task's entry is escalated only with the stop's kind and the recommended answer.
#[test]
fn escalate_refuses_a_range_tasks_entry_without_kind_and_recommended_answer() {
    let org = Org::new("stop-kind", &[issue(61, "Sixty-one", "b", "c")], true);
    let id = org.propose(&[61], 1, json!({}));
    org.approve(id);
    let task = format!("r{id}-61");
    wait("the task", || org.run_exists(&task));
    let parts = json!({"action": "add", "task": task, "question": "Which of the two?",
                       "background": "b", "if_yes": "y", "if_no": "n", "recommend": "Yes.",
                       "why": "w"});
    let pending = org.call(&org.secretary, "pending", parts.clone())["id"]
        .as_i64()
        .unwrap();
    let e = org
        .handle(
            &org.secretary,
            "pending",
            json!({"action": "escalate", "id": pending}),
        )
        .expect_err("escalated without the stop's kind and recommended answer");
    assert_eq!(e.kind.as_str(), "usage", "{e:?}");
    for part in ["stop_kind", "recommend_answer"] {
        assert!(e.message.contains(part), "{part} not named: {e:?}");
    }
    assert!(org.events("belt_stopped").is_empty());
    let mut whole = parts;
    whole["stop_kind"] = json!("design");
    whole["recommend_answer"] = json!("yes");
    assert_eq!(
        org.call(&org.secretary, "pending", whole)["id"],
        json!(pending)
    );
    org.call(
        &org.secretary,
        "pending",
        json!({"action": "escalate", "id": pending}),
    );
    assert_eq!(org.events("belt_stopped").len(), 1);
}
