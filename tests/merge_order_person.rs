//! Issue 338: a merge order the person sets between pull requests ("A merges only after Issue or
//! pull request B has landed") is recorded with `merge` action `order` and held by the merge
//! gate, as a second reason of Issue 331's `merge_order` hold (docs/design/merge-order.md). The
//! case of PR 336 (Issue 284), which the person approved on the condition that it merges only
//! after Issue 332 has landed: the gate refuses 336 under either `merge_approval_mode` while 332
//! has not landed, naming it, and allows it once the ledger shows the landing; `status` shows the
//! order on the pull request. With no order recorded nothing changes.
//!
//! The world is that of `tests/merge_order.rs` (its helpers copied here): the door runs in this
//! process on real git repositories, `gh` is a stand-in script, and each mode runs in a child
//! process of this test binary.

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
const REASON: &str = "the person approved Issue 284's design on this condition";

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n";
/// The environment variable that selects the helper mode of this binary (assertion 11).
const HELPER: &str = "PERSON_ORDER_HELPER";

/// The stand-in for `gh`: `pr view <n> --repo o/<name> ...` prints `view.<n>` (first running this
/// test binary's helper mode once when `hook.<n>` is there); `api --paginate <endpoint> --jq ...`
/// prints `<sha>.check-runs` or `<sha>.runs`.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; dir="$FAKE_GH_DIR/$name"
  echo "pr view $3" >> "$dir/calls"
  if [ -e "$dir/hook.$3" ]; then
    PERSON_ORDER_HELPER="$dir/hook.$3" "$PERSON_ORDER_SELF" helper_mode_appends_an_order --exact --test-threads=1 >> "$dir/helper.log" 2>&1
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
            std::env::set_var("PERSON_ORDER_SELF", std::env::current_exe().unwrap());
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

/// One repository's world: an origin with a branch per pull request, the project's clone,
/// `state.db` and a door.
struct Case {
    _tmp: tempfile::TempDir,
    slug: String,
    repo: String,
    origin: PathBuf,
    gh: PathBuf,
    db: PathBuf,
    door: Door,
    person: String,
    secretary: String,
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
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        let mut c = Case {
            _tmp: tmp,
            slug: name.to_string(),
            repo: format!("o/{name}"),
            origin,
            gh,
            db,
            door,
            person,
            secretary,
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

    /// Publish HEAD as PR `pr`'s head and let the stand-in answer `gh pr view` for it.
    fn publish(&self, pr: i64) -> String {
        let head = self.head();
        self.git(&["update-ref", &format!("refs/pull/{pr}/head"), &head]);
        let view = json!({"number": pr, "url": format!("file://{}/pull/{pr}", self.origin.display()),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(self.gh.join(format!("view.{pr}")), view.to_string()).unwrap();
        head
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

    /// The run of task `task` in `review` (with `issue_refs` when given), its pull request `pr`
    /// recorded at `head`.
    fn run(&self, task: &str, refs: Option<&[&str]>, pr: i64, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: &self.slug,
            title: Some("T"),
            status: Some("review"),
            issue_refs: refs,
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

    /// PR 336, the case of Issue 284: a test commit declaring `tests/test_calc.py`, then the fix
    /// of `calc.py` (no check path: Issue 331's rule does not hold it); CI failed at the test
    /// commit and passed at head; its last diff review passed at head. And the run of Issue 332,
    /// task `t332`, with its pull request 337 recorded. The head of 336.
    fn world(&self) -> String {
        self.branch(336);
        self.commit(
            "test: failing test\n\nAcceptance-Test: tests/test_calc.py",
            &[("tests/test_calc.py", Some(TEST))],
        );
        self.commit("fix", &[("calc.py", Some(FIXED))]);
        let head = self.ci_all(336);
        self.run("t336", None, 336, &head);
        self.reviewed("t336", 336, &head);
        let refs = format!("{}#332", self.slug);
        self.run("t332", Some(&[refs.as_str()]), 337, &"7".repeat(40));
        head
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Result<Value, DoorError> {
        self.door.handle(Some(key), verb, args, None, false)
    }

    /// The secretary's `merge` `action` `order` on 336, with `after` (`after_issue` or
    /// `after_pr`) and the reason.
    fn order(&self, after: &str, number: i64) -> Result<Value, DoorError> {
        let mut args = json!({"action": "order", "repo": self.repo, "pr": 336, "reason": REASON});
        args[after] = json!(number);
        self.call(&self.secretary, "merge", args)
    }

    fn gate_at(&self, head: &str) -> Value {
        self.call(
            &self.person,
            "gate",
            json!({"repo": self.repo, "pr": 336, "head": head}),
        )
        .unwrap_or_else(|e| panic!("gate: {e:?}"))
    }

    fn merge_by(&self, key: &str, head: &str) -> Value {
        self.call(
            key,
            "merge",
            json!({"repo": self.repo, "pr": 336, "head": head, "method": "squash"}),
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

    /// The payloads of every event of `kind`, oldest first.
    fn payloads(&self, kind: &str) -> Vec<Value> {
        let c = Connection::open(&self.db).unwrap();
        let mut st = c
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        st.query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
            .collect()
    }

    /// The payload of the newest `acceptance_gate`.
    fn last_gate(&self) -> Value {
        self.payloads("acceptance_gate").pop().unwrap()
    }

    /// The `merge_order` of 336's entry in the secretary's `status`.
    fn status_order(&self) -> Value {
        let v = self
            .call(&self.secretary, "status", json!({}))
            .unwrap_or_else(|e| panic!("status: {e:?}"));
        let entry = v["pull_requests"]
            .as_array()
            .unwrap_or_else(|| panic!("no pull_requests in {v}"))
            .iter()
            .find(|e| e["pr"] == json!(336))
            .unwrap_or_else(|| panic!("336 is not listed: {}", v["pull_requests"]))
            .clone();
        entry
            .get("merge_order")
            .cloned()
            .unwrap_or_else(|| panic!("no merge_order in {entry}"))
    }

    fn landed(&self, task: &str, pr: i64) {
        self.event(
            "pr_merged",
            Some(task),
            json!({"repo": self.repo, "pr": pr, "merge_commit": C, "task_id": task}),
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

fn recorded(r: Result<Value, DoorError>) -> Value {
    let v = r.unwrap_or_else(|e| panic!("order: {e:?}"));
    assert_eq!(v["status"], json!("recorded"), "{v}");
    v
}

// ---- 1 to 5: the case of PR 336 after Issue 332 --------------------------------------------------

#[test]
fn the_336_case_is_held_until_issue_332_landed_and_shown_in_status() {
    if !in_mode(
        "acceptance",
        "the_336_case_is_held_until_issue_332_landed_and_shown_in_status",
    ) {
        return;
    }
    let c = Case::new("p1");
    let head = c.world();

    // 1. No order recorded: nothing changes.
    assert_allowed(&c.gate_at(&head));
    assert!(
        c.last_gate().get("merge_order").is_none(),
        "{}",
        c.last_gate()
    );
    assert_eq!(c.status_order(), json!([]));

    // 2. The order, recorded once.
    let first = recorded(c.order("after_issue", 332));
    assert_eq!(first["landed"], json!(false), "{first}");
    let id = first["event_id"].clone();
    assert!(id.is_i64(), "{first}");
    let again = recorded(c.order("after_issue", 332));
    assert_eq!(again["event_id"], id, "{again}");
    let set = c.payloads("merge_order_set");
    assert_eq!(set.len(), 1, "{set:?}");
    assert!(
        set[0]["repo"]
            .as_str()
            .is_some_and(|r| r.eq_ignore_ascii_case(&c.repo)),
        "{}",
        set[0]
    );
    assert_eq!(set[0]["pr"], json!(336), "{}", set[0]);
    assert_eq!(set[0]["after"], json!("issue"), "{}", set[0]);
    assert_eq!(set[0]["number"], json!(332), "{}", set[0]);
    assert_eq!(set[0]["by"], json!("secretary"), "{}", set[0]);

    // 3. The merge is refused, naming Issue 332 and the reason; nothing is sent.
    let v = c.merge_by(&c.person, &head);
    assert_eq!(v["status"], json!("refused"), "{v}");
    assert_held(&v, &["Issue 332", REASON]);
    assert_eq!(c.count("merge_attempted"), 0, "nothing is sent");
    let row = c.last_gate();
    assert_eq!(row["allow"], json!(false), "{row}");
    let after = row["merge_order"]["after"]
        .as_array()
        .unwrap_or_else(|| panic!("no merge_order.after in {row}"))
        .clone();
    assert_eq!(after.len(), 1, "{row}");
    assert_eq!(after[0]["after"], json!("issue"), "{row}");
    assert_eq!(after[0]["number"], json!(332), "{row}");

    // 4. status shows it.
    let shown = c.status_order();
    let shown = shown.as_array().unwrap_or_else(|| panic!("{shown}"));
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(shown[0]["after"], json!("issue"), "{shown:?}");
    assert_eq!(shown[0]["number"], json!(332), "{shown:?}");
    assert_eq!(shown[0]["landed"], json!(false), "{shown:?}");

    // 5. Issue 332 landed: allowed as before.
    c.landed("t332", 337);
    assert_allowed(&c.gate_at(&head));
    assert!(
        c.last_gate().get("merge_order").is_none(),
        "{}",
        c.last_gate()
    );
    let shown = c.status_order();
    assert_eq!(shown[0]["landed"], json!(true), "{shown}");
}

// ---- 6: under human it is refused, not a warning ----------------------------------------------

#[test]
fn under_human_the_order_refuses() {
    if !in_mode("human", "under_human_the_order_refuses") {
        return;
    }
    let c = Case::new("p6");
    let head = c.world();
    recorded(c.order("after_issue", 332));
    assert_held(&c.gate_at(&head), &["Issue 332"]);
}

// ---- 7: an order after a pull request ----------------------------------------------------------

#[test]
fn an_order_after_a_pull_request_holds_until_it_merged() {
    if !in_mode(
        "acceptance",
        "an_order_after_a_pull_request_holds_until_it_merged",
    ) {
        return;
    }
    let c = Case::new("p7");
    let head = c.world();
    recorded(c.order("after_pr", 337));
    assert_held(&c.gate_at(&head), &["pull request #337"]);
    c.landed("t332", 337);
    assert_allowed(&c.gate_at(&head));
}

// ---- 8: usage ----------------------------------------------------------------------------------

#[test]
fn a_malformed_order_is_refused_and_writes_nothing() {
    if !in_mode(
        "acceptance",
        "a_malformed_order_is_refused_and_writes_nothing",
    ) {
        return;
    }
    let c = Case::new("p8");
    c.world();
    let base = json!({"action": "order", "repo": c.repo, "pr": 336, "reason": REASON});
    let with = |extra: &[(&str, Value)]| {
        let mut a = base.clone();
        for (k, v) in extra {
            a[*k] = v.clone();
        }
        a
    };
    for args in [
        with(&[("after_issue", json!(332)), ("after_pr", json!(337))]),
        with(&[]),
        with(&[("after_pr", json!(336))]),
        with(&[("after_issue", json!(332)), ("reason", json!("  "))]),
    ] {
        match c.call(&c.secretary, "merge", args.clone()) {
            Err(e) => assert_eq!(e.kind, ErrorKind::Usage, "{args}: {e:?}"),
            Ok(v) => panic!("{args} was not refused: {v}"),
        }
    }
    assert_eq!(c.count("merge_order_set"), 0);
}

// ---- 9: a landing of a pull request the Issue's run has replaced does not count ----------------

#[test]
fn a_merge_of_a_replaced_pull_request_is_not_the_issue_landing() {
    if !in_mode(
        "acceptance",
        "a_merge_of_a_replaced_pull_request_is_not_the_issue_landing",
    ) {
        return;
    }
    let c = Case::new("p9");
    let head = c.world();
    recorded(c.order("after_issue", 332));
    c.landed("t332", 337);
    c.registered("t332", 338, &"8".repeat(40));
    assert_held(&c.gate_at(&head), &["Issue 332"]);
    c.landed("t332", 338);
    assert_allowed(&c.gate_at(&head));
}

// ---- 10: a pending merge approved after the order was recorded --------------------------------

#[test]
fn a_pending_merge_approved_after_the_order_is_refused() {
    if !in_mode(
        "human",
        "a_pending_merge_approved_after_the_order_is_refused",
    ) {
        return;
    }
    let c = Case::new("p10");
    let head = c.world();
    let v = c.merge_by(&c.secretary, &head);
    assert_eq!(v["status"], json!("pending"), "{v}");
    let id = v["pending_id"].as_i64().unwrap_or_else(|| panic!("{v}"));
    recorded(c.order("after_issue", 332));
    let printed = makiba::status_local::merge_entry_text(&c.db, id)
        .unwrap_or_else(|e| panic!("entry {id}: {e}"));
    let v = c
        .call(
            &c.person,
            "merge",
            json!({"action": "approve", "id": id, "shown": sha256_hex(&printed)}),
        )
        .unwrap_or_else(|e| panic!("approve: {e:?}"));
    assert_eq!(v["status"], json!("refused"), "{v}");
    assert_eq!(v["pending_id"], json!(id), "{v}");
    assert_held(&v, &["Issue 332"]);
    assert_eq!(c.count("merge_attempted"), 0, "nothing is sent");
    let status: String = Connection::open(&c.db)
        .unwrap()
        .query_row("SELECT status FROM pending WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "open");
}

// ---- 11: an order recorded while the gate read GitHub and git ----------------------------------

#[test]
fn an_order_recorded_while_the_gate_ran_holds_its_answer() {
    if !in_mode(
        "acceptance",
        "an_order_recorded_while_the_gate_ran_holds_its_answer",
    ) {
        return;
    }
    let c = Case::new("p11");
    let head = c.world();
    let hook = json!({"db": c.db, "repo": c.repo, "project": c.slug, "task": "t336"});
    std::fs::write(c.gh.join("hook.336"), hook.to_string()).unwrap();
    let v = c.gate_at(&head);
    assert!(
        !c.gh.join("hook.336").exists(),
        "the stand-in did not run the helper: {}",
        std::fs::read_to_string(c.gh.join("helper.log")).unwrap_or_default()
    );
    assert_eq!(c.count("merge_order_set"), 1);
    assert_held(&v, &["Issue 332"]);
}

// ---- 12: an order cannot be set while a merge act of makibad awaits its landing ---------------

#[test]
fn an_order_on_a_pull_request_with_an_accepted_merge_is_refused() {
    if !in_mode(
        "acceptance",
        "an_order_on_a_pull_request_with_an_accepted_merge_is_refused",
    ) {
        return;
    }
    let c = Case::new("p12");
    let head = c.world();
    let v = c.merge_by(&c.person, &head);
    assert_eq!(v["status"], json!("accepted"), "{v}");
    assert_eq!(c.count("merge_attempted"), 1);
    match c.order("after_issue", 332) {
        Err(e) => assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}"),
        Ok(v) => panic!("the order was recorded: {v}"),
    }
    assert_eq!(c.count("merge_order_set"), 0);
}

/// The helper mode the stand-in runs (assertion 11): with `PERSON_ORDER_HELPER` naming a file, it
/// appends the `merge_order_set` of 336 after Issue 332 that file describes; without it, nothing.
#[test]
fn helper_mode_appends_an_order() {
    let Ok(path) = std::env::var(HELPER) else {
        return;
    };
    let h: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let w = StateWriter::open(Path::new(h["db"].as_str().unwrap())).unwrap();
    w.append_event(&NewEvent {
        kind: "merge_order_set",
        actor: Some("secretary"),
        payload: Some(
            &json!({"repo": h["repo"], "pr": 336, "after": "issue", "number": 332,
                              "project": h["project"], "reason": REASON, "by": "secretary"}),
        ),
        run_task_id: h["task"].as_str(),
        ..Default::default()
    })
    .unwrap();
}
