//! M5-3: `makibad` owns the PR watches (spec section 5.6, incidents I-6 and I-8).
//!
//! The daemon runs in this process on the stand-in base, and `makiba pr` is the door's verb
//! called through `Door::handle` with the secretary's key. `gh` is a stand-in script
//! (`MAKIBA_GH`): it answers the three calls the watch makes from files in a folder named by the
//! repository, so no test reaches the network and no two tests share an answer.

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::daemon::{door::Door, door::issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const STAND_IN: &str = r#"#!/bin/sh
# gh api --paginate <endpoint> --jq <expr>: the answer is the file of that call, if any.
ep="$3"
case "$ep" in
  repos/*/commits/*/check-runs*) kind=check-runs ;;
  repos/*/commits/*/status*) kind=status ;;
  repos/*/actions/runs*) kind=runs ;;
  *) echo "unexpected call: $*" >&2; exit 2 ;;
esac
path=${ep#repos/}; owner=${path%%/*}; rest=${path#*/}; name=${rest%%/*}; rest=${rest#*/}
dir="$FAKE_GH_DIR/${owner}_${name}"
echo "$kind" >> "$dir/calls"
if [ -e "$dir/fail" ]; then echo "boom" >&2; exit 1; fi
case "$kind" in
  runs) oid=${ep#*head_sha=}; oid=${oid%%&*} ;;
  *) oid=${rest#commits/}; oid=${oid%%/*} ;;
esac
if [ -e "$dir/$oid.$kind" ]; then cat "$dir/$oid.$kind"; fi
exit 0
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The stand-in's folder. The environment is set once, before any daemon of this process starts.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path());
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "40");
            std::env::set_var("MAKIBA_PRWATCH_BUDGET_MS", "600");
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
        let file = self.gh.join(format!("{head}.{call}"));
        let tmp = self.gh.join("answer.tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(tmp, file).unwrap();
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.gh.join("calls"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    fn db(&self) -> Connection {
        Connection::open(self.dir.join("state.db")).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// `(head, status, conclusion)` of every watch, oldest first.
    fn rows(&self) -> Vec<(String, String, Option<String>)> {
        let db = self.db();
        let mut stmt = db
            .prepare("SELECT head_oid, status, conclusion FROM pr_watches ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
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

    /// Start `makibad` and wait until it is subscribed: it migrates the database as it opens it,
    /// which the door (opened after that in a real `makibad`) must not race.
    fn start(&self) -> Daemon {
        let before = self.events("dispatcher_started").len();
        let d = Daemon::start(&self.dir);
        wait("makibad to start", || {
            self.events("dispatcher_started").len() > before
        });
        d
    }

    fn status_of(&self, head: &str) -> String {
        self.rows()
            .into_iter()
            .find(|r| r.0 == head)
            .map(|r| r.1)
            .unwrap_or_default()
    }
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

fn run(name: &str, status: &str, conclusion: Value) -> Value {
    json!({"name": name, "status": status, "conclusion": conclusion})
}

fn green(name: &str) -> Value {
    run(name, "completed", json!("success"))
}

#[test]
fn pr_pushed_with_a_new_head_leaves_one_watching_row_for_the_new_head() {
    let org = Org::new("push");
    let first = org.pr("opened", 5, A);
    assert_eq!(first["status"], json!("watching"));
    let second = org.pr("pushed", 5, B);
    assert_eq!(second["status"], json!("watching"));
    assert_eq!(
        org.rows(),
        vec![
            (A.into(), "superseded".into(), None),
            (B.into(), "watching".into(), None)
        ]
    );
    assert_eq!(
        org.count("SELECT COUNT(*) FROM pr_watches WHERE status = 'watching'"),
        1
    );
    assert_eq!(
        org.events("pr_opened"),
        vec![json!({"repo": "o/push", "pr": 5, "head_oid": A})]
    );
    assert_eq!(
        org.events("fix_pushed"),
        vec![json!({"repo": "o/push", "pr": 5, "head_oid": B})]
    );
    assert_eq!(org.events("ci_watch_started").len(), 2);

    // Another PR's watch is not touched, and the old head reported again is the one watched.
    org.pr("opened", 6, A);
    org.pr("pushed", 5, A);
    assert_eq!(org.status_of(B), "superseded");
    assert_eq!(
        org.count("SELECT COUNT(*) FROM pr_watches WHERE status = 'watching'"),
        2
    );
    assert_eq!(org.count("SELECT COUNT(*) FROM pr_watches"), 3);
}

#[test]
fn a_watch_whose_gh_call_fails_for_good_ends_failed_with_an_event_and_a_notice() {
    let org = Org::new("fails");
    std::fs::write(org.gh.join("fail"), "").unwrap();
    let _d = org.start();
    org.pr("opened", 9, A);
    wait("the watch to end", || org.status_of(A) == "failed");

    let failed = org.events("ci_watch_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["head_oid"], json!(A));
    assert!(
        failed[0]["reason"]
            .as_str()
            .unwrap()
            .starts_with("gh_unreadable"),
        "{failed:?}"
    );
    assert_eq!(org.events("ci_completed").len(), 0);
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM messages m JOIN event_deliveries d ON d.source_event_id = m.event_id
             WHERE m.kind = 'notice' AND m.recipient = 'secretary' AND m.sender = 'makibad'
               AND m.body LIKE 'The CI watch of o/fails#9 %'
               AND m.run_id = (SELECT id FROM runs WHERE task_id = 't1')"
        ),
        1
    );
    // The row stays ended: nothing writes a second result.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(org.events("ci_watch_failed").len(), 1);
    // Reported again, the same head is watched again, and ends the same way.
    assert_eq!(org.pr("pushed", 9, A)["status"], json!("watching"));
    wait("the second end", || {
        org.events("ci_watch_failed").len() == 2
    });
}

#[test]
fn restarting_makibad_resumes_polling_a_watching_row() {
    let org = Org::new("restart");
    org.answer(A, "check-runs", &[run("build", "in_progress", Value::Null)]);
    let mut first = org.start();
    org.pr("opened", 3, A);
    wait("the first polls", || org.calls() >= 4);
    assert_eq!(org.status_of(A), "watching");
    first.stop();

    // No daemon: the row stays `watching`, nobody polls it, and CI finishes meanwhile.
    org.answer(A, "check-runs", &[green("build")]);
    let seen = org.calls();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(org.calls(), seen, "nothing polls while makibad is stopped");
    assert_eq!(org.status_of(A), "watching");

    let _second = org.start();
    wait("the resumed watch to conclude", || {
        org.status_of(A) == "completed"
    });
    assert_eq!(org.events("ci_completed").len(), 1);
}

#[test]
fn ci_completed_carries_the_head_it_was_observed_for() {
    let org = Org::new("head");
    // The old head's CI is red; only the new head is green, and only it is watched.
    org.answer(
        A,
        "check-runs",
        &[run("build", "completed", json!("failure"))],
    );
    org.answer(
        B,
        "check-runs",
        &[green("build"), run("docs", "completed", json!("skipped"))],
    );
    org.answer(
        B,
        "status",
        &[json!({"context": "ci/legacy", "state": "success"})],
    );
    org.pr("opened", 4, A);
    org.pr("pushed", 4, B);
    let _d = org.start();
    wait("the watch to conclude", || org.status_of(B) == "completed");

    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["head_oid"], json!(B));
    assert_eq!(done[0]["status"], json!("passed"));
    assert_eq!(done[0]["repo"], json!("o/head"));
    assert_eq!(done[0]["pr"], json!(4));
    assert_eq!(done[0]["fail_count"], json!(0));
    assert_eq!(done[0]["total_checks"], json!(3));
    assert_eq!(
        org.rows(),
        vec![
            (A.into(), "superseded".into(), None),
            (B.into(), "completed".into(), Some("passed".into()))
        ]
    );
    assert_eq!(org.events("ci_watch_failed").len(), 0);
}

#[test]
fn a_red_check_waits_for_its_running_sibling_and_a_commit_status_counts() {
    let org = Org::new("sibling");
    org.answer(
        A,
        "check-runs",
        &[
            run("lint", "completed", json!("failure")),
            run("test", "in_progress", Value::Null),
        ],
    );
    let _d = org.start();
    org.pr("opened", 1, A);
    wait("some polls", || org.calls() >= 6);
    assert_eq!(org.status_of(A), "watching", "a sibling is still running");

    org.answer(
        A,
        "check-runs",
        &[run("lint", "completed", json!("failure")), green("test")],
    );
    org.answer(
        A,
        "status",
        &[json!({"context": "legacy", "state": "error"})],
    );
    wait("the verdict", || org.status_of(A) == "completed");
    let done = org.events("ci_completed");
    assert_eq!(done[0]["status"], json!("failed"));
    assert_eq!(done[0]["fail_count"], json!(2));
    assert_eq!(done[0]["total_checks"], json!(3));
}

#[test]
fn a_green_head_waits_for_a_workflow_run_that_has_not_started_its_checks() {
    let org = Org::new("queued");
    org.answer(A, "check-runs", &[green("build")]);
    org.raw(A, "runs", "completed\nqueued\n");
    let _d = org.start();
    org.pr("opened", 2, A);
    wait("some polls", || org.calls() >= 9);
    assert_eq!(
        org.status_of(A),
        "watching",
        "a queued workflow holds the verdict"
    );

    org.raw(A, "runs", "completed\n");
    wait("the verdict", || org.status_of(A) == "completed");
    assert_eq!(org.events("ci_completed")[0]["status"], json!("passed"));
}

#[test]
fn a_head_with_no_checks_at_all_ends_failed_after_the_budget() {
    let org = Org::new("empty");
    let _d = org.start();
    org.pr("opened", 8, A);
    wait("the watch to end", || org.status_of(A) == "failed");
    let failed = org.events("ci_watch_failed");
    assert!(
        failed[0]["reason"]
            .as_str()
            .unwrap()
            .starts_with("no_checks"),
        "{failed:?}"
    );
}

#[test]
fn a_result_for_a_watch_that_was_superseded_is_not_recorded() {
    let org = Org::new("late");
    org.answer(A, "check-runs", &[run("build", "in_progress", Value::Null)]);
    org.answer(B, "check-runs", &[run("build", "in_progress", Value::Null)]);
    let _d = org.start();
    org.pr("opened", 7, A);
    wait("a poll of A", || org.calls() >= 4);
    org.pr("pushed", 7, B);
    org.answer(A, "check-runs", &[green("build")]);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(org.status_of(A), "superseded");
    assert_eq!(org.status_of(B), "watching");
    assert_eq!(org.events("ci_completed").len(), 0);
    org.answer(B, "check-runs", &[green("build")]);
    wait("B to conclude", || org.status_of(B) == "completed");
    assert_eq!(org.events("ci_completed")[0]["head_oid"], json!(B));
    assert_eq!(org.status_of(A), "superseded");
}

#[test]
fn the_verb_refuses_what_it_cannot_watch() {
    let org = Org::new("refuse");
    let ask = |args: Value| org.door.handle(Some(&org.key), "pr", args, None, false);
    let ok = json!({"state": "opened", "repo": "o/refuse", "pr": 1, "head": A, "task": "t1"});
    assert!(ask(ok.clone()).is_ok());
    for (field, bad) in [
        ("state", json!("merged")),
        ("repo", json!("no-slash")),
        ("pr", json!(0)),
        ("head", json!("abc123")),
        ("head", json!(A.to_uppercase())),
        ("task", json!("")),
    ] {
        let mut args = ok.clone();
        args[field] = bad;
        assert!(ask(args).is_err(), "{field}");
    }
    let mut unknown = ok.clone();
    unknown["task"] = json!("nope");
    assert!(ask(unknown).is_err());
    assert_eq!(org.rows().len(), 1);
}
