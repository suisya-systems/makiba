//! Issue 567 (docs/design/queue-ready.md): a pull request whose own changes touch a check path
//! (`landing::CHECKS`: `tools/`, `.github/`, `tests/common/`, `scripts/acceptance-check.sh`) is
//! ready for the merge queue only when its head holds every landing since its base that added or
//! changed a test (a file under `tests/`); otherwise makibad updates it first (reason
//! `tests_landed`). The case is PR 534: it changed the acceptance checker on an old base, and a
//! later landing added a test that reaches the checker without naming it.
//!
//! - A check-changing head on a base older than a landed test is updated, not told it can be
//!   queued; its new head is.
//! - A head that changes no check is ready on the same landing (the rule stays narrow).
//! - A ready check-changing head is told it is no longer ready when a test lands, and updated.
//! - A `ready` record written before this rule (no `own_checks`) is judged again by it, also
//!   when the base has not moved since.
//!
//! The world is that of `tests/queue_ready.rs`, copied here: `makibad` runs in this process on a
//! state database of its own, per test, with one project whose repository sits under a temporary
//! repositories folder with a local bare `origin`, and one `review` code run whose worktree is the
//! run's own repository (`run_repo::create`) and whose branch is at origin. Landings are commits
//! pushed to origin's `main` from a clone of its own. `gh` is a stand-in script (`MAKIBA_GH`):
//! `gh pr view` answers `OPEN`, the head of the branch at origin and the merge state of a file per
//! pull request; every CI read answers passed and finished. No network. The tests run one at a
//! time (one lock), since they share the process's environment.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::prwatch;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
const READY: &str = "can be queued";

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<r> --json ...: OPEN, the head of the branch at origin
# (the files origin and branch of pr-<n> in the repository's folder) and the merge state of the
# file status there. gh api --paginate repos/<o>/<r>/...: the CI reads of a head, all passed and
# finished. Every call is logged whole, one line, in the repository's folder (calls).
all="$*"
repo=; num=
case "$1 $2" in
  "pr view")
    num=$3; shift 3
    while [ $# -gt 0 ]; do
      case "$1" in
        --repo) repo=${2#github.com/}; shift 2 ;;
        --json) shift 2 ;;
        *) echo "unexpected argument: $1" >&2; exit 2 ;;
      esac
    done ;;
  "api --paginate")
    path=$3
    rest=${path#repos/}
    owner=${rest%%/*}; rest=${rest#*/}
    repo="$owner/${rest%%/*}" ;;
  *) echo "unexpected call: $all" >&2; exit 2 ;;
esac
case "$repo" in ?*/?*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
dir="$FAKE_GH_DIR/${repo%%/*}_${repo#*/}"
mkdir -p "$dir"
echo "$all" >> "$dir/calls"
case "$all" in
  "pr view"*)
    f="$dir/pr-$num"
    if [ ! -e "$f/origin" ]; then echo "not found" >&2; exit 1; fi
    head=$(git --git-dir="$(cat "$f/origin")" rev-parse --verify "refs/heads/$(cat "$f/branch")") || exit 1
    printf '{"state":"OPEN","mergedAt":null,"mergedBy":null,"headRefOid":"%s","mergeCommit":null,"title":"T","mergeable":"MERGEABLE","mergeStateStatus":"%s","isDraft":false}\n' "$head" "$(cat "$f/status")" ;;
  *check-runs*) echo '{"name":"ci","status":"completed","conclusion":"success"}' ;;
  */status*) ;;
  *actions/runs*) echo completed ;;
  *) echo "unexpected call: $all" >&2; exit 2 ;;
esac
"#;

static HOME: OnceLock<PathBuf> = OnceLock::new();
static INIT: Once = Once::new();
/// One test at a time: each runs a makibad of its own in this process.
static LOCK: Mutex<()> = Mutex::new(());

/// The folder the tests share (the stand-in, its answers, the repositories and workers folders),
/// with the environment set once before any test starts a thread.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        for d in ["repos", "workers", "docs", "answers"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "50");
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        }
        makibad::process_knowledge();
        HOME.set(root).unwrap();
    });
    HOME.get().unwrap().clone()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A commit in `dir` writing `text` to `name` (folders made); its id.
fn commit_in(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", &format!("change {name}")]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Whether `a` is an ancestor of (or is) `b` in `dir`.
fn holds(dir: &Path, a: &str, b: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", a, b])
        .status()
        .unwrap()
        .success()
}

/// One project, its repository and origin, one `review` code run with its pull request recorded,
/// and (once started) a makibad of its own.
struct World {
    state: PathBuf,
    /// The project's repository (`$MAKIBA_REPOS_DIR/<slug>`).
    repo: PathBuf,
    origin: PathBuf,
    /// A clone of origin that lands commits on its `main`.
    author: PathBuf,
    worktree: PathBuf,
    task: String,
    branch: String,
    /// `o/<slug>`.
    name: String,
    pr: i64,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl World {
    /// The project `slug` with `main` at one commit holding `shared.txt` (`base`).
    fn new(slug: &str, pr: i64) -> World {
        let home = init();
        let top = home.join("worlds").join(slug);
        std::fs::create_dir_all(&top).unwrap();
        let repo = home.join("repos").join(slug);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        commit_in(&repo, "shared.txt", "base\n");
        let origin = top.join("origin.git");
        git(&top, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &origin,
            &["fetch", "-q", &repo.to_string_lossy(), "main:main"],
        );
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["fetch", "-q", "origin"]);
        let author = top.join("author");
        git(
            &top,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &author.to_string_lossy(),
            ],
        );
        let state = top.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let task = format!("{slug}-task");
        let branch = format!("makiba/{task}");
        let worktree = home.join("workers").join(slug).join(&task);
        StateWriter::open(&state.join("state.db")).unwrap();
        World {
            state,
            repo,
            origin,
            author,
            worktree,
            task,
            branch,
            name: format!("o/{slug}"),
            pr,
            stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
        }
    }

    /// A landing on origin's `main` writing `text` to `path`; its id.
    fn land(&self, path: &str, text: &str) -> String {
        git(&self.author, &["pull", "-q", "--ff-only", "origin", "main"]);
        let id = commit_in(&self.author, path, text);
        git(&self.author, &["push", "-q", "origin", "main"]);
        id
    }

    /// The run's worktree from the project's `main`, one commit writing `text` to `path`, its
    /// branch pushed to origin, the run `review` with its code launch, and the pull request
    /// recorded at that head with the stand-in reading `status`. The head.
    fn fork(&self, path: &str, text: &str, status: &str) -> String {
        let state = &self.state;
        makiba::daemon::run_repo::create(
            &self.repo,
            &makiba::daemon::run_repo::dir(&state.join("settings"), &self.task),
            &self.worktree,
            &self.branch,
            "main",
            &state.join("git-hooks"),
        )
        .unwrap();
        let head = commit_in(&self.worktree, path, text);
        git(
            &self.origin,
            &[
                "fetch",
                "-q",
                &self.worktree.to_string_lossy(),
                &format!("{}:refs/heads/{}", self.branch, self.branch),
            ],
        );
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        let run = w
            .upsert_run(&RunUpsert {
                task_id: &self.task,
                project_slug: self.name.split('/').nth(1).unwrap(),
                status: Some("review"),
                branch: Some(&self.branch),
                ..Default::default()
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, ?2, 'code', 'fake', 'the brief', ?3, 'verified')",
                rusqlite::params![
                    run,
                    format!("{}-s1", self.task),
                    self.worktree.to_string_lossy()
                ],
            )
            .unwrap();
        let answers = init()
            .join("answers")
            .join(self.name.replace('/', "_"))
            .join(format!("pr-{}", self.pr));
        std::fs::create_dir_all(&answers).unwrap();
        std::fs::write(
            answers.join("origin"),
            self.origin.to_string_lossy().as_bytes(),
        )
        .unwrap();
        std::fs::write(answers.join("branch"), &self.branch).unwrap();
        std::fs::write(answers.join("status"), status).unwrap();
        prwatch::register(&w, "pr_opened", &self.task, &self.name, self.pr, &head).unwrap();
        head
    }

    /// makibad started, once its first Resync is handled (`base_resynced`), so the door's writes
    /// cannot race its start writes (Issue 181).
    fn start(&mut self) {
        let (dir, flag) = (self.state.clone(), self.stop.clone());
        self.daemon = Some(std::thread::spawn(move || {
            makibad::run_until(&dir, &|| Box::new(Stub::new()), flag)
        }));
        let c = self.read();
        let until = Instant::now() + WAIT;
        while c
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'base_resynced'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            == 0
        {
            assert!(
                Instant::now() < until,
                "makibad did not handle its first Resync"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn read(&self) -> Connection {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    /// The payloads of every `queue_readiness`, oldest first.
    fn readiness(&self) -> Vec<Value> {
        self.payloads("queue_readiness")
    }

    /// The `queue_readiness` payloads of `head`.
    fn readiness_of(&self, head: &str) -> Vec<Value> {
        self.readiness()
            .into_iter()
            .filter(|p| p["head_oid"].as_str() == Some(head))
            .collect()
    }

    fn payloads(&self, kind: &str) -> Vec<Value> {
        let c = self.read();
        let mut q = c
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap().unwrap_or_default()).unwrap_or(Value::Null))
            .collect()
    }

    /// makibad's notices to the secretary, oldest first.
    fn notices(&self) -> Vec<String> {
        let c = self.read();
        let mut q = c
            .prepare(
                "SELECT body FROM messages WHERE kind = 'notice' AND sender = 'makibad'
                   AND recipient = 'secretary' ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The `update` commands: actor, status and args.
    fn updates(&self) -> Vec<(String, String, Value)> {
        let c = self.read();
        let mut q = c
            .prepare(
                "SELECT actor, status, args_json FROM commands WHERE verb = 'update' ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// The head of the branch at origin.
    fn origin_head(&self) -> String {
        git(
            &self.origin,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", self.branch),
            ],
        )
    }

    /// Wait until `done` holds, or panic with what makibad wrote.
    fn until(&self, what: &str, done: impl Fn(&World) -> bool) {
        let until = Instant::now() + WAIT;
        while !done(self) {
            assert!(
                Instant::now() < until,
                "{what} not seen after {WAIT:?}; queue_readiness: {:?}; updates: {:?}; notices: {:#?}",
                self.readiness(),
                self.updates(),
                self.notices()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(d) = self.daemon.take() {
            let _ = d.join();
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop();
    }
}

fn short(head: &str) -> &str {
    &head[..7]
}

/// Whether a `landings` list names `sha` with `path` among its paths.
fn names(d: &Value, sha: &str, path: &str) -> bool {
    d["landings"].as_array().is_some_and(|ls| {
        ls.iter().any(|l| {
            l["sha"].as_str() == Some(sha)
                && l["paths"]
                    .as_array()
                    .is_some_and(|p| p.iter().any(|x| x == path))
        })
    })
}

/// The update of a head, once finished: exactly one `update` command, of actor `makibad`, done,
/// for the run's task; origin's branch moved off `first` and holds `landing`. The new head.
fn updated_once(w: &World, first: &str, landing: &str) -> String {
    w.until("the update finished", |w| {
        !w.updates().is_empty() && w.updates().iter().all(|(_, s, _)| s != "accepted")
    });
    let updates = w.updates();
    assert_eq!(updates.len(), 1, "one update: {updates:?}");
    let (actor, status, args) = &updates[0];
    assert_eq!(actor, "makibad", "{updates:?}");
    assert_eq!(status, "done", "{updates:?}; notices: {:#?}", w.notices());
    assert_eq!(args["task"].as_str(), Some(w.task.as_str()), "{args}");
    let new = w.origin_head();
    assert_ne!(new, first, "origin's branch moved");
    assert!(
        holds(&w.origin, landing, &new),
        "origin's branch at {new} does not hold the landing {landing}"
    );
    new
}

/// The #534 shape: the branch changes the acceptance checker on base M0; then main lands a test
/// (no check path). The head is updated (`tests_landed`), never told it can be queued; the new
/// head, which holds the test, is.
#[test]
fn a_check_changing_head_on_an_old_base_is_updated_for_a_landed_test() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pd", 4);
    let m0 = git(&w.repo, &["rev-parse", "HEAD"]);
    let first = w.fork(
        "scripts/acceptance-check.sh",
        "#!/bin/sh\nexit 0\n",
        "CLEAN",
    );
    let l1 = w.land("tests/checker_case.rs", "// a case of the checker\n");
    w.start();

    w.until("a ready verdict of a new head", |w| {
        w.readiness()
            .iter()
            .any(|p| p["verdict"] == "ready" && p["head_oid"].as_str() != Some(&first))
    });
    let new = updated_once(&w, &first, &l1);
    let notices = w.notices();
    let decided = w.readiness_of(&first);
    let updated = w.payloads("branch_updated");
    let decided_new = w.readiness_of(&new);
    w.stop();

    assert_eq!(
        decided.len(),
        1,
        "one decision of the first head: {decided:?}"
    );
    let d = &decided[0];
    assert_eq!(d["verdict"], "update", "{d}");
    assert_eq!(d["reason"], "tests_landed", "{d}");
    assert_eq!(d["ci_base"].as_str(), Some(m0.as_str()), "{d}");
    assert_eq!(
        d["own_checks"],
        json!(["scripts/acceptance-check.sh"]),
        "{d}"
    );
    assert!(
        names(d, &l1, "tests/checker_case.rs"),
        "the landing L1 {l1} with tests/checker_case.rs is named: {d}"
    );

    assert_eq!(updated.len(), 1, "one branch_updated: {updated:?}");
    assert_eq!(
        updated[0]["new"].as_str(),
        Some(new.as_str()),
        "{updated:?}"
    );

    let updating = format!(
        "makibad is updating {}#{} at {}",
        w.name,
        w.pr,
        short(&first)
    );
    assert!(
        notices.iter().any(|n| n.contains(&updating)
            && n.contains("scripts/acceptance-check.sh")
            && n.contains("tests/checker_case.rs")),
        "an update notice naming the check and the test: {notices:#?}"
    );
    let first_ready = format!("{}#{} at {} {READY}", w.name, w.pr, short(&first));
    assert!(
        !notices.iter().any(|n| n.contains(&first_ready)),
        "the first head was told ready: {notices:#?}"
    );
    let new_ready = format!("{}#{} at {} {READY}", w.name, w.pr, short(&new));
    assert_eq!(
        notices.iter().filter(|n| n.contains(&new_ready)).count(),
        1,
        "the new head is told ready once: {notices:#?}"
    );
    assert!(
        decided_new.iter().any(|p| p["verdict"] == "ready"),
        "{decided_new:?}"
    );
}

/// The same history for a head that changes no check: a landed test does not count for it.
#[test]
fn a_head_that_changes_no_check_is_ready_on_the_same_landing() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pe", 5);
    let m0 = git(&w.repo, &["rev-parse", "HEAD"]);
    let head = w.fork("src/lib.rs", "pub fn f() {}\n", "CLEAN");
    w.land("tests/checker_case.rs", "// a case of the checker\n");
    w.start();

    w.until("a decision of the head", |w| {
        !w.readiness_of(&head).is_empty()
    });
    let decided = w.readiness_of(&head);
    let notices = w.notices();
    let updates = w.updates();
    let at = w.origin_head();
    w.stop();

    assert_eq!(decided.len(), 1, "one decision: {decided:?}");
    let d = &decided[0];
    assert_eq!(d["verdict"], "ready", "{d}");
    assert_eq!(d["ci_base"].as_str(), Some(m0.as_str()), "{d}");
    assert_eq!(
        d["landings"].as_array().map(Vec::len),
        Some(0),
        "no landing counts: {d}"
    );
    assert_eq!(d["own_checks"], json!([]), "{d}");
    let link = format!("https://github.com/{}/pull/{}", w.name, w.pr);
    assert_eq!(
        notices
            .iter()
            .filter(|n| n.contains(READY) && n.contains(&link))
            .count(),
        1,
        "one ready notice with the link: {notices:#?}"
    );
    assert!(updates.is_empty(), "{updates:?}");
    assert_eq!(at, head, "origin's branch moved");
}

/// A check-changing head judged ready (main moved only by docs) is told it is no longer ready
/// when a test lands, and updated.
#[test]
fn a_ready_check_changing_head_is_revoked_when_a_test_lands() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pf", 6);
    let head = w.fork("tools/check.py", "print('checked')\n", "CLEAN");
    w.land("docs/note.md", "a note\n");
    w.start();

    w.until("a decision of the head", |w| {
        !w.readiness_of(&head).is_empty()
    });
    let decided = w.readiness_of(&head);
    assert_eq!(decided.len(), 1, "one decision: {decided:?}");
    let d = &decided[0];
    assert_eq!(d["verdict"], "ready", "{d}");
    assert_eq!(d["own_checks"], json!(["tools/check.py"]), "{d}");
    assert_eq!(
        d["landings"].as_array().map(Vec::len),
        Some(0),
        "no landing counts: {d}"
    );
    let ready = format!("{}#{} at {} {READY}", w.name, w.pr, short(&head));
    assert_eq!(
        w.notices().iter().filter(|n| n.contains(&ready)).count(),
        1,
        "the head is told ready once: {:#?}",
        w.notices()
    );

    let l2 = w.land("tests/new_case.py", "def test_new():\n    assert True\n");
    w.until("a second decision of the head", |w| {
        w.readiness_of(&head).len() >= 2
    });
    let new = updated_once(&w, &head, &l2);
    let decided = w.readiness_of(&head);
    let notices = w.notices();
    w.stop();

    assert_eq!(decided.len(), 2, "two decisions of the head: {decided:?}");
    let d = &decided[1];
    assert_eq!(d["verdict"], "update", "{d}");
    assert_eq!(d["reason"], "tests_landed", "{d}");
    assert_eq!(d["watched"], true, "{d}");
    assert!(
        names(d, &l2, "tests/new_case.py"),
        "the landing L2 {l2} with tests/new_case.py is named: {d}"
    );
    let revoked = format!(
        "{}#{} at {} is no longer ready for the queue:",
        w.name,
        w.pr,
        short(&head)
    );
    assert!(
        notices.iter().any(|n| n.contains(&revoked)),
        "the head is told it is no longer ready: {notices:#?}"
    );
    assert!(holds(&w.origin, &l2, &new));
}

/// A `ready` record of the rule before Issue 567 (no `own_checks`), whose base tip is main's tip
/// now, of a check-changing head that lacks a landed test: makibad judges it again by the new
/// rule although the base has not moved.
#[test]
fn a_ready_record_of_the_old_rule_is_judged_again() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pg", 7);
    let m0 = git(&w.repo, &["rev-parse", "HEAD"]);
    let head = w.fork(
        "scripts/acceptance-check.sh",
        "#!/bin/sh\nexit 0\n",
        "CLEAN",
    );
    let l1 = w.land("tests/checker_case.rs", "// a case of the checker\n");
    {
        let sw = StateWriter::open(&w.state.join("state.db")).unwrap();
        let payload = json!({
            "repo": w.name, "pr": w.pr, "head_oid": head, "task_id": w.task, "base": "main",
            "base_tip": l1, "ci_base": m0, "merge_state": "clean", "verdict": "ready",
            "reason": null, "landings": [], "watched": false, "command": null,
        });
        sw.append_event(&NewEvent {
            kind: "queue_readiness",
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: Some(&w.task),
            ..Default::default()
        })
        .unwrap();
    }
    w.start();

    w.until("a second decision of the head", |w| {
        w.readiness_of(&head).len() >= 2
    });
    let new = updated_once(&w, &head, &l1);
    let decided = w.readiness_of(&head);
    w.stop();

    assert_eq!(decided.len(), 2, "two decisions of the head: {decided:?}");
    let d = &decided[1];
    assert_eq!(d["verdict"], "update", "{d}");
    assert_eq!(d["reason"], "tests_landed", "{d}");
    assert_eq!(d["watched"], true, "{d}");
    assert_eq!(
        d["own_checks"],
        json!(["scripts/acceptance-check.sh"]),
        "{d}"
    );
    assert!(
        names(d, &l1, "tests/checker_case.rs"),
        "the landing L1 {l1} with tests/checker_case.rs is named: {d}"
    );
    assert!(holds(&w.origin, &l1, &new));
}
