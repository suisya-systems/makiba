//! Issue 183: the secretary finishes a task. `makibad` does the writes as verbs (`note`, Issue
//! 189, and `push`, Issue 190, have their own tests); this test asserts the third, `update`, which
//! brings a pushed worker branch up to date with its base by a merge and pushes it, and the
//! secretary's settings: the network host `api.github.com` for its own `gh`, and nothing else
//! opened. A worker's settings are unchanged.
//!
//! `makibad` runs in this process on the stand-in base; the door runs in this process too, on the
//! same `state.db`, and is called as a tool (`tool` true), as the secretary's MCP call is. The
//! project `p` is a git repository under a temporary repositories folder whose `main` is pushed
//! to a local bare `origin`: no network. Each test has its own run, worktree, state folder and
//! base branch at the origin (`base-<task>`).
//!
//! The test writes `state.db` only before `makibad` starts and through the door afterwards; it
//! waits on the command row `makibad` finishes, read-only (Issue 181).
//!
//! The two fixtures are the settings main 26cbf71 built for the worker and the secretary from
//! the same `TaskSettings` as `built` below.

mod common;

use makiba::agents::{self, TaskSettings};
use makiba::base::stub::Stub;
use makiba::daemon;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
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
        // makibad's merge commit needs a name; the person's own config gives it on a host.
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "user.email", "t@e"]);
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
    /// The base branch at the origin, and a clone of the origin that moves it.
    base: String,
    theirs: PathBuf,
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

/// An `in_use` code run of `task` with a `verified` launch whose worktree (under the workers
/// folder unless `at` names another place) is on `makiba/<task>`, made from `main`; the base
/// branch `base-<task>` at the origin, at `main`; the keys of the secretary and the worker.
fn org_at(task: &str, at: Option<PathBuf>) -> Org {
    let root = root();
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(&state).unwrap();
    let branch = format!("makiba/{task}");
    let worktree = at.unwrap_or_else(|| root.join("workers/p").join(task));
    common::run_workplace(&state, &root.join("repos/p"), &worktree, task, &branch);
    let base = format!("base-{task}");
    let theirs = root.join(format!("theirs-{task}"));
    git(
        root,
        &[
            "clone",
            "-q",
            &origin().to_string_lossy(),
            &theirs.to_string_lossy(),
        ],
    );
    git(&theirs, &["switch", "-q", "-c", &base, "origin/main"]);
    git(&theirs, &["push", "-q", "origin", &base]);
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
        base,
        theirs,
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

fn org(task: &str) -> Org {
    org_at(task, None)
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

    /// `update` called as a tool, with `key`.
    fn update(&self, key: &str) -> Response {
        self.door.handle(
            Some(key),
            "update",
            json!({"task": self.task, "base": self.base}),
            None,
            true,
        )
    }

    /// A commit in the worktree; returns its id.
    fn commit(&self, name: &str, text: &str) -> String {
        std::fs::write(self.worktree.join(name), text).unwrap();
        git(&self.worktree, &["add", name]);
        git(&self.worktree, &["commit", "-q", "-m", name]);
        git(&self.worktree, &["rev-parse", "HEAD"])
    }

    /// The worktree's branch pushed to the origin by the test, as an earlier push left it.
    fn pushed(&self) -> String {
        git(&self.worktree, &["push", "-q", "origin", &self.branch]);
        at_origin(&self.branch).unwrap()
    }

    /// Somebody else's commit on the base branch at the origin; returns it.
    fn move_base(&self, name: &str, text: &str) -> String {
        std::fs::write(self.theirs.join(name), text).unwrap();
        git(&self.theirs, &["add", name]);
        git(&self.theirs, &["commit", "-q", "-m", name]);
        git(&self.theirs, &["push", "-q", "origin", &self.base]);
        at_origin(&self.base).unwrap()
    }

    fn head(&self) -> String {
        git(&self.worktree, &["rev-parse", "HEAD"])
    }

    /// The status of this task's update command once `makibad` has finished it.
    fn finished(&self) -> String {
        let sql = "SELECT status FROM commands WHERE verb = 'update'
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
        panic!("the update of {} is not finished after {WAIT:?}", self.task);
    }

    /// The (old, new) heads the `branch_updated` events of this run carry.
    fn updates(&self) -> Vec<(String, String)> {
        let c = self.conn().unwrap();
        let mut q = c
            .prepare(
                "SELECT json_extract(e.payload_json, '$.old'), json_extract(e.payload_json, '$.new')
                 FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'branch_updated' AND r.task_id = ?1 ORDER BY e.id",
            )
            .unwrap();
        q.query_map([&self.task], |r| Ok((r.get(0)?, r.get(1)?)))
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

    /// The worktree as git sees it: no change to a tracked file and no merge under way.
    fn assert_untouched(&self, head: &str) {
        assert_eq!(self.head(), head, "the worktree's head moved");
        let merging = Command::new("git")
            .arg("-C")
            .arg(&self.worktree)
            .args(["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .output()
            .unwrap();
        assert!(!merging.status.success(), "a merge is left under way");
    }
}

/// 1. The secretary's tool call after the base moved at the origin: the branch at the origin is a
///    merge of its old head and the base, made without force; the command ends `done`,
///    `branch_updated` carries the old and the new head and the secretary is told the new one.
#[test]
fn the_secretary_brings_a_pushed_branch_up_to_date_through_makibad() {
    let mut org = org("u1");
    org.commit("one.txt", "one\n");
    let old = org.pushed();
    let base = org.move_base("base.txt", "base\n");
    org.start();
    let r = org.update(&org.secretary);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(org.finished(), "done", "{:?}", org.notices());
    let new = at_origin(&org.branch).unwrap();
    assert_ne!(new, old);
    // Issue 241: the worker's tree is not changed; the clone records origin's new head.
    assert_eq!(org.head(), old);
    assert_eq!(
        git(
            &root().join("repos/p"),
            &["rev-parse", &format!("refs/remotes/origin/{}", org.branch)]
        ),
        new
    );
    let parents = git(&org.worktree, &["rev-list", "--parents", "-n", "1", &new]);
    assert_eq!(parents, format!("{new} {old} {base}"));
    assert_eq!(org.updates(), vec![(old, new.clone())]);
    assert!(
        org.notices().iter().any(|n| n.contains(&new)),
        "no notice names the new head {new}: {:?}",
        org.notices()
    );
}

/// 2. The base and the branch change the same line: `failed`, a notice naming the file, the merge
///    undone, and the origin keeps the branch's head.
#[test]
fn a_conflict_ends_failed_and_changes_nothing_at_the_origin() {
    let mut org = org("u2");
    org.commit("file.txt", "mine\n");
    let old = org.pushed();
    org.move_base("file.txt", "theirs\n");
    org.start();
    assert!(org.update(&org.secretary).is_ok());
    assert_eq!(org.finished(), "failed");
    assert_eq!(at_origin(&org.branch), Some(old.clone()));
    org.assert_untouched(&old);
    assert_eq!(
        std::fs::read_to_string(org.worktree.join("file.txt")).unwrap(),
        "mine\n"
    );
    assert!(org.updates().is_empty());
    assert!(
        org.notices().iter().any(|n| n.contains("file.txt")),
        "no notice names the conflicting file: {:?}",
        org.notices()
    );
}

/// 3. Uncommitted tracked changes: `failed`, a notice, nothing merged or pushed and the change
///    kept.
#[test]
fn a_dirty_worktree_ends_failed_and_changes_nothing_at_the_origin() {
    let mut org = org("u3");
    org.commit("three.txt", "three\n");
    let old = org.pushed();
    org.move_base("base.txt", "base\n");
    std::fs::write(org.worktree.join("three.txt"), "changed\n").unwrap();
    org.start();
    assert!(org.update(&org.secretary).is_ok());
    assert_eq!(org.finished(), "failed");
    assert_eq!(at_origin(&org.branch), Some(old.clone()));
    assert_eq!(org.head(), old);
    assert_eq!(
        std::fs::read_to_string(org.worktree.join("three.txt")).unwrap(),
        "changed\n"
    );
    assert!(org.updates().is_empty());
    assert!(!org.notices().is_empty(), "the secretary is not told");
}

/// 4. A worktree outside the workers folder is not one makibad may write: `failed`, and nothing
///    merged or pushed.
#[test]
fn a_worktree_outside_the_workers_folder_is_not_updated() {
    let mut org = org_at("u4", Some(root().join("elsewhere/u4")));
    org.commit("four.txt", "four\n");
    let old = org.pushed();
    org.move_base("base.txt", "base\n");
    org.start();
    assert!(org.update(&org.secretary).is_ok());
    assert_eq!(org.finished(), "failed");
    assert_eq!(at_origin(&org.branch), Some(old.clone()));
    org.assert_untouched(&old);
    assert!(org.updates().is_empty());
}

/// 5. A worker's key cannot call it, and nothing changes at the origin.
#[test]
fn a_worker_may_not_update() {
    let org = org("u5");
    org.commit("five.txt", "five\n");
    let old = org.pushed();
    org.move_base("base.txt", "base\n");
    let e = org.update(&org.worker).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Denied, "{e:?}");
    assert_eq!(at_origin(&org.branch), Some(old.clone()));
    org.assert_untouched(&old);
}

// ---- the settings --------------------------------------------------------------------------

/// The settings file the Claude Code module writes for `role`, as a launch's preparation calls it,
/// with fixed places.
fn built(role: agents::Role) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.json");
    let task = TaskSettings {
        role: Some(role),
        write_dirs: vec!["/org/workplace".into(), "/org/tmp".into()],
        deny_dirs: vec!["/org/base".into()],
        deny_files: vec!["/org/state.db".into(), "/org/person.key".into()],
        deny_write_dirs: vec!["/org/share".into()],
        deny_write_files: vec!["/org/workplace/.mcp.json".into()],
    };
    agents::by_name("claude_code")
        .unwrap()
        .write_task_settings(&file, &task)
        .unwrap();
    serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
}

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(format!("secretary_surface_{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Every hook command of the settings.
fn hook_commands(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for groups in v["hooks"].as_object().into_iter().flat_map(|m| m.values()) {
        for g in groups.as_array().into_iter().flatten() {
            for h in g["hooks"].as_array().into_iter().flatten() {
                out.extend(h["command"].as_str().map(String::from));
            }
        }
    }
    out
}

/// 6. The secretary's settings let its own `gh` reach `api.github.com`, and only that host is
///    added: no `github.com` (pushes go through makibad), no other write place, and every deny and
///    hook it had is still there.
#[test]
fn the_secretarys_settings_allow_api_github_com_and_still_refuse_what_they_refused() {
    let now = built(agents::Role::Secretary);
    let before = fixture("secretary");
    let domains = strings(&now["sandbox"]["network"]["allowedDomains"]);
    assert!(
        domains.iter().any(|d| d == "api.github.com"),
        "api.github.com is not allowed: {now}"
    );
    assert!(
        !domains.iter().any(|d| d == "github.com" || d == "*"),
        "{domains:?}"
    );
    let network = &now["sandbox"]["network"];
    assert!(network["allowUnixSockets"].is_null(), "{now}");
    assert_ne!(network["allowAllUnixSockets"], Value::Bool(true), "{now}");
    // The write surface is the launch's own, as before: no repository, worktree or knowledge.
    assert_eq!(
        now["sandbox"]["filesystem"]["allowWrite"],
        before["sandbox"]["filesystem"]["allowWrite"]
    );
    assert_eq!(now["sandbox"]["enabled"], true);
    assert_eq!(now["sandbox"]["failIfUnavailable"], true);
    assert_eq!(now["sandbox"]["allowUnsandboxedCommands"], false);
    for (list, was, is) in [
        (
            "permissions.deny",
            &before["permissions"]["deny"],
            &now["permissions"]["deny"],
        ),
        (
            "denyRead",
            &before["sandbox"]["filesystem"]["denyRead"],
            &now["sandbox"]["filesystem"]["denyRead"],
        ),
        (
            "denyWrite",
            &before["sandbox"]["filesystem"]["denyWrite"],
            &now["sandbox"]["filesystem"]["denyWrite"],
        ),
    ] {
        let is = strings(is);
        for entry in strings(was) {
            assert!(is.contains(&entry), "{list} lost {entry:?}");
        }
    }
    let hooks = hook_commands(&now);
    for h in hook_commands(&before) {
        assert!(hooks.contains(&h), "the hook {h:?} is gone");
    }
}

/// 7. A worker's settings are what main built before this change.
#[test]
fn a_workers_settings_are_unchanged() {
    assert_eq!(built(agents::Role::Worker), fixture("worker"));
}
