//! Issue 231: the `push` verb scans the commits it is about to send (what the remote does not hold
//! yet) with the pre-commit hook's scanner, and refuses with the commit, the file and the shape;
//! nothing is sent. This holds where the hook does not: the commits here are written with git's
//! plumbing, which runs no hook (the harness installs none either).
//!
//! The harness is that of `tests/push_verb.rs`: `makibad` and the door run in this process on the
//! stand-in base, the project `p` has a local bare `origin`, no network. One origin serves the
//! whole test binary, so every case uses a token text of its own (a blob of its own).
//!
//! The token-shaped test data is joined from parts at run time, so that no file in the repository
//! holds a line a scanner would flag.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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

/// A line shaped like an AWS access key id, distinct per `n` (an uppercase letter).
fn token(n: char) -> String {
    format!("key = {}{}QWERTYUIOPASDF{n}{n}", "AK", "IA")
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

/// Whether the origin holds the object `oid` at all.
fn origin_has(oid: &str) -> bool {
    Command::new("git")
        .arg("--git-dir")
        .arg(origin())
        .args(["cat-file", "-e", oid])
        .output()
        .unwrap()
        .status
        .success()
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
/// made from `main`, and the secretary's key; `makibad` started.
fn org(task: &str) -> Org {
    let root = root();
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(&state).unwrap();
    let branch = format!("makiba/{task}");
    let worktree = root.join("workers/p").join(task);
    // One at a time, as since Issue 240: the runs read the one clone.
    static ADD: Mutex<()> = Mutex::new(());
    let added = ADD.lock().unwrap_or_else(|e| e.into_inner());
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
    drop(added);
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
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let mut org = Org {
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
        daemon: None,
    };
    org.start();
    org
}

impl Org {
    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, flag) = (self.state.clone(), stop.clone());
        let d =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(Stub::new()), flag));
        self.daemon = Some((stop, d));
        // The first `Resync` handled (`base_resynced` is written last), so the door's writes
        // cannot race makibad's start writes (Issue 181).
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

    /// `verb` (`push` or `update`) called as a tool by the secretary.
    fn call(&self, verb: &str) -> Response {
        let args = match verb {
            "update" => json!({"task": self.task, "base": "main"}),
            _ => json!({"task": self.task}),
        };
        self.door
            .handle(Some(&self.secretary), verb, args, None, true)
    }

    /// `verb` called and finished by `makibad`: its final status.
    fn run(&self, verb: &str) -> String {
        let r = self.call(verb);
        assert!(r.is_ok(), "{verb}: {r:?}");
        self.finished(verb)
    }

    /// A commit written with git's plumbing (no hook runs): `files` added or replaced in the
    /// tree of HEAD, `message` as its message. Returns its id; the worktree is left clean.
    fn plumb(&self, files: &[(&str, &str)], message: &str) -> String {
        let wt = &self.worktree;
        for (name, text) in files {
            std::fs::write(wt.join(name), text).unwrap();
            let blob = git(wt, &["hash-object", "-w", name]);
            git(
                wt,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{blob},{name}"),
                ],
            );
        }
        let tree = git(wt, &["write-tree"]);
        let parent = git(wt, &["rev-parse", "HEAD"]);
        let commit = git(wt, &["commit-tree", &tree, "-p", &parent, "-m", message]);
        git(
            wt,
            &[
                "update-ref",
                &format!("refs/heads/{}", self.branch),
                &commit,
            ],
        );
        assert!(
            git(wt, &["status", "--porcelain", "--untracked-files=no"]).is_empty(),
            "the plumbing left the worktree dirty"
        );
        commit
    }

    /// The status of this task's newest `verb` command once `makibad` has finished it.
    fn finished(&self, verb: &str) -> String {
        let sql = "SELECT status FROM commands WHERE verb = ?2
                   AND json_extract(args_json, '$.task') = ?1 ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let s: Option<String> = self
                .conn()
                .and_then(|c| {
                    c.query_row(sql, [&self.task, verb], |r| r.get(0))
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

    /// How many events of `kind` this run has.
    fn events(&self, kind: &str) -> i64 {
        self.conn()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = ?1 AND r.task_id = ?2",
                [kind, &self.task],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The newest notice of `makibad` to the secretary about this run.
    fn last_notice(&self) -> String {
        self.conn()
            .unwrap()
            .query_row(
                "SELECT m.body FROM messages m JOIN runs r ON r.id = m.run_id
                 WHERE m.kind = 'notice' AND m.sender = 'makibad'
                   AND m.recipient = 'secretary' AND r.task_id = ?1 ORDER BY m.id DESC LIMIT 1",
                [&self.task],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The push or update was refused for the token `secret` in `commit` (in `file`, or in its
    /// message when `file` is `None`): the notice names the commit, the place and the shape,
    /// never the token; no success event.
    fn refused(&self, commit: &str, file: Option<&str>, secret: &str) {
        let n = self.last_notice();
        assert!(n.contains(commit), "the notice names no commit: {n}");
        assert!(
            n.contains(file.unwrap_or("message")),
            "the notice names no place: {n}"
        );
        assert!(
            n.contains("AWS access key"),
            "the notice names no shape: {n}"
        );
        assert!(!n.contains(secret), "the notice prints the token: {n}");
        assert_eq!(self.events("branch_pushed"), 0);
        assert_eq!(self.events("branch_updated"), 0);
    }
}

/// A branch whose history holds a token-shaped line, written with plumbing (no hook) and taken
/// out again by a later commit, so its head is clean: the push is refused and nothing reaches
/// the origin, neither the branch nor the commit.
#[test]
fn a_token_committed_without_a_hook_is_refused_and_nothing_reaches_the_remote() {
    let org = org("s1");
    let secret = token('B');
    let bad = org.plumb(&[("conf.txt", &format!("plain\n{secret}\n"))], "add conf");
    let clean = org.plumb(&[("conf.txt", "plain\n")], "take the key out");
    assert_eq!(org.run("push"), "failed");
    assert_eq!(at_origin(&org.branch), None);
    assert!(
        !origin_has(&bad),
        "the commit with the token reached the origin"
    );
    assert!(!origin_has(&clean));
    org.refused(&bad, Some("conf.txt"), &secret);
}

/// A token-shaped line in a commit message only: refused the same way.
#[test]
fn a_token_in_a_commit_message_is_refused() {
    let org = org("s2");
    let secret = token('C');
    let bad = org.plumb(&[("notes.txt", "notes\n")], &format!("notes\n\n{secret}"));
    assert_eq!(org.run("push"), "failed");
    assert_eq!(at_origin(&org.branch), None);
    assert!(!origin_has(&bad));
    org.refused(&bad, None, &secret);
}

/// A branch pushed clean once, then given a token commit: the second push is refused and the
/// origin keeps the first head.
#[test]
fn a_token_on_top_of_a_pushed_branch_is_refused_and_the_remote_keeps_its_head() {
    let org = org("s3");
    let first = org.plumb(&[("a.txt", "a\n")], "a");
    assert_eq!(org.run("push"), "done");
    assert_eq!(at_origin(&org.branch), Some(first.clone()));
    let secret = token('D');
    let bad = org.plumb(&[("b.txt", &secret)], "b");
    assert_eq!(org.run("push"), "failed");
    assert_eq!(at_origin(&org.branch), Some(first));
    assert!(!origin_has(&bad));
    let n = org.last_notice();
    assert!(n.contains(&bad) && n.contains("b.txt"), "{n}");
    assert!(!n.contains(&secret), "{n}");
}

/// `update` sends the head the same way, so it is scanned too: refused, the origin unchanged, no
/// `branch_updated`.
#[test]
fn an_update_that_would_send_a_token_is_refused() {
    let org = org("s4");
    let first = org.plumb(&[("u.txt", "u\n")], "u");
    assert_eq!(org.run("push"), "done");
    let secret = token('E');
    let bad = org.plumb(&[("v.txt", &secret)], "v");
    assert_eq!(org.run("update"), "failed");
    assert_eq!(at_origin(&org.branch), Some(first));
    assert!(!origin_has(&bad));
    assert_eq!(org.events("branch_updated"), 0);
    let n = org.last_notice();
    assert!(n.contains(&bad) && n.contains("v.txt"), "{n}");
    assert!(!n.contains(&secret), "{n}");
}

/// A clean branch is pushed as today: `done`, the origin at the head, `branch_pushed`.
#[test]
fn a_clean_branch_is_pushed_as_today() {
    let org = org("s5");
    org.plumb(&[("one.txt", "one\n")], "one");
    let head = org.plumb(&[("two.txt", "two\n")], "two");
    assert_eq!(org.run("push"), "done");
    assert_eq!(at_origin(&org.branch), Some(head));
    assert_eq!(org.events("branch_pushed"), 1);
}

/// A false positive a person pushed themselves is on the remote, so the verb does not read it
/// again: a clean commit on top is pushed.
#[test]
fn a_commit_a_person_pushed_themselves_is_not_refused_again() {
    let org = org("s6");
    let secret = token('F');
    org.plumb(&[("fixture.txt", &secret)], "a fixture the person allows");
    git(
        &org.worktree,
        &[
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{}", org.branch),
        ],
    );
    let head = org.plumb(&[("more.txt", "more\n")], "more");
    assert_eq!(org.run("push"), "done");
    assert_eq!(at_origin(&org.branch), Some(head));
}
