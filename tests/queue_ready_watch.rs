//! Issue 475 (docs/design/queue-ready.md): a head makibad told ready for the merge queue is
//! watched, and a landing that changes what the checks check revokes it: makibad updates the
//! branch itself, records the new head and judges that one. This holds while makibad runs and for
//! a landing made while it was down (diff review round 7).
//!
//! The world is the one of `tests/queue_ready.rs` (its fixture, repeated here since a declared test
//! cannot change after its test commit): makibad in this process on a state database of its own,
//! one project with a local bare `origin`, one `review` code run, a stand-in for the GitHub CLI
//! that reads the branch's head at origin and answers every CI read passed.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::prwatch;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
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

    /// makibad stopped, `between` run while it is down, and makibad started again on the same
    /// state.
    fn restart(&mut self, between: impl FnOnce(&World)) {
        self.stop();
        between(self);
        self.stop = Arc::new(AtomicBool::new(false));
        self.start();
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

/// What a head told ready and then revoked by a landing under `tools/` must show: the second
/// decision is `update` (watched, `checks_changed`, naming the landing), makibad's update ran, the
/// new head is recorded and watched and is told ready once, and the old head is told ready once.
fn revoked(w: &World, first: &str, landing: &str) {
    let decided = w.readiness_of(first);
    assert_eq!(
        decided.len(),
        2,
        "two decisions of the first head: {decided:?}"
    );
    assert_eq!(decided[0]["verdict"], "ready", "{decided:?}");
    let d = &decided[1];
    assert_eq!(d["verdict"], "update", "{d}");
    assert_eq!(d["reason"], "checks_changed", "{d}");
    assert_eq!(d["watched"], true, "{d}");
    assert!(
        d["landings"]
            .as_array()
            .is_some_and(|l| l.iter().any(|x| x["sha"].as_str() == Some(landing))),
        "the landing {landing} is named: {d}"
    );
    let updates = w.updates();
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0].0, "makibad", "{updates:?}");
    assert_eq!(
        updates[0].1,
        "done",
        "{updates:?}; notices: {:#?}",
        w.notices()
    );
    let new = w.origin_head();
    assert_ne!(new, first);
    assert!(
        holds(&w.origin, landing, &new),
        "origin's branch lacks the landing"
    );
    assert!(
        w.payloads("fix_pushed")
            .iter()
            .any(|p| p["head_oid"].as_str() == Some(new.as_str())),
        "the new head {new} is recorded"
    );
    let notices = w.notices();
    let told = |h: &str| {
        let s = format!("{}#{} at {} {READY}", w.name, w.pr, short(h));
        notices.iter().filter(|n| n.contains(&s)).count()
    };
    assert_eq!(told(first), 1, "{notices:#?}");
    assert_eq!(told(&new), 1, "{notices:#?}");
    assert!(
        notices.iter().any(|n| n.contains(&format!(
            "{}#{} at {} is no longer ready for the queue",
            w.name,
            w.pr,
            short(first)
        ))),
        "the revocation is told: {notices:#?}"
    );
    assert!(
        w.readiness_of(&new).iter().any(|p| p["verdict"] == "ready"),
        "{:?}",
        w.readiness_of(&new)
    );
}

/// A ready head, then a landing that adds `tools/check.py` while makibad runs.
#[test]
fn a_ready_head_is_updated_when_a_landing_under_tools_arrives() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pd", 4);
    let first = w.fork("d.txt", "d\n", "CLEAN");
    w.start();
    w.until("the first head told ready", |w| {
        w.readiness_of(&first)
            .iter()
            .any(|p| p["verdict"] == "ready")
    });
    let landing = w.land("tools/check.py", "print('checked')\n");
    w.until("a ready verdict of a new head", |w| {
        w.readiness()
            .iter()
            .any(|p| p["verdict"] == "ready" && p["head_oid"].as_str() != Some(&first))
    });
    std::thread::sleep(TEN_INTERVALS);
    revoked(&w, &first, &landing);
    w.stop();
}

/// A ready head, then a landing that adds `tools/check.py` while makibad is down: the watch
/// restarted from the ledger reads it.
#[test]
fn a_ready_head_is_updated_for_a_landing_made_while_makibad_was_down() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pe", 5);
    let first = w.fork("e.txt", "e\n", "CLEAN");
    w.start();
    w.until("the first head told ready", |w| {
        w.readiness_of(&first)
            .iter()
            .any(|p| p["verdict"] == "ready")
    });
    let mut landing = String::new();
    w.restart(|w| landing = w.land("tools/check.py", "print('checked')\n"));
    w.until("a ready verdict of a new head", |w| {
        w.readiness()
            .iter()
            .any(|p| p["verdict"] == "ready" && p["head_oid"].as_str() != Some(&first))
    });
    std::thread::sleep(TEN_INTERVALS);
    revoked(&w, &first, &landing);
    w.stop();
}
