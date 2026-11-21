//! Issue 47 (M6-6): the acceptance gate and the secretary's merge gate hook.
//!
//! - The cases of the carried gate tests pass, ported (claude-org-ja `tools/test_acceptance_gate.py`,
//!   `test_acceptance_ci_gate.py`, `test_acceptance_repo_check.py`, `test_acceptance_update_branch.py`,
//!   `test_merge_approval_mode.py`, `test_acceptance_merge_hook.py`,
//!   `test_acceptance_merge_hook_mode.py`, `test_acceptance_merge_hook_false_positive.py`).
//! - A merge command for a pull request with no declared test is refused in `acceptance` mode.
//! - A merge is refused when the latest `ci_completed` is for another head.
//! - Each other way to merge from a command line is refused in `acceptance` mode, and a command
//!   that only mentions one as an argument is not.
//!
//! The gate runs in the door (`Door::handle` with the merge gate's key) on real git repositories:
//! a local origin publishes `refs/pull/<n>/head`, the project's clone sits in the repositories
//! folder, and `gh` is a stand-in script (`MAKIBA_GH`) that answers from files named by the
//! repository. No `makibad` loop runs while a test writes `state.db`. The merge approval mode is
//! makibad's environment, so the cases of each mode run in a child process of this test binary.
//! The tests that start the door's Unix socket need a host that lets them create one.

use makiba::base::stub::Stub;
use makiba::channel::Hub;
use makiba::daemon::door::{self, Door, issue_key};
use makiba::daemon::gate;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::hooks::merge_gate;
use makiba::spec::Role;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once, OnceLock};

const GATE_KEY: &str = "9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e9a7e";
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
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        door.set_gate_key(GATE_KEY);
        let mut c = Case {
            _tmp: tmp,
            name: name.into(),
            repo: format!("o/{name}"),
            origin,
            gh,
            db,
            door,
            secretary,
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

    /// The door's answer to `gate` for PR `pr` at `head`, asked with the merge gate's key.
    fn gate_at(&self, pr: i64, head: &str) -> Value {
        self.door
            .handle(
                Some(GATE_KEY),
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

// ---- tools/test_acceptance_gate.py: GateTest -----------------------------------------------------

#[test]
fn test_first_passes() {
    let c = Case::new("first");
    c.land();
    let v = c.assert_verdict("passed", "failed at the test commit");
    assert_eq!(v["allow"], json!(true));
}

#[test]
fn change_then_revert_passes() {
    let c = Case::new("revert");
    c.land();
    c.commit(
        "weaken",
        &[("tests/test_calc.py", Some("def test_add():\n    pass\n"))],
    );
    c.commit("revert", &[("tests/test_calc.py", Some(TEST))]);
    c.assert_verdict("passed", "");
}

#[test]
fn no_trailer_is_no_manifest() {
    let c = Case::new("notrailer");
    c.commit("fix", &[("calc.py", Some(FIXED))]);
    c.assert_verdict("no_manifest", "trailer");
}

#[test]
fn weakened_test_rejected() {
    let c = Case::new("weakened");
    c.land();
    c.commit(
        "weaken",
        &[(
            "tests/test_calc.py",
            Some("def test_add():\n    assert True\n"),
        )],
    );
    let v = c.assert_verdict("rejected", "tests/test_calc.py");
    assert_eq!(v["allow"], json!(false));
}

#[test]
fn renamed_test_rejected() {
    let c = Case::new("renamed");
    c.land();
    c.git(&["mv", "tests/test_calc.py", "tests/test_calc2.py"]);
    c.commit("rename", &[]);
    c.assert_verdict("rejected", "protected paths changed");
}

#[test]
fn deleted_test_rejected() {
    let c = Case::new("deleted");
    c.land();
    c.commit("delete", &[("tests/test_calc.py", None)]);
    c.assert_verdict("rejected", "protected paths changed");
}

#[test]
fn other_file_renamed_onto_test_rejected() {
    let c = Case::new("ontotest");
    c.land();
    c.commit("drop", &[("tests/test_calc.py", None)]);
    c.git(&["mv", "tests/other.py", "tests/test_calc.py"]);
    c.commit("replace", &[]);
    c.assert_verdict("rejected", "protected paths changed");
}

#[test]
fn second_manifest_rejected() {
    let c = Case::new("second");
    c.land();
    c.commit("new manifest\n\nAcceptance-Test: tests/other.py", &[]);
    c.assert_verdict("rejected", "the manifest was rewritten");
}

#[test]
fn infrastructure_swap_after_test_rejected() {
    let c = Case::new("infra");
    c.land();
    for (path, content) in [
        ("tests/conftest.py", "import pytest\n"),
        ("pytest.ini", "[pytest]\naddopts = -k nothing\n"),
        (
            "pyproject.toml",
            "[tool.pytest.ini_options]\naddopts = '-k nothing'\n",
        ),
        (".github/workflows/ci.yml", "on: workflow_dispatch\n"),
        ("sub/setup.cfg", "[tool:pytest]\n"),
        ("web/vitest.config.ts", "export default {}\n"),
        ("web/package.json", "{}\n"),
    ] {
        c.commit("swap", &[(path, Some(content))]);
        c.assert_verdict("rejected", path);
        c.git(&["reset", "-q", "--hard", "HEAD~1"]);
    }
}

#[test]
fn file_added_to_manifest_directory_rejected() {
    let c = Case::new("mandir");
    c.land_with(TEST, "\n\nAcceptance-Test: tests/", &[]);
    c.commit(
        "add",
        &[("tests/test_extra.py", Some("def test_x():\n    pass\n"))],
    );
    c.assert_verdict("rejected", "tests/test_extra.py");
}

#[test]
fn undeclared_change_in_test_commit_rejected() {
    let c = Case::new("undeclared");
    c.land_with(TEST, TRAILER, &[("calc.py", Some(FIXED))]);
    c.assert_verdict("rejected", "undeclared paths [\"calc.py\"]");
}

#[test]
fn missing_and_escaping_paths_rejected() {
    let c = Case::new("badpaths");
    for (trailer, needle) in [
        ("tests/nope.py", "does not exist"),
        ("../x.py", "invalid manifest path"),
    ] {
        c.commit(&format!("t\n\nAcceptance-Test: {trailer}"), &[]);
        c.assert_verdict("rejected", needle);
        c.git(&["reset", "-q", "--hard", &c.base]);
    }
}

#[test]
fn symlinked_test_rejected() {
    let c = Case::new("symlink");
    let blob = Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(&c.origin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut ch| {
            use std::io::Write;
            ch.stdin.take().unwrap().write_all(b"other.py")?;
            ch.wait_with_output()
        })
        .unwrap();
    let blob = String::from_utf8_lossy(&blob.stdout).trim().to_string();
    c.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("120000,{blob},tests/test_calc.py"),
    ]);
    git_in(
        &c.origin,
        &["commit", "-q", "-m", &format!("link{TRAILER}")],
    );
    c.assert_verdict("rejected", "symlink");
}

#[test]
fn event_recorded() {
    let c = Case::new("event");
    c.land();
    let head = c.publish(12);
    c.door
        .handle(
            Some(&c.secretary),
            "pr",
            json!({"state": "opened", "repo": c.repo, "pr": 12, "head": head, "task": "t1"}),
            None,
            false,
        )
        .unwrap();
    c.ci_all(12);
    assert_eq!(c.gate_at(12, &head)["verdict"], json!("passed"));
    let e = c.gate_events();
    let p = e.last().unwrap();
    assert_eq!(
        (&p["result"], &p["pr"], &p["task"], &p["repo"]),
        (&json!("passed"), &json!(12), &json!("t1"), &json!(c.repo))
    );
    assert_eq!(p["paths"], json!(["tests/test_calc.py"]));
}

#[test]
fn rejected_event_keeps_manifest() {
    let c = Case::new("rejevent");
    c.land();
    c.commit(
        "weaken",
        &[("tests/test_calc.py", Some("def test_add():\n    pass\n"))],
    );
    c.assert_verdict("rejected", "");
    let p = c.gate_events().pop().unwrap();
    assert_eq!(
        (&p["result"], &p["paths"]),
        (&json!("rejected"), &json!(["tests/test_calc.py"]))
    );
}

// --- pin (force push) ---

#[test]
fn rewritten_test_commit_rejected_after_first_sight() {
    let c = Case::new("rewrite");
    c.land_with(
        &TEST.replace("add(2, 2) == 4", "add(2, 2) == 5"),
        TRAILER,
        &[],
    );
    c.verdict();
    c.rewrite_test(TEST);
    c.assert_verdict("rejected", "rewritten since the gate first saw it");
    // Another PR has a pin of its own.
    let head = c.ci_all(2);
    assert_eq!(c.gate_at(2, &head)["verdict"], json!("passed"));
}

#[test]
fn whitespace_only_rewrite_is_still_a_rewrite() {
    let c = Case::new("whitespace");
    c.land_with(
        &TEST.replace("add(2, 2) == 4", "add(2, 2) == 5"),
        TRAILER,
        &[],
    );
    c.verdict();
    c.rewrite_test(&TEST.replace("add(2, 2) == 4", "add(2,2) == 5"));
    c.assert_verdict("rejected", "rewritten");
}

#[test]
fn rebase_keeps_the_pin() {
    let mut c = Case::new("rebase");
    c.land();
    c.assert_verdict("passed", "");
    c.git(&["checkout", "-q", "main"]);
    c.base = c.commit("base moves", &[("README", Some("x\n"))]);
    c.git(&["checkout", "-q", "feature"]);
    c.git(&["rebase", "-q", "main"]);
    c.assert_verdict("passed", "");
}

#[test]
fn removed_manifest_rejected_after_first_sight() {
    let c = Case::new("removed");
    c.land();
    c.verdict();
    c.git(&["reset", "-q", "--hard", &c.base]);
    c.commit("fix", &[("calc.py", Some(FIXED))]);
    c.assert_verdict("rejected", "removed");
}

// ---- GithubReadTest: check runs and run bases read through gh --------------------------------

fn ci_reader() -> gate::Ci {
    gate::Ci::new(&std::env::var("MAKIBA_GH").unwrap())
}

#[test]
fn latest_run_per_app_and_name_of_the_selected_runs() {
    let c = Case::new("ghlatest");
    let a = "a".repeat(40);
    c.answer(
        &a,
        "runs",
        &[json!({"wf": 1, "suite": 10, "created": T1, "bases": [C]})],
    );
    c.answer(
        &a,
        "check-runs",
        &[
            json!({"app": 1, "name": "test", "conclusion": "failure", "completed_at": T1, "suite": 10}),
            json!({"app": 1, "name": "test", "conclusion": "success", "completed_at": T2, "suite": 10}),
            json!({"app": 2, "name": "test", "conclusion": "failure", "completed_at": T2, "suite": 10}),
            json!({"app": 1, "name": "lint", "conclusion": null, "completed_at": null, "suite": 10}),
            json!({"app": 3, "name": "push-ci", "conclusion": "failure", "completed_at": T2, "suite": 99}),
        ],
    );
    let got = ci_reader().check_runs(&c.repo, &a).unwrap();
    let want = [
        ((1, "test".to_string(), 1), "success".to_string()),
        ((2, "test".to_string(), 1), "failure".to_string()),
    ];
    assert_eq!(got, want.into_iter().collect());
    // Every run, not GitHub's latest per name (a push run would hide the PR's).
    assert!(c.calls().contains("filter=all"), "{}", c.calls());
}

#[test]
fn jobs_of_a_run_without_a_base_are_not_compared() {
    let c = Case::new("ghnobase");
    let a = "a".repeat(40);
    c.answer(
        &a,
        "runs",
        &[
            json!({"wf": 1, "suite": 10, "created": T1, "bases": []}),
            json!({"wf": 2, "suite": 20, "created": T1, "bases": [C]}),
        ],
    );
    c.answer(
        &a,
        "check-runs",
        &[
            json!({"app": 1, "name": "test", "conclusion": "failure", "completed_at": T1, "suite": 10}),
            json!({"app": 1, "name": "lint", "conclusion": "success", "completed_at": T1, "suite": 20}),
        ],
    );
    let mut ci = ci_reader();
    assert_eq!(
        ci.check_runs(&c.repo, &a).unwrap(),
        [((1, "lint".to_string(), 2), "success".to_string())]
            .into_iter()
            .collect()
    );
    assert_eq!(
        ci.run_bases(&c.repo, &a).unwrap(),
        [C.to_string()].into_iter().collect()
    );
}

#[test]
fn same_job_name_in_two_workflows_stays_apart() {
    let c = Case::new("ghtwowf");
    let a = "a".repeat(40);
    c.answer(
        &a,
        "runs",
        &[
            json!({"wf": 1, "suite": 10, "created": T1, "bases": [C]}),
            json!({"wf": 2, "suite": 20, "created": T1, "bases": [C]}),
        ],
    );
    c.answer(
        &a,
        "check-runs",
        &[
            json!({"app": 1, "name": "test", "conclusion": "failure", "completed_at": T1, "suite": 10}),
            json!({"app": 1, "name": "test", "conclusion": "success", "completed_at": T2, "suite": 20}),
        ],
    );
    assert_eq!(
        ci_reader().check_runs(&c.repo, &a).unwrap(),
        [
            ((1, "test".to_string(), 1), "failure".to_string()),
            ((1, "test".to_string(), 2), "success".to_string())
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn pushing_the_same_commit_again_supersedes_the_old_run() {
    let c = Case::new("ghretry");
    let a = "a".repeat(40);
    c.answer(
        &a,
        "runs",
        &[
            json!({"wf": 1, "suite": 10, "created": T1, "bases": [C]}),
            json!({"wf": 1, "suite": 11, "created": T2, "bases": [D]}),
        ],
    );
    assert_eq!(
        ci_reader().run_bases(&c.repo, &a).unwrap(),
        [D.to_string()].into_iter().collect()
    );
}

#[test]
fn gh_failure_rejects() {
    let c = Case::new("ghfail");
    std::fs::write(c.gh.join("fail"), "").unwrap();
    let e = ci_reader()
        .check_runs(&c.repo, &"a".repeat(40))
        .unwrap_err();
    assert!(e.0.contains("cannot read"), "{e:?}");
}

// ---- tools/test_acceptance_ci_gate.py: the red run as evidence ----------------------------------

const JS_BUGGY: &str = "export const add = (a, b) => a - b;\n";
const JS_FIXED: &str = "export const add = (a, b) => a + b;\n";
const JS_TEST: &str =
    "import { add } from '../calc.js';\ntest('add', () => expect(add(2, 2)).toBe(4));\n";
const JS_TRAILER: &str = "\n\nAcceptance-Test: tests/calc.test.js";

fn js_case(name: &str) -> Case {
    Case::with_base(
        name,
        &[("calc.js", Some(JS_BUGGY)), ("package.json", Some("{}\n"))],
    )
}

/// The JS test commit and the fix: `(m, head)`, head published as PR 7 and passed.
fn js_land(c: &Case) -> (String, String) {
    let m = c.commit(
        &format!("test: failing test{JS_TRAILER}"),
        &[("tests/calc.test.js", Some(JS_TEST))],
    );
    c.commit("fix", &[("calc.js", Some(JS_FIXED))]);
    let head = c.publish(7);
    (m, head)
}

/// Ask the gate after recording head as passed (the latest row).
fn js_gate(c: &Case, head: &str) -> Value {
    c.ci(7, "passed", head);
    c.gate_at(7, head)
}

fn expect(v: &Value, verdict: &str, needle: &str) {
    assert_eq!(v["verdict"], json!(verdict), "{v}");
    assert!(
        v["reason"].as_str().unwrap().contains(needle),
        "{needle:?} not in {v}"
    );
}

#[test]
fn any_language_passes_on_ci_evidence() {
    let c = js_case("cianylang");
    let (m, head) = js_land(&c);
    c.checks(&m, &[(ACTIONS, "test", "failure", 1)]);
    c.bases(&m, &[C]);
    c.ci(7, "failed", &m);
    c.checks(&head, &[(ACTIONS, "test", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "passed", "test");
}

#[test]
fn no_failed_ci_record_at_the_test_commit_rejected() {
    let c = js_case("cinofail");
    let (m, head) = js_land(&c);
    c.checks(&m, &[(ACTIONS, "test", "failure", 1)]);
    c.bases(&m, &[C]);
    c.checks(&head, &[(ACTIONS, "test", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "rejected", "no failed CI run recorded");
}

/// The test commit failed and was recorded; the job's conclusions at m and head as given.
fn js_flip(
    name: &str,
    at_m: &[(i64, &str, &str, i64)],
    at_head: &[(i64, &str, &str, i64)],
    bm: &[&str],
    bh: &[&str],
) -> Value {
    let c = js_case(name);
    let (m, head) = js_land(&c);
    c.checks(&m, at_m);
    c.bases(&m, bm);
    c.ci(7, "failed", &m);
    c.checks(&head, at_head);
    c.bases(&head, bh);
    js_gate(&c, &head)
}

#[test]
fn cancelled_job_is_not_a_failure() {
    let v = js_flip(
        "cicancel",
        &[(ACTIONS, "test", "cancelled", 1)],
        &[(ACTIONS, "test", "success", 1)],
        &[C],
        &[C],
    );
    expect(&v, "rejected", "no CI job failed at the test commit");
}

#[test]
fn job_still_failing_at_head_rejected() {
    let v = js_flip(
        "cistill",
        &[(ACTIONS, "test", "failure", 1)],
        &[(ACTIONS, "test", "failure", 1)],
        &[C],
        &[C],
    );
    expect(&v, "rejected", "no CI job failed at the test commit");
}

#[test]
fn same_name_from_another_app_does_not_match() {
    let v = js_flip(
        "ciotherapp",
        &[(ACTIONS, "test", "failure", 1)],
        &[(999, "test", "success", 1)],
        &[C],
        &[C],
    );
    expect(&v, "rejected", "no CI job failed at the test commit");
}

#[test]
fn base_moved_between_the_two_runs_rejected() {
    let v = js_flip(
        "cibasemoved",
        &[(ACTIONS, "test", "failure", 1)],
        &[(ACTIONS, "test", "success", 1)],
        &[C],
        &[D],
    );
    expect(&v, "rejected", "not on one known base");
}

#[test]
fn unknown_base_rejected() {
    let c = js_case("ciunknown");
    let (m, head) = js_land(&c);
    // A run with no base is dropped, so its jobs are not compared either.
    c.answer(
        &m,
        "runs",
        &[json!({"wf": 1, "suite": 1, "created": T1, "bases": []})],
    );
    c.checks(&m, &[(ACTIONS, "test", "failure", 1)]);
    c.ci(7, "failed", &m);
    c.checks(&head, &[(ACTIONS, "test", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "rejected", "");
}

#[test]
fn unreadable_check_runs_rejected() {
    let c = js_case("ciunreadable");
    let (m, head) = js_land(&c);
    c.ci(7, "failed", &m);
    // The stand-in answers the PR view, then fails on every API read.
    std::fs::write(c.gh.join("fail-api"), "").unwrap();
    expect(&js_gate(&c, &head), "rejected", "cannot read");
}

#[test]
fn test_commit_must_be_the_first_commit_of_the_pr() {
    let c = js_case("cifirst");
    c.commit(
        "break",
        &[("calc.js", Some("export const add = () => 0;\n"))],
    );
    let m = c.commit(
        &format!("test{JS_TRAILER}"),
        &[("tests/calc.test.js", Some(JS_TEST))],
    );
    let head = c.commit(
        "restore",
        &[("calc.js", Some(&JS_BUGGY.replace("a - b", "a + b")))],
    );
    c.publish(7);
    c.checks(&m, &[(ACTIONS, "test", "failure", 1)]);
    c.bases(&m, &[C]);
    c.ci(7, "failed", &m);
    c.checks(&head, &[(ACTIONS, "test", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "rejected", "first commit");
}

#[test]
fn repo_check_not_trusted_when_the_pr_rewrites_its_workflow() {
    let mut c = js_case("cirewritewf");
    c.git(&["checkout", "-q", "main"]);
    c.base = c.commit(
        "config",
        &[
            (".github/acceptance-check", Some("acceptance\n")),
            (
                ".github/workflows/ci.yml",
                Some("jobs: {acceptance: real}\n"),
            ),
        ],
    );
    c.git(&["checkout", "-q", "-B", "feature", "main"]);
    c.commit(
        &format!("test{JS_TRAILER}"),
        &[
            ("tests/calc.test.js", Some(JS_TEST)),
            (
                ".github/workflows/ci.yml",
                Some("jobs: {acceptance: true}\n"),
            ),
        ],
    );
    let head = c.commit("fix", &[("calc.js", Some(JS_FIXED))]);
    c.publish(7);
    c.checks(&head, &[(ACTIONS, "acceptance", "success", 1)]);
    c.bases(&head, &[C]);
    expect(
        &js_gate(&c, &head),
        "rejected",
        "no failed CI run recorded at the test commit",
    );
}

#[test]
fn repo_check_not_trusted_from_an_older_branch_point() {
    let mut c = js_case("ciolderpoint");
    c.git(&["checkout", "-q", "main"]);
    let fork = c.commit(
        "old job",
        &[(
            ".github/workflows/ci.yml",
            Some("jobs: {acceptance: weak}\n"),
        )],
    );
    c.base = c.commit(
        "config",
        &[
            (".github/acceptance-check", Some("acceptance\n")),
            (
                ".github/workflows/ci.yml",
                Some("jobs: {acceptance: real}\n"),
            ),
        ],
    );
    c.git(&["checkout", "-q", "-B", "feature", &fork]);
    let (_, head) = js_land(&c);
    c.checks(&head, &[(ACTIONS, "acceptance", "success", 1)]);
    c.bases(&head, &[C]);
    expect(
        &js_gate(&c, &head),
        "rejected",
        "no failed CI run recorded at the test commit",
    );
}

#[test]
fn repo_check_name_may_contain_spaces() {
    let mut c = js_case("cispaces");
    c.git(&["checkout", "-q", "main"]);
    c.base = c.commit(
        "config",
        &[(".github/acceptance-check", Some("Acceptance tests\n"))],
    );
    c.git(&["checkout", "-q", "-B", "feature", "main"]);
    let (_, head) = js_land(&c);
    c.checks(&head, &[(ACTIONS, "Acceptance tests", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "passed", "Acceptance tests");
}

// ---- tools/test_acceptance_repo_check.py: both kinds of evidence through the stand-in ----------

/// A JS case whose base names `config` in `.github/acceptance-check` (none: no file).
fn repo_case(name: &str, config: Option<&str>) -> Case {
    let mut c = js_case(name);
    if let Some(text) = config {
        c.git(&["checkout", "-q", "main"]);
        c.base = c.commit("config", &[(".github/acceptance-check", Some(text))]);
        c.git(&["checkout", "-q", "-B", "feature", "main"]);
    }
    c
}

#[test]
fn red_run_at_the_test_commit_passes() {
    let c = repo_case("rcred", None);
    let (m, head) = js_land(&c);
    c.checks(&m, &[(ACTIONS, "test", "failure", 1)]);
    c.bases(&m, &[C]);
    c.ci(7, "failed", &m);
    c.checks(&head, &[(ACTIONS, "test", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "passed", "failed at the test commit");
}

#[test]
fn red_run_still_counts_when_the_repo_names_a_check() {
    let c = repo_case("rcredcheck", Some("acceptance\n"));
    let (m, head) = js_land(&c);
    c.checks(
        &m,
        &[
            (ACTIONS, "test", "failure", 1),
            (ACTIONS, "acceptance", "failure", 1),
        ],
    );
    c.bases(&m, &[C]);
    c.ci(7, "failed", &m);
    c.checks(
        &head,
        &[
            (ACTIONS, "test", "success", 1),
            (ACTIONS, "acceptance", "failure", 1),
        ],
    );
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "passed", "failed at the test commit");
}

#[test]
fn repo_acceptance_check_passing_at_head_passes() {
    let c = repo_case("rcpass", Some("acceptance\n"));
    let (_, head) = js_land(&c);
    c.checks(&head, &[(ACTIONS, "acceptance", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "passed", "acceptance check");
}

#[test]
fn repo_acceptance_check_failing_at_head_without_red_run_rejected() {
    let c = repo_case("rcfail", Some("acceptance\n"));
    let (_, head) = js_land(&c);
    c.checks(&head, &[(ACTIONS, "acceptance", "failure", 1)]);
    c.bases(&head, &[C]);
    expect(
        &js_gate(&c, &head),
        "rejected",
        "no failed CI run recorded at the test commit",
    );
}

#[test]
fn repo_acceptance_check_missing_at_head_rejected() {
    let c = repo_case("rcmissing", Some("acceptance\n"));
    let (_, head) = js_land(&c);
    c.checks(&head, &[(ACTIONS, "test", "success", 1)]);
    c.bases(&head, &[C]);
    expect(
        &js_gate(&c, &head),
        "rejected",
        "no failed CI run recorded at the test commit",
    );
}

#[test]
fn check_named_only_by_the_pr_does_not_count() {
    let c = repo_case("rcnamedbypr", None);
    c.commit(
        &format!("test: failing test{JS_TRAILER}"),
        &[("tests/calc.test.js", Some(JS_TEST))],
    );
    let head = c.commit(
        "fix",
        &[
            ("calc.js", Some(JS_FIXED)),
            (".github/acceptance-check", Some("test\n")),
        ],
    );
    c.publish(7);
    c.checks(&head, &[(ACTIONS, "test", "success", 1)]);
    c.bases(&head, &[C]);
    expect(
        &js_gate(&c, &head),
        "rejected",
        "no failed CI run recorded at the test commit",
    );
}

#[test]
fn other_refusals_still_apply_with_the_repo_check() {
    let c = repo_case("rcother", Some("acceptance\n"));
    js_land(&c);
    let head = c.commit(
        "weaken",
        &[("tests/calc.test.js", Some("test('add', () => {});\n"))],
    );
    c.publish(7);
    c.checks(&head, &[(ACTIONS, "acceptance", "success", 1)]);
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "rejected", "protected paths changed");
}

#[test]
fn malformed_config_rejected() {
    let c = repo_case("rcmalformed", Some("acceptance\ntest\n"));
    let (_, head) = js_land(&c);
    c.checks(
        &head,
        &[
            (ACTIONS, "acceptance", "success", 1),
            (ACTIONS, "test", "success", 1),
        ],
    );
    c.bases(&head, &[C]);
    expect(&js_gate(&c, &head), "rejected", ".github/acceptance-check");
}

// ---- tools/test_acceptance_update_branch.py: the base merged in after the test commit -----------

/// `main` moves on (also touching a protected path, as a real base does).
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

#[test]
fn base_merged_after_the_test_commit_passes() {
    let mut c = Case::new("ubafter");
    c.land();
    move_base(&mut c, &[]);
    merge_base_in(&c, &[]);
    c.assert_verdict("passed", "");
}

#[test]
fn base_merged_twice_after_the_test_commit_passes() {
    let mut c = Case::new("ubtwice");
    c.land();
    for n in ["1\n", "2\n"] {
        move_base(&mut c, &[("README", Some(n))]);
        merge_base_in(&c, &[]);
    }
    c.assert_verdict("passed", "");
}

#[test]
fn base_merged_before_the_test_commit_rejected() {
    let mut c = Case::new("ubbefore");
    move_base(&mut c, &[]);
    merge_base_in(&c, &["--no-ff"]);
    c.land();
    c.assert_verdict("rejected", "must be the first commit");
}

#[test]
fn own_commit_before_the_test_commit_rejected() {
    let mut c = Case::new("ubown");
    c.commit("own work first", &[("other.py", Some("x = 1\n"))]);
    c.land();
    move_base(&mut c, &[]);
    merge_base_in(&c, &[]);
    c.assert_verdict("rejected", "must be the first commit");
}

#[test]
fn other_branch_merged_after_the_test_commit_rejected() {
    let c = Case::new("ubother");
    c.land();
    c.git(&["checkout", "-q", "-b", "side", "main"]);
    c.commit("side work", &[("side.py", Some("x = 1\n"))]);
    c.git(&["checkout", "-q", "feature"]);
    c.git(&[M, "-q", "--no-edit", "side"]);
    c.assert_verdict("rejected", "must be the first commit");
}

#[test]
fn protected_change_by_the_pr_after_the_merge_rejected() {
    let mut c = Case::new("ubprotected");
    c.land();
    move_base(&mut c, &[]);
    merge_base_in(&c, &[]);
    c.commit(
        "weaken",
        &[(
            "pyproject.toml",
            Some("[tool.pytest.ini_options]\naddopts = '-k x'\n"),
        )],
    );
    c.assert_verdict("rejected", "pyproject.toml");
}

#[test]
fn test_changed_after_the_merge_rejected() {
    let mut c = Case::new("ubtest");
    c.land();
    move_base(&mut c, &[]);
    merge_base_in(&c, &[]);
    c.commit(
        "weaken",
        &[("tests/test_calc.py", Some("def test_add():\n    pass\n"))],
    );
    c.assert_verdict("rejected", "tests/test_calc.py");
}

// ---- tools/test_merge_approval_mode.py: the setting's value -------------------------------------

#[test]
fn the_merge_approval_mode_value() {
    for (value, mode, warns) in [
        (None, "human", false),
        (Some("human"), "human", false),
        (Some("acceptance"), "acceptance", false),
        (Some("  ACCEPTANCE "), "acceptance", false),
        (Some("auto"), "human", true),
        (Some(""), "human", true),
        (Some("acceptance human"), "human", true),
    ] {
        let (got, warning) = gate::mode_of(value);
        assert_eq!((got, warning.is_some()), (mode, warns), "{value:?}");
    }
    assert_eq!(
        gate::mode_of(Some("bogus")).1.unwrap(),
        "merge_approval_mode: invalid value \"bogus\", using human"
    );
}

// ---- tools/test_acceptance_merge_hook_mode.py and Issue 47's definition of done ----------------

/// A no-manifest PR changing `files`, published as PR 7 with passed CI at head.
fn no_manifest_pr(name: &str, files: &[(&str, Option<&str>)]) -> (Case, String) {
    let c = Case::new(name);
    c.commit("change", files);
    let head = c.ci_all(7);
    (c, head)
}

#[test]
fn acceptance_mode_cases() {
    if !in_mode("acceptance", "acceptance_mode_cases") {
        return;
    }
    // Issue 47: a PR with no declared test is refused in acceptance mode.
    let (c, head) = no_manifest_pr("amdenied", &[("tools/x.py", Some("x = 1\n"))]);
    let v = c.gate_at(7, &head);
    assert_eq!(
        (&v["verdict"], &v["allow"], &v["mode"]),
        (&json!("no_manifest"), &json!(false), &json!("acceptance")),
        "{v}"
    );
    assert!(
        v["reason"].as_str().unwrap().contains("needs the person"),
        "{v}"
    );
    // Docs-only passes; a mixed diff and an empty one do not.
    let (c, head) = no_manifest_pr(
        "amdocs",
        &[("README.md", Some("x\n")), ("docs/a.txt", Some("y\n"))],
    );
    assert_eq!(c.gate_at(7, &head)["allow"], json!(true));
    let (c, head) = no_manifest_pr(
        "ammixed",
        &[("README.md", Some("x\n")), ("tools/x.py", Some("x = 1\n"))],
    );
    assert_eq!(c.gate_at(7, &head)["allow"], json!(false));
    let (c, head) = no_manifest_pr("amempty", &[]);
    assert_eq!(c.gate_at(7, &head)["allow"], json!(false));
    // Passed is allowed, rejected is not.
    let c = Case::new("ampassed");
    c.land();
    assert_eq!(c.verdict()["allow"], json!(true));
    let c = Case::new("amrejected");
    c.land();
    c.commit(
        "weaken",
        &[("tests/test_calc.py", Some("def test_add():\n    pass\n"))],
    );
    assert_eq!(c.verdict()["allow"], json!(false));
    // Issue 47: each other way to merge is refused in acceptance mode; a mention is not.
    other_ways_are_refused_and_mentions_pass();
}

#[test]
fn human_mode_cases() {
    if !in_mode("human", "human_mode_cases") {
        return;
    }
    let (c, head) = no_manifest_pr("hmallowed", &[("tools/x.py", Some("x = 1\n"))]);
    let v = c.gate_at(7, &head);
    assert_eq!(
        (&v["verdict"], &v["allow"], &v["mode"]),
        (&json!("no_manifest"), &json!(true), &json!("human")),
        "{v}"
    );
    let c = Case::new("hmrejected");
    c.land();
    c.commit(
        "weaken",
        &[("tests/test_calc.py", Some("def test_add():\n    pass\n"))],
    );
    assert_eq!(c.verdict()["allow"], json!(false));
}

// ---- tools/test_acceptance_merge_hook.py: CiHeadTest -------------------------------------------

#[test]
fn no_row_or_another_prs_row_is_not_passed() {
    let c = Case::new("cinorow");
    c.land();
    let head = c.publish(7);
    expect(&c.gate_at(7, &head), "rejected", "no passed ci_completed");
    c.ci(8, "passed", &head);
    expect(&c.gate_at(7, &head), "rejected", "latest: none");
}

#[test]
fn the_latest_row_wins() {
    let c = Case::new("cilatest");
    c.land();
    let head = c.ci_all(7);
    assert_eq!(c.gate_at(7, &head)["verdict"], json!("passed"));
    // Issue 47: the latest ci_completed is for another head (a later push passed).
    c.ci(7, "passed", &"b".repeat(40));
    expect(&c.gate_at(7, &head), "rejected", "no passed ci_completed");
    // The latest failed at this head.
    c.ci(7, "failed", &head);
    expect(&c.gate_at(7, &head), "rejected", "latest: failed");
}

#[test]
fn head_and_ci_are_checked_before_fetching() {
    let c = Case::new("cibefore");
    c.land();
    let head = c.ci_all(7);
    // Pushed after the secretary read the SHA: the PR's head is another.
    let other = "b".repeat(40);
    expect(&c.gate_at(7, &other), "rejected", "PR head is");
    // CI saw a later head.
    c.ci(7, "passed", &other);
    expect(&c.gate_at(7, &head), "rejected", "no passed ci_completed");
    let calls = c.calls();
    let after = calls.rsplit("pr view 7").next().unwrap();
    assert!(
        !after.contains("api"),
        "read check runs after the binding failed: {calls}"
    );
    let clone = init().join("repos").join(&c.name);
    assert!(
        git_in(&clone, &["for-each-ref", "refs/makiba/gate"]).is_empty(),
        "fetched before the binding held"
    );
}

#[test]
fn gh_failure_is_refused() {
    let c = Case::new("ghviewfail");
    c.land();
    let head = c.ci_all(7);
    std::fs::write(c.gh.join("fail"), "").unwrap();
    expect(&c.gate_at(7, &head), "rejected", "gh pr view failed");
}

#[test]
fn only_the_gate_key_holder_and_the_person_ask_the_gate() {
    let c = Case::new("keys");
    // The secretary has no gate verb; the gate's key has no other verb.
    let e = c
        .door
        .handle(
            Some(&c.secretary),
            "gate",
            json!({"repo": c.repo, "pr": 7, "head": C}),
            None,
            false,
        )
        .unwrap_err();
    assert_eq!(e.kind.as_str(), "denied");
    let e = c
        .door
        .handle(Some(GATE_KEY), "status", json!({}), None, false)
        .unwrap_err();
    assert_eq!(e.kind.as_str(), "denied");
    let e = c
        .door
        .handle(
            Some(GATE_KEY),
            "gate",
            json!({"repo": c.repo, "pr": 7, "head": "abc"}),
            None,
            false,
        )
        .unwrap_err();
    assert_eq!(e.kind.as_str(), "usage");
}

// ---- the hook: what it refuses and what it lets through -----------------------------------------

/// `merge_gate::parse` for a command: `Ok(None)` passes, `Ok(Some(_))` asks the gate, `Err` refuses.
fn parse(cmd: &str) -> Result<Option<(i64, String, String)>, String> {
    merge_gate::parse(cmd)
}

fn merge_cmd(n: &str, repo: &str, method: &str, sha: &str, tail: &str) -> String {
    format!("gh pr {M} {n} --repo {repo} {method} --match-head-commit {sha}{tail}")
}

fn sha() -> String {
    "a".repeat(40)
}

#[test]
fn not_a_merge() {
    for cmd in [
        format!("git {M} main"),
        "gh pr view 3".into(),
        "gh pr checks 3".into(),
        format!("echo {M}"),
        "git log --merges".into(),
        format!("grep -n {M} /x/y.md"),
        format!("git {M}-base a b"),
    ] {
        assert_eq!(parse(&cmd), Ok(None), "{cmd}");
    }
}

#[test]
fn the_issue_1229_and_allow_listed_commands_pass() {
    for cmd in [
        "gh pr view 1227 --repo o/r --json state,mergedAt".to_string(),
        format!(
            "gh pr create --repo o/r --title \"fix the {M} gate\" --body-file .worker-scratch/body.md"
        ),
        "gh pr view 1227 --repo o/r --json mergedAt,body".into(),
        "codex exec \"Review src/hooks/merge_gate.rs and roles/secretary/settings.json\"".into(),
        "grep -n DENY $MAKIBA_SHARE_DIR/merge_gate.rs".into(),
        "head -40 /r/src/hooks/merge_gate.rs".into(),
        format!("cat >> notes.md <<'EOF'\n- {M}_approval_mode: acceptance\nEOF"),
        format!("gh issue create --title \"{M} queue\" --body x"),
        format!("gh pr list --search {M}"),
        format!("gh pr comment 3 --body \"ready to {M}\""),
        "gh pr view 3 --json mergeable,autoMergeRequest".into(),
        "gh -R o/r pr view 3 --json mergedAt".into(),
        format!("gh search prs {M} --repo o/r"),
        "gh pr view $PR --repo o/r --json mergedAt".into(),
        "gh pr checks 3 --repo o/r 2>/dev/null".into(),
        "sed -n 10,40p tools/acceptance_merge_hook.py".into(),
        format!("tail -n 5 $LOG/pr-{M}.log"),
    ] {
        assert_eq!(parse(&cmd), Ok(None), "{cmd}");
    }
}

#[test]
fn the_word_inside_a_longer_name_is_not_judged() {
    for cmd in [
        format!("grep -n gh /r/acceptance_{M}_hook.py | head"),
        format!("gh pr view 3 --json {M}dAt; echo $X"),
        format!("cat >> f.md <<'EOF'\n{M}_approval_mode: $MODE\nEOF"),
        format!("gh pr view 3 --json {M}dAt # note"),
        format!("gh pr view 3 \\\n --json {M}dAt"),
        format!("gh pr view `echo 3` --json {M}dAt"),
        format!("git log --{M}s; echo $?"),
        format!("bash .hooks/gate-acceptance-before-{M}.sh < $F"),
        // test_acceptance_merge_hook_false_positive.py
        format!("python3 tools/run_complete_on_{M}.py --check-closed; echo e=$?"),
        format!(
            "python3 tools/{M}_approval_mode.py; gh pr checks 268 --repo o/r 2>&1 | awk '{{print $2}}' | sort | uniq -c\ngh pr view 268 --repo o/r --json state,headRefOid"
        ),
        format!(
            "gh pr view 1243 --repo o/r --json state,{M}dAt,{M}able -q .{M}dAt\ngh pr checks 1243 --repo o/r | head -5"
        ),
        format!(
            "G=$TMPDIR/guidance.txt\npython3 tools/gen_delegate_payload.py apply --task-id t --project-slug p \\\n  --target .hooks/gate-acceptance-before-{M}.sh --target tools/acceptance_{M}_hook.py \\\n  --description \"false positives on tools/run_complete_on_{M}.py, tools/{M}_approval_mode.py and gh pr view --json {M}dAt\" --impl-guidance \"$(cat $G)\""
        ),
    ] {
        assert_eq!(parse(&cmd), Ok(None), "{cmd}");
    }
}

#[test]
fn the_allowed_form() {
    for method in ["--squash", &format!("--{M}"), "--rebase"] {
        assert_eq!(
            parse(&merge_cmd("3", "o/r", method, &sha(), "")),
            Ok(Some((3, "o/r".into(), sha())))
        );
    }
    assert_eq!(
        parse(&merge_cmd(
            "12",
            "suisya-systems/makiba",
            "--squash",
            &sha(),
            " --delete-branch\n"
        )),
        Ok(Some((12, "suisya-systems/makiba".into(), sha())))
    );
}

/// Carried cases (`test_every_other_merge_is_denied`, `test_not_allow_listed_falls_back...`) that
/// run a merge; each is refused.
#[test]
fn every_other_merge_is_refused() {
    let s = sha();
    let mc = merge_cmd("7", "o/r", "--squash", &s, "");
    let passed = [
        format!("gh pr {M} 3 --repo o/r --squash"),
        merge_cmd("7", "o/r", "--squash", "abc1234", ""),
        merge_cmd("7", "o/r", "--squash", &s.to_uppercase(), ""),
        format!("gh pr {M} 3"),
        format!("gh pr {M} 3 --squash"),
        format!("gh pr {M} --repo o/r 3 --squash"),
        format!("gh pr {M} 3 --squash --repo o/r"),
        merge_cmd("7", "o/r", "--squash", &s, " --auto"),
        merge_cmd("7", "o/r", "--squash", &s, " --disable-auto"),
        merge_cmd("7", "o/r", "--squash", &s, " --admin"),
        format!("gh pr {M} 3 -R o/r --squash"),
        format!("gh pr {M} 3 --repo=o/r --squash"),
        format!("gh  {}", &mc[3..]),
        format!("{mc} && git pull"),
        format!("git pull; {mc}"),
        format!("{mc}\n--disable-auto"),
        format!("gh pr \\\n {M} 3 --repo o/r --squash"),
        format!("bash -c \"{mc}\""),
        format!("gh pr {M} $PR --repo o/r --squash"),
        "gh pr m'erge' 3".into(),
        format!("gh pr {} 3", M.to_uppercase()),
        format!("x={M}; gh pr $x 3"),
        merge_cmd("7", "o/r", "--squash", &s, " --body x"),
        format!("gh pr view 7\ngh pr {M} 7 --repo o/r --squash"),
        format!("gh pr create --title t && gh pr {M} 3"),
        format!("g=gh; $g pr {M} 3"),
        format!("$(echo gh) pr {M} 3"),
        format!("`echo gh` pr {M} 3"),
        format!("c=\"x {M}\"; eval \"$c\""),
        format!("python3 -c \"import subprocess; subprocess.run(['gh','pr','{M}','3'])\""),
        format!("gh $p {M} 3"),
        format!("gh alias set pm 'pr {M}'"),
        format!("/usr/bin/gh pr {M} 3"),
        format!("gh -R o/r pr {M} 3 --squash"),
        format!("gh.exe pr {M} 7 --repo o/r --squash"),
        format!("/c/tools/gh.exe pr {M} 7"),
        "gh pr mer\\\nge 7 --repo o/r --squash".into(),
        format!("gh pr ${{ACTION:-{M}}} 7 --repo o/r --squash"),
        format!("gh ${{G:-pr}} {M} 7"),
        format!("${{GH:-gh}} pr {M} 7"),
        format!("gh pr ${{ACTION-{M}}} 7 --repo o/r --squash"),
        format!("gh pr ${{A-${{B-{M}}}}} 7"),
        format!("gh pr ${{A-{M}"),
        format!("gh pr $(printf '{M}\\n') 7 --repo o/r --squash"),
        format!("PREFIX=; gh pr \"$PREFIX\"{M} 7 --squash"),
        format!("gh pr ${{P}}{M} 7"),
        format!("gh pr \"$(echo)\"{M} 7"),
        "P=; gh pr \"$P\"mer'ge' 7 --repo o/r --squash".into(),
        "gh pr $P'mer'ge 7".into(),
        format!(
            "gh pr \"${}_ACTION\" 7 --repo o/r --squash",
            M.to_uppercase()
        ),
        format!("gh pr ${{{}_ACTION}} 7", M.to_uppercase()),
        "gh pr \"$P\"mer'ge\\n' 7".into(),
        "gh pr m\\erge 7".into(),
        format!("x={M}_; gh pr ${{x%_}} 7"),
        format!("x=a-{M}; gh pr ${{x#*-}} 7"),
        format!("env \"$GH\" pr {M} 7 --repo o/r --squash"),
        format!("command \"$GH\" pr {M} 7"),
        format!("sudo $GH pr {M} 7"),
        format!("$GH $P {M} 7"),
        format!("xargs -I{{}} $GH pr {M} {{}}"),
        format!("env GH=1 $GH pr ${{X:-{M}}} 7"),
        format!("gh pr create --title \"{M} gate\" --body \"$(cat body.md)\""),
        format!("gh pr view 3 --json mergedAt && gh pr {M} 3"),
        format!("cat f <(gh pr {M} 3)"),
        format!("cat <<EOF\n$(gh pr {M} 3)\nEOF"),
        format!("cat <<'EOF'\nx\nEOF\ngh pr {M} 3"),
        format!("sed -n '1e gh pr {M} 3' f"),
        format!(
            "cat <<'EOF'\nnotes\nEOF\ngh pr {M} 7 --repo o/r --squash\ncat <<'EOF'\nnotes\nEOF"
        ),
        format!("cat /dev/null # <<\"EOF\"\ngh pr {M} 7 --repo o/r --squash\nEOF"),
        format!("sed -n -e 1p -e '1e gh pr {M} 7' f"),
        format!("sed -n 1p f -e '1e gh pr {M} 7'"),
        format!("less '+!gh pr {M} 3' f"),
        format!("rg --pre $X {M} f"),
        format!("grep x f 2>&1 | gh pr {M} 3"),
    ]
    .into_iter()
    .filter(|cmd| parse(cmd).is_ok())
    .collect::<Vec<_>>();
    assert!(passed.is_empty(), "not refused: {passed:#?}");
}

/// Issue 47: the other ways to merge from a command line are refused; a command that only names
/// one as an argument is not. Called in acceptance mode, by `acceptance_mode_cases`, and through
/// the hook's own binary for a part of them.
fn other_ways_are_refused_and_mentions_pass() {
    let refused = [
        // The pull-request merge command with any flag.
        format!("gh pr {M} 7 --admin"),
        format!("gh pr {M} 7 --auto --squash"),
        format!(
            "gh pr {M} --repo o/r 7 --rebase --match-head-commit {}",
            sha()
        ),
        // The API command with a write method on a pulls/<n>/merge path.
        format!("gh api -X PUT repos/o/r/pulls/3/{M}"),
        format!("gh api --method PUT /repos/o/r/pulls/3/{M}"),
        format!("gh api repos/o/r/pulls/3/{M} -f merge_method=squash"),
        format!("gh api -XPUT repos/o/r/pulls/3/{M}"),
        // Its GraphQL form carrying a merge mutation, or a body the hook cannot read.
        "gh api graphql -f query='mutation{mergePullRequest(input:{})}'".into(),
        "gh api graphql -f query='mutation{enablePullRequestAutoMerge(input:{})}'".into(),
        "gh api graphql --input q.json".into(),
        "gh api graphql -F query=@q.graphql".into(),
        // curl or wget to the GitHub API.
        format!("curl -X PUT https://api.github.com/repos/o/r/pulls/3/{M}"),
        "curl -s -H 'Authorization: token x' https://api.github.com/graphql -d @q.json".into(),
        format!("wget --method=PUT https://api.github.com/repos/o/r/pulls/3/{M}"),
    ];
    for cmd in &refused {
        assert!(parse(cmd).is_err(), "not refused: {cmd:?}");
    }
    let mentions = [
        format!("echo \"gh pr {M} 7 --admin\""),
        format!("git commit -m \"gh pr {M} is gated now\""),
        format!("grep -n \"gh pr {M}\" skills/pull-request/SKILL.md"),
        format!("grep -rn 'pulls/[0-9]*/{M}' src"),
        format!("echo gh api -X PUT repos/o/r/pulls/3/{M}"),
        "printf '%s\\n' 'mutation{mergePullRequest}'".into(),
        "echo curl https://api.github.com".into(),
        "grep -n api.github.com docs/design/spec.md".into(),
        // Reads are not merges.
        format!("gh api repos/o/r/pulls/3/{M}"),
        "gh api graphql -f query='query{repository(owner:\"o\",name:\"r\"){pullRequest(number:3){mergeable}}}'".into(),
        "curl -s https://example.com/merge".into(),
    ];
    for cmd in &mentions {
        assert_eq!(parse(cmd), Ok(None), "refused a mention: {cmd:?}");
    }
    // Through the hook's binary: refused without asking the gate (no door is there), a mention passes.
    let env = [
        ("MAKIBA_DOOR", "/nonexistent/door.sock"),
        ("MAKIBA_GATE_KEY", "/nonexistent/gate.key"),
    ];
    for cmd in &refused[..4] {
        let (code, err) = run_hook(&bash(cmd), &env);
        assert_eq!(code, 2, "{cmd}: {err}");
        assert!(err.contains("merge with exactly"), "{err}");
    }
    for cmd in &mentions[..4] {
        assert_eq!(run_hook(&bash(cmd), &env), (0, String::new()), "{cmd}");
    }
}

#[test]
fn other_ways_are_refused_and_mentions_pass_in_human_mode_too() {
    // The command reading does not depend on the mode; acceptance_mode_cases runs it in that mode.
    other_ways_are_refused_and_mentions_pass();
}

// ---- the hook's binary -------------------------------------------------------------------------

fn bash(cmd: &str) -> String {
    json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string()
}

/// `makiba hook merge-gate` with `stdin`: its exit code and standard error.
fn run_hook(stdin: &str, env: &[(&str, &str)]) -> (i32, String) {
    use std::io::Write;
    let mut c = Command::new(env!("CARGO_BIN_EXE_makiba"));
    c.args(["hook", "merge-gate"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn broken_payloads_are_refused_and_other_tools_pass() {
    for broken in [
        "",
        "  \n\t \n",
        "not json{",
        r#""a string""#,
        "[1,2,3]",
        "42",
        r#"{"tool_name":"Bash","tool_input":[1,2,3]}"#,
        r#"{"tool_name":"Bash","tool_input":{"command":"ls"}} 42"#,
    ] {
        let (code, err) = run_hook(broken, &[]);
        assert_eq!(code, 2, "{broken:?}: {err}");
        assert!(err.contains("payload"), "{err}");
    }
    let edit = r#"{"tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
    assert_eq!(run_hook(edit, &[]), (0, String::new()));
}

#[test]
fn the_allowed_form_without_makibad_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("gate.key");
    let sock = tmp.path().join("door.sock");
    let cmd = merge_cmd("7", "o/r", "--squash", &sha(), "");
    // No key file.
    let (code, err) = run_hook(
        &bash(&cmd),
        &[
            ("MAKIBA_GATE_KEY", key.to_str().unwrap()),
            ("MAKIBA_DOOR", sock.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("gate's key"), "{err}");
    // A key, nothing listening.
    std::fs::write(&key, "k\n").unwrap();
    let (code, err) = run_hook(
        &bash(&cmd),
        &[
            ("MAKIBA_GATE_KEY", key.to_str().unwrap()),
            ("MAKIBA_DOOR", sock.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("cannot reach makibad"), "{err}");
    // An unrelated command asks nothing.
    assert_eq!(
        run_hook(
            &bash("git status"),
            &[("MAKIBA_DOOR", sock.to_str().unwrap())]
        ),
        (0, String::new())
    );
}

// ---- the whole path through the door's socket (HookTest) -----------------------------------------

/// The door listening on a Unix socket of its own, with the gate key written where the hook reads.
struct Served {
    _tmp: tempfile::TempDir,
    _running: door::Running,
    env: Vec<(String, String)>,
}

impl Served {
    fn start(c: &Case) -> Served {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("d.sock");
        let gate_key = tmp.path().join("cfg").join("gate.key");
        let cfg = door::Config {
            socket: Some(socket.clone()),
            port: 0,
            person_key: tmp.path().join("cfg").join("person.key"),
            gate_key: gate_key.clone(),
            agent: "claude_code".into(),
        };
        let running = door::start(&cfg, &c.db, Arc::new(Stub::new()), Hub::new(|_| None))
            .unwrap_or_else(|e| {
                panic!("the door's socket (this test needs a host that lets it create one): {e}")
            });
        let mode = std::fs::metadata(&gate_key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the gate key is the person's alone");
        Served {
            env: vec![
                ("MAKIBA_DOOR".into(), socket.display().to_string()),
                ("MAKIBA_GATE_KEY".into(), gate_key.display().to_string()),
            ],
            _tmp: tmp,
            _running: running,
        }
    }

    fn hook(&self, cmd: &str) -> (i32, String) {
        let env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        run_hook(&bash(cmd), &env)
    }
}

#[test]
fn hook_passing_pr_allowed_and_rejected_pr_refused() {
    let c = Case::new("hkpass");
    c.land();
    let head = c.ci_all(7);
    let served = Served::start(&c);
    assert_eq!(
        served.hook(&merge_cmd("7", &c.repo, "--squash", &head, "")),
        (0, String::new())
    );
    c.commit(
        "weaken",
        &[("tests/test_calc.py", Some("def test_add():\n    pass\n"))],
    );
    let head = c.ci_all(7);
    let (code, err) = served.hook(&merge_cmd("7", &c.repo, "--squash", &head, ""));
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("ACCEPTANCE_GATE rejected"), "{err}");
    // The answer was recorded as the gate's key holder's.
    let db = Connection::open(&c.db).unwrap();
    let actors: i64 = db
        .query_row(
            "SELECT count(*) FROM commands WHERE verb = 'gate' AND actor = 'merge-gate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(actors, 2);
}

#[test]
fn hook_push_after_ci_refused() {
    let c = Case::new("hkpush");
    c.commit("fix", &[("calc.py", Some(FIXED))]);
    let first = c.ci_all(7);
    c.commit("more", &[("x.py", Some("x = 1\n"))]);
    let head = c.publish(7);
    let served = Served::start(&c);
    for sha in [&first, &head] {
        let (code, err) = served.hook(&merge_cmd("7", &c.repo, "--squash", sha, ""));
        assert_eq!(code, 2, "{err}");
    }
}

#[test]
fn hook_no_manifest_refused_in_acceptance_mode() {
    if !in_mode("acceptance", "hook_no_manifest_refused_in_acceptance_mode") {
        return;
    }
    let (c, head) = no_manifest_pr("hkacceptance", &[("tools/x.py", Some("x = 1\n"))]);
    let served = Served::start(&c);
    let (code, err) = served.hook(&merge_cmd("7", &c.repo, "--squash", &head, ""));
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("no_manifest") && err.contains("acceptance"),
        "{err}"
    );
}

#[test]
fn hook_no_manifest_allowed_in_human_mode() {
    if !in_mode("human", "hook_no_manifest_allowed_in_human_mode") {
        return;
    }
    let (c, head) = no_manifest_pr("hkhuman", &[("tools/x.py", Some("x = 1\n"))]);
    let served = Served::start(&c);
    assert_eq!(
        served.hook(&merge_cmd("7", &c.repo, "--squash", &head, "")),
        (0, String::new())
    );
}

#[test]
fn hook_gh_failure_refused() {
    let c = Case::new("hkghfail");
    c.land();
    let head = c.ci_all(7);
    std::fs::write(c.gh.join("fail"), "").unwrap();
    let served = Served::start(&c);
    let (code, err) = served.hook(&merge_cmd("7", &c.repo, "--squash", &head, ""));
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("gh pr view failed"), "{err}");
}

#[test]
fn hook_other_form_refused_without_asking_and_a_read_passes() {
    let c = Case::new("hkform");
    let served = Served::start(&c);
    for cmd in [
        format!("gh pr {M} 7 --squash"),
        format!("gh pr {} 7 --repo o/r --squash", M.to_uppercase()),
    ] {
        let (code, err) = served.hook(&cmd);
        assert_eq!(code, 2);
        assert!(err.contains("merge with exactly"), "{err}");
    }
    assert_eq!(
        served.hook("gh pr view 7 --repo o/r --json state,mergedAt"),
        (0, String::new())
    );
    assert_eq!(c.calls(), "", "the gate was not asked");
}
