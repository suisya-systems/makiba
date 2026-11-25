//! Issue 284, the belt step 4: a candidate held by a dependency does its design while it waits,
//! and a follow-up found on the way is inside the range by default (docs/design/belt.md, sections
//! 4.3 and 8, step 4; the owner's decision Q3).
//!
//! - With `design_ahead`, a candidate whose open dependency is a task of the range with a pull
//!   request is dispatched for its design only. Its design report (`report blocked`) parks it: it
//!   waits for a landing, not for a person. A failed read of the dependency's pull request is no
//!   landing. When the dependency's task has closed `completed`, its pull request is read merged and
//!   the merge commit is in the project's clone (the workers' `origin`), the run is brought back on
//!   its conversation with the reason `landed`.
//! - A proposal's `follow_ups` is `inside` unless it says otherwise: an Issue created after the
//!   approval with a line `Follow-up of #<n>` naming an Issue of the range is dispatched, unless a
//!   line `Needs host:` names a host the project does not list or a line `Stop line:` names the
//!   stop line it would hit.
//!
//! The daemon runs in this process on `Local` with the test agent (as tests/belt_park.rs), and
//! the door is called with the person's, the secretary's and a worker's keys (as tests/belt.rs).
//! The test agent reports `working` (its launch is verified), then `idle`, then waits; a resume
//! runs the steps file its conversation id names in the workplace, which the project's main
//! carries. `gh` is a stand-in script (`MAKIBA_GH`) that answers from files in a folder per
//! repository, logs every call and refuses anything else, so no test reaches the network. Each
//! test has a project of its own with a real clone whose `origin` is a local bare repository.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, Terminal};
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(60);
const HEAD: &str = "Brief head of the range: design first, have it read, then code.";
/// The conversation id the test agent names in every workplace of these projects.
const CONVERSATION: &str = "conv-ahead";

const STAND_IN: &str = r#"#!/bin/sh
# gh issue|pr list --repo github.com/<o>/<n> --state <s> ...: the file <what>-<state>.json, else [].
# gh issue|pr view <n> --repo [github.com/]<o>/<n> ...: the file <what>-<n>.json, else a failure.
# Every call is logged whole, one line, in the repository's folder (calls).
all="$*"
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
echo "$all" >> "$dir/calls"
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

/// The folder the tests share (the stand-in and its answers, the repositories, the workplaces)
/// and the environment, set once before any test starts a thread.
fn init() -> PathBuf {
    INIT.call_once(|| {
        makibad::process_knowledge();
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        for d in [
            "answers", "goals", "repos", "origins", "workers", "docs", "share", "config", "scratch",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let agent = json!([AGENT, "record", "working", "idle", "hang"]).to_string();
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
            std::env::set_var("MAKIBA_FAKE_AGENT", agent);
            std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "3000");
            std::env::set_var("MAKIBA_PARK_AFTER_MS", "300");
            std::env::set_var("MAKIBA_MAX_CONCURRENT_WORKERS", "2");
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

/// An open Issue as `gh issue list` gives it, created at `created`.
fn issue(number: i64, title: &str, body: &str, created: &str) -> Value {
    json!({"number": number, "title": title, "body": body, "labels": [], "milestone": null,
           "createdAt": created, "updatedAt": created, "comments": []})
}

const LONG_AGO: &str = "2026-01-01T00:00:00Z";

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Ends every terminal of the base when the test ends, passed or failed.
struct EndAll(Local);

impl Drop for EndAll {
    fn drop(&mut self) {
        for t in self.0.list().unwrap_or_default() {
            let _ = self.0.end(&t.id);
        }
    }
}

/// One organization with one project `slug` (GitHub `o/<slug>`), its door and its daemon on
/// `Local`.
struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    answers: PathBuf,
    repo: PathBuf,
    origin: PathBuf,
    home: PathBuf,
    local: Local,
    door: Door,
    person: String,
    secretary: String,
    slug: String,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
    _end_all: EndAll,
}

impl Org {
    /// `issues`: the open Issues the stand-in answers.
    fn new(slug: &str, issues: &[Value]) -> Org {
        let home = init();
        // A clone with a GitHub name (`projects.origin_url`) and a local bare origin for git. Its
        // main carries what the test agent needs in a workplace: its conversation id, the steps
        // file a resume runs, and an ignore rule for its record file.
        let repo = home.join("repos").join(slug);
        let origin = home.join("origins").join(format!("{slug}.git"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".fake-conversation"), CONVERSATION).unwrap();
        std::fs::write(repo.join(CONVERSATION), "[]").unwrap();
        std::fs::write(repo.join(".gitignore"), "record\n").unwrap();
        git(
            &repo,
            &["add", ".fake-conversation", CONVERSATION, ".gitignore"],
        );
        git(&repo, &["commit", "-q", "-m", "first"]);
        git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let answers = home.join("answers").join(format!("o_{slug}"));
        std::fs::create_dir_all(&answers).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.ensure_project(slug, None, Some(&format!("https://github.com/o/{slug}")))
            .unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let local = Local::new();
        let door = Door::new(w, Arc::new(local.clone()), "fake".into());
        let stop = Arc::new(AtomicBool::new(false));
        let daemon = {
            let (dir, flag, local) = (dir.clone(), Arc::clone(&stop), local.clone());
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(local.clone()), flag))
        };
        let org = Org {
            _tmp: tmp,
            db,
            answers,
            repo,
            origin,
            home,
            _end_all: EndAll(local.clone()),
            local,
            door,
            person,
            secretary,
            slug: slug.into(),
            stop,
            daemon: Some(daemon),
        };
        org.set_issues(issues);
        org
    }

    fn set_issues(&self, issues: &[Value]) {
        std::fs::write(
            self.answers.join("issue-open.json"),
            Value::Array(issues.to_vec()).to_string(),
        )
        .unwrap();
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

    fn run_status(&self, task: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .ok()
    }

    /// The payloads of the events of `kind`, oldest first, with their actor.
    fn events(&self, kind: &str) -> Vec<(Option<String>, Value)> {
        let c = self.conn();
        let mut q = c
            .prepare("SELECT actor, payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| {
            let p: Option<String> = r.get(1)?;
            Ok((
                r.get(0)?,
                serde_json::from_str(&p.unwrap_or_default()).unwrap_or(Value::Null),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    /// The payloads of the events of `kind` whose `task_id` is `task`.
    fn events_of(&self, kind: &str, task: &str) -> Vec<Value> {
        self.events(kind)
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| p["task_id"] == json!(task))
            .collect()
    }

    /// The `belt_dispatched` events of `issue`, with their actor.
    fn dispatched(&self, issue: i64) -> Vec<(Option<String>, Value)> {
        self.events("belt_dispatched")
            .into_iter()
            .filter(|(_, p)| p["issue"] == json!(issue))
            .collect()
    }

    /// (stage, conversation_id, brief) of every launch of `task`, oldest first.
    fn launches(&self, task: &str) -> Vec<(String, Option<String>, String)> {
        let c = self.conn();
        let mut q = c
            .prepare(
                "SELECT l.stage, l.conversation_id, l.brief FROM run_launch l
                 JOIN runs r ON r.id = l.run_id WHERE r.task_id = ?1 ORDER BY l.id",
            )
            .unwrap();
        q.query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The live terminals carrying `task` as `makiba.task_id`.
    fn terminals(&self, task: &str) -> Vec<Terminal> {
        self.local
            .list()
            .unwrap()
            .into_iter()
            .filter(|t| t.alive && t.tags.get("makiba.task_id").map(String::as_str) == Some(task))
            .collect()
    }

    /// `task` is at work, its newest launch verified, and its terminal has said `idle`.
    fn at_work_and_idle(&self, task: &str) {
        wait(&format!("{task} at work and idle"), || {
            self.run_status(task).as_deref() == Some("in_use")
                && self
                    .launches(task)
                    .last()
                    .is_some_and(|l| l.0 == "verified")
                && self
                    .conn()
                    .query_row(
                        "SELECT count(*) FROM events WHERE kind = 'terminal_status'
                           AND json_extract(payload_json, '$.state') = 'idle'
                           AND run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                        [task],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                    >= 1
        });
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

    /// The candidate `issue` of the newest read of range `id`, if it is one.
    fn candidate(&self, id: i64, issue: i64) -> Option<Value> {
        self.range(id)["candidates"]
            .as_array()
            .unwrap_or_else(|| panic!("no candidates[] in range {id}"))
            .iter()
            .find(|c| c["issue"] == json!(issue))
            .cloned()
    }

    /// The row of `task` in `status`'s `runs[]`.
    fn run_row(&self, task: &str) -> Value {
        let s = self.status();
        s["runs"]
            .as_array()
            .unwrap_or_else(|| panic!("status has no runs[]: {s}"))
            .iter()
            .find(|r| r["task_id"] == json!(task))
            .cloned()
            .unwrap_or_else(|| panic!("run {task} not listed: {s}"))
    }

    fn read_done(&self, id: i64) -> bool {
        self.events("range_read")
            .iter()
            .any(|(_, p)| p["range_id"] == json!(id) && p["complete"] == json!(true))
    }

    /// Propose a range of `issues` as the secretary, with `more` arguments; returns its id.
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

    /// Approve range `id` with the person key, on the read the read surface shows.
    fn approve(&self, id: i64) {
        wait("the read of the draft", || self.read_done(id));
        let range = self.range(id);
        // The mode the read surface shows is the one the approval binds (Issue 326).
        let (read, mode) = (range["read_id"].clone(), range["approves_under"].clone());
        self.call(
            &self.person,
            "range",
            json!({"action": "approve", "id": id, "read": read, "approval_mode": mode}),
        );
    }

    /// The calls the stand-in logged for this project's repository.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.answers.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// A commit pushed to main at the bare origin, as a merge on GitHub would land it; its id.
    fn land_on_origin(&self, what: &str) -> String {
        let work = self
            .home
            .join("scratch")
            .join(format!("{}-land", self.slug));
        git(
            &self.home,
            &[
                "clone",
                "-q",
                &self.origin.to_string_lossy(),
                &work.to_string_lossy(),
            ],
        );
        std::fs::write(work.join("landed.txt"), what).unwrap();
        git(&work, &["add", "landed.txt"]);
        git(&work, &["commit", "-q", "-m", what]);
        git(&work, &["push", "-q", "origin", "main"]);
        git(&work, &["rev-parse", "HEAD"])
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

// ---- the definition of done ----------------------------------------------------------------

/// A dependent candidate is dispatched design-only, parked after its design report, and
/// unparked on its conversation when its dependency closes completed and lands; a failed read of
/// the dependency's pull request is no landing.
#[test]
fn a_dependent_candidate_is_dispatched_design_only_parked_and_unparked_when_its_dependency_closes_completed()
 {
    let org = Org::new(
        "ahead",
        &[
            issue(1, "The base", "The part the second builds on.", LONG_AGO),
            issue(
                2,
                "On top",
                "Builds on the base.\n\nDepends on #1",
                LONG_AGO,
            ),
        ],
    );
    let id = org.propose(&[1, 2], 2, 2, json!({"design_ahead": true}));
    org.approve(id);

    // 1. The dependency goes in; the dependent waits while its dependency has no pull request.
    wait("r1-1 dispatched", || org.run_status("r1-1").is_some());
    let task1 = format!("r{id}-1");
    let task2 = format!("r{id}-2");
    assert_eq!(task1, "r1-1");
    let c2 = org
        .candidate(id, 2)
        .unwrap_or_else(|| panic!("no candidate 2: {}", org.range(id)));
    assert_eq!(c2["admitted"], json!(false), "{c2}");
    assert_eq!(c2["left_out_why"], json!("waits_for #1"), "{c2}");
    assert!(org.run_status(&task2).is_none(), "r1-2 went in too early");

    // 2. Its dependency's pull request is recorded: the dependent goes in for its design only.
    org.call(
        &org.secretary,
        "pr",
        json!({"state": "opened", "repo": "o/ahead", "pr": 11, "head": "a".repeat(40),
               "task": task1}),
    );
    wait("the design-only dispatch of #2", || {
        !org.dispatched(2).is_empty()
    });
    let d = org.dispatched(2);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].0.as_deref(), Some("makibad"), "{d:?}");
    assert_eq!(d[0].1["task_id"], json!(task2), "{d:?}");
    assert_eq!(d[0].1["design_only"], json!(true), "{d:?}");
    assert_eq!(d[0].1["waits_for"], json!([1]), "{d:?}");
    let brief = org.launches(&task2)[0].2.clone();
    assert!(brief.contains("Design only"), "{brief}");
    assert!(brief.contains("#1 has not landed"), "{brief}");

    // 3. It reports its design; it may not report done while #1 has not landed.
    org.at_work_and_idle(&task2);
    let worker = org.worker(&task2);
    org.call(
        &worker,
        "report",
        json!({"state": "blocked", "task": task2,
               "text": "The design: build on the base's new type; no code yet."}),
    );
    let reported = org.events_of("design_reported", &task2);
    assert_eq!(reported.len(), 1, "{reported:?}");
    let refused = org
        .handle(
            &worker,
            "report",
            json!({"state": "done", "task": task2, "text": "Done."}),
        )
        .expect_err("a design-only run reported done before its dependency landed");
    assert_eq!(refused.kind, ErrorKind::Conflict, "{refused:?}");

    // 4. It is parked: no terminal, waiting for the landing of #1.
    wait("r1-2 parked", || {
        org.run_status(&task2).as_deref() == Some("parked")
    });
    wait("r1-2's terminal ended", || org.terminals(&task2).is_empty());
    let parked = org.events_of("run_parked", &task2);
    assert_eq!(parked.len(), 1, "{parked:?}");
    assert_eq!(parked[0]["waits_for"], json!([1]), "{parked:?}");
    let row = org.run_row(&task2);
    assert_eq!(row["waiting"]["for"], json!("landing"), "{row}");
    assert_eq!(row["waiting"]["issues"], json!([1]), "{row}");

    // 5. The dependency closes completed; its pull request cannot be read: no landing.
    let worker1 = org.worker(&task1);
    org.call(
        &worker1,
        "report",
        json!({"state": "done", "task": task1, "text": "Done; ran the tests."}),
    );
    org.call(&org.secretary, "close", json!({"task_id": task1}));
    assert_eq!(org.run_status(&task1).as_deref(), Some("completed"));
    let before = org.calls().len();
    let landing_reads = |org: &Org| {
        org.calls()[before..]
            .iter()
            .filter(|c| c.starts_with("pr view 11 ") && c.contains("mergedAt"))
            .count()
    };
    wait("two failed reads of the landing of pull request 11", || {
        landing_reads(&org) >= 2
    });
    assert_eq!(org.run_status(&task2).as_deref(), Some("parked"));
    assert!(org.events_of("run_unparked", &task2).is_empty());
    assert!(org.events("ahead_settled").is_empty());

    // 6. It lands: merged at GitHub and in the workers' origin. The dependent comes back.
    let merge = org.land_on_origin("the base landed");
    std::fs::write(
        org.answers.join("pr-11.json"),
        json!({"number": 11, "state": "MERGED", "mergedAt": "2026-11-23T00:00:00Z",
               "title": "The base", "mergeCommit": {"oid": merge}})
        .to_string(),
    )
    .unwrap();
    wait("r1-2 unparked", || {
        !org.events_of("run_unparked", &task2).is_empty()
    });
    assert_eq!(org.events_of("pr_merged", &task1).len(), 1);
    let landed = org.events_of("landed_in_clone", &task1);
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert_eq!(landed[0]["merge_commit"], json!(merge), "{landed:?}");
    assert_eq!(
        git(&org.repo, &["rev-parse", "refs/remotes/origin/main"]),
        merge
    );
    let unparked = org.events_of("run_unparked", &task2);
    assert_eq!(unparked.len(), 1, "{unparked:?}");
    assert_eq!(unparked[0]["reason"], json!("landed"), "{unparked:?}");
    assert_eq!(unparked[0]["detail"]["issues"], json!([1]), "{unparked:?}");
    let told: Vec<String> = {
        let c = org.conn();
        let mut q = c
            .prepare("SELECT body FROM messages WHERE recipient = ?1 AND kind = 'notice'")
            .unwrap();
        q.query_map([format!("w-{task2}")], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let landed_notices: Vec<&String> = told.iter().filter(|b| b.contains("#1 landed")).collect();
    assert_eq!(landed_notices.len(), 1, "{told:?}");
    assert!(landed_notices[0].contains(&merge), "{told:?}");

    let back = || {
        let l = org.launches(&task2);
        org.run_status(&task2).as_deref() == Some("in_use")
            && l.len() == 2
            && l[1].0 == "verified"
            && org.terminals(&task2).len() == 1
    };
    wait("r1-2 back at work", back);
    let l = org.launches(&task2);
    assert_eq!(l[0].1.as_deref(), Some(CONVERSATION), "{l:?}");
    assert_eq!(l[1].1.as_deref(), Some(CONVERSATION), "{l:?}");
}

/// A follow-up found on the way is inside a range by default, unless it needs a host the project
/// does not list or names a stop line it would hit; one created before the approval is not one.
#[test]
fn a_follow_up_is_inside_by_default_unless_it_needs_a_host_or_names_a_stop_line() {
    let first = issue(1, "The first", "The first task.", LONG_AGO);
    let org = Org::new("follow", std::slice::from_ref(&first));
    let id = org.propose(&[1], 2, 3, json!({}));
    wait("the read of the draft", || org.read_done(id));
    let draft = org.range(id);
    assert_eq!(draft["follow_ups"], json!("inside"), "{draft}");
    assert_eq!(draft["design_ahead"], json!(false), "{draft}");
    org.approve(id);
    wait("r1-1 dispatched", || org.run_status("r1-1").is_some());

    let later = "2099-01-01T00:00:00Z";
    org.set_issues(&[
        first,
        issue(
            3,
            "Found on the way",
            "Seen in #1.\n\nFollow-up of #1",
            later,
        ),
        issue(
            4,
            "Needs a host",
            "Seen in #1.\n\nFollow-up of #1\nNeeds host: example.org",
            later,
        ),
        issue(
            5,
            "Hits a stop line",
            "Seen in #1.\n\nFollow-up of #1\nStop line: irreversible",
            later,
        ),
        issue(
            6,
            "Older",
            "Seen before.\n\nFollow-up of #1",
            "2020-01-01T00:00:00Z",
        ),
    ]);
    wait("the follow-up #3 dispatched", || {
        !org.dispatched(3).is_empty()
    });
    let d = org.dispatched(3);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].1["task_id"], json!(format!("r{id}-3")), "{d:?}");
    assert_eq!(d[0].1["design_only"], json!(false), "{d:?}");
    wait("a read that lists #4", || org.candidate(id, 4).is_some());
    let c4 = org.candidate(id, 4).unwrap();
    assert_eq!(c4["admitted"], json!(false), "{c4}");
    assert_eq!(
        c4["left_out_why"],
        json!("follow_up (ask: needs host example.org)"),
        "{c4}"
    );
    let c5 = org
        .candidate(id, 5)
        .unwrap_or_else(|| panic!("no candidate 5: {}", org.range(id)));
    assert_eq!(c5["admitted"], json!(false), "{c5}");
    assert_eq!(
        c5["left_out_why"],
        json!("follow_up (ask: stop line irreversible)"),
        "{c5}"
    );
    assert!(
        org.candidate(id, 6).is_none(),
        "an Issue created before the approval is no follow-up: {}",
        org.range(id)
    );
    assert!(org.dispatched(4).is_empty() && org.dispatched(5).is_empty());
}
