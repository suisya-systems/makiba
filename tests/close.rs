//! M5-2 (Issue 37): close ends the terminal and removes the workplace.
//!
//! `makibad` runs in this process on `Local` (tests/common/local.rs); the test keeps a clone of the
//! base, so its terminals outlive a daemon that dies. The door runs in this process too, on the
//! same `state.db`, with the person's key. `close` answers `accepted` and the daemon's close pass
//! finishes it: the terminals of the run ended, its worker keys revoked, the workplace removed (a
//! `document` folder keeps `out/`), the per-task settings file deleted, the command `done`.
//!
//! The workers, documents and repositories folders are one temporary root per test process
//! (`MAKIBA_WORKERS_DIR`, `MAKIBA_DOCS_DIR`, `MAKIBA_REPOS_DIR`); the project `p` is a git
//! repository there whose `main` is pushed to a bare `origin`, so "unpushed" has a remote to be
//! measured against.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, LaunchSpec};
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::collections::BTreeMap;
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
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
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

fn repo() -> PathBuf {
    root().join("repos/p")
}

/// A running `makibad`: its stop flag and its thread.
type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    task: String,
    state: PathBuf,
    workplace: PathBuf,
    base: Local,
    door: Door,
    person: String,
    daemon: Option<Daemon>,
}

/// Stops the daemon and ends every terminal when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        if let Some((stop, d)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = d.join();
        }
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

/// A run of `task` with a `verified` launch, its key, its terminal, its workplace and its
/// settings file, and the worker's `report done`. `workplace` overrides where the launch's row
/// says the workplace is.
fn org(task: &str, kind: &str, workplace: Option<PathBuf>) -> Org {
    let root = root();
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(state.join("settings")).unwrap();
    std::fs::write(state.join(format!("settings/{task}.json")), "{}").unwrap();
    let workplace = workplace.unwrap_or_else(|| {
        if kind == "document" {
            let docs = root.join("docs").join(task);
            std::fs::create_dir_all(docs.join("out/figures")).unwrap();
            std::fs::write(docs.join("out/report.md"), "the result").unwrap();
            std::fs::write(docs.join("out/figures/one.txt"), "a figure").unwrap();
            std::fs::write(docs.join("notes.txt"), "scratch").unwrap();
            std::fs::create_dir_all(docs.join("drafts")).unwrap();
            std::fs::write(docs.join("drafts/d1.md"), "draft").unwrap();
            docs
        } else {
            let wt = root.join("workers/p").join(task);
            let gitdir = makiba::daemon::run_repo::dir(&state.join("settings"), task);
            let (branch, hooks) = (format!("makiba/{task}"), state.join("git-hooks"));
            makiba::daemon::run_repo::create(&repo(), &gitdir, &wt, &branch, "main", &hooks)
                .unwrap();
            wt
        }
    });
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let run = w
        .upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
    let org = Org {
        task: task.into(),
        state: state.clone(),
        workplace: workplace.clone(),
        base: Local::new(),
        door: Door::new(
            StateWriter::open(&state.join("state.db")).unwrap(),
            Arc::new(Local::new()),
            "fake".into(),
        ),
        person: issue_key(&w, Role::Person, "person", None, None).unwrap(),
        daemon: None,
    };
    let worker = org.launch(&w, run, kind, &format!("{task}-s1"), "verified");
    org.door
        .handle(
            Some(&worker),
            "report",
            json!({"state": "done", "task": task, "text": "finished"}),
            None,
            false,
        )
        .unwrap();
    org
}

impl Org {
    /// A `run_launch` row at `stage`, a live key for it and a terminal carrying its tags. Returns
    /// the key.
    fn launch(&self, w: &StateWriter, run: i64, kind: &str, spawn: &str, stage: &str) -> String {
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, ?2, ?3, 'fake', 'the brief', ?4, ?5)",
                rusqlite::params![run, spawn, kind, self.workplace.to_string_lossy(), stage],
            )
            .unwrap();
        let key = issue_key(
            w,
            Role::Worker,
            &format!("w-{}", self.task),
            Some(run),
            Some(spawn),
        )
        .unwrap();
        let org = self.state.to_string_lossy().into_owned();
        self.base
            .launch(&LaunchSpec {
                argv: vec!["sleep".into(), "600".into()],
                cwd: root().to_path_buf(),
                name: spawn.into(),
                tags: BTreeMap::from([
                    ("makiba.role".to_string(), "worker".to_string()),
                    ("makiba.task_id".to_string(), self.task.clone()),
                    ("makiba.spawn".to_string(), spawn.to_string()),
                    ("makiba.org".to_string(), org),
                ]),
                env: BTreeMap::new(),
                unset: vec![],
            })
            .unwrap();
        key
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.state.clone(), self.base.clone(), stop.clone());
        let d =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, d));
        // Subscribed: its start writes are done, so the door's writes cannot race them.
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let up = Connection::open_with_flags(
                self.state.join("state.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .and_then(|c| c.query_row("SELECT subscribed FROM dispatcher", [], |r| r.get(0)));
            if up == Ok(1) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("makibad did not subscribe within {WAIT:?}");
    }

    fn close(&self, abandon: bool) -> Response {
        self.door.handle(
            Some(&self.person),
            "close",
            json!({"task_id": self.task, "abandon": abandon}),
            None,
            false,
        )
    }

    fn one<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        let conn = Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// The status of the newest accepted close command, once it is no longer `accepted`.
    fn finished(&self) -> String {
        let sql = "SELECT status FROM commands WHERE verb = 'close'
                   AND status IN ('accepted', 'done', 'failed')
                   AND json_extract(result_json, '$.result.closed') IS NULL
                   ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let s: String = self.one(sql);
            if s != "accepted" {
                return s;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the close is still accepted after {WAIT:?}");
    }

    fn terminals(&self) -> usize {
        self.base
            .list()
            .unwrap()
            .iter()
            .filter(|t| t.tags.get("makiba.task_id") == Some(&self.task))
            .count()
    }

    fn live_keys(&self) -> i64 {
        self.one(&format!(
            "SELECT count(*) FROM agents WHERE agent_id = 'w-{}' AND revoked_at IS NULL",
            self.task
        ))
    }

    fn settings(&self) -> PathBuf {
        self.state.join(format!("settings/{}.json", self.task))
    }

    /// The state every finished close of a code run ends in.
    fn assert_closed(&self) {
        assert_eq!(self.terminals(), 0, "no terminal carries the task's tag");
        assert!(!self.workplace.exists(), "the worktree folder is removed");
        assert!(!self.settings().exists(), "the settings file is deleted");
        assert_eq!(
            self.live_keys(),
            0,
            "every worker key of the run is revoked"
        );
        let listed = git(&repo(), &["worktree", "list", "--porcelain"]);
        assert!(
            !listed.contains(&*self.workplace.to_string_lossy()),
            "the repository no longer lists the worktree: {listed}"
        );
    }
}

/// After close: no terminal with the task's tag, no worktree folder, no settings file, every key
/// revoked, the key of a launch failed at the dialog stage included.
#[test]
fn close_ends_the_terminals_revokes_the_keys_and_removes_the_worktree() {
    let mut org = org("c1", "code", None);
    {
        let w = StateWriter::open(&org.state.join("state.db")).unwrap();
        org.launch(&w, 1, "code", "c1-s2", "dialog_passed");
    }
    assert_eq!(org.terminals(), 2);
    org.start();
    org.close(false).unwrap();
    assert_eq!(org.finished(), "done");
    org.assert_closed();
    assert_eq!(
        org.one::<String>("SELECT status FROM runs"),
        "completed".to_string()
    );
}

/// A worktree with an unpushed commit is not removed without `--abandon`: the close fails and
/// says why, and nothing is ended. With `--abandon` it is removed and the branch keeps the commit.
#[test]
fn an_unpushed_commit_refuses_the_close_until_abandon() {
    let mut org = org("c2", "code", None);
    git(
        &org.workplace,
        &["commit", "-q", "--allow-empty", "-m", "wip"],
    );
    let wip = git(&org.workplace, &["rev-parse", "HEAD"]);
    org.start();
    let e = org.close(false).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
    assert!(e.message.contains("unpushed"), "{}", e.message);
    assert!(e.hint.contains("--abandon"), "{}", e.hint);
    assert!(org.workplace.exists());
    assert_eq!(org.terminals(), 1);
    assert_eq!(org.live_keys(), 1);
    assert!(org.settings().exists());
    assert_ne!(org.one::<String>("SELECT status FROM runs"), "completed");

    org.close(true).unwrap();
    assert_eq!(org.finished(), "done");
    org.assert_closed();
    assert_eq!(git(&repo(), &["rev-parse", "makiba/c2"]), wip);
}

/// `makibad` killed right after the terminals were ended, before the worktree was removed: a
/// restart finishes the close and ends in the same state.
#[test]
fn a_close_cut_short_by_a_kill_is_finished_after_a_restart() {
    let mut org = org("c3", "code", None);
    let die = org.state.join("makibad.die-at");
    std::fs::write(&die, "close_ended").unwrap();
    org.start();
    org.close(false).unwrap();
    let (_, d) = org.daemon.take().unwrap();
    let until = Instant::now() + WAIT;
    while !d.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(d.is_finished(), "makibad did not die at close_ended");
    let payload = d.join().expect_err("makibad ended without dying");
    assert_eq!(
        payload.downcast_ref::<String>().map(String::as_str),
        Some("makibad died at close_ended")
    );
    assert_eq!(org.terminals(), 0);
    assert!(org.workplace.exists(), "killed before the worktree went");
    std::fs::remove_file(&die).unwrap();

    org.start();
    assert_eq!(org.finished(), "done");
    org.assert_closed();
}

/// After closing a `document` run, `out/` and its files are still there; the rest is gone.
#[test]
fn a_document_close_keeps_out() {
    let mut org = org("d1", "document", None);
    org.start();
    org.close(false).unwrap();
    assert_eq!(org.finished(), "done");
    let docs = &org.workplace;
    assert_eq!(
        std::fs::read_to_string(docs.join("out/report.md")).unwrap(),
        "the result"
    );
    assert_eq!(
        std::fs::read_to_string(docs.join("out/figures/one.txt")).unwrap(),
        "a figure"
    );
    assert!(!docs.join("notes.txt").exists());
    assert!(!docs.join("drafts").exists());
    assert_eq!(org.terminals(), 0);
    assert_eq!(org.live_keys(), 0);
    assert!(!org.settings().exists());
}

/// A workplace outside the workers and documents folders is never removed, even with
/// `--abandon`: the close fails.
#[test]
fn a_workplace_outside_the_organization_folders_is_never_removed() {
    let outside = root().join("elsewhere/c5");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "mine").unwrap();
    let mut org = org("c5", "code", Some(outside.clone()));
    org.start();
    org.close(true).unwrap();
    assert_eq!(org.finished(), "failed");
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "mine"
    );
}
