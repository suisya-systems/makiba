//! Issue 513 (docs/design/helper-fix.md, option D): the merge gate asks the person's key for a
//! test helper changed after the test commit, for a declared test that head holds as git's clean
//! merge of the test commit and the base, and for a helper the base changed after the test
//! commit, under either `merge_approval_mode`.
//!
//! - Each is a warning line naming every path with the object ids it was judged from (`none` for
//!   a missing entry); the answer allows with `warn`, `merge request` is pending, and only the
//!   person's `merge approve` lands it. An approval whose gate now reads other ids is refused.
//! - The `acceptance_gate` payload carries `helpers_changed`, `merged_tests` and `base_helpers`.
//! - What stays as it was: test infrastructure (`tests/conftest.py`) is still protected by
//!   condition (a), also through a clean base merge; a resolved conflict in a declared test is
//!   still refused; an existing test target edited after the test commit is still `tests_edited`
//!   under `acceptance`; a helper the test commit itself changed is part of the test.
//!
//! The fixture is that of `tests/merge_approve.rs` (a local origin publishing
//! `refs/pull/<n>/head`, the project's clone, a stand-in `gh`, the door with the person's and
//! the secretary's keys; the gate is asked with the person's key, as `tests/belt_gate.rs` asks
//! it). The mode is read from makibad's environment, so each case runs in a child process of this
//! test binary, as in `tests/acceptance_gate.rs`.

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key, sha256_hex};
use makiba::daemon::gate;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::{DoorError, ErrorKind, Role};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once, OnceLock};

const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
const T1: &str = "2026-11-05T01:00:00Z";
const ACTIONS: i64 = 15368;
const MERGE_SHA: &str = "abcdefabcdefabcdefabcdefabcdef0123";
/// The word, kept out of this file's own command lines.
const M: &str = "mer\x67e";

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n";
const TRAILER: &str = "\n\nAcceptance-Test: tests/test_calc.py";
const HELPER: &str = "def check(x):\n    assert x\n";
const HELPER_FIXED: &str = "def check(x):\n    assert x, 'check'\n";
const CONFTEST: &str = "import pytest\n\n\n# one\n# two\n# three\n# four\n# end\n";
const OTHER: &str = "def test_other():\n    assert True\n";
const OTHER_EDITED: &str = "def test_other():\n    assert 1 == 1\n";
/// A declared test the base holds before the test commit (cases 3 and 4): the test commit
/// changes its first assertion, the base its last line, far apart.
const CALC_BASE: &str = "from calc import add\n\n\ndef test_add():\n    assert add(1, 1) == 2\n    # a\n    # b\n    # c\n    # d\n    # end\n";
const CALC_M: &str = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n    # a\n    # b\n    # c\n    # d\n    # end\n";

/// The stand-in for `gh` (as `tests/merge_approve.rs`): the case's folder is named by the
/// repository in the arguments. `pr view <n>` prints `view.<n>`; the check runs and workflow runs
/// of a commit print `<sha>.check-runs` / `<sha>.runs`; the REST merge prints `put.out`; any
/// other GraphQL call prints `read.json`. Every call is logged in `calls`.
const STAND_IN: &str = r#"#!/bin/sh
name=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--repo" ]; then name=${a#*/}; fi
  case "$a" in
    repos/*) r=${a#repos/}; r=${r#*/}; name=${r%%/*} ;;
    name=*) name=${a#name=} ;;
    id=PR_*) name=${a#id=PR_} ;;
  esac
  prev="$a"
done
dir="$FAKE_GH_DIR/$name"
echo "$*" >> "$dir/calls"
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then cat "$dir/view.$3"; exit 0; fi
case "$*" in
  *check-runs*) oid=${3#*/commits/}; oid=${oid%%/*}
    if [ -e "$dir/$oid.check-runs" ]; then cat "$dir/$oid.check-runs"; fi; exit 0 ;;
  *actions/runs*) oid=${3#*head_sha=}; oid=${oid%%&*}
    if [ -e "$dir/$oid.runs" ]; then cat "$dir/$oid.runs"; fi; exit 0 ;;
  *"-X PUT"*) cat "$dir/put.out"; exit 0 ;;
  *graphql*) cat "$dir/read.json"; exit 0 ;;
esac
echo "unexpected call: $*" >&2; exit 2
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The stand-in's folder and the repositories folder. The environment is set once; the mode is
/// set by `in_mode` for the child.
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

/// One pull request's world: an origin, the project's clone, `state.db` and the door.
struct Case {
    _tmp: tempfile::TempDir,
    name: String,
    repo: String,
    origin: PathBuf,
    gh: PathBuf,
    db: PathBuf,
    door: Door,
    secretary: String,
    person: String,
    /// The base the branch started from.
    base: String,
}

impl Case {
    /// The base holds the bug, a helper, the test infrastructure and another test (and `extra`);
    /// the branch `feature` starts from it. The task `t1` is open.
    fn new(name: &str, extra: &[(&str, Option<&str>)]) -> Case {
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
        let mut files = vec![
            ("calc.py", Some(BUGGY)),
            ("tests/common/helper.py", Some(HELPER)),
            ("tests/conftest.py", Some(CONFTEST)),
            ("tests/test_other.py", Some(OTHER)),
        ];
        files.extend_from_slice(extra);
        c.base = c.commit("base", &files);
        let clone = home.join("repos").join(name);
        assert!(!clone.exists(), "the case name {name} is used twice");
        git_in(
            &home.join("repos"),
            &["clone", "-q", c.origin.to_str().unwrap(), name],
        );
        let url = format!("https://github.com/o/{name}");
        git_in(&clone, &["remote", "set-url", "origin", &url]);
        git_in(
            &clone,
            &[
                "config",
                &format!("url.{}.insteadOf", c.origin.display()),
                &url,
            ],
        );
        c.git(&["checkout", "-q", "-b", "feature"]);
        c
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
    }

    fn write(&self, files: &[(&str, Option<&str>)]) {
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
    }

    fn commit(&self, msg: &str, files: &[(&str, Option<&str>)]) -> String {
        self.write(files);
        self.git(&["add", "--all"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// The test commit: it declares `tests/test_calc.py`, with `files`. Its id.
    fn test_commit(&self, files: &[(&str, Option<&str>)]) -> String {
        self.commit(&format!("test: failing test{TRAILER}"), files)
    }

    /// The fix: `calc.py` fixed, with `files`.
    fn fix(&self, files: &[(&str, Option<&str>)]) -> String {
        let mut all = vec![("calc.py", Some(FIXED))];
        all.extend_from_slice(files);
        self.commit("fix", &all)
    }

    /// A landing on the base (`main`) with `files`; the branch stays where it was. Its id.
    fn land_on_base(&self, files: &[(&str, Option<&str>)]) -> String {
        self.git(&["checkout", "-q", "main"]);
        let id = self.commit("a landing", files);
        self.git(&["checkout", "-q", "feature"]);
        id
    }

    /// Bring the base into the branch with git's own merge, with no conflict.
    fn base_in_clean(&self) {
        self.git(&[M, "-q", "--no-edit", "main"]);
    }

    /// Bring the base into the branch, the merge commit holding `files` as its own resolution.
    fn base_in_resolved(&self, files: &[(&str, Option<&str>)]) {
        self.git(&[M, "-q", "--no-commit", "--no-ff", "main"]);
        self.write(files);
        self.git(&["add", "--all"]);
        self.git(&["commit", "-q", "--no-edit"]);
    }

    fn blob(&self, rev: &str, path: &str) -> String {
        self.git(&["rev-parse", &format!("{rev}:{path}")])
    }

    /// Publish the branch as PR 7: every commit of the PR but its head failed the job `test`, the
    /// head passed it, all on base C; the worker's diff review passed at the head, a watch ties
    /// the task to the PR, and GitHub reads it open at the head. Its head.
    fn publish(&self) -> String {
        let head = self.git(&["rev-parse", "HEAD"]);
        self.git(&["update-ref", "refs/pull/7/head", &head]);
        let view = json!({"number": 7, "url": format!("file://{}/pull/7", self.origin.display()),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(self.gh.join("view.7"), view.to_string()).unwrap();
        let range = format!("{}..{head}", self.base);
        for sha in self.git(&["rev-list", "--reverse", &range]).lines() {
            let ok = sha == head;
            let conclusion = if ok { "success" } else { "failure" };
            self.answer(
                sha,
                "check-runs",
                &[
                    json!({"app": ACTIONS, "name": "test", "conclusion": conclusion, "completed_at": T1, "suite": 1}),
                ],
            );
            self.answer(
                sha,
                "runs",
                &[json!({"wf": 1, "suite": 1, "created": T1, "bases": [C]})],
            );
            if !ok {
                self.ci("failed", sha);
            }
        }
        self.ci("passed", &head);
        self.reviewed(&head);
        let read = json!({"data": {"repository": {"pullRequest": {
            "id": format!("PR_{}", self.name), "state": "OPEN", "headRefOid": head,
            "isMergeQueueEnabled": false, "isInMergeQueue": false, "mergeQueueEntry": null,
            "mergeCommit": null, "mergedAt": null,
        }}}});
        std::fs::write(self.gh.join("read.json"), read.to_string()).unwrap();
        let body = json!({"sha": MERGE_SHA, "merged": true});
        std::fs::write(
            self.gh.join("put.out"),
            format!("HTTP/2.0 200 OK\n\n{body}\n"),
        )
        .unwrap();
        head
    }

    fn answer(&self, sha: &str, kind: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.gh.join(format!("{sha}.{kind}")), text).unwrap();
    }

    fn ci(&self, status: &str, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        w.append_event(&NewEvent {
            kind: "ci_completed",
            actor: Some("makibad"),
            payload: Some(
                &json!({"repo": self.repo, "pr": 7, "head_oid": head, "status": status,
                                   "fail_count": 0, "pending_count": 0, "total_checks": 1,
                                   "probe_attempts": 1}),
            ),
            ..Default::default()
        })
        .unwrap();
    }

    /// The code task `t1` has a passed diff review of `head`, and a watch ties it to PR 7.
    fn reviewed(&self, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = 't1'", [], |r| r.get(0))
            .unwrap();
        let answer = json!({"verdict": "passed", "kind": "diff", "reader": "codex", "round": 1,
                            "allowed": 3, "head": head,
                            "findings": {"blocker": 0, "major": 0, "minor": 0}});
        let t = |sql: &str, p: &[&dyn rusqlite::ToSql]| w.conn().execute(sql, p).unwrap();
        t(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES (?1, 't1-s1', 'code', 'fake', 'b', '/nonexistent', 'verified')",
            &[&run],
        );
        t(
            "INSERT INTO pr_watches (repo, pr, head_oid, run_id) VALUES (?1, 7, ?2, ?3)",
            &[&self.repo, &head, &run],
        );
        t(
            "INSERT INTO commands (actor, verb, status, result_json) VALUES (?1, 'review', 'done', ?2)",
            &[
                &makiba::spec::worker_agent_id("t1"),
                &json!({"result": answer}).to_string(),
            ],
        );
    }

    /// The door's answer to `gate` for PR 7 at `head`.
    fn gate_at(&self, head: &str) -> Value {
        self.door
            .handle(
                Some(&self.person),
                "gate",
                json!({"repo": self.repo, "pr": 7, "head": head}),
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("gate: {e:?}"))
    }

    /// The secretary's `merge` request for PR 7 at `head`.
    fn request(&self, head: &str) -> Value {
        self.door
            .handle(
                Some(&self.secretary),
                "merge",
                json!({"action": "request", "repo": self.repo, "pr": 7, "head": head}),
                None,
                true,
            )
            .unwrap_or_else(|e| panic!("merge request: {e:?}"))
    }

    /// The person's approval of entry `id` with the sum of `printed`.
    fn approve(&self, id: i64, printed: &str) -> Result<Value, DoorError> {
        self.door.handle(
            Some(&self.person),
            "merge",
            json!({"action": "approve", "id": id, "shown": sha256_hex(printed)}),
            None,
            false,
        )
    }

    /// What `makiba merge approve <id>` prints, read as the CLI reads it.
    fn printed(&self, id: i64) -> String {
        makiba::status_local::merge_entry_text(&self.db, id)
            .unwrap_or_else(|e| panic!("entry {id}: {e}"))
    }

    /// The payloads of the events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let db = Connection::open(&self.db).unwrap();
        let mut st = db
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        st.query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
            .collect()
    }

    fn last_gate(&self) -> Value {
        self.events("acceptance_gate")
            .pop()
            .expect("no acceptance_gate")
    }
}

fn reason(v: &Value) -> &str {
    v["reason"].as_str().unwrap()
}

/// `merge request` answered pending: its entry's id. Nothing was attempted.
fn pending(c: &Case, v: &Value) -> i64 {
    assert_eq!(v["status"], json!("pending"), "{v}");
    assert!(c.events("merge_attempted").is_empty(), "{v}");
    v["pending_id"].as_i64().unwrap_or_else(|| panic!("{v}"))
}

/// The answer allows with a warning.
fn warns(v: &Value) {
    assert_eq!(
        (&v["allow"], &v["warn"]),
        (&json!(true), &json!(true)),
        "{v}"
    );
}

// ---- 1. a helper the fix changes ------------------------------------------------------------------

/// The case of #506: the fix changes the code and a helper the test commit left alone.
fn helper_fix(name: &str) -> (Case, String, String) {
    let c = Case::new(name, &[]);
    let m = c.test_commit(&[("tests/test_calc.py", Some(TEST))]);
    c.fix(&[("tests/common/helper.py", Some(HELPER_FIXED))]);
    let head = c.publish();
    (c, m, head)
}

#[test]
fn a_helper_the_fix_changes_warns_and_lands_only_by_the_persons_key() {
    if !in_mode(
        "acceptance",
        "a_helper_the_fix_changes_warns_and_lands_only_by_the_persons_key",
    ) {
        return;
    }
    let (c, m, head) = helper_fix("hfaccept");
    let (at_m, at_head) = (
        c.blob(&m, "tests/common/helper.py"),
        c.blob(&head, "tests/common/helper.py"),
    );
    let v = c.gate_at(&head);
    assert_eq!(v["verdict"], json!("passed"), "{v}");
    warns(&v);
    assert!(
        reason(&v)
            .lines()
            .any(|l| l.contains("tests/common/helper.py")
                && l.contains(&at_m)
                && l.contains(&at_head)),
        "no line names the helper with both ids: {v}"
    );
    let e = c.last_gate();
    assert_eq!(
        e["helpers_changed"],
        json!(["tests/common/helper.py"]),
        "{e}"
    );
    assert!(e["edited_tests"].is_null(), "{e}");

    let id = pending(&c, &c.request(&head));
    let text = c.printed(id);
    assert!(text.contains(&at_m) && text.contains(&at_head), "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains("tests/common/helper.py") && l.contains('|')),
        "no diff stat line for the helper: {text}"
    );
    let a = c
        .approve(id, &text)
        .unwrap_or_else(|e| panic!("approve: {e:?}"));
    let attempts = c.events("merge_attempted");
    assert_eq!(attempts.len(), 1, "{a}");
    assert_eq!(attempts[0]["approved_by"], json!("person"), "{a}");
}

#[test]
fn a_helper_the_fix_changes_is_pending_under_human_too() {
    if !in_mode(
        "human",
        "a_helper_the_fix_changes_is_pending_under_human_too",
    ) {
        return;
    }
    let (c, _, head) = helper_fix("hfhuman");
    warns(&c.gate_at(&head));
    pending(&c, &c.request(&head));
}

// ---- 2. a helper added or deleted after the test commit -------------------------------------------

#[test]
fn a_helper_added_or_deleted_after_the_test_commit_warns() {
    if !in_mode(
        "acceptance",
        "a_helper_added_or_deleted_after_the_test_commit_warns",
    ) {
        return;
    }
    let c = Case::new("hfadded", &[]);
    c.test_commit(&[("tests/test_calc.py", Some(TEST))]);
    c.fix(&[("tests/common/new.py", Some(HELPER))]);
    let head = c.publish();
    let v = c.gate_at(&head);
    warns(&v);
    assert_eq!(
        c.last_gate()["helpers_changed"],
        json!(["tests/common/new.py"])
    );
    let id = c.blob(&head, "tests/common/new.py");
    assert!(
        reason(&v).contains(&format!(
            "tests/common/new.py (test commit none, head {id})"
        )),
        "{v}"
    );

    let c = Case::new("hfdeleted", &[]);
    let m = c.test_commit(&[("tests/test_calc.py", Some(TEST))]);
    c.fix(&[("tests/common/helper.py", None)]);
    let head = c.publish();
    let v = c.gate_at(&head);
    warns(&v);
    assert_eq!(
        c.last_gate()["helpers_changed"],
        json!(["tests/common/helper.py"])
    );
    let id = c.blob(&m, "tests/common/helper.py");
    assert!(
        reason(&v).contains(&format!("(test commit {id}, head none)")),
        "{v}"
    );
}

// ---- 3 and 4. a base merge into a declared test ---------------------------------------------------

/// The test commit changes the first assertion of `tests/test_calc.py`; the base then changes its
/// last line. The test commit's id.
fn declared_test_moved_on_base(c: &Case) -> String {
    let m = c.test_commit(&[("tests/test_calc.py", Some(CALC_M))]);
    c.land_on_base(&[(
        "tests/test_calc.py",
        Some(&CALC_BASE.replace("# end", "# the end")),
    )]);
    m
}

#[test]
fn a_clean_base_merge_into_a_declared_test_warns() {
    if !in_mode(
        "acceptance",
        "a_clean_base_merge_into_a_declared_test_warns",
    ) {
        return;
    }
    let c = Case::new("hfmerged", &[("tests/test_calc.py", Some(CALC_BASE))]);
    let m = declared_test_moved_on_base(&c);
    c.base_in_clean();
    c.fix(&[]);
    let head = c.publish();
    let v = c.gate_at(&head);
    assert_ne!(v["verdict"], json!("rejected"), "{v}");
    warns(&v);
    assert_eq!(c.last_gate()["merged_tests"], json!(["tests/test_calc.py"]));
    for id in [
        c.blob(&m, "tests/test_calc.py"),
        c.blob("main", "tests/test_calc.py"),
        c.blob(&head, "tests/test_calc.py"),
    ] {
        assert!(reason(&v).contains(&id), "{id} not in {v}");
    }
}

#[test]
fn a_resolved_conflict_in_a_declared_test_is_still_refused() {
    if !in_mode(
        "acceptance",
        "a_resolved_conflict_in_a_declared_test_is_still_refused",
    ) {
        return;
    }
    let c = Case::new("hfresolved", &[("tests/test_calc.py", Some(CALC_BASE))]);
    declared_test_moved_on_base(&c);
    c.base_in_resolved(&[(
        "tests/test_calc.py",
        Some(&CALC_M.replace("# end", "# resolved by hand")),
    )]);
    c.fix(&[]);
    let head = c.publish();
    let v = c.gate_at(&head);
    assert_eq!(v["verdict"], json!("rejected"), "{v}");
    assert!(
        reason(&v).contains("protected paths changed after the test commit")
            && reason(&v).contains("tests/test_calc.py"),
        "{v}"
    );
}

// ---- 5. a helper the base changed -----------------------------------------------------------------

#[test]
fn a_helper_the_base_changed_warns_and_a_second_change_makes_the_approval_stale() {
    if !in_mode(
        "acceptance",
        "a_helper_the_base_changed_warns_and_a_second_change_makes_the_approval_stale",
    ) {
        return;
    }
    let c = Case::new("hfbase", &[]);
    let m = c.test_commit(&[("tests/test_calc.py", Some(TEST))]);
    c.fix(&[]);
    let head = c.publish();
    let first = c.land_on_base(&[("tests/common/helper.py", Some(HELPER_FIXED))]);
    let v = c.gate_at(&head);
    warns(&v);
    assert_eq!(
        c.last_gate()["base_helpers"],
        json!(["tests/common/helper.py"])
    );
    let (at_parent, at_base) = (
        c.blob(&format!("{m}^"), "tests/common/helper.py"),
        c.blob(&first, "tests/common/helper.py"),
    );
    assert!(
        reason(&v).contains(&at_parent) && reason(&v).contains(&at_base),
        "{v}"
    );
    let id = pending(&c, &c.request(&head));
    let shown = c.printed(id);

    // The base lands a second change to the same helper; the pull request's head is unchanged.
    let second = c.land_on_base(&[("tests/common/helper.py", Some("def check(x):\n    pass\n"))]);
    let e = c
        .approve(id, &shown)
        .expect_err("an approval of other contents is refused");
    assert_eq!(e.kind, ErrorKind::Stale, "{e:?}");
    assert!(c.events("merge_attempted").is_empty());
    let again = pending(&c, &c.request(&head));
    let now = c.printed(again);
    assert!(
        now.contains(&c.blob(&second, "tests/common/helper.py")),
        "{now}"
    );
}

// ---- 6. test infrastructure stays protected -------------------------------------------------------

fn conftest_by_the_fix(mode: &str, name: &str) {
    let c = Case::new(name, &[]);
    c.test_commit(&[("tests/test_calc.py", Some(TEST))]);
    c.fix(&[("tests/conftest.py", Some("import pytest\n"))]);
    let head = c.publish();
    let v = c.gate_at(&head);
    assert_eq!(
        (&v["verdict"], &v["allow"]),
        (&json!("rejected"), &json!(false)),
        "{mode}: {v}"
    );
    assert!(
        reason(&v).contains("protected paths changed after the test commit")
            && reason(&v).contains("tests/conftest.py"),
        "{mode}: {v}"
    );
}

#[test]
fn test_infrastructure_the_fix_changes_is_refused_under_acceptance() {
    if !in_mode(
        "acceptance",
        "test_infrastructure_the_fix_changes_is_refused_under_acceptance",
    ) {
        return;
    }
    conftest_by_the_fix("acceptance", "hfconfa");
}

#[test]
fn test_infrastructure_the_fix_changes_is_refused_under_human() {
    if !in_mode(
        "human",
        "test_infrastructure_the_fix_changes_is_refused_under_human",
    ) {
        return;
    }
    conftest_by_the_fix("human", "hfconfh");
}

/// Design review: infrastructure the test commit changed is not let through by a clean base merge.
#[test]
fn test_infrastructure_through_a_clean_base_merge_is_still_refused() {
    if !in_mode(
        "acceptance",
        "test_infrastructure_through_a_clean_base_merge_is_still_refused",
    ) {
        return;
    }
    let c = Case::new("hfconfmerged", &[]);
    c.test_commit(&[
        ("tests/test_calc.py", Some(TEST)),
        (
            "tests/conftest.py",
            Some(&CONFTEST.replace("import pytest", "import pytest  # used")),
        ),
    ]);
    c.land_on_base(&[(
        "tests/conftest.py",
        Some(&CONFTEST.replace("# end", "# the end")),
    )]);
    c.base_in_clean();
    c.fix(&[]);
    let head = c.publish();
    let v = c.gate_at(&head);
    assert_eq!(
        (&v["verdict"], &v["allow"]),
        (&json!("rejected"), &json!(false)),
        "{v}"
    );
    assert!(
        reason(&v).contains("protected paths changed after the test commit")
            && reason(&v).contains("tests/conftest.py"),
        "{v}"
    );
}

// ---- 7. an existing test target edited ------------------------------------------------------------

fn edits_another_test(name: &str) -> (Case, String) {
    let c = Case::new(name, &[]);
    c.test_commit(&[("tests/test_calc.py", Some(TEST))]);
    c.fix(&[("tests/test_other.py", Some(OTHER_EDITED))]);
    let head = c.publish();
    (c, head)
}

#[test]
fn an_edited_test_target_is_still_tests_edited_under_acceptance() {
    if !in_mode(
        "acceptance",
        "an_edited_test_target_is_still_tests_edited_under_acceptance",
    ) {
        return;
    }
    let (c, head) = edits_another_test("hfeditedacc");
    let v = c.gate_at(&head);
    assert_eq!(
        (&v["verdict"], &v["allow"]),
        (&json!("tests_edited"), &json!(false)),
        "{v}"
    );
    let e = c.last_gate();
    assert_eq!(e["edited_tests"], json!(["tests/test_other.py"]), "{e}");
    assert!(e["helpers_changed"].is_null(), "{e}");
}

#[test]
fn an_edited_test_target_is_still_asked_for_under_human() {
    if !in_mode(
        "human",
        "an_edited_test_target_is_still_asked_for_under_human",
    ) {
        return;
    }
    let (c, head) = edits_another_test("hfeditedhum");
    warns(&c.gate_at(&head));
}

// ---- 8. a helper the test commit itself changed ---------------------------------------------------

#[test]
fn a_helper_the_test_commit_extends_is_part_of_the_test() {
    if !in_mode(
        "acceptance",
        "a_helper_the_test_commit_extends_is_part_of_the_test",
    ) {
        return;
    }
    let c = Case::new("hfintest", &[]);
    c.test_commit(&[
        ("tests/test_calc.py", Some(TEST)),
        ("tests/common/helper.py", Some(HELPER_FIXED)),
    ]);
    c.fix(&[]);
    let head = c.publish();
    let v = c.gate_at(&head);
    assert_eq!(
        (&v["verdict"], &v["allow"], &v["warn"]),
        (&json!("passed"), &json!(true), &json!(false)),
        "{v}"
    );
    let e = c.last_gate();
    assert!(e["helpers_changed"].is_null(), "{e}");
    assert!(e["base_helpers"].is_null(), "{e}");
}
