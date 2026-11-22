//! Issue 307 (incident I-13, pull request 303): a branch that declares an acceptance test (a commit
//! with an `Acceptance-Test:` trailer, the test commit M) gets no head past M to origin, and no
//! pull request recorded at a head past M, until a failed CI run is recorded at M. Line L-K9.
//!
//! - `pr opened` or `pr pushed` at a head past M with no failed `ci_completed` at M for that pull
//!   request is refused, naming M and saying to push it alone first; no watch is written. At M
//!   itself it is let through; once a failed run is recorded at M for that pull request, a later
//!   head is let through. A failed run recorded for another pull request does not count (the
//!   merge gate judges per pull request). A base holding `.github/acceptance-check` is not exempt
//!   (pull request 303 was in such a repository). A branch with no declared test, and a
//!   documents-only branch, are let through. An `Acceptance-Test:` line git does not read as a
//!   trailer (a blank line before `Co-Authored-By:`, as on pull request 308) is refused. When the
//!   repository is a project's and its pull request cannot be read, the record is refused; a
//!   repository that is no project's is let through (the gate cannot pass it anyway).
//! - `push` (and `update`) send no head past M to origin before a failed run is recorded at M on
//!   one of the task's pull requests; `push` with `head` M sends M alone; any other `head` is
//!   refused.
//!
//! The door tests build their world as tests/acceptance_gate.rs does: a local origin publishes
//! `refs/pull/<n>/head`, the project's clone sits in the repositories folder with its origin URL
//! mapped to the local origin, and `gh` is a stand-in (`MAKIBA_GH`) that answers `pr view` from
//! files. The push tests run `makibad` in this process as tests/push_verb.rs does, with a local
//! bare origin per test. No network and no socket.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

/// `pr view <n> --repo o/<name> ...` prints `<answers>/<name>/view.<n>` (missing: it fails, as an
/// unreadable pull request); every other call fails.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; f="$FAKE_GH_DIR/$name/view.$3"
  if [ -e "$f" ]; then cat "$f"; exit 0; fi
  echo "no pull request $3" >&2; exit 1
fi
echo "unexpected call: $*" >&2; exit 1
"#;

/// The organization's folders and the stand-in, made once per process.
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

/// Write `files` under `dir`, then commit them all with `msg`; the new commit's id.
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

/// The answer of the stand-in's `pr view <n>` for `o/<name>`.
fn view(name: &str, pr: i64, head: &str) {
    let dir = root().join("answers").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let v = json!({"number": pr, "url": format!("https://github.com/o/{name}/pull/{pr}"),
                   "baseRefName": "main", "headRefOid": head});
    std::fs::write(dir.join(format!("view.{pr}")), v.to_string()).unwrap();
}

/// A failed or passed `ci_completed` of `repo#pr` at `head`, written while no makibad runs.
fn ci(db: &Path, repo: &str, pr: i64, status: &str, head: &str) {
    let w = StateWriter::open(db).unwrap();
    w.append_event(&NewEvent {
        kind: "ci_completed",
        actor: Some("makibad"),
        payload: Some(
            &json!({"repo": repo, "pr": pr, "head_oid": head, "status": status,
                              "fail_count": 1, "pending_count": 0, "total_checks": 1,
                              "probe_attempts": 1}),
        ),
        ..Default::default()
    })
    .unwrap();
}

const TEST_RS: &str = "#[test]\nfn adds() {\n    assert_eq!(2 + 2, 4);\n}\n";
const DECLARES: &str = "test: the failing test\n\nAcceptance-Test: tests/adds.rs";

// ---- the pr verb -------------------------------------------------------------------------------

/// One pull request's world: an origin, the project's clone, `state.db`, a door and the secretary.
struct Case {
    _tmp: tempfile::TempDir,
    name: String,
    repo: String,
    origin: PathBuf,
    db: PathBuf,
    door: Door,
    secretary: String,
}

impl Case {
    fn new(name: &str) -> Case {
        Case::with_base(name, &[("src.txt", "base\n")])
    }

    /// The origin's `main` holds `files`; the work goes on a branch `feature` there. The project
    /// `name` has the URL `https://github.com/o/<name>`, and its clone maps that URL to the origin.
    fn with_base(name: &str, files: &[(&str, &str)]) -> Case {
        let root = root();
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        commit_in(&origin, "base", files);
        let url = format!("https://github.com/o/{name}");
        let clone = root.join("repos").join(name);
        assert!(!clone.exists(), "the case name {name} is used twice");
        git(
            &root.join("repos"),
            &["clone", "-q", origin.to_str().unwrap(), name],
        );
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
        let db = tmp.path().join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: name,
            title: Some("T"),
            status: Some("in_use"),
            branch: Some("feature"),
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
        Case {
            _tmp: tmp,
            name: name.into(),
            repo: format!("o/{name}"),
            origin,
            db,
            door,
            secretary,
        }
    }

    fn commit(&self, msg: &str, files: &[(&str, &str)]) -> String {
        commit_in(&self.origin, msg, files)
    }

    /// The test commit, then the fix: (M, head).
    fn test_then_fix(&self) -> (String, String) {
        let m = self.commit(DECLARES, &[("tests/adds.rs", TEST_RS)]);
        let head = self.commit("fix: the fix", &[("src.txt", "fixed\n")]);
        (m, head)
    }

    /// Publish HEAD as pull request `pr`'s head; its id.
    fn publish(&self, pr: i64) -> String {
        let head = git(&self.origin, &["rev-parse", "HEAD"]);
        git(
            &self.origin,
            &["update-ref", &format!("refs/pull/{pr}/head"), &head],
        );
        view(&self.name, pr, &head);
        head
    }

    fn pr(&self, state: &str, pr: i64, head: &str) -> Response {
        self.door.handle(
            Some(&self.secretary),
            "pr",
            json!({"state": state, "repo": self.repo, "pr": pr, "head": head, "task": "t1"}),
            None,
            false,
        )
    }

    fn ci(&self, pr: i64, status: &str, head: &str) {
        ci(&self.db, &self.repo, pr, status, head);
    }

    fn watches(&self) -> i64 {
        Connection::open(&self.db)
            .unwrap()
            .query_row("SELECT count(*) FROM pr_watches", [], |r| r.get(0))
            .unwrap()
    }
}

/// The refusal's text, or a panic naming what was let through.
fn refusal(r: &Response) -> String {
    match r {
        Ok(v) => panic!("let through: {v}"),
        Err(e) => format!("{e:?}"),
    }
}

fn assert_names_the_test_commit(why: &str, m: &str) {
    assert!(
        why.contains(&m[..12]),
        "the refusal does not name {m}: {why}"
    );
    assert!(
        why.contains("alone first"),
        "the refusal does not say to push it alone first: {why}"
    );
    assert!(
        why.contains("L-K9"),
        "the refusal does not name its line: {why}"
    );
}

/// The case of pull request 303: opened, then recorded, at a head past the test commit with no
/// failed run at it; refused both times, and nothing is watched.
#[test]
fn a_head_past_the_test_commit_with_no_failed_run_is_refused() {
    let c = Case::new("past");
    let (m, _) = c.test_then_fix();
    let head = c.publish(5);
    assert_names_the_test_commit(&refusal(&c.pr("opened", 5, &head)), &m);
    assert_names_the_test_commit(&refusal(&c.pr("pushed", 5, &head)), &m);
    assert_eq!(c.watches(), 0, "a refused record writes no watch");
}

/// At the test commit itself it is let through; a later head is refused until a failed run is
/// recorded at the test commit for that pull request, and let through after.
#[test]
fn the_test_commit_alone_then_a_failed_run_lets_the_later_head_through() {
    let c = Case::new("order");
    let m = c.commit(DECLARES, &[("tests/adds.rs", TEST_RS)]);
    assert_eq!(c.publish(5), m);
    let r = c.pr("opened", 5, &m);
    assert!(r.is_ok(), "opened at the test commit: {r:?}");
    c.commit("fix: the fix", &[("src.txt", "fixed\n")]);
    let head = c.publish(5);
    assert_names_the_test_commit(&refusal(&c.pr("pushed", 5, &head)), &m);
    c.ci(5, "failed", &m);
    let r = c.pr("pushed", 5, &head);
    assert!(
        r.is_ok(),
        "pushed after the failed run at the test commit: {r:?}"
    );
    assert_eq!(c.watches(), 2);
}

/// A failed run at the test commit recorded for another pull request does not count.
#[test]
fn a_failed_run_on_another_pull_request_does_not_count() {
    let c = Case::new("other");
    let (m, _) = c.test_then_fix();
    c.ci(4, "failed", &m);
    let head = c.publish(6);
    assert_names_the_test_commit(&refusal(&c.pr("opened", 6, &head)), &m);
}

/// Pull request 303's repository has its own acceptance check: not exempt.
#[test]
fn a_base_with_its_own_acceptance_check_is_not_exempt() {
    let c = Case::with_base(
        "owncheck",
        &[
            ("src.txt", "base\n"),
            (".github/acceptance-check", "acceptance\n"),
            (".github/workflows/ci.yml", "on: pull_request\n"),
        ],
    );
    let (m, _) = c.test_then_fix();
    let head = c.publish(7);
    assert_names_the_test_commit(&refusal(&c.pr("opened", 7, &head)), &m);
}

/// No declared test, or documents only: let through.
#[test]
fn a_branch_with_no_declared_test_or_only_documents_is_let_through() {
    let c = Case::new("plain");
    c.commit("fix: no test", &[("src.txt", "fixed\n")]);
    c.commit("fix: more", &[("src.txt", "fixed again\n")]);
    let head = c.publish(8);
    let r = c.pr("opened", 8, &head);
    assert!(r.is_ok(), "no declared test: {r:?}");

    let d = Case::new("docsonly");
    d.commit("docs: a page", &[("docs/page.md", "# Page\n")]);
    d.commit("docs: another", &[("README.md", "# Read me\n")]);
    let head = d.publish(9);
    let r = d.pr("opened", 9, &head);
    assert!(r.is_ok(), "documents only: {r:?}");
}

/// The shape of pull request 308: the `Acceptance-Test:` line, then a blank line, then
/// `Co-Authored-By:`. git reads no trailer of that key, and the record is refused naming the
/// commit.
#[test]
fn an_acceptance_test_line_git_does_not_read_as_a_trailer_is_refused() {
    let c = Case::new("stray");
    let m = c.commit(
        "test: the failing test\n\nAcceptance-Test: tests/adds.rs\n\nCo-Authored-By: A <a@example.com>",
        &[("tests/adds.rs", TEST_RS)],
    );
    let read = git(
        &c.origin,
        &[
            "log",
            "-1",
            "--format=%(trailers:key=Acceptance-Test,valueonly)",
            &m,
        ],
    );
    assert_eq!(
        read, "",
        "git reads the line as a trailer; the case is wrong"
    );
    c.commit("fix: the fix", &[("src.txt", "fixed\n")]);
    let head = c.publish(10);
    let why = refusal(&c.pr("opened", 10, &head));
    assert!(
        why.contains(&m[..12]),
        "the refusal does not name {m}: {why}"
    );
    assert!(
        why.contains("trailer"),
        "the refusal does not say why: {why}"
    );
    assert_eq!(c.watches(), 0);
}

/// The repository is a project's, but its pull request cannot be read: refused, not let through.
#[test]
fn a_pull_request_that_cannot_be_read_is_refused() {
    let c = Case::new("unread");
    let (_, head) = c.test_then_fix();
    // No `view.11` answer: the stand-in fails as `gh` does for a pull request it cannot read.
    let r = c.pr("opened", 11, &head);
    assert!(r.is_err(), "let through unread: {r:?}");
    assert_eq!(c.watches(), 0);
}

/// A repository that is no project's is let through: the merge gate cannot pass it anyway.
#[test]
fn a_repository_that_is_no_projects_is_let_through() {
    let c = Case::new("noproject");
    let r = c.door.handle(
        Some(&c.secretary),
        "pr",
        json!({"state": "opened", "repo": "o/elsewhere-307", "pr": 3,
               "head": "a".repeat(40), "task": "t1"}),
        None,
        false,
    );
    assert!(r.is_ok(), "{r:?}");
}

// ---- the push and update verbs -----------------------------------------------------------------

/// A running `makibad`: its stop flag and its thread.
type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

/// A code run `task` of the project `task` (no project URL, so the pr verb has no clone to read
/// for `o/<task>`), whose worktree is on `makiba/<task>`, made from `main` of the project's
/// repository, whose origin is a local bare repository of its own.
struct Org {
    task: String,
    repo: String,
    branch: String,
    clone: PathBuf,
    origin: PathBuf,
    state: PathBuf,
    worktree: PathBuf,
    door: Door,
    secretary: String,
    daemon: Option<Daemon>,
}

impl Drop for Org {
    fn drop(&mut self) {
        self.stop();
    }
}

fn org(task: &str) -> Org {
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
    let gitdir = makiba::daemon::run_repo::dir(&state.join("settings"), task);
    makiba::daemon::run_repo::create(
        &clone,
        &gitdir,
        &worktree,
        &branch,
        "main",
        &state.join("git-hooks"),
    )
    .unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let run = w
        .upsert_run(&RunUpsert {
            task_id: task,
            project_slug: task,
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
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    Org {
        task: task.into(),
        repo: format!("o/{task}"),
        branch,
        clone,
        origin,
        state: state.clone(),
        worktree,
        door: Door::new(w, Arc::new(Stub::new()), "fake".into()),
        secretary,
        daemon: None,
    }
}

impl Org {
    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, flag) = (self.state.clone(), stop.clone());
        let d =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(Stub::new()), flag));
        self.daemon = Some((stop, d));
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

    fn stop(&mut self) {
        if let Some((stop, d)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = d.join();
        }
    }

    fn conn(&self) -> rusqlite::Result<Connection> {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
    }

    fn commit(&self, msg: &str, files: &[(&str, &str)]) -> String {
        commit_in(&self.worktree, msg, files)
    }

    fn call(&self, verb: &str, args: Value) -> Response {
        self.door
            .handle(Some(&self.secretary), verb, args, None, true)
    }

    /// Pull request 1 of `o/<task>` opened at `head` (before makibad starts), with the stand-in's
    /// answer for it.
    fn opened(&self, head: &str) {
        view(&self.task, 1, head);
        let r = self.door.handle(
            Some(&self.secretary),
            "pr",
            json!({"state": "opened", "repo": self.repo, "pr": 1, "head": head, "task": self.task}),
            None,
            false,
        );
        assert!(r.is_ok(), "pr opened at {head}: {r:?}");
    }

    /// A failed CI run at `head` on pull request 1, written while makibad is stopped.
    fn failed_at(&self, head: &str) {
        ci(&self.state.join("state.db"), &self.repo, 1, "failed", head);
    }

    /// The status of this task's newest `verb` command once makibad has finished it.
    fn finished(&self, verb: &str) -> String {
        let sql = "SELECT status FROM commands WHERE verb = ?1
                   AND json_extract(args_json, '$.task') = ?2 ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let s: Option<String> = self
                .conn()
                .and_then(|c| {
                    c.query_row(sql, [verb, &self.task], |r| r.get(0))
                        .optional()
                })
                .unwrap_or(None);
            if let Some(s) = s.filter(|s| s != "accepted") {
                return s;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the {verb} of {} is not finished after {WAIT:?}", self.task);
    }

    /// The commit the branch names at the origin, if it is there.
    fn at_origin(&self) -> Option<String> {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(&self.origin)
            .args([
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{}", self.branch),
            ])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The bodies of makibad's notices to the secretary about this run.
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

    fn last_notice(&self) -> String {
        self.notices().pop().unwrap_or_default()
    }
}

/// The push of a worktree past its test commit, with no failed run at it: refused, origin has
/// nothing, and the secretary is told the test commit and to push it alone first.
#[test]
fn a_push_past_the_test_commit_with_no_failed_run_is_refused() {
    let mut o = org("push-past");
    let m = o.commit(DECLARES, &[("tests/adds.rs", TEST_RS)]);
    o.commit("fix: the fix", &[("file.txt", "fixed\n")]);
    o.start();
    let r = o.call("push", json!({"task": o.task}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(o.finished("push"), "failed");
    assert_eq!(o.at_origin(), None, "nothing reaches the origin");
    assert_names_the_test_commit(&o.last_notice(), &m);
}

/// `push` with `head` the test commit sends it alone; the rest is refused until a failed run is
/// recorded at it on the task's pull request, and sent after.
#[test]
fn the_test_commit_is_pushed_alone_and_the_rest_after_its_failed_run() {
    let mut o = org("push-order");
    let m = o.commit(DECLARES, &[("tests/adds.rs", TEST_RS)]);
    let head = o.commit("fix: the fix", &[("file.txt", "fixed\n")]);
    o.opened(&m);
    o.start();
    let r = o.call("push", json!({"task": o.task, "head": m}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(o.finished("push"), "done", "{:?}", o.notices());
    assert_eq!(o.at_origin(), Some(m.clone()), "the test commit alone");

    let r = o.call("push", json!({"task": o.task}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(o.finished("push"), "failed");
    assert_eq!(
        o.at_origin(),
        Some(m.clone()),
        "the origin keeps the test commit"
    );
    assert_names_the_test_commit(&o.last_notice(), &m);

    o.stop();
    o.failed_at(&m);
    o.start();
    let r = o.call("push", json!({"task": o.task}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(o.finished("push"), "done", "{:?}", o.notices());
    assert_eq!(o.at_origin(), Some(head));
}

/// `push` with a `head` that is not the branch's test commit is refused, and nothing is pushed.
#[test]
fn a_push_of_a_head_that_is_not_the_test_commit_is_refused() {
    let mut o = org("push-other");
    let first = o.commit("feat: a first change", &[("file.txt", "one\n")]);
    o.commit(DECLARES, &[("tests/adds.rs", TEST_RS)]);
    o.commit("fix: the fix", &[("file.txt", "fixed\n")]);
    o.start();
    let r = o.call("push", json!({"task": o.task, "head": first}));
    if r.is_ok() {
        assert_eq!(o.finished("push"), "failed");
    }
    assert_eq!(o.at_origin(), None, "nothing reaches the origin");
}

/// `update` of a branch at its test commit, with no failed run there, would push a merge past it:
/// refused, the origin keeps the test commit. After the failed run, the update goes through.
#[test]
fn an_update_past_the_test_commit_waits_for_its_failed_run() {
    let mut o = org("update-order");
    let m = o.commit(DECLARES, &[("tests/adds.rs", TEST_RS)]);
    o.opened(&m);
    o.start();
    let r = o.call("push", json!({"task": o.task}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(o.finished("push"), "done", "{:?}", o.notices());
    assert_eq!(o.at_origin(), Some(m.clone()));

    // The base moves at the origin.
    let moved = commit_in(&o.clone, "base: moved", &[("other.txt", "other\n")]);
    git(&o.clone, &["push", "-q", "origin", "main"]);

    let r = o.call("update", json!({"task": o.task, "base": "main"}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(o.finished("update"), "failed");
    assert_eq!(
        o.at_origin(),
        Some(m.clone()),
        "the origin keeps the test commit"
    );
    assert_names_the_test_commit(&o.last_notice(), &m);

    o.stop();
    o.failed_at(&m);
    o.start();
    let r = o.call("update", json!({"task": o.task, "base": "main"}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(o.finished("update"), "done", "{:?}", o.notices());
    let now = o.at_origin().expect("the branch is at the origin");
    assert_ne!(now, m);
    git(&o.clone, &["fetch", "-q", "origin"]);
    for c in [&m, &moved] {
        git(&o.clone, &["merge-base", "--is-ancestor", c, &now]);
    }
}
