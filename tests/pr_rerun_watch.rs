//! Issue 375: a rerun of a failed CI job is watched again.
//!
//! After a head's CI is decided and notified, the secretary reruns the failed job and records it
//! with `pr` state `rerun`: the watch of the same head is taken up again, waits for the rerun
//! (the checks may still read the old failed attempt while the workflow run is queued), and
//! records a new `ci_completed` that the merge gate reads as the head's latest, with one notice
//! for each decision.
//!
//! The daemon runs in this process on the stand-in base, and `makiba pr` is the door's verb
//! called through `Door::handle` with the secretary's key. `gh` is a stand-in script
//! (`MAKIBA_GH`): it answers the three calls the watch makes from files in a folder named by the
//! repository; a file `fail` fails every call, a file `fail.<kind>` the calls of that kind.

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
if [ -e "$dir/fail.$kind" ]; then echo "boom" >&2; exit 1; fi
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
            // Long enough that a few unreadable polls in a row stay inside it.
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

    /// `pr <state>` as the secretary: the answer, or the refusal as text.
    fn try_pr(&self, state: &str, number: i64, head: &str) -> Result<Value, String> {
        self.door
            .handle(
                Some(&self.key),
                "pr",
                json!({"state": state, "repo": self.repo, "pr": number, "head": head, "task": "t1"}),
                None,
                false,
            )
            .map_err(|e| format!("{e:?}"))
    }

    fn pr(&self, state: &str, number: i64, head: &str) -> Value {
        self.try_pr(state, number, head)
            .unwrap_or_else(|e| panic!("pr {state}: {e}"))
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

/// The checks of the first attempt: one failed job.
fn red(org: &Org, head: &str) {
    org.answer(
        head,
        "check-runs",
        &[check("build", "failure"), check("lint", "success")],
    );
}

fn green(org: &Org, head: &str) {
    org.answer(
        head,
        "check-runs",
        &[check("build", "success"), check("lint", "success")],
    );
}

#[test]
fn a_failed_head_rerun_and_passed_ends_with_a_passed_ci_completed_the_gate_reads() {
    let org = Org::new("rerun");
    let failed_like = "CI failed for o/rerun#5 %";
    let passed_like = "CI passed for o/rerun#5 %";

    // 1-2. The first run fails: one ci_completed, one notice.
    red(&org, A);
    org.raw(A, "runs", "completed\n");
    let _d = org.start();
    assert_eq!(org.pr("opened", 5, A)["status"], json!("watching"));
    wait("the first decision", || org.row(A).0 == "completed");
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["status"], json!("failed"));
    assert_eq!(done[0]["head_oid"], json!(A));
    assert_eq!(org.notices(failed_like), 1);

    // 3. Recording the same head again as pushed does not watch it again.
    assert_eq!(org.pr("pushed", 5, A)["status"], json!("completed"));

    // 4. The failed job is rerun: the run is queued, the checks still read the old failure.
    org.raw(A, "runs", "queued\n");
    assert_eq!(org.pr("rerun", 5, A)["status"], json!("watching"));
    assert_eq!(org.events("ci_rerun").len(), 1);
    assert_eq!(org.events("ci_watch_started").len(), 2);

    // 5. The stale failure is not concluded while the run is going.
    org.polls(9);
    assert_eq!(org.events("ci_completed").len(), 1);
    assert_eq!(org.row(A).0, "watching");

    // 6. Nor while the workflow runs cannot be read.
    std::fs::write(org.gh.join("fail.runs"), "").unwrap();
    org.polls(9);
    assert_eq!(org.events("ci_completed").len(), 1);
    assert_eq!(org.row(A).0, "watching");

    // 7. The rerun passes.
    green(&org, A);
    org.raw(A, "runs", "completed\n");
    std::fs::remove_file(org.gh.join("fail.runs")).unwrap();
    wait("the rerun's decision", || org.row(A).0 == "completed");
    assert_eq!(org.row(A).1.as_deref(), Some("passed"));

    // 8. A second ci_completed, passed, is what the gate reads; one notice for each decision.
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 2, "{done:?}");
    assert_eq!(done[1]["status"], json!("passed"));
    assert_eq!(done[1]["head_oid"], json!(A));
    assert_eq!(
        org.latest_ci(5),
        Some(("passed".to_string(), A.to_string()))
    );
    assert_eq!(org.notices(failed_like), 1);
    assert_eq!(org.notices(passed_like), 1);
    assert_eq!(org.notices("CI %"), 2);

    // 9. A passed head has nothing to rerun, and nothing more is written.
    assert!(org.try_pr("rerun", 5, A).is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(org.events("ci_completed").len(), 2);
    assert_eq!(org.notices("CI %"), 2);
    assert_eq!(org.events("ci_rerun").len(), 1);

    // 10. A head never recorded, and a head that is not the pull request's current one, are
    // refused.
    assert!(org.try_pr("rerun", 5, B).is_err());
    org.pr("pushed", 5, B);
    assert!(org.try_pr("rerun", 5, A).is_err());
    assert_eq!(org.events("ci_rerun").len(), 1);
}

#[test]
fn a_rerun_of_a_watch_that_ended_unreadable_waits_for_the_queued_run() {
    let org = Org::new("unreadable");
    std::fs::write(org.gh.join("fail"), "").unwrap();
    let mut first = org.start();
    org.pr("opened", 8, A);
    wait("the watch to end unreadable", || org.row(A).0 == "failed");
    assert_eq!(org.events("ci_watch_failed").len(), 1);
    assert_eq!(org.events("ci_completed").len(), 0);

    // The failed job is rerun: the checks read the old failure, the run is queued.
    std::fs::remove_file(org.gh.join("fail")).unwrap();
    red(&org, A);
    org.raw(A, "runs", "queued\n");
    assert_eq!(org.pr("rerun", 8, A)["status"], json!("watching"));

    // makibad restarts: the watch is still the rerun's and waits for the run.
    first.stop();
    let _second = org.start();
    org.polls(9);
    assert_eq!(org.events("ci_completed").len(), 0);
    assert_eq!(org.row(A).0, "watching");

    green(&org, A);
    org.raw(A, "runs", "completed\n");
    wait("the rerun's decision", || org.row(A).0 == "completed");
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["status"], json!("passed"));
    assert_eq!(done[0]["head_oid"], json!(A));
    assert_eq!(
        org.latest_ci(8),
        Some(("passed".to_string(), A.to_string()))
    );
}
