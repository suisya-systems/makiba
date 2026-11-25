//! Issue 337: a declared test is not committed or pushed before its design is approved (line
//! L-K13 of docs/design/lines.md).
//!
//! A task dispatched with `design_first` needs the person's `yes` to a pending entry of stop kind
//! `design` before its test commit (a commit with an `Acceptance-Test:` trailer) goes to origin:
//! `makibad`'s push refuses the branch until then, naming the missing approval, and sends nothing.
//! The secretary's own `yes` is not the person's. A task not marked design-first, and a
//! design-first branch with no test commit, are not affected. The worker's commit hook refuses the
//! test commit in the same case, so it is not even made, and lets it through once the person has
//! said yes.
//!
//! Tests 1 to 5 run `makibad` in this process on the stand-in base, with the door on the same
//! `state.db` called as the secretary's and the person's tools, as `tests/push_verb.rs` does: the
//! project `p` is a git repository whose `main` is pushed to a local bare `origin`, no network.
//! The run is made by the door's `dispatch` before `makibad` starts; the test then writes the run
//! `in_use` and its launch `verified`, also before the start. Test 6 starts `makibad` as a process
//! that prepares three code runs, as `tests/secret_scan.rs` does, so that their worktrees carry
//! the hooks `makibad` writes.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

/// The message of a test commit that declares `tests/x.rs`.
const TEST_COMMIT: &str = "test\n\nAcceptance-Test: tests/x.rs";

fn git_out(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn head(wt: &Path) -> String {
    git(wt, &["rev-parse", "HEAD"])
}

/// The five context parts of a design question, its stop kind and the recommended answer.
fn design_question(task: &str) -> Value {
    json!({
        "action": "add",
        "task": task,
        "question": format!("Approve the design of {task}?"),
        "background": "the design is written and reviewed once",
        "if_yes": "the worker makes the test commit",
        "if_no": "the worker changes the design",
        "recommend": "yes",
        "why": "the review found nothing left",
        "stop_kind": "design",
        "recommend_answer": "yes",
    })
}

// ---- tests 1 to 5: makibad in this process, the push --------------------------------------------

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

/// The commit `refs/heads/<branch>` names at the origin, if it exists there.
fn at_origin(branch: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(root().join("origin.git"))
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
    person: String,
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

/// A code run of `task` dispatched by the secretary through the door (with `design_first` when
/// `design_first`), then made `in_use` with its launch `verified`, its worktree on
/// `makiba/<task>` made from `main`; the keys of the secretary and the person.
fn org(task: &str, design_first: bool) -> Org {
    let root = root();
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(&state).unwrap();
    let branch = format!("makiba/{task}");
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let door = Door::new(
        StateWriter::open(&state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );
    let mut args = json!({
        "kind": "code",
        "task_id": task,
        "title": format!("the task {task}"),
        "brief": "write the design first",
        "project": "p",
        "branch": branch,
    });
    if design_first {
        args["design_first"] = json!(true);
    }
    let r = door.handle(Some(&secretary), "dispatch", args, None, true);
    assert!(r.is_ok(), "dispatch: {r:?}");
    let worktree: String = w
        .conn()
        .query_row(
            "SELECT l.worktree FROM run_launch l JOIN runs r ON r.id = l.run_id
             WHERE r.task_id = ?1",
            [task],
            |r| r.get(0),
        )
        .unwrap();
    let worktree = PathBuf::from(worktree);
    let gitdir = makiba::daemon::run_repo::dir(&state.join("settings"), task);
    makiba::daemon::run_repo::create(
        &root.join("repos/p"),
        &gitdir,
        &worktree,
        &branch,
        "main",
        &state.join("git-hooks"),
    )
    .unwrap();
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some("in_use"),
        branch: Some(&branch),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "UPDATE run_launch SET stage = 'verified'
             WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
            [task],
        )
        .unwrap();
    Org {
        task: task.into(),
        branch,
        state,
        worktree,
        door,
        secretary,
        person,
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
        // The first `Resync` handled, so the door's writes cannot race makibad's start writes.
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

    /// A door call as the secretary's tool.
    fn secretary(&self, verb: &str, args: Value) -> Response {
        self.door
            .handle(Some(&self.secretary), verb, args, None, true)
    }

    /// A door call with the person's key, as `makiba` in the person's shell.
    fn person(&self, verb: &str, args: Value) -> Response {
        self.door
            .handle(Some(&self.person), verb, args, None, false)
    }

    /// The design question added and escalated by the secretary; its id.
    fn escalated_design_question(&self) -> i64 {
        let r = self
            .secretary("pending", design_question(&self.task))
            .unwrap();
        let id = r["id"].as_i64().unwrap();
        let r = self.secretary("pending", json!({"action": "escalate", "id": id}));
        assert!(r.is_ok(), "escalate: {r:?}");
        id
    }

    /// A commit in the worktree adding `name`, with `message`; its id.
    fn commit(&self, name: &str, message: &str) -> String {
        let path = self.worktree.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#[test]\nfn t() {}\n").unwrap();
        git(&self.worktree, &["add", name]);
        git(&self.worktree, &["commit", "-q", "-m", message]);
        head(&self.worktree)
    }

    /// The test commit: `tests/x.rs` declared by its trailer.
    fn test_commit(&self) -> String {
        self.commit("tests/x.rs", TEST_COMMIT)
    }

    /// `push` of `head` alone, as the secretary's tool; how `makibad` finished it.
    fn push(&self, head: &str) -> String {
        let r = self.secretary("push", json!({"task": self.task, "head": head}));
        assert!(r.is_ok(), "push: {r:?}");
        self.finished()
    }

    /// The status of this task's newest push command once `makibad` has finished it.
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

    fn branch_pushed(&self) -> i64 {
        self.conn()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'branch_pushed' AND r.task_id = ?1",
                [&self.task],
                |r| r.get(0),
            )
            .unwrap()
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

    /// Some notice holds every one of `words`.
    fn assert_notice(&self, words: &[&str]) {
        let notices = self.notices();
        assert!(
            notices.iter().any(|n| words.iter().all(|w| n.contains(w))),
            "no notice holds {words:?}: {notices:?}"
        );
    }
}

/// 1. The Issue 333 sequence: the design question is escalated, not yet answered, and the test
///    commit is pushed alone. Refused with the missing approval named, nothing at origin; after the
///    person's `yes` the same push goes.
#[test]
fn the_333_sequence_a_test_commit_before_the_designs_approval_is_refused_at_the_push() {
    let mut org = org("d1", true);
    let test = org.test_commit();
    org.start();
    let id = org.escalated_design_question();

    assert_eq!(org.push(&test), "failed");
    assert_eq!(
        at_origin(&org.branch),
        None,
        "the test commit reached origin"
    );
    assert_eq!(org.branch_pushed(), 0);
    org.assert_notice(&[
        "design",
        &format!("pending entry {id}"),
        "nothing was pushed",
    ]);

    let r = org.person("answer", json!({"id": id, "answer": "yes"}));
    assert!(r.is_ok(), "the person's answer: {r:?}");
    assert_eq!(org.push(&test), "done");
    assert_eq!(at_origin(&org.branch), Some(test));
}

/// 2. A design-first task with no design entry at all: refused, naming that none is recorded.
#[test]
fn a_design_first_task_with_no_design_entry_is_refused() {
    let mut org = org("d2", true);
    let test = org.test_commit();
    org.start();
    assert_eq!(org.push(&test), "failed");
    assert_eq!(
        at_origin(&org.branch),
        None,
        "the test commit reached origin"
    );
    org.assert_notice(&["no pending entry of stop kind design"]);
}

/// 3. The secretary answering the design question `yes` is not the person's approval.
#[test]
fn the_secretarys_yes_is_not_the_persons_approval() {
    let mut org = org("d3", true);
    let test = org.test_commit();
    org.start();
    let id = org.escalated_design_question();
    let r = org.secretary("answer", json!({"id": id, "answer": "yes"}));
    assert!(r.is_ok(), "the secretary's answer: {r:?}");
    assert_eq!(org.push(&test), "failed");
    assert_eq!(
        at_origin(&org.branch),
        None,
        "the test commit reached origin"
    );
    org.assert_notice(&["no yes by the person"]);
}

/// 4. A task dispatched without `design_first` pushes its test commit with no design entry.
#[test]
fn a_task_not_marked_design_first_is_not_affected() {
    let mut org = org("d4", false);
    let test = org.test_commit();
    org.start();
    assert_eq!(org.push(&test), "done");
    assert_eq!(at_origin(&org.branch), Some(test));
}

/// 5. A design-first branch whose commits declare no test (a design note) is pushed.
#[test]
fn a_design_first_branch_without_a_test_commit_goes() {
    let mut org = org("d5", true);
    let note = org.commit("docs/design/note.md", "the design note");
    org.start();
    let r = org.secretary("push", json!({"task": org.task}));
    assert!(r.is_ok(), "push: {r:?}");
    assert_eq!(org.finished(), "done");
    assert_eq!(at_origin(&org.branch), Some(note));
}

// ---- test 6: makibad as a process, the commit hook ----------------------------------------------

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A state folder and a project repository with one commit on `main`, under one temporary
/// folder; queued code runs are added with `run`.
struct Prepared {
    _dir: tempfile::TempDir,
    root: PathBuf,
    conn: Connection,
}

impl Prepared {
    fn new() -> Prepared {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repos/p");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("README.md"), "readme\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        drop(w);
        let conn = Connection::open(state.join("state.db")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        Prepared {
            _dir: dir,
            root,
            conn,
        }
    }

    fn db(&self) -> PathBuf {
        self.root.join("state/state.db")
    }

    /// A queued code run of `task` with its `planned` launch and its `delegate_sent`, which
    /// carries `design_first` when `design_first`.
    fn run(&self, task: &str, design_first: bool) {
        let w = StateWriter::open(&self.db()).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        let spawn = format!("spawn-{task}");
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'code', 'fake', 'b', ?3)",
                rusqlite::params![
                    task,
                    spawn,
                    self.root.join("workers/p").join(task).to_string_lossy()
                ],
            )
            .unwrap();
        let mut payload = json!({"task_id": task, "spawn_id": spawn, "kind": "code"});
        if design_first {
            payload["design_first"] = json!(true);
        }
        w.append_event(&NewEvent {
            kind: "delegate_sent",
            actor: Some("secretary"),
            payload: Some(&payload),
            run_task_id: Some(task),
            ..Default::default()
        })
        .unwrap();
    }

    fn start(&self) -> Process {
        Process(
            makibad::daemon(&self.root)
                .env("MAKIBA_STATE_DIR", self.root.join("state"))
                .env("MAKIBA_DOOR", "none")
                .env("MAKIBA_PORT", "0")
                .env("MAKIBA_PERSON_KEY", self.root.join("key/person.key"))
                .env("MAKIBA_BASE", "stub")
                .env("MAKIBA_WORKERS_DIR", self.root.join("workers"))
                .env("MAKIBA_REPOS_DIR", self.root.join("repos"))
                .env("MAKIBA_DOCS_DIR", self.root.join("docs"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    /// Wait until the task's launch is past `worktree_ready` (a launch that fails later, on the
    /// stand-in base, keeps its prepared worktree); its worktree.
    fn prepared(&self, task: &str) -> PathBuf {
        let until = Instant::now() + WAIT;
        loop {
            let (stage, worktree, why): (String, String, Option<String>) = self
                .conn
                .query_row(
                    "SELECT stage, worktree, failed_reason FROM run_launch
                     WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                    [task],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            if !matches!(stage.as_str(), "planned" | "worktree_ready") {
                let wt = PathBuf::from(worktree);
                assert!(
                    wt.join(".git").exists(),
                    "the launch of {task} is {stage} with no worktree: {why:?}"
                );
                return wt;
            }
            assert!(Instant::now() < until, "{task} is still {stage}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Stage `tests/x.rs` (a plain test) in `wt` and commit it with `message`.
fn try_test_commit(wt: &Path, message: &str) -> Output {
    std::fs::create_dir_all(wt.join("tests")).unwrap();
    std::fs::write(wt.join("tests/x.rs"), "#[test]\nfn t() {}\n").unwrap();
    git(wt, &["add", "--", "tests/x.rs"]);
    git_out(wt, &["commit", "-q", "-m", message])
}

/// 6. The worker's commit hook: before the person's `yes` a design-first task's test commit is
///    refused, in each spelling git reads as the trailer or a stray line of it; an ordinary commit
///    goes. Once the person has said `yes` through the door, the test commit is made. A task whose
///    design the person approved before its launch, and a task not marked design-first, make it.
#[test]
fn the_commit_hook_refuses_a_test_commit_until_the_persons_yes() {
    let org = Prepared::new();
    org.run("h1", true);
    org.run("h2", true);
    org.run("h3", false);
    // h2's design was answered yes by the person before its launch.
    org.conn
        .execute(
            "INSERT INTO pending (run_id, kind, question, context_json, status, answer,
                                  answered_by, answered_at)
             VALUES ((SELECT id FROM runs WHERE task_id = 'h2'), 'question',
                     'Approve the design of h2?', ?1, 'answered', 'yes', 'person',
                     strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            [
                json!({"background": "b", "if_yes": "y", "if_no": "n", "recommend": "yes",
                    "why": "w", "kind": "design", "recommend_answer": "yes"})
                .to_string(),
            ],
        )
        .unwrap();
    let (secretary, person) = {
        let w = StateWriter::open(&org.db()).unwrap();
        (
            issue_key(&w, Role::Secretary, "secretary", None, None).unwrap(),
            issue_key(&w, Role::Person, "person", None, None).unwrap(),
        )
    };
    let _d = org.start();

    let h1 = org.prepared("h1");
    let before = head(&h1);
    for message in [
        TEST_COMMIT,
        "test\n\nAcceptance-Test : tests/x.rs",
        "test\n\nacceptance-test:\ttests/x.rs",
    ] {
        let out = try_test_commit(&h1, message);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "the test commit {message:?} of h1 was made before the approval"
        );
        assert!(
            err.contains("L-K13"),
            "{message:?}: the refusal names no L-K13: {err}"
        );
        assert!(
            err.contains("design"),
            "{message:?}: the refusal names no design: {err}"
        );
        assert_eq!(head(&h1), before, "{message:?}: something was committed");
    }
    // An ordinary commit of another file goes.
    git(&h1, &["reset", "-q", "--", "tests/x.rs"]);
    std::fs::write(h1.join("note.md"), "a design note\n").unwrap();
    git(&h1, &["add", "--", "note.md"]);
    git(&h1, &["commit", "-q", "-m", "the design note"]);
    assert_ne!(head(&h1), before, "an ordinary commit was refused");

    // The person's yes, through the door, to the design question the secretary escalated.
    let door = Door::new(
        StateWriter::open(&org.db()).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );
    let r = door
        .handle(
            Some(&secretary),
            "pending",
            design_question("h1"),
            None,
            true,
        )
        .unwrap();
    let id = r["id"].as_i64().unwrap();
    let r = door.handle(
        Some(&secretary),
        "pending",
        json!({"action": "escalate", "id": id}),
        None,
        true,
    );
    assert!(r.is_ok(), "escalate: {r:?}");
    let r = door.handle(
        Some(&person),
        "answer",
        json!({"id": id, "answer": "yes"}),
        None,
        false,
    );
    assert!(r.is_ok(), "the person's answer: {r:?}");
    let noted = head(&h1);
    let until = Instant::now() + WAIT;
    loop {
        let out = try_test_commit(&h1, TEST_COMMIT);
        if out.status.success() {
            break;
        }
        assert!(
            Instant::now() < until,
            "the test commit of h1 is still refused after the person's yes: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_ne!(head(&h1), noted, "the test commit of h1 was not made");

    for task in ["h2", "h3"] {
        let wt = org.prepared(task);
        let before = head(&wt);
        let out = try_test_commit(&wt, TEST_COMMIT);
        assert!(
            out.status.success(),
            "the test commit of {task} was refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_ne!(head(&wt), before, "the test commit of {task} was not made");
    }
}
