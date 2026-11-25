//! Issue 331: a pull request that adds or tightens a check over `tests/` waits while another open
//! pull request makibad follows has a test commit, whose declared tests cannot change
//! (docs/design/merge-order.md). The case of PR 314 (Issue 312), which landed a scan over
//! `tests/` with its helper `tests/common/makibad.rs` while PR 315 and PR 316 had test commits
//! written before it: under `merge_approval_mode` `acceptance` the gate refuses 314 with verdict
//! `merge_order` naming them, and allows it once they landed; under `human` it warns.
//!
//! The world is that of `tests/gate_review_head.rs` (its helpers copied here): the door runs in
//! this process on real git repositories, `gh` is a stand-in script, and each mode runs in a child
//! process of this test binary.

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
const T1: &str = "2026-11-05T01:00:00Z";
const ACTIONS: i64 = 15368;

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n";
/// The environment variable that selects the helper mode of this binary (assertion 9).
const HELPER: &str = "MERGE_ORDER_HELPER";

/// The stand-in for `gh`: `pr view <n> --repo o/<name> ...` prints `view.<n>` (first running this
/// test binary's helper mode once when `hook.<n>` is there); `api --paginate <endpoint> --jq ...`
/// prints `<sha>.check-runs` or `<sha>.runs`.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; dir="$FAKE_GH_DIR/$name"
  echo "pr view $3" >> "$dir/calls"
  if [ -e "$dir/hook.$3" ]; then
    MERGE_ORDER_HELPER="$dir/hook.$3" "$MERGE_ORDER_SELF" helper_mode_appends_a_registration --exact --test-threads=1 >> "$dir/helper.log" 2>&1
    rm -f "$dir/hook.$3"
  fi
  cat "$dir/view.$3"; exit 0
fi
ep="$3"; path=${ep#repos/}; rest=${path#*/}; name=${rest%%/*}; dir="$FAKE_GH_DIR/$name"
case "$ep" in
  */check-runs*) kind=check-runs; oid=${ep#*/commits/}; oid=${oid%%/*} ;;
  */actions/runs*) kind=runs; oid=${ep#*head_sha=}; oid=${oid%%&*} ;;
  *) echo "unexpected call: $*" >&2; exit 2 ;;
esac
echo "api $kind $oid $ep" >> "$dir/calls"
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
            std::env::set_var("MERGE_ORDER_SELF", std::env::current_exe().unwrap());
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
        .env_remove(HELPER)
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

fn short(s: &str) -> &str {
    &s[..7]
}

/// One repository's world: an origin with a branch per pull request, the project's clone,
/// `state.db` and a door.
struct Case {
    _tmp: tempfile::TempDir,
    repo: String,
    origin: PathBuf,
    gh: PathBuf,
    db: PathBuf,
    door: Door,
    person: String,
    base: String,
}

impl Case {
    fn new(name: &str) -> Case {
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
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        let mut c = Case {
            _tmp: tmp,
            repo: format!("o/{name}"),
            origin,
            gh,
            db,
            door,
            person,
            base: String::new(),
        };
        c.base = c.commit(
            "base",
            &[
                ("calc.py", Some(BUGGY)),
                ("pyproject.toml", Some("[project]\nname = 'x'\n")),
                (
                    "tests/other.py",
                    Some("def test_other():\n    assert True\n"),
                ),
            ],
        );
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

    /// A new branch for PR `pr` from the base, checked out.
    fn branch(&self, pr: i64) {
        self.git(&["checkout", "-q", "-b", &format!("pr{pr}"), &self.base]);
    }

    /// A test commit declaring `declared`, which writes those files and `extra`.
    fn test_commit(&self, declared: &[&str], extra: &[(&str, &str)]) -> String {
        let trailers: Vec<String> = declared
            .iter()
            .map(|d| format!("Acceptance-Test: {d}"))
            .collect();
        let mut files: Vec<(&str, Option<&str>)> =
            declared.iter().map(|d| (*d, Some("// a test\n"))).collect();
        files.extend(extra.iter().map(|(p, t)| (*p, Some(*t))));
        self.commit(
            &format!("test: failing test\n\n{}", trailers.join("\n")),
            &files,
        )
    }

    fn fix(&self) -> String {
        self.commit("fix", &[("calc.py", Some(FIXED))])
    }

    /// Publish HEAD as PR `pr`'s head and let the stand-in answer `gh pr view` for it.
    fn publish(&self, pr: i64) -> String {
        let head = self.head();
        self.git(&["update-ref", &format!("refs/pull/{pr}/head"), &head]);
        self.view(pr, &head);
        head
    }

    fn view(&self, pr: i64, head: &str) {
        let view = json!({"number": pr, "url": format!("file://{}/pull/{pr}", self.origin.display()),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(self.gh.join(format!("view.{pr}")), view.to_string()).unwrap();
    }

    fn answer(&self, sha: &str, kind: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.gh.join(format!("{sha}.{kind}")), text).unwrap();
    }

    fn event(&self, kind: &str, task: Option<&str>, payload: Value) {
        let w = StateWriter::open(&self.db).unwrap();
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: task,
            ..Default::default()
        })
        .unwrap();
    }

    /// CI as the secretary's procedure leaves it: every own commit but the head failed (job
    /// `test`), the head passed; all on base C.
    fn ci_all(&self, pr: i64) -> String {
        let head = self.publish(pr);
        let range = format!("{}..{head}", self.base);
        for sha in self.git(&["rev-list", "--reverse", &range]).lines() {
            let ok = sha == head;
            self.answer(
                sha,
                "check-runs",
                &[json!({"app": ACTIONS, "name": "test",
                         "conclusion": if ok { "success" } else { "failure" },
                         "completed_at": T1, "suite": 1})],
            );
            self.answer(
                sha,
                "runs",
                &[json!({"wf": 1, "suite": 1, "created": T1, "bases": [C]})],
            );
            if !ok {
                self.ci(pr, "failed", sha);
            }
        }
        self.ci(pr, "passed", &head);
        head
    }

    fn ci(&self, pr: i64, status: &str, head: &str) {
        self.event(
            "ci_completed",
            None,
            json!({"repo": self.repo, "pr": pr, "head_oid": head, "status": status,
                   "fail_count": 0, "pending_count": 0, "total_checks": 1, "probe_attempts": 1}),
        );
    }

    /// The run of task `task` in `review`, its pull request `pr` recorded at `head`.
    fn run(&self, task: &str, pr: i64, head: &str) {
        let slug = self.repo.trim_start_matches("o/").to_string();
        let w = StateWriter::open(&self.db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: &slug,
            title: Some("T"),
            status: Some("review"),
            ..Default::default()
        })
        .unwrap();
        drop(w);
        self.registered(task, pr, head);
    }

    /// A `pr_opened` of `task`'s run at `head`.
    fn registered(&self, task: &str, pr: i64, head: &str) {
        self.event(
            "pr_opened",
            Some(task),
            json!({"repo": self.repo, "pr": pr, "head_oid": head}),
        );
    }

    /// The code task `task` tied by a watch to PR `pr` at `head`, its last diff review passed at
    /// `head` with no blocker.
    fn reviewed(&self, task: &str, pr: i64, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, ?2, 'code', 'fake', 'b', '/nonexistent', 'verified')",
                rusqlite::params![run, format!("{task}-s1")],
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO pr_watches (repo, pr, head_oid, run_id) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![self.repo, pr, head, run],
            )
            .unwrap();
        let answer = json!({"verdict": "passed", "kind": "diff", "reader": "codex", "round": 1,
                            "allowed": 3, "head": head,
                            "findings": {"blocker": 0, "major": 0, "minor": 0}});
        w.conn()
            .execute(
                "INSERT INTO commands (actor, verb, status, result_json)
                 VALUES (?1, 'review', 'done', ?2)",
                rusqlite::params![
                    makiba::spec::worker_agent_id(task),
                    json!({"result": answer}).to_string()
                ],
            )
            .unwrap();
    }

    /// PR 314, the case of Issue 312: its test commit adds the scan and the helper
    /// `tests/common/makibad.rs` (declared), then the fix; CI and review passed. Its head.
    fn pr314(&self) -> String {
        self.branch(314);
        self.test_commit(
            &["tests/knowledge_isolation.rs", "tests/common/makibad.rs"],
            &[],
        );
        self.fix();
        let head = self.ci_all(314);
        self.run("t314", 314, &head);
        self.reviewed("t314", 314, &head);
        head
    }

    /// An open pull request `pr` whose branch has a test commit declaring `tests/pr<pr>.rs`
    /// (with `extra` files) when `declares`, else a commit with no trailer; published and
    /// recorded for its task `t<pr>`. Its test commit (or its head).
    fn open_pr(&self, pr: i64, declares: bool, extra: &[(&str, &str)]) -> String {
        self.branch(pr);
        let declared = format!("tests/pr{pr}.rs");
        let m = if declares {
            self.test_commit(&[&declared], extra)
        } else {
            self.commit("work", &[(&declared, Some("// no trailer\n"))])
        };
        let head = self.publish(pr);
        self.run(&format!("t{pr}"), pr, &head);
        m
    }

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

    fn merge_at(&self, pr: i64, head: &str) -> Value {
        self.door
            .handle(
                Some(&self.person),
                "merge",
                json!({"repo": self.repo, "pr": pr, "head": head, "method": "squash"}),
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("merge: {e:?}"))
    }

    fn count(&self, kind: &str) -> i64 {
        Connection::open(&self.db)
            .unwrap()
            .query_row("SELECT count(*) FROM events WHERE kind = ?1", [kind], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// The payload of the newest `acceptance_gate`.
    fn last_gate(&self) -> Value {
        let t: String = Connection::open(&self.db)
            .unwrap()
            .query_row(
                "SELECT payload_json FROM events WHERE kind = 'acceptance_gate'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&t).unwrap()
    }

    fn landed(&self, pr: i64) {
        self.event(
            "pr_merged",
            Some(&format!("t{pr}")),
            json!({"repo": self.repo, "pr": pr, "merge_commit": C}),
        );
    }
}

fn reason(v: &Value) -> &str {
    v["reason"].as_str().unwrap_or_default()
}

fn assert_allowed(v: &Value) {
    assert_eq!(v["verdict"], json!("passed"), "{v}");
    assert_eq!(v["allow"], json!(true), "{v}");
}

fn assert_held(v: &Value, names: &[&str]) {
    assert_eq!(v["verdict"], json!("merge_order"), "{v}");
    assert_eq!(v["allow"], json!(false), "{v}");
    for n in names {
        assert!(reason(v).contains(n), "{n:?} not in {v}");
    }
}

// ---- 1 and 2: the case of PR 314 ---------------------------------------------------------------

#[test]
fn the_314_case_is_refused_at_the_merge_while_315_and_316_are_open_and_allowed_once_they_landed() {
    if !in_mode(
        "acceptance",
        "the_314_case_is_refused_at_the_merge_while_315_and_316_are_open_and_allowed_once_they_landed",
    ) {
        return;
    }
    let c = Case::new("m314");
    let m315 = c.open_pr(315, true, &[]);
    let m316 = c.open_pr(316, true, &[]);
    let head = c.pr314();

    let v = c.merge_at(314, &head);
    assert_eq!(v["status"], json!("refused"), "{v}");
    assert_held(
        &v,
        &[
            "#315",
            "#316",
            "tests/common/makibad.rs",
            short(&m315),
            short(&m316),
        ],
    );
    assert_eq!(c.count("merge_attempted"), 0, "nothing is sent");
    let row = c.last_gate();
    let mut waits: Vec<(i64, String)> = row["merge_order"]["waits_for"]
        .as_array()
        .unwrap_or_else(|| panic!("no merge_order.waits_for in {row}"))
        .iter()
        .map(|w| {
            (
                w["pr"].as_i64().unwrap(),
                w["test_commit"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    waits.sort();
    assert_eq!(waits, vec![(315, m315.clone()), (316, m316.clone())]);

    c.landed(315);
    let v = c.gate_at(314, &head);
    assert_held(&v, &["#316"]);
    assert!(!reason(&v).contains("#315"), "315 landed: {v}");

    c.landed(316);
    let v = c.gate_at(314, &head);
    assert_allowed(&v);
    assert!(
        c.last_gate().get("merge_order").is_none(),
        "{}",
        c.last_gate()
    );
}

// ---- 3: an open pull request without a test commit holds nothing -------------------------------

#[test]
fn an_open_pull_request_without_a_test_commit_does_not_hold_the_check() {
    if !in_mode(
        "acceptance",
        "an_open_pull_request_without_a_test_commit_does_not_hold_the_check",
    ) {
        return;
    }
    let c = Case::new("m3");
    c.open_pr(315, false, &[]);
    let head = c.pr314();
    assert_allowed(&c.gate_at(314, &head));
}

// ---- 4: a pull request that adds no check is not held ------------------------------------------

#[test]
fn a_pull_request_that_touches_no_check_path_is_not_held() {
    if !in_mode(
        "acceptance",
        "a_pull_request_that_touches_no_check_path_is_not_held",
    ) {
        return;
    }
    let c = Case::new("m4");
    c.open_pr(315, true, &[]);
    c.open_pr(316, true, &[]);
    c.branch(320);
    c.commit(
        "test: failing test\n\nAcceptance-Test: tests/test_calc.py",
        &[("tests/test_calc.py", Some(TEST))],
    );
    c.fix();
    let head = c.ci_all(320);
    c.run("t320", 320, &head);
    c.reviewed("t320", 320, &head);
    assert_allowed(&c.gate_at(320, &head));
}

// ---- 5: under human the person decides ---------------------------------------------------------

#[test]
fn under_human_the_hold_is_a_warning_for_the_person() {
    if !in_mode("human", "under_human_the_hold_is_a_warning_for_the_person") {
        return;
    }
    let c = Case::new("m5");
    c.open_pr(315, true, &[]);
    let head = c.pr314();
    let v = c.gate_at(314, &head);
    assert_eq!(v["allow"], json!(true), "{v}");
    assert_eq!(v["warn"], json!(true), "{v}");
    assert!(reason(&v).contains("WARNING"), "{v}");
    assert!(reason(&v).contains("#315"), "{v}");
}

// ---- 6: a pull request that cannot be read holds ------------------------------------------------

#[test]
fn an_open_pull_request_that_cannot_be_read_holds_the_check() {
    if !in_mode(
        "acceptance",
        "an_open_pull_request_that_cannot_be_read_holds_the_check",
    ) {
        return;
    }
    let c = Case::new("m6");
    c.branch(315);
    let m = c.test_commit(&["tests/pr315.rs"], &[]);
    // Recorded, but its head is not published at the origin: the gate cannot fetch it.
    c.run("t315", 315, &m);
    let head = c.pr314();
    let v = c.gate_at(314, &head);
    assert_held(&v, &["#315", "not read"]);
}

// ---- 7: read closed, then reopened -------------------------------------------------------------

#[test]
fn a_pull_request_read_closed_holds_nothing_and_holds_again_when_reopened() {
    if !in_mode(
        "acceptance",
        "a_pull_request_read_closed_holds_nothing_and_holds_again_when_reopened",
    ) {
        return;
    }
    let c = Case::new("m7");
    c.open_pr(315, true, &[]);
    let head = c.pr314();
    c.event(
        "pr_closed",
        Some("t315"),
        json!({"repo": c.repo, "pr": 315}),
    );
    assert_allowed(&c.gate_at(314, &head));
    c.event(
        "pr_reopened",
        Some("t315"),
        json!({"repo": c.repo, "pr": 315}),
    );
    assert_held(&c.gate_at(314, &head), &["#315"]);
}

// ---- 8: two check-adding pull requests hold each other ------------------------------------------

#[test]
fn two_check_adding_pull_requests_hold_each_other_and_say_so() {
    if !in_mode(
        "acceptance",
        "two_check_adding_pull_requests_hold_each_other_and_say_so",
    ) {
        return;
    }
    let c = Case::new("m8");
    c.branch(317);
    c.test_commit(
        &["tests/pr317.rs"],
        &[("tests/common/other.rs", "// a helper\n")],
    );
    c.fix();
    let h317 = c.ci_all(317);
    c.run("t317", 317, &h317);
    c.reviewed("t317", 317, &h317);
    let h314 = c.pr314();

    let v = c.gate_at(314, &h314);
    assert_held(&v, &["#317", "hold each other"]);
    let v = c.gate_at(317, &h317);
    assert_held(&v, &["#314", "hold each other"]);
}

// ---- 9: a pull request recorded while the gate ran ---------------------------------------------

#[test]
fn a_pull_request_recorded_while_the_gate_ran_makes_it_ask_again() {
    if !in_mode(
        "acceptance",
        "a_pull_request_recorded_while_the_gate_ran_makes_it_ask_again",
    ) {
        return;
    }
    let c = Case::new("m9");
    c.open_pr(315, false, &[]);
    let new = c.test_commit(&["tests/pr315.rs"], &[]);
    let head = c.pr314();
    let hook = json!({"db": c.db, "task": "t315", "repo": c.repo, "pr": 315, "head": new});
    std::fs::write(c.gh.join("hook.314"), hook.to_string()).unwrap();
    let v = c.gate_at(314, &head);
    assert!(
        !c.gh.join("hook.314").exists(),
        "the stand-in did not run the helper: {}",
        std::fs::read_to_string(c.gh.join("helper.log")).unwrap_or_default()
    );
    assert_eq!(v["verdict"], json!("rejected"), "{v}");
    assert_eq!(v["allow"], json!(false), "{v}");
    assert!(reason(&v).contains("changed while the gate ran"), "{v}");
}

/// The helper mode the stand-in runs (assertion 9): with `MERGE_ORDER_HELPER` naming a file, it
/// appends the `pr_opened` that file describes; without it, it does nothing.
#[test]
fn helper_mode_appends_a_registration() {
    let Ok(path) = std::env::var(HELPER) else {
        return;
    };
    let h: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let w = StateWriter::open(Path::new(h["db"].as_str().unwrap())).unwrap();
    w.append_event(&NewEvent {
        kind: "pr_opened",
        actor: Some("makibad"),
        payload: Some(&json!({"repo": h["repo"], "pr": h["pr"], "head_oid": h["head"]})),
        run_task_id: h["task"].as_str(),
        ..Default::default()
    })
    .unwrap();
}
