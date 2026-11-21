//! Issue 135, the secretary's request after commit 1: a code task's done report carries the state
//! of its review for the reported head, recorded by the door (not the worker's words), in a
//! `worker_reported` event on the read surface and in the line the secretary is told.
//!
//! Also the regressions of the diff review's round 1: a committed link cannot make makibad write
//! outside the snapshot, a secret in a reader's error is withheld, and a review no reader could
//! run is not reported stale.
//!
//! The helpers are those of `tests/review_gate.rs` (the declared test, not changed after its
//! commit).
//!
//! The door runs in this process on the stand-in base and is called as a tool (`tool` true), as a
//! worker's MCP call is. No model is called: the readers are stand-in scripts this test writes,
//! named to the door with `Door::with_review_programs`. The "other" stand-in answers as
//! `codex exec --json` does (JSON lines), the "same" stand-in as `claude -p --output-format
//! stream-json` does. The project `p` is a git repository whose `main` is at a local bare origin;
//! each test has its own run, worktree and state folder.

// Some copied helpers are not used here.
#![allow(dead_code)]

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

/// The person key's content, which no answer may carry back.
const PERSON_KEY: &str = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";

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
        let key = root.join("config/makiba/person.key");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, format!("{PERSON_KEY}\n")).unwrap();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_PERSON_KEY", &key);
        }
        root
    })
}

/// What the "other" stand-in does when asked to review.
const OTHER_OK: &str = r#"printf '%s\n' '{"type":"item.completed","item":{"type":"command_execution","command":"cat review.diff","exit_code":0,"status":"completed"}}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"[Major] file.txt:1 - a finding - a fix\nFINDINGS: blocker=0 major=1 minor=0"}}'"#;

/// What the "same" stand-in does: one successful read, then its result.
const SAME_OK: &str = r#"cat > "$0.stdin"
prev=; for a in "$@"; do [ "$prev" = --settings ] && cp "$a" "$0.settings"; prev=$a; done
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"u1","name":"Read","input":{"file_path":"review.diff"}}]}}'
printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"u1","content":"the diff"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"FINDINGS: blocker=0 major=0 minor=0"}'"#;

/// A stand-in program at `path`: it appends its arguments (one per line, then a line `--`) to
/// `<path>.args` and its folder to `<path>.cwd`, answers `login status` with `login`, and else
/// runs `body`.
fn stand_in(path: &Path, login: i32, body: &str) {
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" -- >> \"$0.args\"\npwd >> \"$0.cwd\"\nif [ \"$1\" = login ]; then exit {login}; fi\n{body}\n"
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The calls a stand-in received: each its list of arguments.
fn calls(path: &Path) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(path.with_extension("args")).unwrap_or_default();
    let mut out = vec![];
    let mut one = vec![];
    for l in text.lines() {
        if l == "--" {
            out.push(std::mem::take(&mut one));
        } else {
            one.push(l.to_string());
        }
    }
    out
}

/// The review calls (not `login status`) a stand-in received.
fn reviews(path: &Path) -> Vec<Vec<String>> {
    calls(path)
        .into_iter()
        .filter(|c| c.first().map(String::as_str) != Some("login"))
        .collect()
}

struct Org {
    task: String,
    state: PathBuf,
    worktree: PathBuf,
    door: Door,
    secretary: String,
    worker: String,
    other: PathBuf,
    same: PathBuf,
}

/// An `in_use` run of `task` of `kind` (`code` with its own repository on `makiba/<task>`, or
/// `document`), the keys of the secretary and the worker, and the stand-ins: the other reader
/// logged in and answering `OTHER_OK`, the same model answering `SAME_OK`.
fn org(task: &str, kind: &str) -> Org {
    let root = root();
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(&state).unwrap();
    let branch = format!("makiba/{task}");
    let worktree = if kind == "code" {
        let worktree = root.join("workers/p").join(task);
        makiba::daemon::run_repo::create(
            &root.join("repos/p"),
            &makiba::daemon::run_repo::dir(&state.join("settings"), task),
            &worktree,
            &branch,
            "main",
            &state.join("git-hooks"),
        )
        .unwrap();
        worktree
    } else {
        let d = root.join("docs").join(task);
        std::fs::create_dir_all(d.join("out")).unwrap();
        d
    };
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
             VALUES (?1, ?2, ?3, 'fake', 'the brief', ?4, 'verified')",
            rusqlite::params![run, spawn, kind, worktree.to_string_lossy()],
        )
        .unwrap();
    let worker = issue_key(
        &w,
        Role::Worker,
        &format!("w-{task}"),
        Some(run),
        Some(&spawn),
    )
    .unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let bin = state.join("bin");
    std::fs::create_dir_all(bin.join("other")).unwrap();
    std::fs::create_dir_all(bin.join("same")).unwrap();
    let other = bin.join("other/codex");
    let same = bin.join("same/claude");
    stand_in(&other, 0, OTHER_OK);
    stand_in(&same, 0, SAME_OK);
    let door = Door::new(
        StateWriter::open(&state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    )
    .with_review_programs(other.clone(), same.clone());
    Org {
        task: task.into(),
        state,
        worktree,
        door,
        secretary,
        worker,
        other,
        same,
    }
}

impl Org {
    fn conn(&self) -> Connection {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Response {
        self.door.handle(Some(key), verb, args, None, true)
    }

    fn diff(&self) -> Response {
        self.call(&self.worker, "review", json!({"kind": "diff"}))
    }

    fn design(&self) -> Response {
        self.call(
            &self.worker,
            "review",
            json!({"kind": "design", "note": "# Design\nChange file.txt."}),
        )
    }

    /// A commit in the worktree; returns its id.
    fn commit(&self, name: &str) -> String {
        std::fs::write(self.worktree.join(name), format!("{name}\n")).unwrap();
        git(&self.worktree, &["add", name]);
        git(&self.worktree, &["commit", "-q", "-m", name]);
        git(&self.worktree, &["rev-parse", "HEAD"])
    }

    /// The payloads of this task's events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let c = self.conn();
        let mut q = c
            .prepare(
                "SELECT payload_json FROM events WHERE kind = ?1
                 AND json_extract(payload_json, '$.task') = ?2 ORDER BY id",
            )
            .unwrap();
        q.query_map([kind, &self.task], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|s| serde_json::from_str(&s.unwrap()).unwrap())
            .collect()
    }

    fn started(&self, mode: &str) -> usize {
        self.events("codex_round_started")
            .iter()
            .filter(|p| p["mode"] == json!(mode))
            .count()
    }
}

impl Org {
    fn done(&self) -> Response {
        self.call(
            &self.worker,
            "report",
            json!({"state": "done", "task": self.task, "text": "all done"}),
        )
    }

    /// The `review` of this task's last `worker_reported` event.
    fn recorded(&self) -> Value {
        let mut e = self.events("worker_reported");
        e.pop().expect("no worker_reported event")["review"].clone()
    }

    /// The body of the last done report the secretary got.
    fn told(&self) -> String {
        self.conn()
            .query_row(
                "SELECT body FROM messages WHERE kind = 'report_done' AND recipient = 'secretary'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
}

#[test]
fn a_done_report_after_a_passed_review_records_passed_and_the_reader() {
    let o = org("rr1", "code");
    let head = o.commit("a.txt");
    assert_eq!(o.diff().unwrap()["verdict"], "passed");
    o.done().unwrap();
    let r = o.recorded();
    assert_eq!(r["state"], "passed", "{r}");
    assert_eq!(r["reader"], "codex", "{r}");
    assert_eq!(r["head"], head, "{r}");
    let told = o.told();
    assert!(told.starts_with("all done"), "{told}");
    assert!(told.contains("[makiba] review of"), "{told}");
    assert!(told.contains("passed by codex"), "{told}");
}

#[test]
fn a_done_report_after_a_later_commit_records_the_review_stale() {
    let o = org("rr2", "code");
    o.commit("a.txt");
    o.diff().unwrap();
    let head = o.commit("b.txt");
    o.done().unwrap();
    let r = o.recorded();
    assert_eq!(r["state"], "stale", "{r}");
    assert_eq!(r["head"], head, "{r}");
    assert!(o.told().contains("stale"), "{}", o.told());
}

#[test]
fn a_done_report_without_a_review_records_none() {
    let o = org("rr3", "code");
    o.commit("a.txt");
    o.done().unwrap();
    assert_eq!(o.recorded()["state"], "none");
    assert!(o.told().contains("none"), "{}", o.told());
}

#[test]
fn a_done_report_after_a_review_without_a_verdict_records_not_established() {
    let o = org("rr4", "code");
    o.commit("a.txt");
    stand_in(&o.other, 0, "exit 0");
    assert_eq!(o.diff().unwrap()["verdict"], "not-established");
    o.done().unwrap();
    let r = o.recorded();
    assert_eq!(r["state"], "not-established", "{r}");
    assert!(!r["reason"].as_str().unwrap_or_default().is_empty(), "{r}");
}

#[test]
fn a_document_tasks_done_report_records_no_review() {
    let o = org("rr5", "document");
    o.done().unwrap();
    assert!(o.events("worker_reported").is_empty());
    assert_eq!(o.told(), "all done");
}

/// Review round 1, P1: a link committed under the snapshot's file names is not written through.
#[test]
fn a_committed_link_under_a_snapshot_name_is_not_written_through() {
    let o = org("rr6", "code");
    let outside = o.state.join("outside.txt");
    std::fs::write(&outside, "untouched\n").unwrap();
    for name in ["MAKIBA_REVIEW.diff", "MAKIBA_DESIGN.md"] {
        std::os::unix::fs::symlink(&outside, o.worktree.join(name)).unwrap();
        git(&o.worktree, &["add", name]);
    }
    git(&o.worktree, &["commit", "-q", "-m", "links"]);
    assert_eq!(o.diff().unwrap()["verdict"], "passed");
    assert_eq!(o.design().unwrap()["verdict"], "passed");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "untouched\n");
}

/// Review round 1, P2: a secret in a reader's error output does not come back in the reasons.
#[test]
fn a_secret_in_a_readers_error_is_withheld() {
    let o = org("rr7", "code");
    o.commit("a.txt");
    stand_in(&o.other, 0, r#"cat "$MAKIBA_PERSON_KEY" >&2; exit 1"#);
    let v = o.diff().unwrap();
    assert_eq!(v["reader"], "fresh-session", "{v}");
    assert!(!v.to_string().contains(PERSON_KEY), "{v}");
}

/// Review round 1, P2: a review no reader could run keeps its verdict at the done report.
#[test]
fn a_review_no_reader_could_run_is_not_reported_stale() {
    let o = org("rr8", "code");
    o.commit("a.txt");
    let door = Door::new(
        StateWriter::open(&o.state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    )
    .with_review_programs(
        o.state.join("bin/nowhere/codex"),
        o.state.join("bin/nowhere/claude"),
    );
    let v = door
        .handle(
            Some(&o.worker),
            "review",
            json!({"kind": "diff"}),
            None,
            true,
        )
        .unwrap();
    assert_eq!(v["reader"], "none", "{v}");
    o.done().unwrap();
    let r = o.recorded();
    assert_eq!(r["state"], "not-established", "{r}");
    assert!(!r["reason"].as_str().unwrap_or_default().is_empty(), "{r}");
}
