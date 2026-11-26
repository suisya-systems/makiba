//! Issue 512: a read of GitHub that failed in the merge gate is not a refusal on the pull request.
//!
//! - A failed read (gh's non-zero exit) is tried again a bounded number of times inside the merge
//!   call; a read that answers on a later try lets the merge be accepted, with no stop.
//! - A read that still fails answers its own kind, `unavailable`, with gh's message, and for a
//!   range task is one stop of line `gate` saying GitHub could not be read, not "Should it land as
//!   it is?". An answer that allows, later, withdraws it.
//! - The pause of a range after two refusals in a row does not count failed reads.
//!
//! The world is that of `tests/merge_approve.rs` (a local origin publishing `refs/pull/<n>/head`,
//! the project's clone in the repositories folder, the door in this process, a stand-in `gh`
//! answering from the case's folder), copied here, under `merge_approval_mode` `acceptance` with
//! the tasks in a range approved under `acceptance`, so that an allowed merge is attempted at once.
//! The stand-in has two switches more: `fail.view` and `fail.check-runs` hold the number of reads
//! of that kind still to fail, or `always`; a failing read says "HTTP 502: Bad Gateway" and exits 1.

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::{DoorError, Role};
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
const TRAILER: &str = "\n\nAcceptance-Test: tests/test_calc.py";

/// The stand-in for `gh`. The case's folder is named by the repository in the arguments
/// (`--repo o/<name>` or `repos/o/<name>/...`). Every call is logged in `calls`, one line each.
/// `pr view <n>` prints `view.<n>`; the check runs and workflow runs of a commit print
/// `<sha>.check-runs` / `<sha>.runs`. `fail.view` and `fail.check-runs` make reads of that kind
/// fail: a count, decremented by each failing read, or `always`.
const STAND_IN: &str = r#"#!/bin/sh
name=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--repo" ]; then name=${a#*/}; fi
  case "$a" in
    repos/*) r=${a#repos/}; r=${r#*/}; name=${r%%/*} ;;
  esac
  prev="$a"
done
dir="$FAKE_GH_DIR/$name"
echo "$*" >> "$dir/calls"
fails() {
  f="$dir/fail.$1"
  [ -e "$f" ] || return 1
  n=$(cat "$f")
  if [ "$n" = "always" ]; then return 0; fi
  if [ "$n" -gt 0 ]; then echo $((n - 1)) > "$f"; return 0; fi
  return 1
}
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  if fails view; then echo "HTTP 502: Bad Gateway" >&2; exit 1; fi
  cat "$dir/view.$3"; exit 0
fi
case "$*" in
  *check-runs*) oid=${3#*/commits/}; oid=${oid%%/*}
    if fails check-runs; then echo "HTTP 502: Bad Gateway" >&2; exit 1; fi
    if [ -e "$dir/$oid.check-runs" ]; then cat "$dir/$oid.check-runs"; fi; exit 0 ;;
  *actions/runs*) oid=${3#*head_sha=}; oid=${oid%%&*}
    if [ -e "$dir/$oid.runs" ]; then cat "$dir/$oid.runs"; fi; exit 0 ;;
esac
echo "unexpected call: $*" >&2; exit 2
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
            std::env::set_var("MAKIBA_MERGE_APPROVAL_MODE", "acceptance");
            std::env::set_var("MAKIBA_GATE_READ_WAIT_MS", "10");
        }
    });
    HOME.get().unwrap().path().to_path_buf()
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

/// One repository's world: an origin, the project's clone, `state.db`, the door, and a range.
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
    /// The head of PR 7 (task `t1`), judged.
    head: String,
    /// The range `t1` belongs to.
    range: i64,
}

impl Case {
    /// PR 7 of task `t1` at head H: the test commit failed CI, H passed it, the worker's last diff
    /// review passed at H, and `t1` is a task of a range approved under `acceptance`, so the gate
    /// allows H and the merge is attempted at once.
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
            head: String::new(),
            range: 0,
        };
        c.base = c.commit(
            "base",
            &[
                ("calc.py", Some(BUGGY)),
                ("pyproject.toml", Some("[project]\nname = 'x'\n")),
                (".github/workflows/ci.yml", Some("on: push\n")),
            ],
        );
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
        c.commit(
            &format!("test: failing test{TRAILER}"),
            &[("tests/test_calc.py", Some(TEST))],
        );
        c.commit("fix", &[("calc.py", Some(FIXED))]);
        c.head = c.ci_all(7);
        c.tie("t1", 7, &c.head);
        c.reviewed("t1", &c.head);
        c.range = c.in_range();
        c
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
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
        self.git(&["rev-parse", "HEAD"])
    }

    /// Publish `head` as PR `pr`'s head and let the stand-in answer `gh pr view` for it.
    fn publish_at(&self, pr: i64, head: &str) {
        self.git(&["update-ref", &format!("refs/pull/{pr}/head"), head]);
        let view = json!({"number": pr, "url": format!("file://{}/pull/{pr}", self.origin.display()),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(self.gh.join(format!("view.{pr}")), view.to_string()).unwrap();
    }

    fn answer(&self, sha: &str, kind: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.gh.join(format!("{sha}.{kind}")), text).unwrap();
    }

    /// A `ci_completed` row.
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

    /// Publish HEAD as PR `pr`: every commit of the PR but its head failed the job `test`, the
    /// head passed it, all on base C. Its head.
    fn ci_all(&self, pr: i64) -> String {
        let head = self.git(&["rev-parse", "HEAD"]);
        self.publish_at(pr, &head);
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
                self.ci(pr, "failed", sha);
            }
        }
        self.ci(pr, "passed", &head);
        head
    }

    fn run_of(&self, task: &str) -> i64 {
        self.conn()
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// The code task `task` is launched, and a watch ties it to PR `pr` at `head`.
    fn tie(&self, task: &str, pr: i64, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        let run = self.run_of(task);
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
    }

    /// The worker of `task` ran a diff review of `head` that passed with no finding.
    fn reviewed(&self, task: &str, head: &str) {
        let answer = json!({"verdict": "passed", "kind": "diff", "reader": "codex", "round": 1,
                            "allowed": 3, "head": head,
                            "findings": {"blocker": 0, "major": 0, "minor": 0}});
        self.conn()
            .execute(
                "INSERT INTO commands (actor, verb, status, result_json) VALUES (?1, 'review', 'done', ?2)",
                rusqlite::params![
                    makiba::spec::worker_agent_id(task),
                    json!({"result": answer}).to_string()
                ],
            )
            .unwrap();
    }

    /// Make `t1` a task of an active range of this project approved under `acceptance`.
    fn in_range(&self) -> i64 {
        let w = StateWriter::open(&self.db).unwrap();
        w.conn()
            .execute(
                "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                     proposed_by, status, approval_mode, approved_by, approved_at)
                 VALUES ((SELECT id FROM projects WHERE slug = ?1), '[1, 2]', 2, 5, 'h',
                         'secretary', 'active', 'acceptance', 'person',
                         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [self.name.as_str()],
            )
            .unwrap();
        let id = w.conn().last_insert_rowid();
        w.conn()
            .execute("UPDATE runs SET range_id = ?1 WHERE task_id = 't1'", [id])
            .unwrap();
        id
    }

    /// A second task `t2` of the same range, with PR 8 at the head of PR 7 (its own CI rows and
    /// watch), and no diff review: the gate refuses it on its content once it can read GitHub.
    fn second_task_without_review(&self) {
        let w = StateWriter::open(&self.db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t2",
            project_slug: &self.name,
            title: Some("T2"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET range_id = ?1 WHERE task_id = 't2'",
                [self.range],
            )
            .unwrap();
        drop(w);
        self.publish_at(8, &self.head);
        let range = format!("{}..{}", self.base, self.head);
        for sha in self.git(&["rev-list", "--reverse", &range]).lines() {
            if sha != self.head {
                self.ci(8, "failed", sha);
            }
        }
        self.ci(8, "passed", &self.head);
        self.tie("t2", 8, &self.head);
    }

    /// Make the reads of `kind` (`view` or `check-runs`) fail: `n` more times, or `always`.
    fn fail(&self, kind: &str, n: &str) {
        std::fs::write(self.gh.join(format!("fail.{kind}")), n).unwrap();
    }

    fn heal(&self, kind: &str) {
        let _ = std::fs::remove_file(self.gh.join(format!("fail.{kind}")));
    }

    /// The secretary's `merge` request for PR `pr` at `head`.
    fn request(&self, pr: i64, head: &str) -> Result<Value, DoorError> {
        self.door.handle(
            Some(&self.secretary),
            "merge",
            json!({"action": "request", "repo": self.repo, "pr": pr, "head": head}),
            None,
            true,
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.gh.join("calls")).unwrap_or_default()
    }

    /// The number of `gh pr view <pr>` calls.
    fn views_of(&self, pr: i64) -> usize {
        let start = format!("pr view {pr} ");
        self.calls()
            .lines()
            .filter(|l| l.starts_with(&start))
            .count()
    }

    fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    /// The payload of every event of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let db = self.conn();
        let mut st = db
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        st.query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
            .collect()
    }

    /// The statuses of the `merge` commands, oldest first.
    fn merge_commands(&self) -> Vec<String> {
        let db = self.conn();
        let mut st = db
            .prepare("SELECT status FROM commands WHERE verb = 'merge' ORDER BY id")
            .unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The pending entries makibad wrote: (id, stop line, status, question).
    fn stops(&self) -> Vec<(i64, String, String, String)> {
        let db = self.conn();
        let mut st = db
            .prepare(
                "SELECT id, json_extract(context_json, '$.stop_line'), status, question
                 FROM pending WHERE json_extract(context_json, '$.by') = 'makibad' ORDER BY id",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn range_status(&self) -> String {
        self.conn()
            .query_row(
                "SELECT status FROM ranges WHERE id = ?1",
                [self.range],
                |r| r.get(0),
            )
            .unwrap()
    }
}

fn answered(r: Result<Value, DoorError>) -> Value {
    r.unwrap_or_else(|e| panic!("merge: {e:?}"))
}

/// The merge was accepted with one attempt, one passing verdict, and no stop.
fn accepted_with_no_stop(c: &Case, v: &Value) {
    assert_eq!(v["status"], json!("accepted"), "{v}\ncalls:\n{}", c.calls());
    assert_eq!(c.events("merge_attempted").len(), 1);
    let verdicts = c.events("acceptance_gate");
    assert_eq!(verdicts.len(), 1, "{verdicts:?}");
    assert_eq!(verdicts[0]["result"], json!("passed"), "{}", verdicts[0]);
    assert_eq!(verdicts[0]["allow"], json!(true), "{}", verdicts[0]);
    assert!(c.stops().is_empty(), "{:?}", c.stops());
}

#[test]
fn a_pr_view_that_fails_once_is_read_again_and_the_merge_is_accepted_with_no_stop() {
    let c = Case::new("viewonce");
    c.fail("view", "1");
    let v = answered(c.request(7, &c.head));
    accepted_with_no_stop(&c, &v);
    assert_eq!(c.views_of(7), 2, "{}", c.calls());
}

#[test]
fn a_check_runs_read_that_fails_once_is_read_again_and_the_merge_is_accepted_with_no_stop() {
    let c = Case::new("runsonce");
    c.fail("check-runs", "1");
    let v = answered(c.request(7, &c.head));
    accepted_with_no_stop(&c, &v);
}

#[test]
fn a_read_that_always_fails_is_one_stop_that_names_the_failed_read() {
    let c = Case::new("viewalways");
    c.fail("view", "always");
    let v = answered(c.request(7, &c.head));
    assert_eq!(v["status"], json!("unavailable"), "{v}");
    assert_eq!(v["verdict"], json!("unavailable"), "{v}");
    assert_eq!(v["allow"], json!(false), "{v}");
    assert!(
        v["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("HTTP 502"),
        "{v}"
    );
    assert!(c.events("merge_attempted").is_empty());
    // Bounded: three tries in all.
    assert_eq!(c.views_of(7), 3, "{}", c.calls());
    let verdicts = c.events("acceptance_gate");
    assert_eq!(verdicts.len(), 1, "{verdicts:?}");
    assert_eq!(
        verdicts[0]["result"],
        json!("unavailable"),
        "{}",
        verdicts[0]
    );
    assert_eq!(verdicts[0]["allow"], json!(false), "{}", verdicts[0]);
    let stops = c.stops();
    assert_eq!(stops.len(), 1, "{stops:?}");
    let (id, line, status, question) = &stops[0];
    assert_eq!((line.as_str(), status.as_str()), ("gate", "escalated"));
    assert!(question.contains("could not read GitHub"), "{question}");
    assert!(question.contains("HTTP 502"), "{question}");
    assert!(!question.contains("Should it land as it is?"), "{question}");
    assert_eq!(v["pending_id"], json!(id), "{v}");
    assert_eq!(c.merge_commands(), ["failed"]);

    // Asked again at the same head: the same one stop.
    let again = answered(c.request(7, &c.head));
    assert_eq!(again["status"], json!("unavailable"), "{again}");
    assert_eq!(c.stops().len(), 1, "{:?}", c.stops());

    // GitHub answers again: the merge is accepted and the stop is withdrawn.
    c.heal("view");
    let v = answered(c.request(7, &c.head));
    assert_eq!(v["status"], json!("accepted"), "{v}");
    assert_eq!(c.events("merge_attempted").len(), 1);
    let stops = c.stops();
    assert_eq!(stops.len(), 1, "{stops:?}");
    assert_eq!(stops[0].2, "withdrawn", "{stops:?}");
}

#[test]
fn failed_reads_do_not_count_for_the_pause_of_the_range() {
    let c = Case::new("nopause");
    c.second_task_without_review();
    c.fail("view", "always");
    let v7 = answered(c.request(7, &c.head));
    assert_eq!(v7["status"], json!("unavailable"), "{v7}");
    let v8 = answered(c.request(8, &c.head));
    assert_eq!(v8["status"], json!("unavailable"), "{v8}");
    assert_eq!(c.range_status(), "active");

    // A refusal on the content after the failed reads is one refusal, not the second in a row.
    c.heal("view");
    let v8 = answered(c.request(8, &c.head));
    assert_eq!(v8["allow"], json!(false), "{v8}");
    assert_eq!(v8["verdict"], json!("review_missing"), "{v8}");
    assert_eq!(c.range_status(), "active");
    assert!(
        c.events("range_paused").is_empty(),
        "{:?}",
        c.events("range_paused")
    );
}
