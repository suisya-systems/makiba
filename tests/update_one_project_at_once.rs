//! Issue 369: two `update`s of one project called at once both merge the same new base.
//!
//! On 2026-11-23 three updates of one project ran at once; each fetched `main` of the project's
//! repository into `refs/remotes/origin/main`, and git's ref lock let only one through: the other
//! two failed with "cannot lock ref ... is at <new> but expected <old>".
//!
//! The world: one `state.db` with two `in_use` code runs `u1` and `u2` of the project `p`, whose
//! repository sits under a temporary repositories folder with a local bare `origin`; no network,
//! no socket. Both branches are at origin; then `main` moves at origin. The project's repository
//! reaches origin through an `upload-pack` whose first call waits, up to ten seconds, for a second
//! call; the second sets a moment half a second ahead and both answer at that moment, so two
//! fetches that run at once go on together, as the three of the incident did. Fetches that run one
//! after another only cost the wait. `makibad` and the door run in this process.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::json;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);

/// What the project's repository runs in place of `git-upload-pack`, with the folder of its marks
/// set as `MARKS` before it: the first call waits for the second's moment (or ten seconds), the
/// second sets the moment, both sleep until it; every later call answers at once.
const MEET: &str = r#"
until_go() {
  go=$(cat "$MARKS/go")
  sleep "$(awk -v g="$go" -v n="$(date +%s.%N)" 'BEGIN { d = g - n; if (d < 0) d = 0; print d }')"
}
if mkdir "$MARKS/first" 2>/dev/null; then
  i=0
  while [ ! -f "$MARKS/go" ] && [ "$i" -lt 200 ]; do sleep 0.05; i=$((i + 1)); done
  if [ -f "$MARKS/go" ]; then until_go; fi
elif mkdir "$MARKS/second" 2>/dev/null; then
  awk -v n="$(date +%s.%N)" 'BEGIN { printf "%.6f\n", n + 0.5 }' > "$MARKS/go.new"
  mv "$MARKS/go.new" "$MARKS/go"
  until_go
fi
exec git-upload-pack "$@"
"#;

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

/// A commit of `name` in `dir`; its id.
fn commit_in(dir: &Path, name: &str) -> String {
    std::fs::write(dir.join(name), name).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", name]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Whether `a` is an ancestor of (or is) `b` in `dir`.
fn holds(dir: &Path, a: &str, b: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", a, b])
        .status()
        .unwrap()
        .success()
}

/// The status of `task`'s newest `update` once `makibad` has finished it.
fn finished(c: &Connection, task: &str) -> String {
    let sql = "SELECT status FROM commands WHERE verb = 'update'
               AND json_extract(args_json, '$.task') = ?1 ORDER BY id DESC LIMIT 1";
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        let s: Option<String> = c
            .query_row(sql, [task], |r| r.get(0))
            .optional()
            .unwrap_or(None);
        if let Some(s) = s.filter(|s| s != "accepted") {
            return s;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the update of {task} is not finished after {WAIT:?}");
}

/// `makibad`'s notices to the secretary about `task`.
fn notices(c: &Connection, task: &str) -> Vec<String> {
    let mut q = c
        .prepare(
            "SELECT m.body FROM messages m JOIN runs r ON r.id = m.run_id
             WHERE m.kind = 'notice' AND m.sender = 'makibad'
               AND m.recipient = 'secretary' AND r.task_id = ?1 ORDER BY m.id",
        )
        .unwrap();
    q.query_map([task], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The `(base, old, new)` of `task`'s `branch_updated` events.
fn updated(c: &Connection, task: &str) -> Vec<(String, String, String)> {
    let mut q = c
        .prepare(
            "SELECT json_extract(e.payload_json, '$.base'), json_extract(e.payload_json, '$.old'),
                    json_extract(e.payload_json, '$.new')
             FROM events e JOIN runs r ON r.id = e.run_id
             WHERE e.kind = 'branch_updated' AND r.task_id = ?1 ORDER BY e.id",
        )
        .unwrap();
    q.query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn two_updates_of_one_project_at_once_both_merge_the_new_base() {
    let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
    for d in ["repos", "workers", "docs"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // SAFETY: set before any thread of this test starts; this file has one test.
    unsafe {
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
    }
    let repo = root.join("repos/p");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit_in(&repo, "first.txt");
    let origin = root.join("origin.git");
    git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    git(
        &origin,
        &["fetch", "-q", &repo.to_string_lossy(), "main:main"],
    );
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["fetch", "-q", "origin"]);

    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let tasks = ["u1", "u2"];
    for task in tasks {
        let branch = format!("makiba/{task}");
        let worktree = root.join("workers/p").join(task);
        makiba::daemon::run_repo::create(
            &repo,
            &makiba::daemon::run_repo::dir(&state.join("settings"), task),
            &worktree,
            &branch,
            "main",
            &state.join("git-hooks"),
        )
        .unwrap();
        commit_in(&worktree, &format!("{task}.txt"));
        git(
            &origin,
            &[
                "fetch",
                "-q",
                &worktree.to_string_lossy(),
                &format!("{branch}:refs/heads/{branch}"),
            ],
        );
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
    }
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    drop(w);

    // main moves at origin, as when a pull request lands.
    let author = root.join("author");
    git(
        &root,
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &author.to_string_lossy(),
        ],
    );
    let new_main = commit_in(&author, "landed.txt");
    git(
        &origin,
        &["fetch", "-q", &author.to_string_lossy(), "main:main"],
    );
    let marks = root.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let meet = root.join("meeting-upload-pack");
    let script = format!("#!/bin/sh\nMARKS='{}'\n{MEET}", marks.display());
    common::exe::write(&meet, script);
    git(
        &repo,
        &[
            "config",
            "remote.origin.uploadpack",
            &meet.to_string_lossy(),
        ],
    );

    let stop = Arc::new(AtomicBool::new(false));
    let (dir, flag) = (state.clone(), stop.clone());
    let daemon =
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(Stub::new()), flag));
    let c = Connection::open_with_flags(state.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    // The first Resync handled (`base_resynced` is written last), so the door's writes cannot
    // race makibad's start writes (Issue 181).
    let until = Instant::now() + WAIT;
    while c
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'base_resynced'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 0
    {
        assert!(
            Instant::now() < until,
            "makibad did not handle its first Resync"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let door = Door::new(
        StateWriter::open(&state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );

    // The secretary's two calls in one turn.
    for task in tasks {
        let r = door.handle(
            Some(&secretary),
            "update",
            json!({"task": task, "base": "main"}),
            None,
            true,
        );
        assert!(r.is_ok(), "update {task}: {r:?}");
    }
    let statuses: Vec<String> = tasks.iter().map(|t| finished(&c, t)).collect();
    let said: Vec<Vec<String>> = tasks.iter().map(|t| notices(&c, t)).collect();
    stop.store(true, Ordering::SeqCst);
    let _ = daemon.join();

    for (i, task) in tasks.iter().enumerate() {
        assert_eq!(
            statuses[i], "done",
            "the update of {task} did not finish done; makibad said: {:?}",
            said[i]
        );
        assert!(
            !said[i].iter().any(|n| n.contains("cannot lock ref")),
            "the update of {task} lost the fetch's ref lock: {:?}",
            said[i]
        );
        let branch = format!("makiba/{task}");
        let at = git(
            &origin,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        );
        let events = updated(&c, task);
        assert_eq!(
            events.len(),
            1,
            "{task}: one branch_updated is written: {events:?}"
        );
        let (base, _old, new) = &events[0];
        assert_eq!(base, "main", "{task}: the base merged");
        assert_eq!(new, &at, "{task}: the head pushed is the one recorded");
        assert!(
            holds(&origin, &new_main, &at),
            "{task}: origin's {branch} at {at} does not hold the new main {new_main}"
        );
    }
}
