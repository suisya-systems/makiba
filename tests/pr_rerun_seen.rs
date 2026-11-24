//! Issue 427: the PR watch sees a rerun the secretary did not record.
//!
//! When the newest `ci_completed` of a pull request's current head is `failed` and GitHub shows a
//! newer attempt of a workflow run at that head, `makibad` takes the watch of the head up again as
//! `pr rerun` does (`ci_rerun` naming the run and the attempt) and records the attempt's result,
//! with no `pr rerun`. An attempt concluded on is never taken up twice; one taken up whose watch
//! ended without a result is taken up again; a head no longer current and a closed pull request
//! are left alone.
//!
//! The daemon runs in this process on the stand-in base, and `makiba pr` is the door's verb called
//! through `Door::handle` with the secretary's key. `gh` is a stand-in script (`MAKIBA_GH`): it
//! answers the calls the watch makes from files in a folder named by the repository. A workflow
//! runs call whose `--jq` reads `run_attempt` is an attempts call, answered from `<oid>.attempts`;
//! any other runs call from `<oid>.runs`. A file `fail.<kind>` fails the calls of that kind. On an
//! attempts call, a folder `flip` has its files moved over the answers after the answer is
//! printed.

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::daemon::{door::Door, door::issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const STAND_IN: &str = r#"#!/bin/sh
# gh api --paginate <endpoint> --jq <expr>: the answer is the file of that call, if any.
ep="$3"
jq="$5"
case "$ep" in
  repos/*/commits/*/check-runs*) kind=check-runs ;;
  repos/*/commits/*/status*) kind=status ;;
  repos/*/actions/runs*)
    case "$jq" in
      *run_attempt*) kind=attempts ;;
      *) kind=runs ;;
    esac ;;
  *) echo "unexpected call: $*" >&2; exit 2 ;;
esac
path=${ep#repos/}; owner=${path%%/*}; rest=${path#*/}; name=${rest%%/*}; rest=${rest#*/}
dir="$FAKE_GH_DIR/${owner}_${name}"
echo "$kind" >> "$dir/calls"
if [ -e "$dir/fail.$kind" ]; then echo "boom" >&2; exit 1; fi
case "$kind" in
  runs|attempts) oid=${ep#*head_sha=}; oid=${oid%%&*} ;;
  *) oid=${rest#commits/}; oid=${oid%%/*} ;;
esac
if [ -e "$dir/$oid.$kind" ]; then cat "$dir/$oid.$kind"; fi
if [ "$kind" = attempts ] && [ -d "$dir/flip" ]; then
  for f in "$dir"/flip/*; do mv "$f" "$dir/"; done
  rmdir "$dir/flip"
fi
exit 0
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The stand-in's folder. The environment is set once, before any daemon of this process starts.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        common::exe::write(&script, STAND_IN);
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path());
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "40");
            std::env::set_var("MAKIBA_PRWATCH_BUDGET_MS", "3000");
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// One organization's folder with `state.db`, a run `t1` and the secretary's key.
struct Org {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    door: Door,
    key: String,
    /// What the stand-in answers for this repository.
    gh: PathBuf,
    repo: String,
}

impl Org {
    /// `repo` is `o/<name>`; the stand-in's folder for it is made empty.
    fn new(name: &str) -> Org {
        let home = init();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        let key = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        let gh = home.join(format!("o_{name}"));
        std::fs::create_dir_all(&gh).unwrap();
        Org {
            _tmp: tmp,
            dir,
            door,
            key,
            gh,
            repo: format!("o/{name}"),
        }
    }

    fn pr(&self, state: &str, number: i64, head: &str) -> Value {
        self.door
            .handle(
                Some(&self.key),
                "pr",
                json!({"state": state, "repo": self.repo, "pr": number, "head": head, "task": "t1"}),
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("pr {state}: {e:?}"))
    }

    /// The answer of one call of the stand-in for `head`: the lines `gh --jq` would print.
    fn answer(&self, head: &str, call: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        self.raw(head, call, &text);
    }

    /// The same, as the text itself (`--jq` prints a string without quotes).
    fn raw(&self, head: &str, call: &str, text: &str) {
        write_atomic(&self.gh, &format!("{head}.{call}"), text);
    }

    fn fail(&self, kind: &str) {
        std::fs::write(self.gh.join(format!("fail.{kind}")), "").unwrap();
    }

    fn unfail(&self, kind: &str) {
        std::fs::remove_file(self.gh.join(format!("fail.{kind}"))).unwrap();
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.gh.join("calls"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    /// Wait until the stand-in has answered `n` more calls than now.
    fn polls(&self, n: usize) {
        let seen = self.calls();
        wait("more polls", || self.calls() >= seen + n);
    }

    fn db(&self) -> Connection {
        Connection::open(self.dir.join("state.db")).unwrap()
    }

    /// `(status, conclusion)` of the watch of `head`.
    fn row(&self, head: &str) -> (String, Option<String>) {
        self.db()
            .query_row(
                "SELECT status, conclusion FROM pr_watches WHERE head_oid = ?1",
                [head],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_default()
    }

    fn row_id(&self, head: &str) -> i64 {
        self.db()
            .query_row(
                "SELECT id FROM pr_watches WHERE head_oid = ?1",
                [head],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The payloads of the events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let db = self.db();
        let mut stmt = db
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        stmt.query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
            .collect()
    }

    /// The `ci_completed` payloads of `head`, oldest first.
    fn ci_of(&self, head: &str) -> Vec<Value> {
        self.events("ci_completed")
            .into_iter()
            .filter(|v| v["head_oid"] == json!(head))
            .collect()
    }

    /// The merge gate's read: `(status, head_oid)` of the newest `ci_completed` of `repo#pr` by
    /// event id, as `door.rs` `ci_rows(..).last()`.
    fn latest_ci(&self, number: i64) -> Option<(String, String)> {
        self.events("ci_completed")
            .into_iter()
            .filter(|v| {
                v["pr"].as_i64() == Some(number)
                    && v["repo"]
                        .as_str()
                        .is_some_and(|r| r.eq_ignore_ascii_case(&self.repo))
            })
            .last()
            .map(|v| {
                (
                    v["status"].as_str().unwrap_or_default().to_string(),
                    v["head_oid"].as_str().unwrap_or_default().to_string(),
                )
            })
    }

    /// The notices `makibad` left the secretary whose body is like `like`.
    fn notices(&self, like: &str) -> i64 {
        self.db()
            .query_row(
                "SELECT COUNT(*) FROM messages m JOIN event_deliveries d ON d.source_event_id = m.event_id
                 WHERE m.kind = 'notice' AND m.recipient = 'secretary' AND m.sender = 'makibad'
                   AND m.body LIKE ?1",
                [like],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// `pr_closed` of the run's pull request, as the state read writes it.
    fn closed(&self, number: i64) {
        let w = StateWriter::open(&self.dir.join("state.db")).unwrap();
        w.append_event(&NewEvent {
            kind: "pr_closed",
            actor: Some("makibad"),
            payload: Some(&json!({"repo": self.repo, "pr": number, "title": "T", "task_id": "t1"})),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap();
    }

    /// Start `makibad` and wait until it is subscribed: it migrates the database as it opens it,
    /// which the door must not race.
    fn start(&self) -> Daemon {
        let before = self.events("dispatcher_started").len();
        let d = Daemon::start(&self.dir);
        wait("makibad to start", || {
            self.events("dispatcher_started").len() > before
        });
        d
    }
}

fn write_atomic(dir: &Path, name: &str, text: &str) {
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(tmp, dir.join(name)).unwrap();
}

/// `makibad` in this process, stopped when dropped.
struct Daemon {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl Daemon {
    fn start(dir: &Path) -> Daemon {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (dir, stop) = (dir.to_path_buf(), Arc::clone(&stop));
            std::thread::spawn(move || {
                makibad::run_until(&dir, &|| Box::new(Stub::new()) as Box<dyn Base>, stop)
            })
        };
        Daemon {
            stop,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            t.join().unwrap().unwrap();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn check(name: &str, conclusion: &str) -> Value {
    json!({"name": name, "status": "completed", "conclusion": conclusion})
}

fn red_lines() -> Vec<Value> {
    vec![check("build", "failure"), check("lint", "success")]
}

fn green_lines() -> Vec<Value> {
    vec![check("build", "success"), check("lint", "success")]
}

fn red(org: &Org, head: &str) {
    org.answer(head, "check-runs", &red_lines());
}

fn green(org: &Org, head: &str) {
    org.answer(head, "check-runs", &green_lines());
}

/// Workflow run 11 at `attempt`, completed.
fn a11(attempt: i64) -> Value {
    json!({"id": 11, "run_attempt": attempt, "status": "completed"})
}

fn attempts(org: &Org, head: &str, attempt: i64) {
    org.answer(head, "attempts", &[a11(attempt)]);
}

/// The first attempt at `head` fails and is recorded: red checks, run 11 attempt 1.
fn first_failure(org: &Org, number: i64, head: &str) {
    red(org, head);
    org.raw(head, "runs", "completed\n");
    attempts(org, head, 1);
    assert_eq!(org.pr("opened", number, head)["status"], json!("watching"));
    wait("the first decision", || org.row(head).0 == "completed");
}

#[test]
fn a_newer_attempt_of_a_failed_head_is_recorded_passed_without_pr_rerun() {
    let org = Org::new("seen");
    let _d = org.start();
    first_failure(&org, 5, A);
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["status"], json!("failed"));
    assert_eq!(done[0]["head_oid"], json!(A));
    assert_eq!(done[0]["run_attempts"], json!({"11": 1}));
    assert_eq!(org.notices("CI failed for o/seen#5 %"), 1);

    // Nothing newer: nothing is taken up.
    org.polls(9);
    assert_eq!(org.events("ci_rerun").len(), 0);
    assert_eq!(org.events("ci_completed").len(), 1);
    assert_eq!(org.row(A), ("completed".into(), Some("failed".into())));

    // Attempt 2 of run 11 passed; nobody records it.
    green(&org, A);
    attempts(&org, A, 2);
    wait("the newer attempt's result", || {
        org.row(A) == ("completed".into(), Some("passed".into()))
    });
    let rerun = org.events("ci_rerun");
    assert_eq!(rerun.len(), 1, "{rerun:?}");
    assert_eq!(rerun[0]["watch_id"], json!(org.row_id(A)));
    assert_eq!(rerun[0]["head_oid"], json!(A));
    assert_eq!(rerun[0]["run_id"], json!(11));
    assert_eq!(rerun[0]["run_attempt"], json!(2));
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 2, "{done:?}");
    assert_eq!(done[1]["status"], json!("passed"));
    assert_eq!(done[1]["head_oid"], json!(A));
    assert!(done[1].get("run_attempts").is_none(), "{done:?}");
    assert_eq!(
        org.latest_ci(5),
        Some(("passed".to_string(), A.to_string()))
    );
    assert_eq!(org.notices("CI failed for o/seen#5 %"), 1);
    assert_eq!(org.notices("CI passed for o/seen#5 %"), 1);
    assert_eq!(org.notices("CI %"), 2);

    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(org.events("ci_rerun").len(), 1);
    assert_eq!(org.events("ci_completed").len(), 2);
}

#[test]
fn a_newer_attempt_that_fails_again_is_taken_up_once() {
    let org = Org::new("again");
    let _d = org.start();
    first_failure(&org, 5, A);

    attempts(&org, A, 2);
    wait("the newer attempt's result", || {
        org.events("ci_completed").len() == 2
    });
    let done = org.events("ci_completed");
    assert_eq!(done[1]["status"], json!("failed"));
    assert_eq!(done[1]["head_oid"], json!(A));
    assert_eq!(done[1]["run_attempts"], json!({"11": 2}));
    assert_eq!(org.events("ci_rerun").len(), 1);

    org.polls(9);
    assert_eq!(org.events("ci_rerun").len(), 1);
    assert_eq!(org.events("ci_completed").len(), 2);
    assert_eq!(org.row(A), ("completed".into(), Some("failed".into())));
    assert_eq!(org.notices("CI failed for o/again#5 %"), 2);
}

#[test]
fn a_rerun_unseen_while_makibad_was_down_is_recorded_after_its_restart() {
    let org = Org::new("down");
    org.fail("attempts");
    let mut first = org.start();
    first_failure(&org, 5, A);
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["status"], json!("failed"));
    assert!(done[0].get("run_attempts").is_none(), "{done:?}");

    // The rerun runs and passes while makibad is down.
    first.stop();
    attempts(&org, A, 2);
    green(&org, A);
    org.unfail("attempts");
    let _second = org.start();
    wait("the newer attempt's result", || {
        org.row(A) == ("completed".into(), Some("passed".into()))
    });
    let rerun = org.events("ci_rerun");
    assert_eq!(rerun.len(), 1, "{rerun:?}");
    assert_eq!(rerun[0]["run_attempt"], json!(2));
    assert_eq!(
        org.latest_ci(5),
        Some(("passed".to_string(), A.to_string()))
    );
}

#[test]
fn a_newer_attempt_of_a_head_no_longer_current_is_not_taken_up() {
    let org = Org::new("moved");
    let _d = org.start();
    first_failure(&org, 5, A);

    red(&org, B);
    org.raw(B, "runs", "completed\n");
    org.pr("pushed", 5, B);
    wait("the new head's decision", || org.row(B).0 == "completed");

    attempts(&org, A, 2);
    green(&org, A);
    org.polls(9);
    assert_eq!(org.events("ci_rerun").len(), 0);
    assert_eq!(org.row(A), ("completed".into(), Some("failed".into())));
    assert_eq!(org.ci_of(A).len(), 1);
}

#[test]
fn an_attempt_that_finishes_while_the_failure_is_read_is_not_lost() {
    let org = Org::new("race");
    // The first attempts call (after the failed reading) finds attempt 2 done and green.
    let flip = org.gh.join("flip");
    std::fs::create_dir_all(&flip).unwrap();
    let text = |lines: &[Value]| -> String { lines.iter().map(|l| format!("{l}\n")).collect() };
    write_atomic(&flip, &format!("{A}.check-runs"), &text(&green_lines()));
    write_atomic(&flip, &format!("{A}.attempts"), &text(&[a11(2)]));
    let _d = org.start();
    first_failure(&org, 5, A);

    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["status"], json!("passed"));
    assert_eq!(done[0]["head_oid"], json!(A));
    assert_eq!(org.events("ci_rerun").len(), 0);
}

#[test]
fn a_take_up_that_ends_unreadable_is_taken_up_again_after_a_restart() {
    let org = Org::new("unread");
    let mut first = org.start();
    first_failure(&org, 5, A);
    assert_eq!(
        org.events("ci_completed")[0]["run_attempts"],
        json!({"11": 1})
    );

    // Attempt 2 is seen, but its checks cannot be read for the budget.
    org.fail("check-runs");
    attempts(&org, A, 2);
    wait("the take-up", || org.events("ci_rerun").len() == 1);
    org.fail("attempts");
    wait("the watch to end without a result", || {
        org.events("ci_watch_failed").len() == 1
    });
    assert_eq!(org.row(A).0, "failed");
    assert_eq!(org.events("ci_rerun").len(), 1);

    first.stop();
    org.unfail("check-runs");
    org.unfail("attempts");
    green(&org, A);
    let _second = org.start();
    wait("the newer attempt's result", || {
        org.row(A) == ("completed".into(), Some("passed".into()))
    });
    let rerun = org.events("ci_rerun");
    assert_eq!(rerun.len(), 2, "{rerun:?}");
    assert!(
        rerun.iter().all(|r| r["run_attempt"] == json!(2)),
        "{rerun:?}"
    );
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 2, "{done:?}");
    assert_eq!(done[1]["status"], json!("passed"));
}

#[test]
fn a_closed_pull_requests_newer_attempt_is_not_taken_up() {
    let org = Org::new("closed");
    let _d = org.start();
    first_failure(&org, 5, A);

    org.closed(5);
    attempts(&org, A, 2);
    green(&org, A);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(org.events("ci_rerun").len(), 0);
    assert_eq!(org.events("ci_completed").len(), 1);
}
