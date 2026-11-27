//! Issue 564: the head a code pull request is held to is that of its task's newest passed diff
//! review that names a head (a round is passed only when its reader read the change, Issue 280),
//! as Issue 557 built for docs-only pull requests. A later round that read nothing (no reader could
//! start, so no head; or one that names the new head of its snapshot without reading it) does not
//! hide it: a head after the reviewed one is refused as `stale` under both merge approval modes.
//! The same case without the attempt is unchanged, and at the reviewed head (or one carried from
//! it) the attempt's own state still shows.
//!
//! The helpers are those of `tests/gate_review_head.rs`, copied (that file keeps its own).

// Some copied helpers are not used here.
#![allow(dead_code)]

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::gate;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once, OnceLock};

const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
const D: &str = "dddddddddddddddddddddddddddddddddddddddd";
const T1: &str = "2026-11-05T01:00:00Z";
const T2: &str = "2026-11-05T02:00:00Z";
const ACTIONS: i64 = 15368;

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n";
const TRAILER: &str = "\n\nAcceptance-Test: tests/test_calc.py";
/// The word, kept out of this file's own command lines.
const M: &str = "mer\x67e";

/// The stand-in for `gh`: `pr view <n> --repo o/<name> ...` prints `view.<n>`; `api --paginate
/// <endpoint> --jq ...` prints `<sha>.check-runs` or `<sha>.runs`. Every call is logged in `calls`.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; dir="$FAKE_GH_DIR/$name"
  echo "pr view $3" >> "$dir/calls"
  if [ -e "$dir/fail" ]; then echo "no auth" >&2; exit 1; fi
  cat "$dir/view.$3"; exit 0
fi
ep="$3"; path=${ep#repos/}; rest=${path#*/}; name=${rest%%/*}; dir="$FAKE_GH_DIR/$name"
case "$ep" in
  */check-runs*) kind=check-runs; oid=${ep#*/commits/}; oid=${oid%%/*} ;;
  */actions/runs*) kind=runs; oid=${ep#*head_sha=}; oid=${oid%%&*} ;;
  *) echo "unexpected call: $*" >&2; exit 2 ;;
esac
echo "api $kind $oid $ep" >> "$dir/calls"
if [ -e "$dir/fail" ] || [ -e "$dir/fail-api" ]; then echo "boom" >&2; exit 1; fi
if [ -e "$dir/$oid.$kind" ]; then cat "$dir/$oid.$kind"; fi
exit 0
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The stand-in's folder and the repositories folder. The environment is set once.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        common::exe::write(&script, STAND_IN);
        std::fs::create_dir_all(home.path().join("repos")).unwrap();
        // SAFETY: set once, before this file's tests read the environment.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path());
            std::env::set_var("MAKIBA_REPOS_DIR", home.path().join("repos"));
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

/// Run the calling test's body in a child of this binary with `MAKIBA_MERGE_APPROVAL_MODE` set:
/// `true` in the child (run the body), `false` in the parent once the child passed.
fn in_mode(mode: &str, test: &str) -> bool {
    if std::env::var("GATE_TEST_CHILD").is_ok() {
        assert_eq!(
            std::env::var(gate::MODE_ENV).as_deref(),
            Ok(mode),
            "the child runs in its mode"
        );
        return true;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--test-threads=1", "--nocapture"])
        .env("GATE_TEST_CHILD", "1")
        .env(gate::MODE_ENV, mode)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "{test} in mode {mode} failed:\n{text}"
    );
    assert!(
        text.contains("1 passed"),
        "{test} did not run in the child:\n{text}"
    );
    false
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One pull request's world: an origin, the project's clone, `state.db` and a door.
struct Case {
    _tmp: tempfile::TempDir,
    name: String,
    repo: String,
    origin: PathBuf,
    gh: PathBuf,
    db: PathBuf,
    door: Door,
    secretary: String,
    /// The person's key: the gate is the person's verb (Issue 433 retired the gate key).
    person: String,
    base: String,
}

impl Case {
    /// A case whose base holds the bug, a pyproject, a workflow and another test.
    fn new(name: &str) -> Case {
        Case::with_base(
            name,
            &[
                ("calc.py", Some(BUGGY)),
                ("pyproject.toml", Some("[project]\nname = 'x'\n")),
                (".github/workflows/ci.yml", Some("on: push\n")),
                (
                    "tests/other.py",
                    Some("def test_other():\n    assert True\n"),
                ),
            ],
        )
    }

    fn with_base(name: &str, files: &[(&str, Option<&str>)]) -> Case {
        let home = init();
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git_in(&origin, &["init", "-q", "-b", "main"]);
        let gh = home.join(name);
        std::fs::create_dir_all(&gh).unwrap();
        let state = tmp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let db = state.join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: name,
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT OR IGNORE INTO projects (slug, display_name) VALUES (?1, ?1)",
                [name],
            )
            .unwrap();
        w.conn()
            .execute(
                "UPDATE projects SET origin_url = ?2 WHERE slug = ?1",
                [name, &format!("https://github.com/o/{name}")],
            )
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        let mut c = Case {
            _tmp: tmp,
            name: name.into(),
            repo: format!("o/{name}"),
            origin,
            gh,
            db,
            door,
            secretary,
            person,
            base: String::new(),
        };
        c.base = c.commit("base", files);
        let clone = home.join("repos").join(name);
        assert!(!clone.exists(), "the case name {name} is used twice");
        git_in(
            &home.join("repos"),
            &["clone", "-q", c.origin.to_str().unwrap(), name],
        );
        git_in(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://github.com/o/{name}"),
            ],
        );
        git_in(
            &clone,
            &[
                "config",
                &format!("url.{}.insteadOf", c.origin.display()),
                &format!("https://github.com/o/{name}"),
            ],
        );
        c.git(&["checkout", "-q", "-b", "feature"]);
        c
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    fn commit(&self, msg: &str, files: &[(&str, Option<&str>)]) -> String {
        for (path, content) in files {
            let dest = self.origin.join(path);
            match content {
                None => std::fs::remove_file(&dest).unwrap(),
                Some(text) => {
                    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    std::fs::write(&dest, text).unwrap();
                }
            }
        }
        self.git(&["add", "--all"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.head()
    }

    /// The test commit (with `extra` files) and the fix.
    fn land_with(&self, test: &str, trailer: &str, extra: &[(&str, Option<&str>)]) -> String {
        let mut files = vec![("tests/test_calc.py", Some(test))];
        files.extend_from_slice(extra);
        let m = self.commit(&format!("test: failing test{trailer}"), &files);
        self.commit("fix", &[("calc.py", Some(FIXED))]);
        m
    }

    fn land(&self) -> String {
        self.land_with(TEST, TRAILER, &[])
    }

    /// Publish HEAD as PR `pr`'s head and let the stand-in answer `gh pr view` for it.
    fn publish(&self, pr: i64) -> String {
        let head = self.head();
        self.git(&["update-ref", &format!("refs/pull/{pr}/head"), &head]);
        let view = json!({"number": pr, "url": format!("file://{}/pull/{pr}", self.origin.display()),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(self.gh.join(format!("view.{pr}")), view.to_string()).unwrap();
        head
    }

    /// The stand-in's answer of one `gh api` call at `sha`: the lines `--jq` would print.
    fn answer(&self, sha: &str, kind: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.gh.join(format!("{sha}.{kind}")), text).unwrap();
    }

    /// The check runs at `sha`, each `(app, name, conclusion, suite)`, completed at T1.
    fn checks(&self, sha: &str, runs: &[(i64, &str, &str, i64)]) {
        let lines: Vec<Value> = runs
            .iter()
            .map(|(app, name, c, suite)| {
                json!({"app": app, "name": name, "conclusion": c, "completed_at": T1, "suite": suite})
            })
            .collect();
        self.answer(sha, "check-runs", &lines);
    }

    /// The workflow run at `sha`: workflow 1, suite 1, made against `bases`.
    fn bases(&self, sha: &str, bases: &[&str]) {
        self.answer(
            sha,
            "runs",
            &[json!({"wf": 1, "suite": 1, "created": T1, "bases": bases})],
        );
    }

    /// A `ci_completed` row, written while no makibad runs.
    fn ci(&self, pr: i64, status: &str, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        w.append_event(&NewEvent {
            kind: "ci_completed",
            actor: Some("makibad"),
            payload: Some(
                &json!({"repo": self.repo, "pr": pr, "head_oid": head, "status": status,
                                   "fail_count": 0, "pending_count": 0, "total_checks": 1,
                                   "probe_attempts": 1}),
            ),
            ..Default::default()
        })
        .unwrap();
    }

    /// CI as the secretary's procedure leaves it: every commit of the PR but its head was pushed
    /// and failed (the job `test` failed there), the head passed; all on base C.
    fn ci_all(&self, pr: i64) -> String {
        let head = self.publish(pr);
        let range = format!("{}..{head}", self.base);
        for sha in self.git(&["rev-list", "--reverse", &range]).lines() {
            let ok = sha == head;
            self.checks(
                sha,
                &[(ACTIONS, "test", if ok { "success" } else { "failure" }, 1)],
            );
            self.bases(sha, &[C]);
            if !ok {
                self.ci(pr, "failed", sha);
            }
        }
        self.ci(pr, "passed", &head);
        head
    }

    /// The door's answer to `gate` for PR `pr` at `head`, asked with the person's key.
    fn gate_at(&self, pr: i64, head: &str) -> Value {
        self.door
            .handle(
                Some(&self.person),
                "gate",
                json!({"repo": self.repo, "pr": pr, "head": head}),
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("gate: {e:?}"))
    }

    /// Publish HEAD as PR 7 with CI as above, and ask the gate.
    fn verdict(&self) -> Value {
        let head = self.ci_all(7);
        self.gate_at(7, &head)
    }

    fn assert_verdict(&self, verdict: &str, needle: &str) -> Value {
        let v = self.verdict();
        assert_eq!(v["verdict"], json!(verdict), "{v}");
        assert!(
            v["reason"].as_str().unwrap().contains(needle),
            "{needle:?} not in {v}"
        );
        v
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.gh.join("calls")).unwrap_or_default()
    }

    /// The payloads of `acceptance_gate`, oldest first.
    fn gate_events(&self) -> Vec<Value> {
        let db = Connection::open(&self.db).unwrap();
        let mut st = db
            .prepare("SELECT payload_json FROM events WHERE kind = 'acceptance_gate' ORDER BY id")
            .unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
            .collect()
    }

    /// Back to the base and land again: the test commit rewritten (a force push).
    fn rewrite_test(&self, test: &str) {
        self.git(&["reset", "-q", "--hard", &self.base]);
        self.land_with(test, TRAILER, &[]);
    }
}

fn move_base(c: &mut Case, files: &[(&str, Option<&str>)]) {
    let default = [
        ("README", Some("x\n")),
        (
            "pyproject.toml",
            Some("[project]\nname = 'x'\nversion = '2'\n"),
        ),
    ];
    c.git(&["checkout", "-q", "main"]);
    c.base = c.commit(
        "base moves",
        if files.is_empty() { &default } else { files },
    );
    c.git(&["checkout", "-q", "feature"]);
}

fn merge_base_in(c: &Case, flags: &[&str]) {
    let mut args = vec![M, "-q", "--no-edit"];
    args.extend_from_slice(flags);
    args.push("main");
    c.git(&args);
}

fn merge_cmd(n: &str, repo: &str, method: &str, sha: &str, tail: &str) -> String {
    format!("gh pr {M} {n} --repo {repo} {method} --match-head-commit {sha}{tail}")
}

// ---- the review state of the head ---------------------------------------------------------------

impl Case {
    /// Task `t1` is launched as `kind`, and a watch ties it to PR `pr` at `head`.
    fn tie(&self, pr: i64, head: &str, kind: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = 't1'", [], |r| r.get(0))
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, 't1-s1', ?2, 'fake', 'b', '/nonexistent', 'verified')",
                rusqlite::params![run, kind],
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO pr_watches (repo, pr, head_oid, run_id) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![self.repo, pr, head, run],
            )
            .unwrap();
    }

    /// A diff review the worker of `t1` ran, recorded as the door records it.
    fn review(&self, verdict: &str, head: Option<&str>, findings: Value, reason: Option<&str>) {
        let w = StateWriter::open(&self.db).unwrap();
        let answer = json!({"verdict": verdict, "kind": "diff", "reader": "codex", "round": 1,
                            "allowed": 3, "head": head, "findings": findings, "reason": reason});
        w.conn()
            .execute(
                "INSERT INTO commands (actor, verb, status, result_json)
                 VALUES (?1, 'review', 'done', ?2)",
                rusqlite::params![
                    makiba::spec::worker_agent_id("t1"),
                    json!({"result": answer}).to_string()
                ],
            )
            .unwrap();
    }

    fn clean_review(&self, head: &str) {
        self.review(
            "passed",
            Some(head),
            json!({"blocker": 0, "major": 1, "minor": 0}),
            None,
        );
    }
}

/// A test-first pull request 7 with passed CI, tied to the code task `t1`; its head.
fn tied(name: &str) -> (Case, String) {
    let c = Case::new(name);
    c.land();
    let head = c.ci_all(7);
    c.tie(7, &head, "code");
    (c, head)
}

fn reason(v: &Value) -> &str {
    v["reason"].as_str().unwrap()
}

// ---- Issue 564 ----------------------------------------------------------------------------------

/// The full ids the answer's review names as read by no review.
fn unread(v: &Value) -> Vec<String> {
    v["review"]["unread"]
        .as_array()
        .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// The test commit and the fix (the head the review read), then `later` commits, each changing
/// `calc.py` to the text given; CI passed at the new head, tied to the code task `t1`, whose last
/// diff review read the fix. (case, reviewed head, later commits).
fn reviewed_then(name: &str, later: &[&str]) -> (Case, String, Vec<String>) {
    let c = Case::new(name);
    c.land();
    let reviewed = c.head();
    let shas: Vec<String> = later
        .iter()
        .enumerate()
        .map(|(i, text)| {
            c.commit(
                &format!("round {} fixes", i + 4),
                &[("calc.py", Some(text))],
            )
        })
        .collect();
    let head = c.ci_all(7);
    assert_eq!(Some(&head), shas.last().or(Some(&reviewed)));
    c.tie(7, &head, "code");
    c.clean_review(&reviewed);
    (c, reviewed, shas)
}

const ROUND4: &str = "def add(a, b):\n    return b + a\n";

const NO_READER: &str = "no reader could start";

/// A review attempt after the passed one: no reader could start, so no head.
fn headless_attempt(c: &Case) {
    c.review("not-established", None, Value::Null, Some(NO_READER));
}

/// The refusal of a code head after the reviewed one: verdict, state, heads named, and the record.
fn assert_stale(c: &Case, v: &Value, reviewed: &str, head: &str) {
    assert_eq!(
        (&v["verdict"], &v["allow"]),
        (&json!("review_missing"), &json!(false)),
        "{v}"
    );
    assert_ne!(v["warn"], json!(true), "{v}");
    assert_eq!(v["review"]["state"], json!("stale"), "{v}");
    assert_eq!(v["review"]["reviewed_head"], json!(reviewed), "{v}");
    assert_eq!(unread(v), vec![head.to_string()], "{v}");
    assert!(
        reason(v).contains(&reviewed[..12]) && reason(v).contains(&head[..12]),
        "{v}"
    );
    assert!(!reason(v).contains("WARNING"), "{v}");
    let e = c.gate_events().pop().unwrap();
    assert_eq!(e["allow"], json!(false), "{e}");
    assert_eq!(e["review"]["unread"], json!([head]), "{e}");
}

#[test]
fn a_headless_attempt_after_the_reviewed_code_head_does_not_hide_it_under_human() {
    if !in_mode(
        "human",
        "a_headless_attempt_after_the_reviewed_code_head_does_not_hide_it_under_human",
    ) {
        return;
    }
    let (c, reviewed, later) = reviewed_then("cn564hh", &[ROUND4]);
    headless_attempt(&c);
    let v = c.gate_at(7, &later[0]);
    assert_stale(&c, &v, &reviewed, &later[0]);
}

#[test]
fn a_headless_attempt_after_the_reviewed_code_head_does_not_hide_it_under_acceptance() {
    if !in_mode(
        "acceptance",
        "a_headless_attempt_after_the_reviewed_code_head_does_not_hide_it_under_acceptance",
    ) {
        return;
    }
    let (c, reviewed, later) = reviewed_then("cn564ha", &[ROUND4]);
    headless_attempt(&c);
    let v = c.gate_at(7, &later[0]);
    assert_stale(&c, &v, &reviewed, &later[0]);
}

#[test]
fn a_round_that_read_nothing_at_the_code_head_does_not_replace_the_reviewed_one() {
    if !in_mode(
        "human",
        "a_round_that_read_nothing_at_the_code_head_does_not_replace_the_reviewed_one",
    ) {
        return;
    }
    let (c, reviewed, later) = reviewed_then("cn564rn", &[ROUND4]);
    // A round at the new head whose reader opened nothing of the change: not-established, with
    // the head of its snapshot.
    c.review(
        "not-established",
        Some(&later[0]),
        Value::Null,
        Some("the reader opened no file of the change"),
    );
    let v = c.gate_at(7, &later[0]);
    assert_stale(&c, &v, &reviewed, &later[0]);
}

#[test]
fn without_the_attempt_the_code_head_after_the_reviewed_one_is_stale_under_human() {
    if !in_mode(
        "human",
        "without_the_attempt_the_code_head_after_the_reviewed_one_is_stale_under_human",
    ) {
        return;
    }
    let (c, reviewed, later) = reviewed_then("cn564wh", &[ROUND4]);
    let v = c.gate_at(7, &later[0]);
    assert_stale(&c, &v, &reviewed, &later[0]);
}

#[test]
fn without_the_attempt_the_code_head_after_the_reviewed_one_is_stale_under_acceptance() {
    if !in_mode(
        "acceptance",
        "without_the_attempt_the_code_head_after_the_reviewed_one_is_stale_under_acceptance",
    ) {
        return;
    }
    let (c, reviewed, later) = reviewed_then("cn564wa", &[ROUND4]);
    let v = c.gate_at(7, &later[0]);
    assert_stale(&c, &v, &reviewed, &later[0]);
}

#[test]
fn a_headless_attempt_at_the_reviewed_code_head_stays_not_established() {
    if !in_mode(
        "human",
        "a_headless_attempt_at_the_reviewed_code_head_stays_not_established",
    ) {
        return;
    }
    let (c, reviewed, _) = reviewed_then("cn564same", &[]);
    headless_attempt(&c);
    let v = c.gate_at(7, &reviewed);
    assert_eq!(
        (&v["verdict"], &v["allow"], &v["warn"]),
        (&json!("passed"), &json!(true), &json!(true)),
        "{v}"
    );
    assert_eq!(v["review"]["state"], json!("not-established"), "{v}");
    assert_eq!(v["review"]["reviewed_head"], json!(reviewed), "{v}");
    assert_eq!(v["review"]["reason"], json!(NO_READER), "{v}");
    assert!(reason(&v).contains("WARNING"), "{v}");
}

#[test]
fn a_headless_attempt_after_a_clean_merge_of_the_base_keeps_the_carried_review() {
    if !in_mode(
        "human",
        "a_headless_attempt_after_a_clean_merge_of_the_base_keeps_the_carried_review",
    ) {
        return;
    }
    let mut c = Case::new("cn564join");
    c.land();
    let reviewed = c.ci_all(7);
    move_base(&mut c, &[("README", Some("moved\n"))]);
    merge_base_in(&c, &[]);
    let head = c.ci_all(7);
    assert_ne!(head, reviewed);
    c.tie(7, &head, "code");
    c.clean_review(&reviewed);
    headless_attempt(&c);
    let v = c.gate_at(7, &head);
    assert_eq!(
        (&v["verdict"], &v["allow"], &v["warn"]),
        (&json!("passed"), &json!(true), &json!(true)),
        "{v}"
    );
    assert_eq!(v["review"]["state"], json!("not-established"), "{v}");
    assert_eq!(v["review"]["reviewed_head"], json!(reviewed), "{v}");
    assert_eq!(v["review"]["same_changes"], json!(true), "{v}");
}
