//! Issue 241: a code run shares no writable `.git` with the project clone. A code run that
//! `makibad` prepares has a repository of its own: its git folder is neither the clone's `.git`
//! nor inside the clone or the workplace, the run's settings give no write to the clone (no
//! `allowWrite` entry is the clone, inside it or above it) and deny the run's own configuration
//! for writing; a commit in the workplace works and leaves the clone's refs and objects as they
//! were; and the `push` verb still sends that commit to the remote.
//!
//! `makibad` runs as its own process (`MAKIBA_DOOR=none`, the stand-in base, the `fake` agent),
//! with its temporary folder beside the repositories, not above them; the door runs in this
//! process on the same `state.db`, as the secretary's tool call. The remote is a local bare
//! repository: no network.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);
const TASK: &str = "own1";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The clone `repos/p` with one commit on `main`, pushed to the bare `origin.git` and fetched
/// back; a state folder with a queued code run of `TASK` and its `planned` launch; the
/// secretary's key.
struct Org {
    _dir: tempfile::TempDir,
    root: PathBuf,
    clone: PathBuf,
    origin: PathBuf,
    secretary: String,
}

impl Org {
    fn new() -> Org {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let clone = root.join("repos/p");
        std::fs::create_dir_all(&clone).unwrap();
        std::fs::write(clone.join("README.md"), "readme\n").unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["add", "README.md"]);
        git(&clone, &["commit", "-q", "-m", "first"]);
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &clone,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&clone, &["push", "-q", "origin", "main"]);
        git(&clone, &["fetch", "-q", "origin"]);
        std::fs::create_dir_all(root.join("tmp")).unwrap();
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: TASK,
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'code', 'fake', 'b', ?3)",
                rusqlite::params![
                    TASK,
                    format!("spawn-{TASK}"),
                    root.join("workers/p").join(TASK).to_string_lossy()
                ],
            )
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        Org {
            _dir: dir,
            root,
            clone,
            origin,
            secretary,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn start(&self) -> Daemon {
        Daemon(
            makibad::daemon(&self.root)
                .env("MAKIBA_STATE_DIR", self.state())
                .env("MAKIBA_DOOR", "none")
                .env("MAKIBA_PORT", "0")
                .env("MAKIBA_PERSON_KEY", self.root.join("key/person.key"))
                .env("MAKIBA_BASE", "stub")
                .env("MAKIBA_WORKERS_DIR", self.root.join("workers"))
                .env("MAKIBA_REPOS_DIR", self.root.join("repos"))
                .env("MAKIBA_DOCS_DIR", self.root.join("docs"))
                .env("TMPDIR", self.root.join("tmp"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn conn(&self) -> Connection {
        let c = Connection::open_with_flags(
            self.state().join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c
    }

    /// Wait until the launch is past `settings_ready`'s write; its workplace.
    fn prepared(&self) -> PathBuf {
        let until = Instant::now() + WAIT;
        loop {
            let (stage, worktree, why): (String, String, Option<String>) = self
                .conn()
                .query_row(
                    "SELECT stage, worktree, failed_reason FROM run_launch
                     WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                    [TASK],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert!(stage != "failed", "the launch failed: {why:?}");
            if !matches!(stage.as_str(), "planned" | "worktree_ready") {
                return PathBuf::from(worktree);
            }
            assert!(Instant::now() < until, "still {stage}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The status of the push command once `makibad` has finished it, and the notices.
    fn pushed(&self) -> (String, Vec<String>) {
        let sql = "SELECT status FROM commands WHERE verb = 'push'
                   AND json_extract(args_json, '$.task') = ?1 ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        loop {
            let c = self.conn();
            let s: Option<String> = c.query_row(sql, [TASK], |r| r.get(0)).optional().unwrap();
            if let Some(s) = s.filter(|s| s != "accepted") {
                let mut q = c
                    .prepare(
                        "SELECT body FROM messages WHERE kind = 'notice' AND sender = 'makibad'
                         ORDER BY id",
                    )
                    .unwrap();
                let notices = q
                    .query_map([], |r| r.get(0))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                return (s, notices);
            }
            assert!(Instant::now() < until, "the push is not finished");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
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

/// A settings path as the sandbox reads it: `~/` is the home folder; an existing path resolved.
fn resolved(entry: &str) -> PathBuf {
    let p = match entry.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap()).join(rest),
        None => PathBuf::from(entry),
    };
    std::fs::canonicalize(&p).unwrap_or(p)
}

/// The clone's refs and its object counts (loose and packed).
fn clone_state(clone: &Path) -> (String, Vec<String>) {
    let counts = git(clone, &["count-objects", "-v"])
        .lines()
        .filter(|l| l.starts_with("count:") || l.starts_with("in-pack:"))
        .map(String::from)
        .collect();
    (git(clone, &["for-each-ref"]), counts)
}

#[test]
fn a_code_run_has_a_repository_of_its_own_and_its_commit_still_reaches_the_remote() {
    let org = Org::new();
    let _d = org.start();
    let wt = std::fs::canonicalize(org.prepared()).unwrap();
    let clone = &org.clone;

    // 1. The run's git folder is its own: not the clone's, not in the clone, not in the
    //    workplace, and not a linked worktree of anything.
    let common = PathBuf::from(git(
        &wt,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let gitdir = PathBuf::from(git(
        &wt,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let common = std::fs::canonicalize(common).unwrap();
    assert_eq!(
        std::fs::canonicalize(&gitdir).unwrap(),
        common,
        "the workplace is a linked worktree of {}",
        common.display()
    );
    assert!(
        !common.starts_with(clone),
        "the run's git folder {} is the clone's",
        common.display()
    );
    assert!(
        !common.starts_with(&wt),
        "the run's git folder {} is inside the workplace",
        common.display()
    );

    // 2. The settings: no write entry is the clone, inside it or above it; the run's own
    //    configuration is denied for writing.
    let text = std::fs::read_to_string(org.state().join(format!("settings/{TASK}.json"))).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let fs = &v["sandbox"]["filesystem"];
    for entry in strings(&fs["allowWrite"]) {
        let p = resolved(&entry);
        assert!(
            !p.starts_with(clone) && !clone.starts_with(&p),
            "allowWrite {entry} reaches the clone {}: {text}",
            clone.display()
        );
    }
    let config = common.join("config");
    assert!(
        strings(&fs["denyWrite"])
            .iter()
            .any(|e| resolved(e) == config),
        "denyWrite does not name the run's configuration {}: {text}",
        config.display()
    );

    // 3. A commit in the workplace works, and the clone's refs and objects stay as they were.
    let before = clone_state(clone);
    std::fs::write(wt.join("work.txt"), "the worker's change\n").unwrap();
    git(&wt, &["add", "work.txt"]);
    git(&wt, &["commit", "-q", "-m", "the worker's commit"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    assert_eq!(
        clone_state(clone),
        before,
        "a commit in the workplace changed the clone"
    );

    // 4. The push verb sends the commit to the remote.
    let door = Door::new(
        StateWriter::open(&org.state().join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );
    let r = door.handle(
        Some(&org.secretary),
        "push",
        json!({"task": TASK}),
        None,
        true,
    );
    assert!(r.is_ok(), "{r:?}");
    let (status, notices) = org.pushed();
    assert_eq!(status, "done", "{notices:?}");
    let at_origin = git(
        &org.origin,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/makiba/{TASK}"),
        ],
    );
    assert_eq!(at_origin, head);
}
