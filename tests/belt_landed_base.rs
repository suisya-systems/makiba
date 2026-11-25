//! Issue 490: a range item admitted after its dependency landed starts on a base holding that
//! landing (docs/design/belt.md section 11.4).
//!
//! A ready candidate whose `Depends on #<n>` Issue is closed is held while the dependency's run
//! has a pull request the PR watch follows and whose merge is not yet seen in the project's clone:
//! first with no merge recorded, then with a merge recorded that the clone's origin does not hold
//! yet (the range stays active, though none of its runs is open). Once the belt's clone check has
//! fetched the merge (`landed_in_clone`), the candidate is dispatched, its `belt_dispatched` names
//! the landing, and its worktree's HEAD holds the merge commit, without the verb `project` ever
//! moving the clone's own default branch.
//!
//! The harness is that of tests/belt_ahead.rs, kept inside this file: the daemon runs in this
//! process on `Local` with the test agent, the door is called with the person's, the secretary's
//! and a worker's keys, and the GitHub CLI is a stand-in script (`MAKIBA_GH`) that answers from
//! files in a folder per repository, logs every call and refuses anything else, so no test reaches
//! the network. The project has a real clone whose `origin` is a local bare repository.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::Base;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
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
/// The conversation id the test agent names in every workplace of this project.
const CONVERSATION: &str = "conv-based";
/// `MAKIBA_LANDING_RETRY_MS` of this file.
const LANDING_RETRY_MS: u64 = 300;

const STAND_IN: &str = r#"#!/bin/sh
# <cli> issue|pr list --repo github.com/<o>/<n> --state <s> ...: the file <what>-<state>.json, else [].
# <cli> issue|pr view <n> --repo [github.com/]<o>/<n> ...: the file <what>-<n>.json, else a failure.
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

/// The folder the test uses (the stand-in and its answers, the repositories, the workplaces) and
/// the environment, set once before any thread of the test starts.
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
            std::env::set_var("MAKIBA_LANDING_RETRY_MS", LANDING_RETRY_MS.to_string());
            std::env::set_var("MAKIBA_PRWATCH_BUDGET_MS", "1000");
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "200");
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_status(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_status(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
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
        .unwrap()
}

/// An open Issue as the CLI's `issue list` gives it, created at `created`.
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
            _end_all: EndAll(local),
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

    /// The stand-in's answer to a view of an Issue or pull request (`what` `issue` or `pr`).
    fn answer_view(&self, what: &str, number: i64, v: Value) {
        std::fs::write(
            self.answers.join(format!("{what}-{number}.json")),
            v.to_string(),
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

    /// (event id, payload) of the events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<(i64, Value)> {
        let c = self.conn();
        let mut q = c
            .prepare("SELECT id, payload_json FROM events WHERE kind = ?1 ORDER BY id")
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

    /// (event id, payload) of the events of `kind` whose `task_id` is `task`.
    fn events_of(&self, kind: &str, task: &str) -> Vec<(i64, Value)> {
        self.events(kind)
            .into_iter()
            .filter(|(_, p)| p["task_id"] == json!(task))
            .collect()
    }

    /// The complete `range_read` events of range `id`.
    fn complete_reads(&self, id: i64) -> usize {
        self.events("range_read")
            .iter()
            .filter(|(_, p)| p["range_id"] == json!(id) && p["complete"] == json!(true))
            .count()
    }

    /// (stage, worktree) of every launch of `task`, oldest first.
    fn launches(&self, task: &str) -> Vec<(String, String)> {
        let c = self.conn();
        let mut q = c
            .prepare(
                "SELECT l.stage, l.worktree FROM run_launch l
                 JOIN runs r ON r.id = l.run_id WHERE r.task_id = ?1 ORDER BY l.id",
            )
            .unwrap();
        q.query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// `task` is at work, its newest launch verified.
    fn at_work(&self, task: &str) {
        wait(&format!("{task} at work"), || {
            self.run_status(task).as_deref() == Some("in_use")
                && self
                    .launches(task)
                    .last()
                    .is_some_and(|l| l.0 == "verified")
        });
    }

    /// The range `id` on the person's read surface.
    fn range(&self, id: i64) -> Value {
        let s = self.call(&self.person, "status", json!({}));
        s["ranges"]
            .as_array()
            .unwrap_or_else(|| panic!("status has no ranges[]: {s}"))
            .iter()
            .find(|r| r["id"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("range {id} not listed: {s}"))
    }

    /// The candidate `issue` of the newest read of range `id`.
    fn candidate(&self, id: i64, issue: i64) -> Value {
        self.range(id)["candidates"]
            .as_array()
            .unwrap_or_else(|| panic!("no candidates[] in range {id}"))
            .iter()
            .find(|c| c["issue"] == json!(issue))
            .cloned()
            .unwrap_or_else(|| panic!("no candidate {issue} in range {id}"))
    }

    fn range_status(&self, id: i64) -> String {
        self.conn()
            .query_row("SELECT status FROM ranges WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// The range's runs that are not closed.
    fn open_runs(&self, id: i64) -> i64 {
        self.conn()
            .query_row(
                "SELECT count(*) FROM runs WHERE range_id = ?1
                   AND status NOT IN ('completed', 'abandoned', 'failed')",
                [id],
                |r| r.get(0),
            )
            .unwrap()
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
        wait("the read of the draft", || self.complete_reads(id) > 0);
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

    /// The PR watch's state reads of pull request `pr` so far.
    fn state_reads(&self, pr: i64) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.starts_with(&format!("pr view {pr} ")) && c.contains("mergedAt"))
            .count()
    }

    /// A clone of the bare origin, for making the merge that lands.
    fn landing_work(&self) -> PathBuf {
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
        work
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

/// A range with A (#1) and B (#2, `Depends on #1`): A's pull request lands on the origin, the
/// belt admits B, and B's worktree HEAD holds A's merge commit, with no `project` call.
#[test]
fn a_candidate_admitted_after_its_dependency_landed_starts_on_a_base_with_that_merge() {
    let org = Org::new(
        "based",
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
    let old = git(&org.repo, &["rev-parse", "refs/heads/main"]);
    let id = org.propose(&[1, 2], 2, 2, json!({"design_ahead": false}));
    org.approve(id);
    let task1 = format!("r{id}-1");
    let task2 = format!("r{id}-2");

    // 1. A goes in; B waits for it.
    wait("A dispatched", || org.run_status(&task1).is_some());
    let c2 = org.candidate(id, 2);
    assert_eq!(c2["admitted"], json!(false), "{c2}");
    assert_eq!(c2["left_out_why"], json!("waits_for #1"), "{c2}");
    assert!(org.run_status(&task2).is_none(), "B went in too early");

    // 2. A's pull request is registered and A closes completed. Issue 1 reads closed while the
    //    pull request is still open: B is admitted but held, as no merge is recorded.
    org.at_work(&task1);
    org.call(
        &org.secretary,
        "pr",
        json!({"state": "opened", "repo": "o/based", "pr": 11, "head": "a".repeat(40),
               "task": task1}),
    );
    org.answer_view(
        "pr",
        11,
        json!({"number": 11, "state": "OPEN", "title": "The base", "mergedAt": null,
               "headRefOid": "a".repeat(40), "isDraft": false,
               "mergeStateStatus": "BLOCKED", "mergeCommit": null}),
    );
    org.answer_view(
        "issue",
        1,
        json!({"number": 1, "title": "The base", "state": "CLOSED"}),
    );
    org.set_issues(&[issue(
        2,
        "On top",
        "Builds on the base.\n\nDepends on #1",
        LONG_AGO,
    )]);
    let worker1 = org.worker(&task1);
    org.call(
        &worker1,
        "report",
        json!({"state": "done", "task": task1, "text": "Done; ran the tests."}),
    );
    org.call(&org.secretary, "close", json!({"task_id": task1}));
    assert_eq!(org.run_status(&task1).as_deref(), Some("completed"));
    let reads = org.complete_reads(id);
    let watched = org.state_reads(11);
    wait("two complete reads after A's close", || {
        org.complete_reads(id) >= reads + 2
    });
    wait("a state read of pull request 11 after A's close", || {
        org.state_reads(11) > watched
    });
    assert!(
        org.run_status(&task2).is_none(),
        "B went in while A's pull request was still open"
    );
    let c2 = org.candidate(id, 2);
    assert_eq!(c2["admitted"], json!(true), "{c2}");
    assert_eq!(org.range_status(id), "active");
    assert!(org.events_of("pr_merged", &task1).is_empty());

    // 3. A's pull request reads merged with a merge commit the origin does not hold yet: B is
    //    still held, and the range, with none of its runs open, stays active.
    let work = org.landing_work();
    std::fs::write(work.join("landed.txt"), "the base landed").unwrap();
    git(&work, &["add", "landed.txt"]);
    git(&work, &["commit", "-q", "-m", "the base landed"]);
    let merge = git(&work, &["rev-parse", "HEAD"]);
    org.answer_view(
        "pr",
        11,
        json!({"number": 11, "state": "MERGED", "mergedAt": "2026-11-25T00:00:22Z",
               "title": "The base", "headRefOid": "a".repeat(40),
               "mergeCommit": {"oid": merge}}),
    );
    wait("A's merge recorded", || {
        !org.events_of("pr_merged", &task1).is_empty()
    });
    let merged = org.events_of("pr_merged", &task1);
    assert_eq!(merged.len(), 1, "{merged:?}");
    assert_eq!(merged[0].1["merge_commit"], json!(merge), "{merged:?}");
    std::thread::sleep(Duration::from_millis(5 * LANDING_RETRY_MS + 500));
    assert!(
        org.run_status(&task2).is_none(),
        "B went in before the clone held A's merge"
    );
    assert_eq!(org.open_runs(id), 0);
    assert_eq!(org.range_status(id), "active");
    assert!(org.events_of("landed_in_clone", &task1).is_empty());

    // 4. The merge lands on the origin: the belt's fetch sees it, B goes in, and its worktree
    //    starts on a base holding the merge.
    git(&work, &["push", "-q", "origin", "main"]);
    wait("B dispatched", || org.run_status(&task2).is_some());
    wait("B's worktree made", || {
        org.launches(&task2)
            .first()
            .is_some_and(|l| l.0 != "planned")
    });
    let launch = org.launches(&task2)[0].clone();
    assert_ne!(launch.0, "failed", "B's launch failed: {launch:?}");

    let dispatched: Vec<(i64, Value)> = org
        .events("belt_dispatched")
        .into_iter()
        .filter(|(_, p)| p["issue"] == json!(2))
        .collect();
    assert_eq!(dispatched.len(), 1, "{dispatched:?}");
    let (dispatched_id, d) = &dispatched[0];
    assert_eq!(d["task_id"], json!(task2), "{d}");
    assert_eq!(d["design_only"], json!(false), "{d}");
    assert_eq!(
        d["landed"],
        json!([{"issue": 1, "task_id": task1, "merge_commit": merge}]),
        "{d}"
    );

    let workplace = PathBuf::from(&launch.1);
    let held = git_status(&workplace, &["merge-base", "--is-ancestor", &merge, "HEAD"]);
    assert!(
        held.status.success(),
        "B's worktree HEAD {} does not hold A's merge {merge}: {}",
        git(&workplace, &["rev-parse", "HEAD"]),
        String::from_utf8_lossy(&held.stderr)
    );

    let landed = org.events_of("landed_in_clone", &task1);
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert_eq!(landed[0].1["merge_commit"], json!(merge), "{landed:?}");
    assert!(
        landed[0].0 < *dispatched_id,
        "landed_in_clone {} came after B's belt_dispatched {dispatched_id}",
        landed[0].0
    );

    let project_calls: i64 = org
        .conn()
        .query_row(
            "SELECT count(*) FROM commands WHERE verb = 'project'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(project_calls, 0, "the verb project was called");
    assert_eq!(
        git(&org.repo, &["rev-parse", "refs/heads/main"]),
        old,
        "the clone's own main moved"
    );
}
