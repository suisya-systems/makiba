//! Issue 300, after the diff review: git's own sequencer runs `git commit -n` as a child when a
//! rebase continues after a conflict. In a worktree prepared by a real `makibad`, whose
//! `prepare-commit-msg` refuses that flag, `git rebase --continue` and `git cherry-pick
//! --continue` still land; a commit with the flag that `git rebase --exec` starts is still
//! refused, through a shell and as the rebase's direct child (the shell's `exec`).
//!
//! Linux only, as the flag check is.
#![cfg(target_os = "linux")]

mod common;

use common::makibad;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const REFUSED_BY_GIT: &str =
    "makiba: refused by hook prepare-commit-msg (line L-K3 in docs/design/lines.md)";

/// A `makibad` that is killed when the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// git in `dir` with a fixed identity and no editor.
fn git_out(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_EDITOR", "true")
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
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A state folder and a project whose `main` holds a file the run will change on both sides, and
/// a queued code run with its `planned` launch (as in tests/secret_scan.rs).
struct Org {
    _dir: tempfile::TempDir,
    root: PathBuf,
    conn: Connection,
}

impl Org {
    fn new(task: &str) -> Org {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repos/p");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("f.txt"), "base\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        drop(w);
        let conn = Connection::open(state.join("state.db")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'code', 'fake', 'b', ?3)",
            rusqlite::params![
                task,
                format!("spawn-{task}"),
                root.join("workers/p").join(task).to_string_lossy()
            ],
        )
        .unwrap();
        Org {
            _dir: dir,
            root,
            conn,
        }
    }

    fn start(&self) -> Daemon {
        Daemon(
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

    /// Wait until the task's launch is `settings_ready`; its worktree.
    fn prepared(&self, task: &str) -> PathBuf {
        let until = Instant::now() + Duration::from_secs(60);
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
            if stage == "settings_ready" {
                return PathBuf::from(worktree);
            }
            assert!(stage != "failed", "the launch failed: {why:?}");
            assert!(Instant::now() < until, "still {stage}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Commit `text` as f.txt on the current branch.
fn change(wt: &Path, text: &str, message: &str) {
    std::fs::write(wt.join("f.txt"), text).unwrap();
    git(wt, &["commit", "-q", "-a", "-m", message]);
}

#[test]
fn a_rebase_or_cherry_pick_continued_after_a_conflict_lands_and_an_exec_commit_with_the_flag_does_not()
 {
    let org = Org::new("t1");
    let _d = org.start();
    let wt = org.prepared("t1");
    let work = git(&wt, &["branch", "--show-current"]);

    // Two branches that change the same line: one at the run's branch, one at `other`.
    git(&wt, &["branch", "other"]);
    change(&wt, "ours\n", "ours");
    git(&wt, &["checkout", "-q", "other"]);
    change(&wt, "theirs\n", "theirs");
    git(&wt, &["checkout", "-q", &work]);

    // A rebase stops on the conflict; resolved and continued, it lands.
    let out = git_out(&wt, &["rebase", "other"]);
    assert!(
        !out.status.success(),
        "the rebase did not stop on the conflict"
    );
    std::fs::write(wt.join("f.txt"), "resolved\n").unwrap();
    git(&wt, &["add", "f.txt"]);
    let out = git_out(&wt, &["rebase", "--continue"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "git rebase --continue failed: {err}");
    assert!(!err.contains(REFUSED_BY_GIT), "{err}");
    assert_eq!(git(&wt, &["log", "-1", "--format=%s"]), "ours");
    assert_eq!(git(&wt, &["show", "HEAD:f.txt"]), "resolved");

    // A cherry-pick stops on a conflict; resolved and continued, it lands.
    git(&wt, &["checkout", "-q", "other"]);
    change(&wt, "theirs again\n", "theirs again");
    git(&wt, &["checkout", "-q", &work]);
    let out = git_out(&wt, &["cherry-pick", "other"]);
    assert!(!out.status.success(), "the cherry-pick did not stop");
    std::fs::write(wt.join("f.txt"), "resolved again\n").unwrap();
    git(&wt, &["add", "f.txt"]);
    let out = git_out(&wt, &["cherry-pick", "--continue"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "git cherry-pick --continue failed: {err}"
    );
    assert_eq!(git(&wt, &["log", "-1", "--format=%s"]), "theirs again");

    // A commit with the flag that rebase --exec starts is still refused: through a shell, and
    // when the shell's `exec` makes it the rebase's direct child.
    let before = git(&wt, &["rev-parse", "HEAD"]);
    let commit = "git -c user.name=t -c user.email=t@example.com commit --allow-empty -n -m y";
    for exec in [commit.to_string(), format!("exec {commit}")] {
        let out = git_out(&wt, &["rebase", "-x", &exec, "HEAD~1"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "the exec commit with the flag landed: {exec}"
        );
        assert!(
            err.contains(REFUSED_BY_GIT) && err.contains("Matched: \"-n\""),
            "{exec}: {err}"
        );
        git(&wt, &["rebase", "--abort"]);
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), before, "{exec}");
    }
}
