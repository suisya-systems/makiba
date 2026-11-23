//! Issue 345: a run's current pull request is the repository and number of its newest
//! registration (`pr opened` or `pr pushed`), and only a merge of that pull request is its
//! landing.
//!
//! - A pull request registered again after another (A, then B, then A again) is current, though
//!   `prwatch::register` leaves A's watch row older than B's: A's landing is recorded and B's is
//!   neither read nor recorded.
//! - A pull request replaced by one of another repository with the same number: the merge of the
//!   old one is not the landing of the new one, which is read and recorded on its own.
//!
//! The belt's pass runs in this process on a state database of its own (makibad is not started).
//! Each task is a `completed` run of an `ended` range, so the landing read takes it and nothing is
//! read, filled or woken otherwise. `gh` is a stand-in script (`MAKIBA_GH`) that answers
//! `gh pr view` from files in a folder per repository, logs every call and refuses anything else.

use makiba::daemon::belt::Belt;
use makiba::daemon::prwatch;
use makiba::db::writer::StateWriter;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<n> ...: the file pr-<n>.json of the repository's folder,
# else a failure. Every call is logged whole, one line, in the repository's folder (calls).
all="$*"
what="$1 $2"; shift 2
case "$what" in
  "pr view") num=$1; shift ;;
  *) echo "unexpected call: $what" >&2; exit 2 ;;
esac
repo=
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --json) shift 2 ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done
case "$repo" in github.com/*/*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
r=${repo#github.com/}
dir="$FAKE_GH_DIR/${r%%/*}_${r#*/}"
mkdir -p "$dir"
echo "$all" >> "$dir/calls"
f="$dir/pr-$num.json"
if [ -e "$f" ]; then cat "$f"; else echo "not found" >&2; exit 1; fi
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The folder the tests share (the stand-in, its answers and an empty folder of clones), with the
/// environment set once before any test starts a thread.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for d in ["answers", "repos"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_LANDING_RETRY_MS", "0");
            std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
}

/// The stand-in's folder of `repo` (`o/n`).
fn answers(home: &Path, repo: &str) -> PathBuf {
    let dir = home.join("answers").join(repo.replace('/', "_"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The stand-in answers `repo#pr` merged with the merge commit `merge`.
fn merged(home: &Path, repo: &str, pr: i64, merge: &str) {
    std::fs::write(
        answers(home, repo).join(format!("pr-{pr}.json")),
        json!({"state": "MERGED", "mergedAt": "2026-11-20T00:00:00Z",
               "title": format!("{repo}#{pr}"), "mergeCommit": {"oid": merge}})
        .to_string(),
    )
    .unwrap();
}

/// The calls the stand-in logged for `repo`.
fn calls(home: &Path, repo: &str) -> Vec<String> {
    std::fs::read_to_string(answers(home, repo).join("calls"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// A state database with one project and an ended range, and a belt on it.
struct Org {
    _tmp: tempfile::TempDir,
    w: StateWriter,
    belt: Belt,
    project: i64,
    range: i64,
}

impl Org {
    fn new(home: &Path) -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
        let project = w
            .ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO ranges (project_id, at_once, budget_tasks, brief_head, proposed_by,
                                     status, ended_reason, ended_at)
                 VALUES (?1, 1, 5, 'head', 'secretary', 'ended', 'person',
                         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [project],
            )
            .unwrap();
        let range = w.conn().last_insert_rowid();
        let belt = Belt::from_env(home.join("repos"));
        Org {
            _tmp: tmp,
            w,
            belt,
            project,
            range,
        }
    }

    /// A run of the range for `task`, closed `completed`.
    fn completed_run(&self, task: &str) {
        self.w
            .conn()
            .execute(
                "INSERT INTO runs (task_id, project_id, pattern, title, status, range_id)
                 VALUES (?1, ?2, 'B', ?1, 'completed', ?3)",
                rusqlite::params![task, self.project, self.range],
            )
            .unwrap();
    }

    fn register(&self, kind: &str, task: &str, repo: &str, pr: i64, head: &str) {
        prwatch::register(&self.w, kind, task, repo, pr, head).unwrap();
    }

    /// (repo, pr, merge commit) of every `pr_merged` of `task`, oldest first.
    fn merges(&self, task: &str) -> Vec<(String, i64, String)> {
        let mut q = self
            .w
            .conn()
            .prepare(
                "SELECT e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'pr_merged' AND r.task_id = ?1 ORDER BY e.id",
            )
            .unwrap();
        q.query_map([task], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| {
                let v: Value = serde_json::from_str(&p.unwrap()).unwrap();
                (
                    v["repo"].as_str().unwrap_or_default().to_string(),
                    v["pr"].as_i64().unwrap_or_default(),
                    v["merge_commit"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// Run the belt's pass until `ok` holds; panics after `WAIT`.
    fn pass_until(&mut self, what: &str, mut ok: impl FnMut(&Org) -> bool) {
        let until = Instant::now() + WAIT;
        loop {
            self.belt.pass(&self.w).unwrap();
            if ok(self) {
                return;
            }
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Run the belt's pass for `d`.
    fn pass_for(&mut self, d: Duration) {
        let until = Instant::now() + d;
        while Instant::now() < until {
            self.belt.pass(&self.w).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A, then B, then A again: A is current (its watch row is older than B's), its landing is
/// recorded, and B's is neither read nor recorded.
#[test]
fn a_pull_request_registered_again_is_current_and_the_landing_of_the_one_it_left_is_ignored() {
    let home = init();
    let mut org = Org::new(&home);
    org.completed_run("a");
    let (h1, h2) = ("1".repeat(40), "2".repeat(40));
    let (m11, m12) = ("b".repeat(40), "c".repeat(40));
    merged(&home, "o/p", 11, &m11);
    merged(&home, "o/p", 12, &m12);
    org.register("pr_opened", "a", "o/p", 11, &h1);
    org.register("pr_opened", "a", "o/p", 12, &h2);
    org.register("fix_pushed", "a", "o/p", 11, &h1);

    org.pass_until("a landing of a", |o| !o.merges("a").is_empty());
    assert_eq!(
        org.merges("a"),
        vec![("o/p".to_string(), 11, m11.clone())],
        "the landing of the pull request registered last"
    );
    org.pass_for(Duration::from_secs(1));
    assert_eq!(
        org.merges("a"),
        vec![("o/p".to_string(), 11, m11.clone())],
        "no landing of the pull request it left"
    );
    let calls = calls(&home, "o/p");
    assert!(
        !calls.iter().any(|c| c.contains("pr view 12")),
        "the pull request it left is not read: {calls:?}"
    );
}

/// old/p#11 replaced by new/p#11: the merge of old/p#11 is not the landing of new/p#11, which is
/// read and recorded with its own merge commit.
#[test]
fn a_merge_in_another_repository_with_the_same_number_is_not_the_landing() {
    let home = init();
    let mut org = Org::new(&home);
    org.completed_run("b");
    let (h1, h2) = ("3".repeat(40), "4".repeat(40));
    let (m_old, m_new) = ("d".repeat(40), "e".repeat(40));
    merged(&home, "old/p", 11, &m_old);
    merged(&home, "new/p", 11, &m_new);
    org.register("pr_opened", "b", "old/p", 11, &h1);

    org.pass_until("the landing of old/p#11", |o| !o.merges("b").is_empty());
    assert_eq!(
        org.merges("b"),
        vec![("old/p".to_string(), 11, m_old.clone())]
    );

    org.register("pr_opened", "b", "new/p", 11, &h2);
    org.pass_until("the landing of new/p#11", |o| o.merges("b").len() >= 2);
    assert_eq!(
        org.merges("b"),
        vec![
            ("old/p".to_string(), 11, m_old.clone()),
            ("new/p".to_string(), 11, m_new.clone()),
        ],
        "the replacing pull request lands on its own"
    );
    let calls = calls(&home, "new/p");
    assert!(
        calls.iter().any(|c| c.contains("pr view 11")),
        "new/p#11 is read: {calls:?}"
    );
}
