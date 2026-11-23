//! Issue 355: every git call that finds a project's repository has a deadline.
//!
//! - A dispatch that names an Issue finds the project's repository through its clone's git
//!   configuration when no URL is recorded. A git whose background child keeps stdout open after
//!   git itself has exited is ended, child and all, at the read's deadline, and the dispatch goes
//!   in with the Issue said to be unreadable.
//! - A code dispatch without a project is refused before anything is read: no git, no GitHub.
//!
//! The door is called in this process on the stand-in base; `makibad` is not started. `git` is a
//! stand-in found first on `PATH`, and `gh` one named by `MAKIBA_GH`; both record each call, so
//! no test reaches a real repository or the network.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

const BRIEF: &str = "Secretary brief.";

/// The read's deadline is 20 s (`ISSUE_READ_TIMEOUT`); this leaves a margin under the 30 s
/// `makiba dispatch` waits for the door.
const ANSWER_WITHIN: Duration = Duration::from_secs(28);

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// A `git` that exits at once and writes nothing, leaving a background child that inherited
/// its stdout and keeps it open for a minute. `@ROOT@` is replaced by the shared folder.
const GIT: &str = r#"#!/bin/sh
echo "$PWD $*" >> "@ROOT@/git-calls"
sleep 60 &
echo $! >> "@ROOT@/git-children"
exit 0
"#;

/// A `gh` that records its call and fails.
const GH: &str = r#"#!/bin/sh
echo "$*" >> "@ROOT@/gh-calls"
exit 1
"#;

fn script(path: &Path, text: &str, root: &Path) {
    std::fs::write(path, text.replace("@ROOT@", &root.display().to_string())).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The folder the tests share: the stand-ins, their records, repositories and workplaces.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        for d in ["bin", "repos/p355", "repos/documents", "workers", "docs"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        script(&root.join("bin").join("git"), GIT, &root);
        script(&root.join("gh"), GH, &root);
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("PATH", path);
            std::env::set_var("MAKIBA_GH", root.join("gh"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
}

/// One organization, optionally with project `slug` recorded without a URL, and its door.
struct Org {
    _tmp: tempfile::TempDir,
    door: Door,
    secretary: String,
}

impl Org {
    fn new(slug: Option<&str>) -> Org {
        init();
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        if let Some(slug) = slug {
            w.ensure_project(slug, None, None).unwrap();
        }
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        Org {
            _tmp: tmp,
            door,
            secretary,
        }
    }

    fn dispatch(&self, args: Value) -> (Result<Value, String>, Duration) {
        let started = Instant::now();
        let r = self
            .door
            .handle(Some(&self.secretary), "dispatch", args, None, true)
            .map_err(|e| format!("{e:?}"));
        (r, started.elapsed())
    }
}

fn record(name: &str) -> String {
    std::fs::read_to_string(init().join(name)).unwrap_or_default()
}

/// Ended: no such process, or one that is dead and only waits to be reaped.
fn ended(pid: &str) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        // The state follows the command, which is in parentheses.
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|s| s == "Z" || s == "X"),
    }
}

#[test]
fn a_git_whose_child_keeps_stdout_open_is_ended_at_the_deadline() {
    let org = Org::new(Some("p355"));
    let (r, elapsed) = org.dispatch(json!({"kind": "code", "task_id": "t355", "title": "fix",
                                           "project": "p355", "brief": BRIEF, "issue": 7}));
    let answer = r.unwrap_or_else(|e| panic!("dispatch refused: {e}"));
    assert_eq!(answer["status"], json!("queued"), "{answer}");
    assert!(
        answer["issue_text"]
            .as_str()
            .is_some_and(|s| s.starts_with("unreadable")),
        "{answer}"
    );
    assert!(
        elapsed < ANSWER_WITHIN,
        "the dispatch answered after {elapsed:?}: the git lookup was not ended at its deadline"
    );
    let calls = record("git-calls");
    assert!(
        calls
            .lines()
            .any(|l| l.contains("p355") && l.contains("remote.origin.url")),
        "the lookup did not run git: {calls}"
    );
    let children = record("git-children");
    let pids: Vec<&str> = children.split_whitespace().collect();
    assert!(!pids.is_empty(), "git started no child");
    let until = Instant::now() + Duration::from_secs(2);
    while !pids.iter().all(|p| ended(p)) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    let alive: Vec<&&str> = pids.iter().filter(|p| !ended(p)).collect();
    assert!(alive.is_empty(), "git's child still runs: {alive:?}");
    let gh = record("gh-calls");
    assert!(!gh.contains("issue view"), "{gh}");
}

#[test]
fn a_code_dispatch_without_a_project_reads_nothing() {
    let org = Org::new(None);
    let (r, elapsed) = org.dispatch(json!({"kind": "code", "task_id": "t355b", "title": "fix",
                                           "brief": BRIEF, "issue": 7}));
    let e = r.expect_err("a code dispatch without a project is refused");
    assert!(e.contains("--project is required"), "{e}");
    assert!(
        elapsed < Duration::from_secs(5),
        "refused after {elapsed:?}"
    );
    let calls = record("git-calls");
    assert!(
        !calls.lines().any(|l| l.contains("documents")),
        "git ran for a dispatch that is refused: {calls}"
    );
    let gh = record("gh-calls");
    assert!(!gh.contains("issue view"), "{gh}");
}
