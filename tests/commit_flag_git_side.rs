//! Issue 300: git's flag that skips the hooks is refused by git's own `prepare-commit-msg` in a
//! worker's code run, from the argv of the git that runs the hook, not from the Bash command text;
//! so a commit whose message holds the flag's letters is let through.
//!
//! - The worker's Bash hook (`deny-no-verify-code-run`, as `roles/worker/settings.json` names it)
//!   lets a `git commit` whose message holds " -n" or "--no-verify" through, and still refuses the
//!   hooks' other switches; the secretary's and the curator's `deny-no-verify` still refuses the
//!   commit flag, naming the remedy `-F`.
//! - In a worktree prepared by a real `makibad`, such a message commits, and so does `-uno`.
//! - There the flag is refused in each spelling, by the hook `prepare-commit-msg` and line L-K3,
//!   naming the element of git's argv that matched; HEAD does not move.
//! - No role's settings deny `Bash(git commit ...)` text, and lines.md names the holder.
//!
//! Linux only: the git-side reader reads `/proc/<pid>/cmdline`; elsewhere the Bash hook keeps the
//! text check.
#![cfg(target_os = "linux")]

mod common;

use common::makibad;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const REFUSED_BY_GIT: &str =
    "makiba: refused by hook prepare-commit-msg (line L-K3 in docs/design/lines.md)";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

fn settings(role: &str) -> Value {
    serde_json::from_str(&read(&format!("roles/{role}/settings.json"))).unwrap()
}

/// The hook name (the word after `hook`) of the role's Bash hook command that names a no-verify
/// hook.
fn no_verify_hook(role: &str) -> String {
    let s = settings(role);
    let mut found = Vec::new();
    for groups in s["hooks"].as_object().into_iter().flat_map(|m| m.values()) {
        for g in groups.as_array().into_iter().flatten() {
            for h in g["hooks"].as_array().into_iter().flatten() {
                let Some(cmd) = h["command"].as_str() else {
                    continue;
                };
                let words: Vec<&str> = cmd.split_whitespace().collect();
                let name = words
                    .iter()
                    .position(|w| *w == "hook")
                    .and_then(|i| words.get(i + 1));
                if let Some(name) = name.filter(|n| n.starts_with("deny-no-verify")) {
                    found.push(name.to_string());
                }
            }
        }
    }
    assert_eq!(found.len(), 1, "roles/{role}/settings.json: {found:?}");
    found.remove(0)
}

/// The folders a Bash hook run sees.
struct Place {
    _dir: tempfile::TempDir,
    home: PathBuf,
    worktree: PathBuf,
    workers: PathBuf,
}

impl Place {
    fn new() -> Place {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let p = Place {
            home: base.join("home"),
            worktree: base.join("workers/t1"),
            workers: base.join("workers"),
            _dir: dir,
        };
        for d in [&p.home, &p.worktree] {
            std::fs::create_dir_all(d).unwrap();
        }
        p
    }

    /// The exit code and standard error of `makiba hook <name>` on a Bash payload.
    fn bash(&self, name: &str, command: &str) -> (i32, String) {
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": command}}).to_string();
        let mut child = Command::new(MAKIBA)
            .args(["hook", name])
            .env("HOME", &self.home)
            .env("MAKIBA_WORKERS_DIR", &self.workers)
            .env("MAKIBA_DOCS_DIR", self.home.join("docs"))
            .env("CLAUDE_PROJECT_DIR", &self.worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn makiba");
        let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
        let out = child.wait_with_output().expect("wait for makiba");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

#[test]
fn the_workers_bash_hook_lets_a_commit_message_holding_the_flags_letters_through() {
    let worker = no_verify_hook("worker");
    assert_eq!(worker, "deny-no-verify-code-run");
    let p = Place::new();
    let mut wrong = Vec::new();
    for command in [
        "git commit -m 'bash -n checks the script'",
        "git commit -m \"never pass --no-verify\"",
        "git commit -m x -m \"run bash -n; --no-verify is refused\"",
        "git commit -F msg.txt",
    ] {
        let (code, err) = p.bash(&worker, command);
        if code != 0 {
            wrong.push(format!("{worker} refused {command:?}: {code} {err}"));
        }
    }
    for command in [
        "git push --no-verify origin x",
        "git -c core.hooksPath=/dev/null commit -m x",
        "HUSKY=0 git commit -m x",
    ] {
        let (code, err) = p.bash(&worker, command);
        if code != 2 {
            wrong.push(format!("{worker} let {command:?} through: {code} {err}"));
        }
    }
    for role in ["secretary", "curator"] {
        let name = no_verify_hook(role);
        if name != "deny-no-verify" {
            wrong.push(format!("roles/{role}/settings.json names hook {name}"));
            continue;
        }
        let (code, err) = p.bash(&name, "git commit -n -m x");
        if code != 2
            || !err.contains("by hook deny-no-verify ")
            || !err.contains("(line L-K3 in docs/design/lines.md)")
            || !err.contains("-F")
        {
            wrong.push(format!(
                "{role}: deny-no-verify on the commit flag: {code} {err}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `git -C <dir> -c user.name=t -c user.email=t@example.com <args>`: global options before the
/// subcommand, so the hook's reading of git's argv passes over them.
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
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn head(wt: &Path) -> String {
    git(wt, &["rev-parse", "HEAD"])
}

/// A state folder and a project repository with one commit on `main`, and a code run queued with
/// its `planned` launch (as in tests/secret_scan.rs).
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
        std::fs::write(repo.join("README.md"), "readme\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
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
        let until = Instant::now() + Duration::from_secs(30);
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

/// Change the tracked README.md and stage it, so that every commit has something to commit.
fn stage_change(wt: &Path, n: usize) {
    std::fs::write(wt.join("README.md"), format!("readme {n}\n")).unwrap();
    git(wt, &["add", "README.md"]);
}

#[test]
fn in_a_prepared_worktree_a_message_holding_the_flags_letters_commits() {
    let org = Org::new("t1");
    let _d = org.start();
    let wt = org.prepared("t1");

    let text = "bash -n checks it; never --no-verify";
    stage_change(&wt, 1);
    let before = head(&wt);
    let out = git_out(&wt, &["commit", "-q", "-m", text]);
    assert!(
        out.status.success(),
        "a message holding the flag's letters was refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(head(&wt), before);
    assert_eq!(git(&wt, &["log", "-1", "--format=%B"]), text);

    let file_text = format!("{text}\n\n-n --no-verify\n");
    let dir = tempfile::tempdir().unwrap();
    let msg = dir.path().join("msg.txt");
    std::fs::write(&msg, &file_text).unwrap();
    stage_change(&wt, 2);
    let before = head(&wt);
    let out = git_out(&wt, &["commit", "-q", "-F", msg.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "a message file holding the flag's letters was refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(head(&wt), before);
    assert_eq!(git(&wt, &["log", "-1", "--format=%B"]), file_text.trim());

    stage_change(&wt, 3);
    let before = head(&wt);
    let out = git_out(&wt, &["commit", "-q", "-uno", "-m", "x"]);
    assert!(
        out.status.success(),
        "-uno was refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(head(&wt), before);
}

#[test]
fn in_a_prepared_worktree_the_flag_is_refused_in_each_spelling() {
    let org = Org::new("t2");
    let _d = org.start();
    let wt = org.prepared("t2");
    // The message file `-F --` names, in the worktree (git reads -F's path from there).
    std::fs::write(wt.join("--"), "a message\n").unwrap();

    let cases: [(&[&str], &str); 11] = [
        (&["commit", "-n", "-m", "x"], "-n"),
        (&["commit", "--no-verify", "-m", "x"], "--no-verify"),
        (&["commit", "--no-veri", "-m", "x"], "--no-veri"),
        (&["commit", "-an", "-m", "x"], "-an"),
        (&["commit", "-anm", "x"], "-anm"),
        (&["commit", "-qn", "-m", "x"], "-qn"),
        (&["commit", "-m", "x", "-n"], "-n"),
        (&["commit", "-m", "x", "--no-verify"], "--no-verify"),
        (&["commit", "-m", "--", "-n"], "-n"),
        (&["commit", "-F", "--", "-n"], "-n"),
        (&["-c", "alias.ci=commit -n", "ci", "-m", "x"], "-n"),
    ];
    let mut wrong = Vec::new();
    let mut check = |what: String, out: Output, spelling: &str, before: &str, now: String| {
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            wrong.push(format!("{what}: not refused"));
        }
        if now != before {
            wrong.push(format!("{what}: something was committed"));
        }
        if !err.contains(REFUSED_BY_GIT) {
            wrong.push(format!("{what}: not refused by prepare-commit-msg: {err}"));
        }
        if !err.contains(&format!("Matched: \"{spelling}\"")) {
            wrong.push(format!(
                "{what}: the refusal does not match {spelling:?}: {err}"
            ));
        }
    };
    for (n, (args, spelling)) in cases.iter().enumerate() {
        stage_change(&wt, n + 10);
        let before = head(&wt);
        let out = git_out(&wt, args);
        check(format!("{args:?}"), out, spelling, &before, head(&wt));
    }
    // The shell sets the flag through a variable: git's argv holds it, the Bash text does not.
    stage_change(&wt, 99);
    let before = head(&wt);
    let script =
        "F=--no-verify; git -C \"$1\" -c user.name=t -c user.email=t@example.com commit $F -m x";
    let out = Command::new("/bin/sh")
        .args(["-c", script, "sh"])
        .arg(&wt)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    check(
        "sh -c with F=--no-verify".into(),
        out,
        "--no-verify",
        &before,
        head(&wt),
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_roles_settings_deny_no_git_commit_text() {
    let mut held = Vec::new();
    for role in ["worker", "secretary", "curator"] {
        let s = settings(role);
        for r in s["permissions"]["deny"].as_array().into_iter().flatten() {
            let r = r.as_str().unwrap_or_default();
            if r.starts_with("Bash(git commit") {
                held.push(format!("{role}: {r}"));
            }
        }
    }
    assert!(
        held.is_empty(),
        "deny rules on git commit text remain: {held:?}"
    );
}

#[test]
fn lines_md_names_the_git_side_holder_of_the_flag() {
    let doc = read("docs/design/lines.md");
    let row = doc
        .lines()
        .find(|l| l.starts_with("| L-K3 "))
        .expect("lines.md has an L-K3 row");
    for needle in [
        "`hook prepare-commit-msg`",
        "`hook deny-no-verify-code-run`",
        "`tests/commit_flag_git_side.rs::in_a_prepared_worktree_the_flag_is_refused_in_each_spelling`",
    ] {
        assert!(row.contains(needle), "the L-K3 row does not name {needle}");
    }
    assert!(
        !doc.contains("Bash(git commit -n*)"),
        "lines.md still names the removed deny rule"
    );
    let stale: Vec<&str> = doc
        .split(". ")
        .filter(|s| s.contains("git's flag") && s.contains("on the worker's side"))
        .collect();
    assert!(
        stale.is_empty(),
        "lines.md still says git's flag is not held on the worker's side: {stale:?}"
    );
}
