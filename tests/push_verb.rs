//! Issue 190: `push`, the secretary's verb that has `makibad` push a worker's branch.
//!
//! `makibad` runs in this process on the stand-in base; the door runs in this process too, on the
//! same `state.db`, and is called as a tool (`tool` true), as the secretary's MCP call is. The
//! project `p` is a git repository under a temporary repositories folder whose `main` is pushed
//! to a local bare `origin`: no network. Each test has its own run, worktree and state folder.
//!
//! The test writes `state.db` only before `makibad` starts and through the door afterwards; it
//! waits on the command row `makibad` finishes, read-only (Issue 181).

use makiba::base::stub::Stub;
use makiba::daemon;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@e"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The root of the organization's folders, made once per process.
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let repo = root.join("repos/p");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("file.txt"), "first\n").unwrap();
        git(&repo, &["add", "file.txt"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        }
        root
    })
}

fn origin() -> PathBuf {
    root().join("origin.git")
}

/// The commit `refs/heads/<branch>` names at the origin, if it exists there.
fn at_origin(branch: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(origin())
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A running `makibad`: its stop flag and its thread.
type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    task: String,
    branch: String,
    state: PathBuf,
    worktree: PathBuf,
    door: Door,
    secretary: String,
    worker: String,
    daemon: Option<Daemon>,
}

/// Stops the daemon when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        if let Some((stop, d)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = d.join();
        }
    }
}

/// An `in_use` code run of `task` with a `verified` launch whose worktree is on `makiba/<task>`,
/// made from `main` of the project's repository, and the keys of the secretary and the worker.
fn org(task: &str) -> Org {
    let root = root();
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(&state).unwrap();
    let branch = format!("makiba/{task}");
    let worktree = root.join("workers/p").join(task);
    let gitdir = makiba::daemon::run_repo::dir(&state.join("settings"), task);
    let hooks = state.join("git-hooks");
    makiba::daemon::run_repo::create(
        &root.join("repos/p"),
        &gitdir,
        &worktree,
        &branch,
        "main",
        &hooks,
    )
    .unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let run = w
        .upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("in_use"),
            branch: Some(&branch),
            ..Default::default()
        })
        .unwrap();
    let spawn = format!("{task}-s1");
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES (?1, ?2, 'code', 'fake', 'the brief', ?3, 'verified')",
            rusqlite::params![run, spawn, worktree.to_string_lossy()],
        )
        .unwrap();
    let worker = issue_key(
        &w,
        Role::Worker,
        &format!("w-{task}"),
        Some(run),
        Some(&spawn),
    )
    .unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    Org {
        task: task.into(),
        branch,
        state: state.clone(),
        worktree,
        door: Door::new(
            StateWriter::open(&state.join("state.db")).unwrap(),
            Arc::new(Stub::new()),
            "fake".into(),
        ),
        secretary,
        worker,
        daemon: None,
    }
}

impl Org {
    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, flag) = (self.state.clone(), stop.clone());
        let d =
            std::thread::spawn(move || daemon::run_until(&dir, &|| Box::new(Stub::new()), flag));
        self.daemon = Some((stop, d));
        // The first `Resync` handled: `base_resynced` is written last, after the view is rebuilt
        // (spec section 10), so the door's writes cannot race makibad's start writes (Issue 181).
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let up = self.conn().and_then(|c| {
                c.query_row(
                    "SELECT count(*) FROM events WHERE kind = 'base_resynced'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            });
            if up.is_ok_and(|n| n > 0) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("makibad did not handle its first Resync within {WAIT:?}");
    }

    fn conn(&self) -> rusqlite::Result<Connection> {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
    }

    /// `push` called as a tool, with `key`.
    fn push(&self, key: &str) -> Response {
        self.door
            .handle(Some(key), "push", json!({"task": self.task}), None, true)
    }

    /// A commit in the worktree; returns its id.
    fn commit(&self, name: &str) -> String {
        std::fs::write(self.worktree.join(name), name).unwrap();
        git(&self.worktree, &["add", name]);
        git(&self.worktree, &["commit", "-q", "-m", name]);
        git(&self.worktree, &["rev-parse", "HEAD"])
    }

    /// The status of this task's push command once `makibad` has finished it.
    fn finished(&self) -> String {
        let sql = "SELECT status FROM commands WHERE verb = 'push'
                   AND json_extract(args_json, '$.task') = ?1 ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let s: Option<String> = self
                .conn()
                .and_then(|c| c.query_row(sql, [&self.task], |r| r.get(0)).optional())
                .unwrap_or(None);
            if let Some(s) = s.filter(|s| s != "accepted") {
                return s;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the push of {} is not finished after {WAIT:?}", self.task);
    }

    /// The heads the `branch_pushed` events of this run carry.
    fn pushed_heads(&self) -> Vec<String> {
        let c = self.conn().unwrap();
        let mut q = c
            .prepare(
                "SELECT json_extract(e.payload_json, '$.head') FROM events e
                 JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'branch_pushed' AND r.task_id = ?1 ORDER BY e.id",
            )
            .unwrap();
        q.query_map([&self.task], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The bodies of `makibad`'s notices to the secretary about this run.
    fn notices(&self) -> Vec<String> {
        let c = self.conn().unwrap();
        let mut q = c
            .prepare(
                "SELECT m.body FROM messages m JOIN runs r ON r.id = m.run_id
                 WHERE m.kind = 'notice' AND m.sender = 'makibad'
                   AND m.recipient = 'secretary' AND r.task_id = ?1 ORDER BY m.id",
            )
            .unwrap();
        q.query_map([&self.task], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}

/// 1. The secretary's tool call: the branch arrives at the origin with the worktree's head, the
///    command ends `done`, `branch_pushed` carries the head and the secretary is told it.
#[test]
fn the_secretary_pushes_a_workers_branch_through_makibad() {
    let mut org = org("p1");
    let head = org.commit("one.txt");
    org.start();
    let r = org.push(&org.secretary);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(org.finished(), "done");
    assert_eq!(at_origin(&org.branch), Some(head.clone()));
    assert_eq!(org.pushed_heads(), std::slice::from_ref(&head));
    assert!(
        org.notices().iter().any(|n| n.contains(&head)),
        "no notice names the head {head}: {:?}",
        org.notices()
    );
}

/// 2. Uncommitted tracked changes, or a worktree on another branch: `failed`, a notice, and
///    nothing pushed.
#[test]
fn a_dirty_worktree_or_one_on_another_branch_is_not_pushed() {
    let mut dirty = org("p2");
    dirty.commit("two.txt");
    std::fs::write(dirty.worktree.join("file.txt"), "changed\n").unwrap();
    let mut other = org("p3");
    git(&other.worktree, &["switch", "-q", "-c", "elsewhere/p3"]);
    other.commit("three.txt");

    dirty.start();
    assert!(dirty.push(&dirty.secretary).is_ok());
    assert_eq!(dirty.finished(), "failed");
    assert_eq!(at_origin(&dirty.branch), None);
    assert!(dirty.pushed_heads().is_empty());
    assert!(!dirty.notices().is_empty(), "the secretary is not told");

    other.start();
    assert!(other.push(&other.secretary).is_ok());
    assert_eq!(other.finished(), "failed");
    assert_eq!(at_origin(&other.branch), None);
    assert_eq!(at_origin("elsewhere/p3"), None);
    assert!(other.pushed_heads().is_empty());
    assert!(!other.notices().is_empty(), "the secretary is not told");
}

/// 3. The branch at the origin moved to a commit the worktree does not hold: the push is not
///    forced, the command ends `failed` and the origin keeps its commit.
#[test]
fn a_branch_whose_origin_moved_is_not_forced() {
    let mut org = org("p4");
    // Somebody else's commit on the same branch name at the origin.
    let theirs = root().join("theirs-p4");
    git(
        root(),
        &[
            "clone",
            "-q",
            &origin().to_string_lossy(),
            &theirs.to_string_lossy(),
        ],
    );
    git(&theirs, &["switch", "-q", "-c", &org.branch]);
    std::fs::write(theirs.join("theirs.txt"), "theirs").unwrap();
    git(&theirs, &["add", "theirs.txt"]);
    git(&theirs, &["commit", "-q", "-m", "theirs"]);
    git(&theirs, &["push", "-q", "origin", &org.branch]);
    let moved = git(&theirs, &["rev-parse", "HEAD"]);

    org.commit("four.txt");
    org.start();
    assert!(org.push(&org.secretary).is_ok());
    assert_eq!(org.finished(), "failed");
    assert_eq!(at_origin(&org.branch), Some(moved));
    assert!(org.pushed_heads().is_empty());
}

/// 4. A worker's key cannot call it, and nothing is pushed.
#[test]
fn a_worker_may_not_push() {
    let org = org("p5");
    org.commit("five.txt");
    let e = org.push(&org.worker).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
    assert_eq!(at_origin(&org.branch), None);
}
