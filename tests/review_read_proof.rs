//! Issue 280: a review round passes only when the reader opened a file of the change under review.
//!
//! A reader that runs commands unrelated to the snapshot (`pwd`, a listing, a file the diff does
//! not touch, a command that only names a changed file) and reports zero findings is not passed;
//! one that opens the diff file or a file the diff changes is (line L-K10 of
//! `docs/design/lines.md`). The readers are stand-in scripts that print the event stream a real
//! reader would (JSON lines); the door runs in this process, as in `tests/review_gate.rs`.

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

/// The person key's content.
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

/// A stand-in program at `path`: it appends its arguments (one per line, then a line `--`) to
/// `<path>.args` and its folder to `<path>.cwd`, answers `login status` with `login`, and else
/// runs `body`.
fn stand_in(path: &Path, login: i32, body: &str) {
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" -- >> \"$0.args\"\npwd >> \"$0.cwd\"\nif [ \"$1\" = login ]; then exit {login}; fi\n{body}\n"
    );
    common::exe::write(path, script);
}

struct Org {
    worktree: PathBuf,
    door: Door,
    worker: String,
    other: PathBuf,
    same: PathBuf,
}

/// An `in_use` run of `task` of `kind` (`code` with its own repository on `makiba/<task>`, or
/// `document`), the keys of the secretary and the worker, and the stand-ins: the other reader
/// logged in, the same model not (each test writes the stream a stand-in prints).
fn org(task: &str) -> Org {
    let kind = "code";
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
    let bin = state.join("bin");
    std::fs::create_dir_all(bin.join("other")).unwrap();
    std::fs::create_dir_all(bin.join("same")).unwrap();
    let other = bin.join("other/codex");
    let same = bin.join("same/claude");
    stand_in(&other, 0, "cat \"$0.out\"");
    stand_in(&same, 0, "cat > /dev/null; cat \"$0.out\"");
    let door = Door::new(
        StateWriter::open(&state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    )
    .with_review_programs(other.clone(), same.clone());
    Org {
        worktree,
        door,
        worker,
        other,
        same,
    }
}

impl Org {
    fn call(&self, kind: &str) -> Value {
        let args = if kind == "design" {
            json!({"kind": "design", "note": "# Design\nChange a.txt."})
        } else {
            json!({"kind": kind})
        };
        self.door
            .handle(Some(&self.worker), "review", args, None, true)
            .unwrap()
    }

    /// A commit in the worktree; returns its id.
    fn commit(&self, name: &str) {
        std::fs::write(self.worktree.join(name), format!("{name}\n")).unwrap();
        git(&self.worktree, &["add", name]);
        git(&self.worktree, &["commit", "-q", "-m", name]);
    }
}

const FINDINGS: &str = "FINDINGS: blocker=0 major=0 minor=0";

/// The stream of the other reader: each command run (exit 0), then its answer.
fn codex(commands: &[&str]) -> String {
    let mut out = String::new();
    for c in commands {
        let e = json!({"type": "item.completed", "item": {"type": "command_execution", "command": c, "exit_code": 0, "status": "completed"}});
        out += &format!("{e}\n");
    }
    let m = json!({"type": "item.completed", "item": {"type": "agent_message", "text": FINDINGS}});
    out + &format!("{m}\n")
}

/// The stream of the same-model reader: each (tool, input, is_error) call and its result.
fn claude(calls: &[(&str, Value, bool)]) -> String {
    let mut out = String::new();
    for (i, (name, input, err)) in calls.iter().enumerate() {
        let id = format!("u{i}");
        let a = json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}});
        let u = json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "is_error": err, "content": "x"}]}});
        out += &format!("{a}\n{u}\n");
    }
    let r = json!({"type": "result", "subtype": "success", "is_error": false, "result": FINDINGS});
    out + &format!("{r}\n")
}

/// One diff review of a run with a commit of `a.txt`, the other reader printing `stream`.
fn review_other(task: &str, kind: &str, stream: &str) -> Value {
    let o = org(task);
    o.commit("a.txt");
    std::fs::write(o.other.with_extension("out"), stream).unwrap();
    o.call(kind)
}

fn not_established(v: &Value, what: &str) {
    assert_eq!(v["verdict"], "not-established", "{what}: {v}");
    let why = v["reason"].as_str().unwrap_or_default();
    assert!(why.contains("opened no file of the change"), "{what}: {v}");
}

/// A reader that runs only commands unrelated to the snapshot, or only names a file of the change
/// without opening it, and reports zero findings is not passed; the reason says what it ran.
#[test]
fn a_reader_that_opened_nothing_of_the_change_is_not_passed() {
    let cases: [&[&str]; 8] = [
        &["pwd"],
        &["ls -la"],
        &["/bin/bash -lc 'pwd; ls'"],
        &["cat file.txt"],
        &["echo MAKIBA_REVIEW.diff"],
        &["ls a.txt"],
        &["true || cat a.txt"],
        &["cat ../MAKIBA_REVIEW.diff"],
    ];
    for (i, cmds) in cases.iter().enumerate() {
        let v = review_other(&format!("rp1x{i}"), "diff", &codex(cmds));
        not_established(&v, cmds[0]);
        assert!(v["reason"].as_str().unwrap().contains(cmds[0]), "{v}");
        assert_eq!(v["reader"], "codex", "{v}");
    }
    let v = review_other("rp1none", "diff", &codex(&[]));
    not_established(&v, "no command");
}

/// A reader that opens the diff file or a file the diff changes is passed, in the forms a real
/// reader writes them (a wrapper shell, a pipe, several commands in a row).
#[test]
fn a_reader_that_opened_a_changed_file_is_passed() {
    let cases: [&[&str]; 6] = [
        &["cat MAKIBA_REVIEW.diff"],
        &["cat a.txt"],
        &["/bin/bash -lc \"sed -n '1,200p' a.txt\""],
        &["zsh -lc 'pwd; nl -ba a.txt | sed -n 1,3p'"],
        &["pwd", "rg -n a a.txt | head"],
        &["ls", "head -c 100 ./MAKIBA_REVIEW.diff 2>/dev/null"],
    ];
    for (i, cmds) in cases.iter().enumerate() {
        let v = review_other(&format!("rp2x{i}"), "diff", &codex(cmds));
        assert_eq!(v["verdict"], "passed", "{}: {v}", cmds[0]);
    }
}

/// The same rule for the same-model reader (the fallback): a `Read` of a changed file passes; a
/// `Read` of another file, a `Glob`, a search of the folder, or a failed `Read` does not.
#[test]
fn the_fallback_reader_is_held_to_the_same_rule() {
    type Call<'a> = (&'a str, Value, bool);
    let cases: [(&str, Vec<Call>, bool); 7] = [
        (
            "changed",
            vec![("Read", json!({"file_path": "a.txt"}), false)],
            true,
        ),
        (
            "diff",
            vec![("Read", json!({"file_path": "MAKIBA_REVIEW.diff"}), false)],
            true,
        ),
        (
            "grep-file",
            vec![("Grep", json!({"pattern": "a", "path": "a.txt"}), false)],
            true,
        ),
        (
            "unrelated",
            vec![("Read", json!({"file_path": "file.txt"}), false)],
            false,
        ),
        (
            "glob",
            vec![("Glob", json!({"pattern": "*"}), false)],
            false,
        ),
        (
            "grep-folder",
            vec![("Grep", json!({"pattern": "a", "path": "."}), false)],
            false,
        ),
        (
            "failed",
            vec![("Read", json!({"file_path": "a.txt"}), true)],
            false,
        ),
    ];
    for (name, calls, passes) in cases {
        let o = org(&format!("rp3{name}"));
        o.commit("a.txt");
        // The other reader is not logged in: the same model runs.
        stand_in(&o.other, 1, "exit 1");
        std::fs::write(o.same.with_extension("out"), claude(&calls)).unwrap();
        let v = o.call("diff");
        assert_eq!(v["reader"], "fresh-session", "{name}: {v}");
        if passes {
            assert_eq!(v["verdict"], "passed", "{name}: {v}");
        } else {
            not_established(&v, name);
        }
    }
}

/// A design review is held to its own note: reading the note passes, reading only the diff file
/// (empty before the work) or the repository does not.
#[test]
fn a_design_review_must_open_the_note() {
    let v = review_other("rp4note", "design", &codex(&["cat MAKIBA_DESIGN.md"]));
    assert_eq!(v["verdict"], "passed", "{v}");
    let v = review_other("rp4diff", "design", &codex(&["cat MAKIBA_REVIEW.diff"]));
    not_established(&v, "design, diff only");
    let v = review_other("rp4repo", "design", &codex(&["cat file.txt"]));
    not_established(&v, "design, repository only");
}
