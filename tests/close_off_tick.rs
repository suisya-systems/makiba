//! Issue 644: a close takes the run's folders out of their places on the tick and removes them off
//! it, so the heartbeat does not stop while tens of GB go.
//!
//! `makibad` runs in this process on `Local` (tests/common/local.rs), the door in this process too,
//! on the same `state.db`, with the person's key; the run is made as tests/close.rs makes one (a
//! real worktree of the run's own repository, a `verified` launch, its terminal, a done report).
//!
//! The removal is made slow by a debug build's hold point, not by the size of the tree (as
//! tests/tick_slow.rs makes a slow pass, Issue 624): `removal` in `<state>/makibad.hold-at` holds
//! the first removal of a folder's contents, on whichever thread does it, until the test removes
//! `<state>/makibad.held`. While it is held, the close must be `done`, the folders out of their
//! places, and the heartbeat must go on being written; once it is released the holding folders
//! empty. A removal on the tick would hold the tick there, and the heartbeat with it.
//!
//! The workers, documents and repositories folders are one temporary root per test process; each
//! test has its own state folder.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, LaunchSpec};
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// makibad's heartbeat interval (`daemon::HEARTBEAT`), and the slack a beat may be late by.
const HEARTBEAT: Duration = Duration::from_secs(10);
const SLACK: Duration = Duration::from_secs(5);
/// How long the test watches the heartbeat while the removal is held.
const WATCH: Duration = Duration::from_secs(25);
const WAIT: Duration = Duration::from_secs(60);
/// The holding folder's name in each folder a close removes from.
const HOLDING: &str = ".makiba-removing";

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
        std::fs::create_dir_all(root.join("workers")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
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

/// `count` empty files under `dir`, 100 to a folder.
fn tree(dir: &Path, count: usize) {
    for i in 0..count {
        let d = dir.join(format!("d{}", i / 100));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("f{i}")), "").unwrap();
    }
}

/// Whether `dir` is absent or holds nothing.
fn empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut d| d.next().is_none())
}

/// A running `makibad`: its stop flag and its thread.
type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    task: String,
    state: PathBuf,
    base: Local,
    daemon: Option<Daemon>,
}

/// Stops the daemon, lets a held removal go and ends every terminal when the test ends.
impl Drop for Org {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.state.join("makibad.held"));
        if let Some((stop, d)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = d.join();
        }
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

impl Org {
    fn new(task: &str) -> Org {
        let state = root().join(format!("state-{task}"));
        std::fs::create_dir_all(state.join("settings")).unwrap();
        Org {
            task: task.into(),
            state,
            base: Local::new(),
            daemon: None,
        }
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.state.clone(), self.base.clone(), stop.clone());
        let d =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, d));
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if self.read::<i64>("SELECT subscribed FROM dispatcher") == Some(1) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("makibad did not subscribe within {WAIT:?}");
    }

    fn read<T: rusqlite::types::FromSql>(&self, sql: &str) -> Option<T> {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .and_then(|c| c.query_row(sql, [], |r| r.get(0)))
        .ok()
    }

    fn heartbeat(&self) -> Option<String> {
        self.read("SELECT heartbeat_at FROM dispatcher WHERE id = 1")
    }

    fn close_status(&self) -> Option<String> {
        self.read("SELECT status FROM commands WHERE verb = 'close' ORDER BY id DESC LIMIT 1")
    }

    fn held(&self) -> PathBuf {
        self.state.join("makibad.held")
    }
}

/// Wait up to `limit` for `done`, polling the heartbeat meanwhile into `beats`.
fn watch(org: &Org, beats: &mut Beats, limit: Duration, done: impl Fn() -> bool) -> bool {
    let until = Instant::now() + limit;
    loop {
        beats.poll(org);
        if done() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The heartbeat's changes as the test's clock saw them, from `since`.
struct Beats {
    since: Instant,
    last: Option<String>,
    changes: Vec<Instant>,
}

impl Beats {
    fn new(org: &Org) -> Beats {
        Beats {
            since: Instant::now(),
            last: org.heartbeat(),
            changes: Vec::new(),
        }
    }

    fn poll(&mut self, org: &Org) {
        let now = org.heartbeat();
        if now.is_some() && now != self.last {
            self.changes.push(Instant::now());
            self.last = now;
        }
    }

    /// The longest time the heartbeat did not move, from `since` to now.
    fn longest_gap(&self) -> Duration {
        let mut at = self.since;
        let mut longest = Duration::ZERO;
        for c in self.changes.iter().chain(std::iter::once(&Instant::now())) {
            longest = longest.max(c.duration_since(at));
            at = *c;
        }
        longest
    }
}

/// A close of a run whose worktree and temporary folder hold a tree is `done`, with every folder
/// out of its place, while the removal of what it took out is still held; the heartbeat goes on
/// meanwhile; released, the holding folders empty.
#[test]
fn a_close_whose_removal_is_held_keeps_the_heartbeat() {
    let root = root();
    let mut org = Org::new("h1");
    let task = org.task.clone();
    let (state, settings) = (org.state.clone(), org.state.join("settings"));
    std::fs::write(settings.join(format!("{task}.json")), "{}").unwrap();
    let wt = root.join("workers/p").join(&task);
    let gitdir = makiba::daemon::run_repo::dir(&settings, &task);
    makiba::daemon::run_repo::create(
        &root.join("repos/p"),
        &gitdir,
        &wt,
        &format!("makiba/{task}"),
        "main",
        &state.join("git-hooks"),
    )
    .unwrap();
    tree(&wt.join("target/big"), 1000);

    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let run = w
        .upsert_run(&RunUpsert {
            task_id: &task,
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
    let spawn = format!("{task}-s1");
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES (?1, ?2, 'code', 'fake', 'the brief', ?3, 'verified')",
            rusqlite::params![run, spawn, wt.to_string_lossy()],
        )
        .unwrap();
    let launch: i64 = w.conn().last_insert_rowid();
    let tmp = state.join("tmp").join(launch.to_string());
    tree(&tmp.join("base-target"), 1000);
    let worker = issue_key(
        &w,
        Role::Worker,
        &format!("w-{task}"),
        Some(run),
        Some(&spawn),
    )
    .unwrap();
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    org.base
        .launch(&LaunchSpec {
            argv: vec!["sleep".into(), "600".into()],
            cwd: root.to_path_buf(),
            name: spawn.clone(),
            tags: BTreeMap::from([
                ("makiba.role".to_string(), "worker".to_string()),
                ("makiba.task_id".to_string(), task.clone()),
                ("makiba.spawn".to_string(), spawn.clone()),
                (
                    "makiba.org".to_string(),
                    state.to_string_lossy().into_owned(),
                ),
            ]),
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
    let door = Door::new(
        StateWriter::open(&state.join("state.db")).unwrap(),
        Arc::new(Local::new()),
        "fake".into(),
    );
    door.handle(
        Some(&worker),
        "report",
        json!({"state": "done", "task": task, "text": "finished"}),
        None,
        false,
    )
    .unwrap();

    std::fs::write(state.join("makibad.hold-at"), "removal").unwrap();
    org.start();
    let until = Instant::now() + WAIT;
    while org.heartbeat().is_none() {
        assert!(
            Instant::now() < until,
            "no heartbeat within {WAIT:?} of the start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut beats = Beats::new(&org);
    door.handle(
        Some(&person),
        "close",
        json!({"task_id": task, "abandon": true}),
        None,
        false,
    )
    .expect("the close is accepted");

    assert!(
        watch(&org, &mut beats, WAIT, || org.held().exists()),
        "the removal of what the close took out did not reach its hold within {WAIT:?} \
         (close {:?}; the worktree {} exists: {})",
        org.close_status(),
        wt.display(),
        wt.exists()
    );
    assert!(
        watch(&org, &mut beats, WAIT, || org.close_status().as_deref()
            == Some("done")),
        "the close is not done while its removal is held: {:?}",
        org.close_status()
    );
    assert!(!wt.exists(), "the worktree is out of its place");
    assert!(!gitdir.exists(), "the run's git folder is out of its place");
    assert!(
        !tmp.exists(),
        "the launch's temporary folder is out of its place"
    );
    watch(&org, &mut beats, WATCH, || false);
    assert!(
        org.held().exists(),
        "the removal is still held after the watch"
    );
    assert!(
        !empty(&state.join("tmp").join(HOLDING)),
        "what the close took out of <state>/tmp waits in its holding folder while the removal is held"
    );
    assert!(
        beats.changes.len() >= 2,
        "the heartbeat moved {} times in the {:?} after the close",
        beats.changes.len(),
        beats.since.elapsed()
    );
    let gap = beats.longest_gap();
    assert!(
        gap <= HEARTBEAT + SLACK,
        "the heartbeat did not move for {gap:?} while the close's removal was held"
    );

    std::fs::remove_file(org.held()).unwrap();
    let holdings = [
        root.join("workers").join(HOLDING),
        settings.with_file_name("git").join(HOLDING),
        state.join("tmp").join(HOLDING),
    ];
    assert!(
        watch(&org, &mut beats, WAIT, || holdings.iter().all(|h| empty(h))),
        "the holding folders are not emptied within {WAIT:?} of the release: {:?}",
        holdings.iter().filter(|h| !empty(h)).collect::<Vec<_>>()
    );
}

/// What a close left in the holding folders (a stop or a crash before it was removed) is removed
/// once `makibad` starts, without a close; a holding folder that is a link is not followed.
#[test]
fn a_start_empties_what_was_left_in_the_holding_folders() {
    let root = root();
    let mut org = Org::new("h2");
    let state = org.state.clone();
    let left_workers = root.join("workers").join(HOLDING).join("left-1");
    let left_tmp = state.join("tmp").join(HOLDING).join("left-2");
    tree(&left_workers, 1000);
    tree(&left_tmp, 1000);
    let outside = root.join("outside-h2");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "kept").unwrap();
    std::fs::create_dir_all(state.join("git")).unwrap();
    std::os::unix::fs::symlink(&outside, state.join("git").join(HOLDING)).unwrap();

    org.start();
    let until = Instant::now() + WAIT;
    while !(left_workers.symlink_metadata().is_err() && empty(&state.join("tmp").join(HOLDING))) {
        assert!(
            Instant::now() < until,
            "what was left in the holding folders is not removed within {WAIT:?} of the start \
             (workers: {}, tmp: {})",
            left_workers.exists(),
            left_tmp.exists()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        outside.join("keep.txt").exists(),
        "a holding folder that is a link is not followed"
    );
    while org.heartbeat().is_none() {
        assert!(
            Instant::now() < until,
            "no heartbeat within {WAIT:?} of the start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
