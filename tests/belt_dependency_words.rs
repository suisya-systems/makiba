//! Issue 299: a dependency written in words is not read as no dependency.
//!
//! On 2026-11-22 a range admitted two Issues whose bodies said "Depends on the belt's step 2."
//! and "Depends on the belt's step 3.": a dependency line naming no Issue number was read as no
//! dependency. The range's read now leaves such a candidate out with `dependency_unreadable`,
//! shown in `candidates[].left_out_why` and in what the approval showed; an approved range holding
//! only such a candidate stays active and dispatches nothing; once the line reads `Depends on #<n>`
//! and that Issue is closed, the candidate is admitted and dispatched.
//!
//! The scaffolding is that of `tests/belt.rs`: the daemon runs in this process on the stand-in
//! base, `gh` is a stand-in script answering from files, and no test reaches the network.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
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
            // An active range is read again every 300 ms, so the test sees reads after approval.
            std::env::set_var("MAKIBA_RANGE_READ_INTERVAL_MS", "300");
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
        let range = self.range(id);
        // The mode the read surface shows is the one the approval binds (Issue 326).
        let (read, mode) = (range["read_id"].clone(), range["approves_under"].clone());
        self.call(
            &self.person,
            "range",
            json!({"action": "approve", "id": id, "read": read, "approval_mode": mode}),
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

/// The open Issues the stand-in answers for `org`, replaced.
fn answer_open(org: &Org, issues: &[Value]) {
    std::fs::write(
        org.answers.join("issue-open.json"),
        Value::Array(issues.to_vec()).to_string(),
    )
    .unwrap();
}

fn candidate(range: &Value, n: i64) -> Value {
    range["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("no candidates[]: {range}"))
        .iter()
        .find(|c| c["issue"] == json!(n))
        .cloned()
        .unwrap_or_else(|| panic!("no candidate {n}: {range}"))
}

fn unreadable(c: &Value) -> bool {
    c["admitted"] == json!(false)
        && c["left_out_why"]
            .as_str()
            .is_some_and(|w| w.starts_with("dependency_unreadable"))
}

/// A candidate whose body says "Depends on the belt's step 2." is left out with
/// `dependency_unreadable` in the draft and in what the approval showed; approved, the range stays
/// active and dispatches nothing; once the line reads `Depends on #30` and #30 is closed, it is
/// admitted and dispatched.
#[test]
fn a_dependency_written_in_words_is_left_out_until_it_names_a_closed_issue() {
    let org = Org::new(
        "words",
        &[issue(
            31,
            "Step three",
            "Step three.\n\nDepends on the belt's step 2.\n",
            "-",
        )],
        true,
    );
    std::fs::write(
        org.answers.join("issue-30.json"),
        json!({"number": 30, "title": "Step two", "state": "CLOSED"}).to_string(),
    )
    .unwrap();
    let id = org.propose(&[31], 1);
    wait("the read of the draft", || org.read_done(id));
    let draft = org.range(id);
    let c = candidate(&draft, 31);
    assert!(unreadable(&c), "{draft}");
    assert!(
        c["left_out_why"]
            .as_str()
            .is_some_and(|w| w.contains("Depends on the belt's step 2.")),
        "the reason names the line: {draft}"
    );
    assert!(org.tasks().is_empty(), "a draft dispatches nothing");

    // Approved while the line is still words: what the approval showed names the reason.
    org.approve(id);
    let approved = org.events("range_approved");
    assert_eq!(approved.len(), 1, "{approved:?}");
    let shown = approved[0].1["shown"]
        .as_array()
        .unwrap_or_else(|| panic!("no shown[]: {approved:?}"))
        .iter()
        .find(|c| c["issue"] == json!(31))
        .cloned()
        .unwrap_or_else(|| panic!("31 not shown: {approved:?}"));
    assert!(unreadable(&shown), "{shown}");

    // Reads after the approval: still left out, the range stays active, nothing is dispatched.
    wait("reads after the approval", || {
        org.events("range_read")
            .iter()
            .filter(|(_, p)| p["range_id"] == json!(id) && p["complete"] == json!(true))
            .count()
            >= 3
    });
    let active = org.range(id);
    assert_eq!(active["status"], json!("active"), "{active}");
    assert!(unreadable(&candidate(&active, 31)), "{active}");
    assert!(org.tasks().is_empty(), "{:?}", org.tasks());

    // The line now names a closed Issue: admitted and dispatched by makibad's next read.
    answer_open(
        &org,
        &[issue(
            31,
            "Step three",
            "Step three.\n\nDepends on #30.\n",
            "-",
        )],
    );
    let task = format!("r{id}-31");
    wait("the task of 31", || org.run_exists(&task));
    let after = org.range(id);
    assert_eq!(candidate(&after, 31)["admitted"], json!(true), "{after}");
}
