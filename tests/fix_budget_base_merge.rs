//! Issue 358: a merge of the base is not counted against a range task's fix budget.
//!
//! - A head recorded with `pr pushed` whose only new commit is a clean merge of the base (two
//!   parents: the head recorded before and a commit of the base, with the tree git's own merge
//!   makes) is recorded and watched as any push, but is not a fix: it never reaches
//!   `budget_fixes`, also when it is recorded again.
//! - A merge of the base that resolved a conflict is a fix; a clean merge that touches a file of
//!   the pull request is not.
//! - The stop says how many fixes, not how many pushes.
//!
//! The world is that of tests/pr_open_test_commit.rs: a local origin publishes
//! `refs/pull/<n>/head`, the project's clone sits in the repositories folder with its origin URL
//! mapped to the local origin, and `gh` is a stand-in (`MAKIBA_GH`) that answers `pr view` from
//! files. The task's run belongs to an active range written directly in `state.db`. No network
//! and no socket.

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

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
        common::exe::write(&gh, STAND_IN);
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

/// git in `dir`; with `ok` false a failure is allowed (a merge that stops at a conflict).
fn git_in(dir: &Path, args: &[&str], ok: bool) -> String {
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
            "-c",
            "merge.conflictstyle=merge",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        !ok || out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_in(dir, args, true)
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

const PR: i64 = 5;
const FIVE: &str = "one\ntwo\nthree\nfour\nfive\n";

/// One range task's pull request: an origin (work on `feature`, the base on `main`), the
/// project's clone, `state.db` with an active range of `budget_fixes` holding the run `t1`, a
/// door and the secretary.
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
    fn new(name: &str, budget_fixes: i64) -> Case {
        let root = root();
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        commit_in(&origin, "base", &[("src.txt", FIVE)]);
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
        w.conn()
            .execute(
                "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, budget_fixes,
                                     brief_head, proposed_by, approved_by, approved_at, status)
                 VALUES ((SELECT id FROM projects WHERE slug = ?1), '[358]', 1, 5, ?2,
                         'the brief head', 'secretary', 'person',
                         strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'active')",
                rusqlite::params![name, budget_fixes],
            )
            .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET range_id = last_insert_rowid() WHERE task_id = 't1'",
                [],
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

    /// A commit of the work on `feature`.
    fn work(&self, msg: &str, files: &[(&str, &str)]) -> String {
        commit_in(&self.origin, msg, files)
    }

    /// A commit on the base `main`, the work's branch checked out again after.
    fn base_moves(&self, msg: &str, files: &[(&str, &str)]) -> String {
        git(&self.origin, &["checkout", "-q", "main"]);
        let id = commit_in(&self.origin, msg, files);
        git(&self.origin, &["checkout", "-q", "feature"]);
        id
    }

    /// `main` merged into `feature` as `update` does: a merge commit git makes by itself.
    fn merge_base_in(&self) -> String {
        git(
            &self.origin,
            &["merge", "-q", "--no-ff", "--no-edit", "main"],
        );
        git(&self.origin, &["rev-parse", "HEAD"])
    }

    /// Publish HEAD as the pull request's head; its id.
    fn publish(&self) -> String {
        let head = git(&self.origin, &["rev-parse", "HEAD"]);
        git(
            &self.origin,
            &["update-ref", &format!("refs/pull/{PR}/head"), &head],
        );
        view(&self.name, PR, &head);
        head
    }

    fn pr(&self, state: &str, head: &str) -> Response {
        self.door.handle(
            Some(&self.secretary),
            "pr",
            json!({"state": state, "repo": self.repo, "pr": PR, "head": head, "task": "t1"}),
            None,
            false,
        )
    }

    /// Publish HEAD and record it with `state`; the record must be let through.
    fn record(&self, state: &str) -> String {
        let head = self.publish();
        let r = self.pr(state, &head);
        assert!(r.is_ok(), "pr {state} at {head}: {r:?}");
        head
    }

    fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    /// The keys of the stops of line `budget`, oldest first.
    fn budget_stops(&self) -> Vec<String> {
        let c = self.conn();
        let mut q = c
            .prepare(
                "SELECT json_extract(context_json, '$.key') FROM pending
                 WHERE json_extract(context_json, '$.stop_line') = 'budget' ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn fix_pushed(&self) -> i64 {
        self.conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'fix_pushed'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// `status` through the door.
    fn status(&self) -> Value {
        self.door
            .handle(Some(&self.secretary), "status", json!({}), None, false)
            .expect("status")
    }
}

/// The case of r3-317: one fix and two `update` merges of the base (one of them recorded twice)
/// are one fix, under `budget_fixes` 2. The next fix reaches it, and the stop counts fixes.
#[test]
fn one_fix_and_two_base_merges_are_not_stopped_at_budget_fixes_2() {
    let c = Case::new("onefix", 2);
    c.work("feat: the work", &[("work.txt", "work\n")]);
    c.record("opened");
    c.work("fix: the first fix", &[("work.txt", "work, fixed\n")]);
    c.record("pushed");
    c.base_moves("base: one", &[("base1.txt", "one\n")]);
    c.merge_base_in();
    let m1 = c.record("pushed");
    let r = c.pr("pushed", &m1);
    assert!(r.is_ok(), "the merge recorded again: {r:?}");
    c.base_moves("base: two", &[("base2.txt", "two\n")]);
    c.merge_base_in();
    c.record("pushed");
    assert_eq!(c.fix_pushed(), 4, "every push is recorded");
    assert_eq!(
        c.budget_stops(),
        Vec::<String>::new(),
        "merges of the base are not fixes"
    );

    c.work(
        "fix: the second fix",
        &[("work.txt", "work, fixed twice\n")],
    );
    c.record("pushed");
    assert_eq!(c.budget_stops(), vec!["fixes:t1:2".to_string()]);
    let status = c.status();
    let pending = status["pending"].as_array().cloned().unwrap_or_default();
    let questions: Vec<&str> = pending
        .iter()
        .filter_map(|p| p["question"].as_str())
        .collect();
    assert!(
        questions
            .iter()
            .any(|q| q.contains("2 fix(es)") && !q.contains("pushed")),
        "status shows the count as fixes: {questions:?}"
    );
}

/// Three fixes under `budget_fixes` 2: the second is the stop, the third adds none.
#[test]
fn three_fixes_are_stopped_at_budget_fixes_2() {
    let c = Case::new("threefixes", 2);
    c.work("feat: the work", &[("work.txt", "work\n")]);
    c.record("opened");
    c.work("fix: one", &[("work.txt", "1\n")]);
    c.record("pushed");
    assert_eq!(c.budget_stops(), Vec::<String>::new());
    c.work("fix: two", &[("work.txt", "2\n")]);
    c.record("pushed");
    assert_eq!(c.budget_stops(), vec!["fixes:t1:2".to_string()]);
    c.work("fix: three", &[("work.txt", "3\n")]);
    c.record("pushed");
    assert_eq!(c.budget_stops(), vec!["fixes:t1:2".to_string()]);
}

/// A merge of the base that resolved a conflict is work: it counts, under `budget_fixes` 1.
#[test]
fn a_merge_of_the_base_that_resolves_a_conflict_counts_as_a_fix() {
    let c = Case::new("conflict", 1);
    c.work(
        "feat: the work",
        &[("src.txt", "ONE\ntwo\nthree\nfour\nfive\n")],
    );
    c.record("opened");
    c.base_moves(
        "base: the same line",
        &[("src.txt", "uno\ntwo\nthree\nfour\nfive\n")],
    );
    git_in(
        &c.origin,
        &["merge", "-q", "--no-ff", "--no-edit", "main"],
        false,
    );
    std::fs::write(
        c.origin.join("src.txt"),
        "ONE and uno\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    git(&c.origin, &["add", "src.txt"]);
    git(&c.origin, &["commit", "-q", "--no-edit"]);
    let parents = git(&c.origin, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert_eq!(
        parents.split_whitespace().count(),
        3,
        "the case is a merge commit: {parents}"
    );
    c.record("pushed");
    assert_eq!(c.budget_stops(), vec!["fixes:t1:1".to_string()]);
}

/// A clean merge of a base that changed another line of a file the pull request changes is not
/// a fix, under `budget_fixes` 1.
#[test]
fn a_clean_merge_touching_the_same_file_is_not_counted() {
    let c = Case::new("samefile", 1);
    c.work(
        "feat: the work",
        &[("src.txt", "ONE\ntwo\nthree\nfour\nfive\n")],
    );
    c.record("opened");
    c.base_moves(
        "base: another line",
        &[("src.txt", "one\ntwo\nthree\nfour\nFIVE\n")],
    );
    c.merge_base_in();
    assert_eq!(
        std::fs::read_to_string(c.origin.join("src.txt")).unwrap(),
        "ONE\ntwo\nthree\nfour\nFIVE\n",
        "the case is a clean merge"
    );
    c.record("pushed");
    assert_eq!(c.fix_pushed(), 1);
    assert_eq!(c.budget_stops(), Vec::<String>::new());
}
