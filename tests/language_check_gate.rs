//! Issue 339: a change to the language check (`tools/language-allowlist.txt`, or
//! `tools/check_language.py`, its scope) in a pull request is a stop line for the person. The
//! case of #305 (Issue 169): a worker's draft added an entry to the allowlist so the language
//! check would let a file in; the person had said the check gets no new exception.
//!
//! The merge gate refuses such a pull request, under both merge approval modes, with verdict
//! `language_check`, until the person answers yes with their own key to the question that names
//! each changed file with its mode and full object id at the head:
//! `Approve the change to the language check of <repo>#<pr>: <path> at <mode> <oid>` (or
//! `<path> deleted`), the newest answer of that question on the pull request's run deciding. A
//! file there that is not a regular file is never approved. A range task's refusal is a stop that
//! asks that question. A pull request whose own changes touch neither file is not affected.
//!
//! The world is that of `tests/merge_order.rs` (its helpers copied here): the door runs in this
//! process on real git repositories, `gh` is a stand-in script, and each mode runs in a child
//! process of this test binary.

// Some copied helpers are not used by every case.
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
const T1: &str = "2026-11-05T01:00:00Z";
const ACTIONS: i64 = 15368;

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n";
const ALLOW: &str = "tools/language-allowlist.txt";
const CHECKER: &str = "tools/check_language.py";
const ALLOW_BASE: &str = "docs/old.md\n";
const ALLOW_305: &str = "docs/old.md\ndocs/design/screen-draft.ja.html\n";
const ALLOW_MORE: &str = "docs/old.md\ndocs/design/screen-draft.ja.html\ndocs/more.md\n";
const CHECKER_BASE: &str = "DIRS = ['src', 'docs']\n";
const CHECKER_NARROW: &str = "DIRS = ['src']\n";

/// The stand-in for `gh`: `pr view <n> --repo o/<name> ...` prints `view.<n>`; `api --paginate
/// <endpoint> --jq ...` prints `<sha>.check-runs` or `<sha>.runs`.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; dir="$FAKE_GH_DIR/$name"
  echo "pr view $3" >> "$dir/calls"
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

/// One repository's world: an origin with a branch per pull request, the project's clone,
/// `state.db` and a door.
struct Case {
    _tmp: tempfile::TempDir,
    name: String,
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
            name: name.to_string(),
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
                (ALLOW, Some(ALLOW_BASE)),
                (CHECKER, Some(CHECKER_BASE)),
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

    /// The test commit (declaring `tests/test_calc.py`) and the fix, with `extra` in the fix.
    fn test_and_fix(&self, extra: &[(&str, Option<&str>)]) -> String {
        self.commit(
            "test: failing test\n\nAcceptance-Test: tests/test_calc.py",
            &[("tests/test_calc.py", Some(TEST))],
        );
        let mut files: Vec<(&str, Option<&str>)> = vec![("calc.py", Some(FIXED))];
        files.extend_from_slice(extra);
        self.commit("fix", &files)
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

    fn answer_gh(&self, sha: &str, kind: &str, lines: &[Value]) {
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
            self.answer_gh(
                sha,
                "check-runs",
                &[json!({"app": ACTIONS, "name": "test",
                         "conclusion": if ok { "success" } else { "failure" },
                         "completed_at": T1, "suite": 1})],
            );
            self.answer_gh(
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

    /// A run of task `task` in `review`, with no pull request.
    fn bare_run(&self, task: &str) {
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
    }

    /// The run of task `task` in `review`, its pull request `pr` recorded at `head`.
    fn run(&self, task: &str, pr: i64, head: &str) {
        self.bare_run(task);
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
                "INSERT OR IGNORE INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
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

    /// PR `pr` of task `t<pr>` from the base: its test commit and its fix (with `extra`), CI
    /// failed at the test commit and passed at the head, the head recorded and reviewed. Its head.
    fn pr(&self, pr: i64, extra: &[(&str, Option<&str>)]) -> String {
        self.branch(pr);
        self.test_and_fix(extra);
        self.landable(pr)
    }

    /// The current HEAD as PR `pr`'s head, with CI, the record and a passed review there.
    fn landable(&self, pr: i64) -> String {
        let task = format!("t{pr}");
        let head = self.ci_all(pr);
        self.run(&task, pr, &head);
        self.reviewed(&task, pr, &head);
        head
    }

    fn ask(&self, key: &str, verb: &str, args: Value) -> Value {
        self.door
            .handle(Some(key), verb, args, None, false)
            .unwrap_or_else(|e| panic!("{verb}: {e:?}"))
    }

    fn gate_at(&self, pr: i64, head: &str) -> Value {
        self.ask(
            &self.person,
            "gate",
            json!({"repo": self.repo, "pr": pr, "head": head}),
        )
    }

    fn merge_at(&self, pr: i64, head: &str) -> Value {
        self.ask(
            &self.secretary,
            "merge",
            json!({"repo": self.repo, "pr": pr, "head": head, "method": "squash"}),
        )
    }

    /// A pending question of `task` added by the secretary; its id.
    fn add_question(&self, task: &str, question: &str) -> i64 {
        let v = self.ask(
            &self.secretary,
            "pending",
            json!({"action": "add", "task": task, "question": question,
                   "background": "the pull request changes the language check",
                   "if_yes": "the change to the language check may land",
                   "if_no": "the worker takes the change out",
                   "recommend": "no", "why": "the language check gets no new exception"}),
        );
        v["id"].as_i64().unwrap_or_else(|| panic!("no id in {v}"))
    }

    /// `key` answers pending entry `id`.
    fn answer(&self, key: &str, id: i64, answer: &str) {
        self.ask(key, "answer", json!({"id": id, "answer": answer}));
    }

    /// The question asked of `task`, answered by `key`; the entry's id.
    fn approve(&self, key: &str, task: &str, question: &str, answer: &str) -> i64 {
        let id = self.add_question(task, question);
        self.answer(key, id, answer);
        id
    }

    /// `<mode> <oid>` of `path` at `head`, as `git ls-tree` reads it.
    fn entry(&self, head: &str, path: &str) -> String {
        let line = self.git(&["ls-tree", head, "--", path]);
        let mut words = line.split_whitespace();
        let mode = words
            .next()
            .unwrap_or_else(|| panic!("{path} not at {head}"));
        let _kind = words.next().unwrap();
        let oid = words.next().unwrap();
        assert_eq!(oid.len(), 40, "{line}");
        format!("{mode} {oid}")
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

    /// Make `task` a task of a range of this project approved under `mode`; the range's id.
    fn in_range(&self, task: &str, mode: &str) -> i64 {
        let w = StateWriter::open(&self.db).unwrap();
        w.conn()
            .execute(
                "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                     proposed_by, status, approval_mode, approved_by, approved_at)
                 VALUES ((SELECT id FROM projects WHERE slug = ?1), '[1]', 1, 5, 'h', 'secretary',
                         'active', ?2, 'person', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [self.name.as_str(), mode],
            )
            .unwrap();
        let id = w.conn().last_insert_rowid();
        w.conn()
            .execute(
                "UPDATE runs SET range_id = ?1 WHERE task_id = ?2",
                rusqlite::params![id, task],
            )
            .unwrap();
        id
    }

    /// The stops `makibad` wrote: (id, status, question, context).
    fn stops(&self) -> Vec<(i64, String, String, Value)> {
        let db = Connection::open(&self.db).unwrap();
        let mut st = db
            .prepare(
                "SELECT id, status, question, context_json FROM pending
                 WHERE json_extract(context_json, '$.by') = 'makibad' ORDER BY id",
            )
            .unwrap();
        st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap(),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }
}

/// The question the gate asks, word for word, for the changed files `entries` (each
/// `<path> at <mode> <oid>` or `<path> deleted`) of `repo`#`pr`.
fn question(repo: &str, pr: i64, entries: &[String]) -> String {
    format!(
        "Approve the change to the language check of {repo}#{pr}: {}",
        entries.join(", ")
    )
}

fn reason(v: &Value) -> &str {
    v["reason"].as_str().unwrap_or_default()
}

fn assert_allowed(v: &Value) {
    assert_eq!(v["verdict"], json!("passed"), "{v}");
    assert_eq!(v["allow"], json!(true), "{v}");
}

/// Refused as a change to the language check, the reason naming `question` word for word.
fn assert_refused(v: &Value, question: &str) {
    assert_eq!(v["verdict"], json!("language_check"), "{v}");
    assert_eq!(v["allow"], json!(false), "{v}");
    assert!(
        reason(v).contains(question),
        "the reason does not ask {question:?}: {v}"
    );
}

/// The #305 case: PR 305 adds an entry to the allowlist; its head and the gate's question.
fn the_305_case(c: &Case) -> (String, String) {
    let head = c.pr(305, &[(ALLOW, Some(ALLOW_305))]);
    let q = question(
        &c.repo,
        305,
        &[format!("{ALLOW} at {}", c.entry(&head, ALLOW))],
    );
    (head, q)
}

// ---- 1 and 2: the #305 case is refused under both modes ----------------------------------------

#[test]
fn the_305_case_is_refused_at_the_merge_under_acceptance() {
    if !in_mode(
        "acceptance",
        "the_305_case_is_refused_at_the_merge_under_acceptance",
    ) {
        return;
    }
    let c = Case::new("l1");
    let (head, q) = the_305_case(&c);
    assert!(q.contains(" at 100644 "), "{q}");

    let v = c.gate_at(305, &head);
    assert_refused(&v, &q);
    let row = c.last_gate();
    let lc = &row["language_check"];
    assert_eq!(lc["paths"], json!([ALLOW]), "{row}");
    assert_eq!(lc["question"], json!(q), "{row}");
    assert!(lc["approved"].is_null(), "{row}");
    assert_eq!(row["allow"], json!(false), "{row}");

    let v = c.merge_at(305, &head);
    assert_eq!(v["status"], json!("refused"), "{v}");
    assert_refused(&v, &q);
    assert_eq!(c.count("merge_attempted"), 0, "nothing is sent");
}

#[test]
fn the_305_case_is_refused_under_human_too() {
    if !in_mode("human", "the_305_case_is_refused_under_human_too") {
        return;
    }
    let c = Case::new("l2");
    let (head, q) = the_305_case(&c);
    assert_refused(&c.gate_at(305, &head), &q);
    let v = c.merge_at(305, &head);
    assert_eq!(v["status"], json!("refused"), "{v}");
    assert_eq!(c.count("merge_attempted"), 0, "nothing is sent");
}

// ---- 3: the person's yes to that question lets it land ----------------------------------------

#[test]
fn the_persons_yes_to_the_exact_question_lets_it_land() {
    if !in_mode(
        "acceptance",
        "the_persons_yes_to_the_exact_question_lets_it_land",
    ) {
        return;
    }
    let c = Case::new("l3");
    let (head, q) = the_305_case(&c);
    let id = c.approve(&c.person, "t305", &q, "yes");

    let v = c.gate_at(305, &head);
    assert_allowed(&v);
    assert_ne!(v["warn"], json!(true), "{v}");
    let row = c.last_gate();
    assert_eq!(row["language_check"]["approved"], json!(id), "{row}");
    assert_eq!(row["language_check"]["question"], json!(q), "{row}");

    let v = c.merge_at(305, &head);
    assert_eq!(v["status"], json!("accepted"), "{v}");
    assert_eq!(c.count("merge_attempted"), 1);
}

// ---- 4: only the person's yes to that very question counts -------------------------------------

#[test]
fn a_no_another_question_another_run_a_relayed_yes_or_a_newer_no_does_not_approve() {
    if !in_mode(
        "acceptance",
        "a_no_another_question_another_run_a_relayed_yes_or_a_newer_no_does_not_approve",
    ) {
        return;
    }
    let c = Case::new("l4");
    let (head, q) = the_305_case(&c);

    // A no to the exact question.
    c.approve(&c.person, "t305", &q, "no");
    assert_refused(&c.gate_at(305, &head), &q);

    // A yes to the question of another version of the file.
    let base_entry = format!("{ALLOW} at {}", c.entry(&c.base, ALLOW));
    let other = question(&c.repo, 305, &[base_entry]);
    assert_ne!(other, q);
    c.approve(&c.person, "t305", &other, "yes");
    assert_refused(&c.gate_at(305, &head), &q);

    // A yes on another task's run.
    c.bare_run("t999");
    c.approve(&c.person, "t999", &q, "yes");
    assert_refused(&c.gate_at(305, &head), &q);

    // A yes the secretary relays is not the person's own answer.
    c.approve(&c.secretary, "t305", &q, "yes");
    assert_refused(&c.gate_at(305, &head), &q);

    // The person's yes, then a newer entry of the same question answered no: the newest decides.
    c.approve(&c.person, "t305", &q, "yes");
    assert_allowed(&c.gate_at(305, &head));
    c.approve(&c.person, "t305", &q, "no");
    assert_refused(&c.gate_at(305, &head), &q);
}

// ---- 5: a later change of the file asks again ------------------------------------------------

#[test]
fn a_later_change_of_the_allowlist_asks_again() {
    if !in_mode("acceptance", "a_later_change_of_the_allowlist_asks_again") {
        return;
    }
    let c = Case::new("l5");
    let (head, q) = the_305_case(&c);
    c.approve(&c.person, "t305", &q, "yes");
    assert_allowed(&c.gate_at(305, &head));

    c.commit("more", &[(ALLOW, Some(ALLOW_MORE))]);
    let new = c.landable(305);
    let q2 = question(
        &c.repo,
        305,
        &[format!("{ALLOW} at {}", c.entry(&new, ALLOW))],
    );
    assert_ne!(q2, q);
    assert_refused(&c.gate_at(305, &new), &q2);
}

// ---- 6: the scope, a deletion, a file that is not a regular one -------------------------------

#[test]
fn a_change_of_the_checker_is_a_change_of_the_language_check() {
    if !in_mode(
        "acceptance",
        "a_change_of_the_checker_is_a_change_of_the_language_check",
    ) {
        return;
    }
    let c = Case::new("l6a");
    let head = c.pr(306, &[(CHECKER, Some(CHECKER_NARROW))]);
    let q = question(
        &c.repo,
        306,
        &[format!("{CHECKER} at {}", c.entry(&head, CHECKER))],
    );
    let v = c.gate_at(306, &head);
    assert_refused(&v, &q);
    assert_eq!(c.last_gate()["language_check"]["paths"], json!([CHECKER]));
}

#[test]
fn a_deleted_allowlist_is_a_change_of_the_language_check() {
    if !in_mode(
        "acceptance",
        "a_deleted_allowlist_is_a_change_of_the_language_check",
    ) {
        return;
    }
    let c = Case::new("l6b");
    let head = c.pr(307, &[(ALLOW, None)]);
    let q = question(&c.repo, 307, &[format!("{ALLOW} deleted")]);
    assert_refused(&c.gate_at(307, &head), &q);
    c.approve(&c.person, "t307", &q, "yes");
    assert_allowed(&c.gate_at(307, &head));
}

#[test]
fn an_allowlist_that_is_not_a_regular_file_is_never_approved() {
    if !in_mode(
        "acceptance",
        "an_allowlist_that_is_not_a_regular_file_is_never_approved",
    ) {
        return;
    }
    let c = Case::new("l6c");
    c.branch(308);
    c.commit(
        "test: failing test\n\nAcceptance-Test: tests/test_calc.py",
        &[("tests/test_calc.py", Some(TEST))],
    );
    let dest = c.origin.join(ALLOW);
    std::fs::remove_file(&dest).unwrap();
    std::os::unix::fs::symlink("../docs/elsewhere.txt", &dest).unwrap();
    c.commit("fix", &[("calc.py", Some(FIXED))]);
    let head = c.landable(308);
    assert!(c.entry(&head, ALLOW).starts_with("120000 "));

    let v = c.gate_at(308, &head);
    assert_eq!(v["verdict"], json!("language_check"), "{v}");
    assert_eq!(v["allow"], json!(false), "{v}");
    assert!(reason(&v).contains("not a regular file"), "{v}");
    let q = question(
        &c.repo,
        308,
        &[format!("{ALLOW} at {}", c.entry(&head, ALLOW))],
    );
    c.approve(&c.person, "t308", &q, "yes");
    let v = c.gate_at(308, &head);
    assert_eq!(v["verdict"], json!("language_check"), "{v}");
    assert_eq!(v["allow"], json!(false), "{v}");
}

// ---- 7: a pull request that does not touch it is not affected ---------------------------------

#[test]
fn a_pull_request_that_touches_neither_file_is_not_affected() {
    if !in_mode(
        "acceptance",
        "a_pull_request_that_touches_neither_file_is_not_affected",
    ) {
        return;
    }
    let c = Case::new("l7a");
    let head = c.pr(309, &[]);
    assert_allowed(&c.gate_at(309, &head));
    let row = c.last_gate();
    assert!(row.get("language_check").is_none(), "{row}");
}

#[test]
fn an_allowlist_change_the_base_brought_is_not_the_pull_requests() {
    if !in_mode(
        "acceptance",
        "an_allowlist_change_the_base_brought_is_not_the_pull_requests",
    ) {
        return;
    }
    let c = Case::new("l7b");
    c.branch(310);
    c.test_and_fix(&[]);
    // The base takes an allowlist change (landed with the person's word elsewhere).
    c.git(&["checkout", "-q", "main"]);
    c.commit("landed elsewhere", &[(ALLOW, Some(ALLOW_305))]);
    c.git(&["checkout", "-q", "pr310"]);
    c.git(&["merge", "-q", "--no-edit", "main"]);
    let head = c.landable(310);
    assert_allowed(&c.gate_at(310, &head));
    let row = c.last_gate();
    assert!(row.get("language_check").is_none(), "{row}");
}

// ---- 8: a range task: the stop asks the gate's question, and its yes lets it land ---------------

#[test]
fn a_range_tasks_refusal_is_a_stop_asking_the_question_and_the_persons_yes_lets_it_land() {
    if !in_mode(
        "acceptance",
        "a_range_tasks_refusal_is_a_stop_asking_the_question_and_the_persons_yes_lets_it_land",
    ) {
        return;
    }
    let c = Case::new("l8");
    let (head, q) = the_305_case(&c);
    c.in_range("t305", "acceptance");

    let v = c.gate_at(305, &head);
    assert_refused(&v, &q);
    let stops = c.stops();
    let stop = stops
        .iter()
        .find(|(_, _, question, _)| *question == q)
        .unwrap_or_else(|| panic!("no stop asks {q:?}: {stops:?}"));
    assert_eq!(stop.1, "escalated", "{stops:?}");
    assert_eq!(
        stop.3["key"],
        json!(format!("language:{}#305@{head}", c.repo)),
        "{stops:?}"
    );
    assert_eq!(v["pending_id"], json!(stop.0), "{v}");

    c.answer(&c.person, stop.0, "yes");
    assert_allowed(&c.gate_at(305, &head));
    let v = c.merge_at(305, &head);
    assert_eq!(v["status"], json!("accepted"), "{v}");
}
