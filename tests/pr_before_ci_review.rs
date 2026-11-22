//! Issue 307, the review's two holes in the check before CI:
//!
//! - `pr` with a head other than the pull request's own (an older commit the clone holds) is
//!   refused: the head recorded is the head the pull request has at origin.
//! - `update` with `--base` naming the branch itself, after the test commit was pushed alone, does
//!   not hide the test commit, and neither does a recorded pull request whose base is the branch:
//!   push and update judge against the project's default branch, so the fix is not sent before CI
//!   failed at the test commit.
//!
//! The worlds are built as in tests/pr_open_test_commit.rs: a local origin, the project's clone
//! in the repositories folder, a `gh` stand-in (`MAKIBA_GH`) answering `pr view` from files, and
//! `makibad` in this process for `update`. No network and no socket.

use makiba::base::stub::Stub;
use makiba::daemon;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; f="$FAKE_GH_DIR/$name/view.$3"
  if [ -e "$f" ]; then cat "$f"; exit 0; fi
  echo "no pull request $3" >&2; exit 1
fi
echo "unexpected call: $*" >&2; exit 1
"#;

const TEST_RS: &str = "#[test]\nfn adds() {\n    assert_eq!(2 + 2, 4);\n}\n";
const DECLARES: &str = "test: the failing test\n\nAcceptance-Test: tests/adds.rs";

fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        for d in ["repos", "workers", "docs", "answers"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let gh = root.join("gh");
        std::fs::write(&gh, STAND_IN).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_GH", &gh);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        }
        root
    })
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_in(dir: &Path, msg: &str, files: &[(&str, &str)]) -> String {
    for (path, text) in files {
        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, text).unwrap();
        git(dir, &["add", path]);
    }
    git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn view(name: &str, pr: i64, head: &str) {
    let dir = root().join("answers").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let v = json!({"number": pr, "url": format!("https://github.com/o/{name}/pull/{pr}"),
                   "baseRefName": "main", "headRefOid": head});
    std::fs::write(dir.join(format!("view.{pr}")), v.to_string()).unwrap();
}

/// `pr` at a head other than the pull request's own, though the clone holds it: refused, and
/// nothing is watched. At the pull request's head it is recorded.
#[test]
fn a_head_other_than_the_pull_requests_own_is_refused() {
    let name = "otherhead";
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    let base = commit_in(&origin, "base", &[("src.txt", "base\n")]);
    let url = format!("https://github.com/o/{name}");
    let repos = root().join("repos");
    git(&repos, &["clone", "-q", origin.to_str().unwrap(), name]);
    let clone = repos.join(name);
    git(&clone, &["remote", "set-url", "origin", &url]);
    git(
        &clone,
        &[
            "config",
            &format!("url.{}.insteadOf", origin.display()),
            &url,
        ],
    );
    git(&origin, &["checkout", "-q", "-b", "feature"]);
    let head = commit_in(&origin, "fix: no test", &[("src.txt", "fixed\n")]);
    git(&origin, &["update-ref", "refs/pull/3/head", &head]);
    view(name, 3, &head);

    let db = tmp.path().join("state.db");
    let w = StateWriter::open(&db).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: name,
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "UPDATE projects SET origin_url = ?2 WHERE slug = ?1",
            [name, &url],
        )
        .unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
    let pr = |h: &str| -> Response {
        door.handle(
            Some(&secretary),
            "pr",
            json!({"state": "opened", "repo": format!("o/{name}"), "pr": 3, "head": h, "task": "t1"}),
            None,
            false,
        )
    };
    // The base commit is in the clone, but it is not the pull request's head.
    let r = pr(&base);
    let why = format!("{r:?}");
    assert!(r.is_err(), "a head other than the pull request's: {why}");
    assert!(
        why.contains(&head[..12]),
        "the refusal names the PR's head: {why}"
    );
    let watches: i64 = Connection::open(&db)
        .unwrap()
        .query_row("SELECT count(*) FROM pr_watches", [], |r| r.get(0))
        .unwrap();
    assert_eq!(watches, 0);
    let r = pr(&head);
    assert!(r.is_ok(), "the pull request's own head: {r:?}");
}

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Run {
    state: PathBuf,
    daemon: Option<Daemon>,
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some((stop, d)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = d.join();
        }
    }
}

impl Run {
    fn conn(&self) -> Connection {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn finished(&self, verb: &str) -> String {
        let sql = "SELECT status FROM commands WHERE verb = ?1 ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let s: Option<String> = self
                .conn()
                .query_row(sql, [verb], |r| r.get(0))
                .optional()
                .unwrap_or(None);
            if let Some(s) = s.filter(|s| s != "accepted") {
                return s;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the {verb} is not finished after {WAIT:?}");
    }
}

/// After the test commit was pushed alone, `update --base` naming the branch itself would make the
/// test commit part of the base; the check still judges against the pull request's base, so the
/// fix is not sent.
#[test]
fn an_update_whose_base_is_the_branch_itself_does_not_hide_the_test_commit() {
    let task = "selfbase";
    let root = root();
    let clone = root.join("repos").join(task);
    std::fs::create_dir_all(&clone).unwrap();
    git(&clone, &["init", "-q", "-b", "main"]);
    commit_in(&clone, "first", &[("file.txt", "first\n")]);
    let origin = root.join(format!("origin-{task}.git"));
    git(root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    git(
        &clone,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&clone, &["push", "-q", "origin", "main"]);
    git(&clone, &["fetch", "-q", "origin"]);
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(&state).unwrap();
    let branch = format!("makiba/{task}");
    let worktree = root.join("workers").join(task);
    makiba::daemon::run_repo::create(
        &clone,
        &makiba::daemon::run_repo::dir(&state.join("settings"), task),
        &worktree,
        &branch,
        "main",
        &state.join("git-hooks"),
    )
    .unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let run_id = w
        .upsert_run(&RunUpsert {
            task_id: task,
            project_slug: task,
            status: Some("in_use"),
            branch: Some(&branch),
            ..Default::default()
        })
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES (?1, 'selfbase-s1', 'code', 'fake', 'the brief', ?2, 'verified')",
            rusqlite::params![run_id, worktree.to_string_lossy()],
        )
        .unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "fake".into());

    let m = commit_in(&worktree, DECLARES, &[("tests/adds.rs", TEST_RS)]);
    commit_in(&worktree, "fix: the fix", &[("file.txt", "fixed\n")]);
    view(task, 1, &m);
    let r = door.handle(
        Some(&secretary),
        "pr",
        json!({"state": "opened", "repo": format!("o/{task}"), "pr": 1, "head": m, "task": task}),
        None,
        false,
    );
    assert!(r.is_ok(), "{r:?}");

    let stop = Arc::new(AtomicBool::new(false));
    let (dir, flag) = (state.clone(), stop.clone());
    let d = std::thread::spawn(move || daemon::run_until(&dir, &|| Box::new(Stub::new()), flag));
    let mut run = Run {
        state: state.clone(),
        daemon: Some((stop, d)),
    };
    let until = Instant::now() + WAIT;
    while run
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'base_resynced'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 0
    {
        assert!(Instant::now() < until, "makibad did not start");
        std::thread::sleep(Duration::from_millis(25));
    }
    let call = |verb: &str, args| door.handle(Some(&secretary), verb, args, None, true);

    assert!(call("push", json!({"task": task, "head": m})).is_ok());
    assert_eq!(run.finished("push"), "done");
    let at_origin = || git(&origin, &["rev-parse", &format!("refs/heads/{branch}")]);
    assert_eq!(at_origin(), m);

    let r = call("update", json!({"task": task, "base": branch}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(run.finished("update"), "failed");
    assert_eq!(
        at_origin(),
        m,
        "the fix is not sent before CI failed at the test commit"
    );

    // A recorded pull request whose base is the branch itself does not steer the check either.
    let dir = root.join("answers").join(task);
    let v = json!({"number": 1, "url": format!("https://github.com/o/{task}/pull/1"),
                   "baseRefName": branch, "headRefOid": m});
    std::fs::write(dir.join("view.1"), v.to_string()).unwrap();
    assert!(call("push", json!({"task": task})).is_ok());
    assert_eq!(run.finished("push"), "failed");
    assert_eq!(
        at_origin(),
        m,
        "a pull request's base does not hide the test commit"
    );
    if let Some((stop, d)) = run.daemon.take() {
        stop.store(true, Ordering::SeqCst);
        let _ = d.join();
    }
}
