//! Issue 135: a worker's review by a second reader goes through the door's verb `review`, and its
//! rounds are counted and capped outside the worker (line L-K10 of `docs/design/lines.md`).
//!
//! The door runs in this process on the stand-in base and is called as a tool (`tool` true), as a
//! worker's MCP call is. No model is called: the readers are stand-in scripts this test writes,
//! named to the door with `Door::with_review_programs`. The "other" stand-in answers as
//! `codex exec --json` does (JSON lines), the "same" stand-in as `claude -p --output-format
//! stream-json` does. The project `p` is a git repository whose `main` is at a local bare origin;
//! each test has its own run, worktree and state folder.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
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

fn denied(r: &Response, kind: ErrorKind) -> String {
    match r {
        Err(e) if e.kind == kind => e.message.clone(),
        other => panic!("expected {kind:?}, got {other:?}"),
    }
}

/// 1. Within the allowance a diff review runs, passes with the other reader named, and is counted
///    once per call, three times by default; the design review is counted apart from them.
#[test]
fn a_review_within_the_allowance_runs_and_is_counted() {
    let o = org("rg1", "code");
    let head = o.commit("a.txt");
    for round in 1..=3 {
        let v = o.diff().unwrap();
        assert_eq!(v["verdict"], "passed", "{v}");
        assert_eq!(v["kind"], "diff", "{v}");
        assert_eq!(v["reader"], "codex", "{v}");
        assert_eq!(v["round"], round, "{v}");
        assert_eq!(v["allowed"], 3, "{v}");
        assert_eq!(v["head"], head, "{v}");
        assert_eq!(v["findings"], json!({"blocker": 0, "major": 1, "minor": 0}));
        assert!(
            v["text"].as_str().unwrap().contains("[Major] file.txt:1"),
            "{v}"
        );
        assert_eq!(o.started("diff"), round as usize);
    }
    let v = o.design().unwrap();
    assert_eq!(v["verdict"], "passed", "{v}");
    assert_eq!(v["kind"], "design", "{v}");
    assert_eq!(v["round"], 1, "{v}");
    assert_eq!(v["allowed"], 1, "{v}");
    assert_eq!(o.started("design"), 1);
    assert_eq!(o.started("diff"), 3);
    assert_eq!(reviews(&o.other).len(), 4);
    assert!(reviews(&o.same).is_empty(), "the fallback ran");
}

/// 2. Over the allowance the call is refused, a `codex_round_denied` row is written and no reader
///    runs; the design review's one round is refused the same way.
#[test]
fn a_review_over_the_allowance_is_refused_and_recorded() {
    let o = org("rg2", "code");
    o.commit("a.txt");
    for _ in 0..3 {
        assert_eq!(o.diff().unwrap()["verdict"], "passed");
    }
    let m = denied(&o.diff(), ErrorKind::Conflict);
    assert!(m.contains("REVIEW_ROUND_DENIED"), "{m}");
    let d = o.events("codex_round_denied");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0]["mode"], "diff");
    assert_eq!(d[0]["used"], 3);
    assert_eq!(reviews(&o.other).len(), 3);
    assert_eq!(o.started("diff"), 3);

    assert_eq!(o.design().unwrap()["verdict"], "passed");
    let m = denied(&o.design(), ErrorKind::Conflict);
    assert!(m.contains("REVIEW_ROUND_DENIED"), "{m}");
    assert_eq!(o.events("codex_round_denied").len(), 2);
    assert_eq!(o.started("design"), 1);
    assert_eq!(reviews(&o.other).len(), 4);
}

/// 3. A worker cannot raise its allowance (nor name another task); the secretary can, with a
///    reason, and that is recorded with the previous value.
#[test]
fn a_worker_cannot_raise_its_own_allowance() {
    let o = org("rg3", "code");
    o.commit("a.txt");
    for args in [
        json!({"kind": "diff", "allowed": 9}),
        json!({"task": o.task, "allowed": 9, "reason": "more"}),
    ] {
        denied(&o.call(&o.worker, "review", args), ErrorKind::Denied);
    }
    assert!(o.events("codex_round_cap_changed").is_empty());
    assert!(reviews(&o.other).is_empty());
    // The secretary: a reason is required.
    denied(
        &o.call(
            &o.secretary,
            "review",
            json!({"task": o.task, "allowed": 4}),
        ),
        ErrorKind::Usage,
    );
    let r = o.call(
        &o.secretary,
        "review",
        json!({"task": o.task, "allowed": 4, "reason": "a fourth round for the last finding"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let c = o.events("codex_round_cap_changed");
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0]["allowed"], 4);
    assert_eq!(c[0]["previous"], 3);
    assert_eq!(c[0]["reason"], "a fourth round for the last finding");
    for round in 1..=4 {
        let v = o.diff().unwrap();
        assert_eq!(v["round"], round, "{v}");
        assert_eq!(v["allowed"], 4, "{v}");
    }
    denied(&o.diff(), ErrorKind::Conflict);
}

/// 4. When the other reader is not usable (absent, not logged in, or failing before it read
///    anything) the same model runs in a fresh session, and the answer names it.
#[test]
fn the_fallback_reader_is_used_and_named() {
    let o = org("rg4", "code");
    o.commit("a.txt");
    let cases: [(i32, &str); 2] = [
        (1, OTHER_OK),
        (0, "echo 'stream error: at capacity' >&2; exit 1"),
    ];
    for (login, body) in cases {
        stand_in(&o.other, login, body);
        let v = o.diff().unwrap();
        assert_eq!(v["verdict"], "passed", "{v}");
        assert_eq!(v["reader"], "fresh-session", "{v}");
        assert!(
            !v["fallback_reason"].as_str().unwrap_or_default().is_empty(),
            "{v}"
        );
    }
    // Absent: a program that is not there.
    let gone = o.state.join("bin/nowhere/codex");
    let door = Door::new(
        StateWriter::open(&o.state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    )
    .with_review_programs(gone, o.same.clone());
    let v = door
        .handle(
            Some(&o.worker),
            "review",
            json!({"kind": "diff"}),
            None,
            true,
        )
        .unwrap();
    assert_eq!(v["reader"], "fresh-session", "{v}");
    // The fresh session: its own process, never a resumed conversation, no MCP server (no door
    // key), only reading tools, and settings with the sandbox on.
    let runs = reviews(&o.same);
    assert_eq!(runs.len(), 3, "{runs:?}");
    for a in &runs {
        assert!(a.iter().any(|x| x == "-p"), "{a:?}");
        assert!(
            !a.iter()
                .any(|x| x == "--resume" || x == "--continue" || x == "-c"),
            "{a:?}"
        );
        assert!(a.iter().any(|x| x == "--strict-mcp-config"), "{a:?}");
        assert!(a.iter().any(|x| x == "--no-session-persistence"), "{a:?}");
        let tools = a.iter().position(|x| x == "--tools").expect("--tools");
        assert_eq!(a[tools + 1], "Read,Grep,Glob", "{a:?}");
        assert!(a.iter().any(|x| x == "--settings"), "{a:?}");
    }
    // The settings the last fresh session was given, as it read them while it ran.
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(o.same.with_extension("settings")).unwrap())
            .unwrap();
    assert_eq!(settings["sandbox"]["enabled"], true, "{settings}");
    assert_eq!(
        settings["sandbox"]["allowUnsandboxedCommands"], false,
        "{settings}"
    );
    assert_eq!(
        settings["sandbox"]["network"]["strictAllowlist"], true,
        "{settings}"
    );
}

/// 5. Neither reader usable: `not-established`, the reader `none`, never `passed`, and no round
///    is spent since nothing ran.
#[test]
fn neither_reader_gives_not_established() {
    let o = org("rg5", "code");
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
    assert_eq!(v["verdict"], "not-established", "{v}");
    assert_eq!(v["reader"], "none", "{v}");
    assert!(!v["reason"].as_str().unwrap_or_default().is_empty(), "{v}");
    assert_eq!(o.started("diff"), 0);
}

/// 6. A reader that answers without having read the repository, or without the findings line,
///    is not established; the round it used stays spent.
#[test]
fn findings_without_a_read_are_not_established() {
    let o = org("rg6", "code");
    o.commit("a.txt");
    stand_in(
        &o.other,
        0,
        r#"printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"FINDINGS: blocker=0 major=0 minor=0"}}'"#,
    );
    let v = o.diff().unwrap();
    assert_eq!(v["verdict"], "not-established", "{v}");
    assert_eq!(v["reader"], "codex", "{v}");
    stand_in(
        &o.other,
        0,
        r#"printf '%s\n' '{"type":"item.completed","item":{"type":"command_execution","command":"cat review.diff","exit_code":0,"status":"completed"}}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"looks fine"}}'"#,
    );
    let v = o.diff().unwrap();
    assert_eq!(v["verdict"], "not-established", "{v}");
    assert_eq!(o.started("diff"), 2);
    assert!(reviews(&o.same).is_empty());
}

/// 7. A document task has no repository: `review` is refused there and spends nothing.
#[test]
fn a_document_task_cannot_spend_a_round() {
    let o = org("rg7", "document");
    denied(&o.diff(), ErrorKind::Usage);
    denied(&o.design(), ErrorKind::Usage);
    assert_eq!(o.started("diff") + o.started("design"), 0);
    assert!(reviews(&o.other).is_empty());
}

/// 8. An answer that carries the person key's content is withheld: nothing of it comes back.
#[test]
fn an_answer_with_the_person_key_is_withheld() {
    let o = org("rg8", "code");
    o.commit("a.txt");
    stand_in(
        &o.other,
        0,
        r#"k=$(cat "$MAKIBA_PERSON_KEY")
printf '%s\n' '{"type":"item.completed","item":{"type":"command_execution","command":"cat x","exit_code":0,"status":"completed"}}'
printf '%s\n' "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"the key is $k\\nFINDINGS: blocker=0 major=0 minor=0\"}}""#,
    );
    let v = o.diff().unwrap();
    assert_eq!(v["verdict"], "not-established", "{v}");
    assert!(!v.to_string().contains(PERSON_KEY), "{v}");
}

/// 9. The reader sees the committed head in a folder of its own, not the worktree: uncommitted
///    work is not in what it reads, and the answer names the base and the head.
#[test]
fn the_reader_sees_the_committed_head_not_the_worktree() {
    let o = org("rg9", "code");
    let head = o.commit("a.txt");
    std::fs::write(o.worktree.join("file.txt"), "uncommitted\n").unwrap();
    stand_in(
        &o.other,
        0,
        r#"cp MAKIBA_REVIEW.diff "$0.diff"; cp a.txt "$0.a" 2>/dev/null; cp file.txt "$0.file"
printf '%s\n' '{"type":"item.completed","item":{"type":"command_execution","command":"cat review.diff","exit_code":0,"status":"completed"}}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"FINDINGS: blocker=0 major=0 minor=0"}}'"#,
    );
    let v = o.diff().unwrap();
    assert_eq!(v["verdict"], "passed", "{v}");
    assert_eq!(v["head"], head, "{v}");
    let base = git(&root().join("repos/p"), &["rev-parse", "origin/main"]);
    assert_eq!(v["base"], base, "{v}");
    let seen = |ext: &str| std::fs::read_to_string(o.other.with_extension(ext)).unwrap();
    assert!(seen("diff").contains("+a.txt"), "{}", seen("diff"));
    assert!(!seen("diff").contains("uncommitted"));
    assert_eq!(seen("a"), "a.txt\n");
    assert_eq!(seen("file"), "first\n");
    let cwd = std::fs::read_to_string(o.other.with_extension("cwd")).unwrap();
    for d in cwd.lines() {
        assert!(!Path::new(d).starts_with(&o.worktree), "{d}");
    }
}

/// 10. `report done` is refused while the task's last diff review found a blocker; after a fixed
///     head is reviewed clean, it is accepted.
#[test]
fn report_done_is_refused_while_the_last_diff_review_found_a_blocker() {
    let o = org("rg10", "code");
    o.commit("a.txt");
    stand_in(
        &o.other,
        0,
        r#"printf '%s\n' '{"type":"item.completed","item":{"type":"command_execution","command":"cat review.diff","exit_code":0,"status":"completed"}}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"[Blocker] a.txt:1 - broken - fix\nFINDINGS: blocker=1 major=0 minor=0"}}'"#,
    );
    assert_eq!(o.diff().unwrap()["findings"]["blocker"], 1);
    let done = json!({"state": "done", "task": o.task, "text": "done"});
    let m = denied(
        &o.call(&o.worker, "report", done.clone()),
        ErrorKind::Conflict,
    );
    assert!(m.to_lowercase().contains("blocker"), "{m}");
    o.commit("b.txt");
    stand_in(&o.other, 0, OTHER_OK);
    assert_eq!(o.diff().unwrap()["findings"]["blocker"], 0);
    let r = o.call(&o.worker, "report", done);
    assert!(r.is_ok(), "{r:?}");
}

/// 11. Line L-K10 of lines.md is held: it names no open Issue, and every test of this file it
///     names exists here.
#[test]
fn the_line_in_lines_md_names_an_existing_holder() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design/lines.md"))
            .unwrap();
    let row = doc
        .lines()
        .find(|l| l.starts_with("| L-K10 "))
        .expect("lines.md has no row L-K10");
    let held = row.trim_matches('|').split(" | ").last().unwrap();
    assert!(!held.contains("Open: #"), "{held}");
    let this = include_str!("review_gate.rs");
    let named: Vec<&str> = held
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|r| r.strip_prefix("tests/review_gate.rs::"))
        .collect();
    assert!(named.len() >= 3, "{held}");
    for f in named {
        assert!(
            this.contains(&format!("fn {f}(")),
            "tests/review_gate.rs has no fn {f}"
        );
    }
}
