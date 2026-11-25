//! Issue 475 (docs/design/queue-ready.md): makibad judges when a pull request is ready for the
//! merge queue, and updates it when it is not.
//!
//! - A head whose CI passed is ready when it does not conflict with its base and no landing since
//!   the base it holds (`git merge-base`) changed what the checks check (`tests/common/`, `tools/`,
//!   the CI's jobs). The decision is one `queue_readiness` event; a ready head gets one notice that
//!   it can be queued, with its link.
//! - A head that conflicts, or whose base lacks such a landing, is updated by makibad itself (the
//!   `update` job, as actor `makibad`); the new head is recorded and watched.
//! - A move of the base that changes none of those paths writes nothing and updates nothing.
//! - The `merge` verb refuses a head makibad judged not ready.
//!
//! The world: `makibad` runs in this process on a state database of its own, per test, with one
//! project whose repository sits under a temporary repositories folder with a local bare `origin`,
//! and one `review` code run whose worktree is the run's own repository (`run_repo::create`) and
//! whose branch is at origin. Landings are commits pushed to origin's `main` from a clone of its
//! own. `gh` is a stand-in script (`MAKIBA_GH`): `gh pr view` answers `OPEN`, the head of the
//! branch at origin and the merge state of a file per pull request; every CI read answers passed
//! and finished. No network. The tests run one at a time (one lock), since they share the
//! process's environment.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::prwatch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
/// Ten intervals of the PR watch (`MAKIBA_PRWATCH_INTERVAL_MS`).
const TEN_INTERVALS: Duration = Duration::from_millis(500);
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
    secretary: String,
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
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
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
            secretary,
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

    /// The project's repository's `main` brought to origin's.
    fn sync_main(&self) {
        git(&self.repo, &["fetch", "-q", "origin"]);
        git(&self.repo, &["merge", "-q", "--ff-only", "origin/main"]);
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

/// The design's case 1: the head was green on a base before a landing that changed `tools/`, so it
/// is updated and not reported ready; its new head, which holds that landing, is.
#[test]
fn a_head_green_before_a_landing_under_tools_is_updated_not_ready() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pa", 1);
    let m0 = git(&w.repo, &["rev-parse", "HEAD"]);
    let first = w.fork("a.txt", "a\n", "CLEAN");
    let l1 = w.land("tools/check.py", "print('checked')\n");
    w.start();

    w.until("a ready verdict of a new head", |w| {
        w.readiness()
            .iter()
            .any(|p| p["verdict"] == "ready" && p["head_oid"].as_str() != Some(&first))
    });
    w.until("the update finished", |w| {
        w.updates().iter().all(|(_, s, _)| s != "accepted")
    });
    let new = w.origin_head();
    let notices = w.notices();
    let decided = w.readiness_of(&first);
    let updates = w.updates();
    let updated = w.payloads("branch_updated");
    let pushed = w.payloads("fix_pushed");
    let watches: i64 = w
        .read()
        .query_row(
            "SELECT count(*) FROM pr_watches WHERE repo = ?1 AND pr = ?2 AND head_oid = ?3",
            rusqlite::params![w.name, w.pr, new],
            |r| r.get(0),
        )
        .unwrap();
    let decided_new = w.readiness_of(&new);
    w.stop();

    assert_eq!(
        decided.len(),
        1,
        "one decision of the first head: {decided:?}"
    );
    let d = &decided[0];
    assert_eq!(d["verdict"], "update", "{d}");
    assert_eq!(d["reason"], "checks_changed", "{d}");
    assert_eq!(d["ci_base"].as_str(), Some(m0.as_str()), "{d}");
    let landings = d["landings"].as_array().expect("landings is a list");
    assert!(
        landings
            .iter()
            .any(|l| l["sha"].as_str() == Some(l1.as_str())
                && l["paths"]
                    .as_array()
                    .is_some_and(|p| p.iter().any(|x| x == "tools/check.py"))),
        "the landing L1 {l1} with tools/check.py is named: {d}"
    );

    assert_eq!(updates.len(), 1, "one update: {updates:?}");
    let (actor, status, args) = &updates[0];
    assert_eq!(actor, "makibad", "{updates:?}");
    assert_eq!(status, "done", "{updates:?}; notices: {notices:#?}");
    assert_eq!(args["task"].as_str(), Some(w.task.as_str()), "{args}");

    assert_eq!(updated.len(), 1, "one branch_updated: {updated:?}");
    assert_eq!(
        updated[0]["new"].as_str(),
        Some(new.as_str()),
        "{updated:?}"
    );
    assert_ne!(new, first, "origin's branch moved");
    assert!(
        holds(&w.origin, &l1, &new),
        "origin's branch at {new} does not hold L1 {l1}"
    );

    assert!(
        pushed
            .iter()
            .any(|p| p["head_oid"].as_str() == Some(new.as_str())),
        "the new head {new} is recorded: {pushed:?}"
    );
    assert_eq!(watches, 1, "the new head {new} is watched");

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

/// The design's case 2: the head holds the landing that changed `tools/`, and main moved since only
/// by landings under `tests/` (not `tests/common/`): it is reported ready once and never updated,
/// also after main moves again.
#[test]
fn a_head_green_after_a_tools_landing_with_main_moved_under_tests_only_is_ready_once() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pb", 2);
    let l1 = w.land("tools/check.py", "print('checked')\n");
    w.sync_main();
    let head = w.fork("b.txt", "b\n", "CLEAN");
    w.land("tests/other.rs", "// other\n");
    w.start();

    w.until("a decision of the head", |w| {
        !w.readiness_of(&head).is_empty()
    });
    std::thread::sleep(TEN_INTERVALS);
    let link = format!("https://github.com/{}/pull/{}", w.name, w.pr);
    let check = |w: &World, when: &str| {
        let decided = w.readiness_of(&head);
        let notices = w.notices();
        assert_eq!(decided.len(), 1, "{when}: one decision: {decided:?}");
        let d = &decided[0];
        assert_eq!(d["verdict"], "ready", "{when}: {d}");
        assert_eq!(d["ci_base"].as_str(), Some(l1.as_str()), "{when}: {d}");
        assert_eq!(
            d["landings"].as_array().map(Vec::len),
            Some(0),
            "{when}: no landing counts: {d}"
        );
        assert_eq!(
            notices
                .iter()
                .filter(|n| n.contains(READY) && n.contains(&link))
                .count(),
            1,
            "{when}: one ready notice with the link: {notices:#?}"
        );
        assert!(w.updates().is_empty(), "{when}: {:?}", w.updates());
        assert_eq!(w.origin_head(), head, "{when}: origin's branch moved");
    };
    check(&w, "after the first judgement");

    let l3 = w.land("tests/more.rs", "// more\n");
    let base_ref = format!("refs/makiba/ready/{}/base", w.task);
    w.until("the base read again at L3", |w| {
        Command::new("git")
            .arg("-C")
            .arg(&w.repo)
            .args(["rev-parse", "--verify", "-q", &base_ref])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == l3)
    });
    std::thread::sleep(TEN_INTERVALS);
    check(&w, "after main moved under tests/ again");
    w.stop();
}

/// The design's case 3: a head that conflicts with its base is updated by makibad; the merge
/// cannot resolve the conflict, so the secretary is told, as for any update; and the merge verb
/// refuses that head.
#[test]
fn a_conflicting_head_is_updated() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pc", 3);
    let head = w.fork("shared.txt", "branch\n", "DIRTY");
    w.land("shared.txt", "main\n");
    w.start();

    w.until("a finished update by makibad", |w| {
        w.updates()
            .iter()
            .any(|(a, s, _)| a == "makibad" && s != "accepted")
    });
    std::thread::sleep(TEN_INTERVALS);
    let decided = w.readiness_of(&head);
    let updates = w.updates();
    let notices = w.notices();

    let door = Door::new(
        StateWriter::open(&w.state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );
    let answer = door.handle(
        Some(&w.secretary),
        "merge",
        json!({"repo": w.name, "pr": w.pr, "head": head}),
        None,
        true,
    );
    let attempts = w.payloads("merge_attempted");
    w.stop();

    assert_eq!(decided.len(), 1, "one decision: {decided:?}");
    assert_eq!(decided[0]["verdict"], "update", "{decided:?}");
    assert_eq!(decided[0]["reason"], "conflict", "{decided:?}");
    assert_eq!(updates.len(), 1, "one update: {updates:?}");
    assert_eq!(updates[0].0, "makibad", "{updates:?}");
    assert_eq!(updates[0].1, "failed", "{updates:?}");
    assert!(
        notices
            .iter()
            .any(|n| n.contains("conflicts in shared.txt")),
        "the conflict is told: {notices:#?}"
    );
    assert!(
        !notices.iter().any(|n| n.contains(READY)),
        "told ready: {notices:#?}"
    );

    let e = answer.expect_err("the merge of a head not ready is refused");
    assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
    assert!(e.message.contains("not ready for the queue"), "{e:?}");
    assert!(attempts.is_empty(), "{attempts:?}");
}
