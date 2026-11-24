//! Issue 327: a failed CI notice carries the failed step's log, so the secretary and the worker
//! read why the CI failed without a person in between.
//!
//! The daemon runs in this process on the stand-in base, and `makiba pr` is the door's verb called
//! through `Door::handle` with the secretary's key. `gh` is a stand-in script (`MAKIBA_GH`): it
//! answers the check runs, the statuses, the workflow runs, a job's steps and a job's log from
//! files in a folder named by the repository, and writes the kind of each call it answers to
//! `calls`, so no test reaches the network and no two tests share an answer.

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::daemon::{door::Door, door::issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{BODY_LIMIT, Role};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const STAND_IN: &str = r#"#!/bin/sh
# gh api [--paginate] <endpoint> [--jq <expr>]: the answer is the file of that call, if any.
ep=""
for a in "$@"; do
  case "$a" in repos/*) ep="$a"; break ;; esac
done
case "$ep" in
  repos/*/commits/*/check-runs*) kind=check-runs ;;
  repos/*/commits/*/status*) kind=status ;;
  repos/*/actions/runs*) kind=runs ;;
  repos/*/actions/jobs/*/logs) kind=logs ;;
  repos/*/actions/jobs/*) kind=job ;;
  *) echo "unexpected call: $*" >&2; exit 2 ;;
esac
path=${ep#repos/}; owner=${path%%/*}; rest=${path#*/}; name=${rest%%/*}; rest=${rest#*/}
dir="$FAKE_GH_DIR/${owner}_${name}"
echo "$kind" >> "$dir/calls"
if [ -e "$dir/$kind.fail" ]; then echo "boom" >&2; exit 1; fi
case "$kind" in
  job|logs) id=${ep#*/actions/jobs/}; id=${id%%/*}; file="$dir/$kind-$id" ;;
  runs) oid=${ep#*head_sha=}; oid=${oid%%&*}; file="$dir/$oid.$kind" ;;
  *) oid=${rest#commits/}; oid=${oid%%/*}; file="$dir/$oid.$kind" ;;
esac
if [ -e "$file" ]; then cat "$file"; fi
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

    /// The answer of a check-runs, status or runs call for `head`: the lines `gh --jq` prints.
    fn answer(&self, head: &str, call: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        self.file(&format!("{head}.{call}"), &text);
    }

    /// The steps of job `id`, as the `--jq` of the job call prints them.
    fn job(&self, id: i64, steps: Value) {
        self.file(
            &format!("job-{id}"),
            &format!("{}\n", json!({"steps": steps})),
        );
    }

    /// The log of job `id`.
    fn log(&self, id: i64, text: &str) {
        self.file(&format!("logs-{id}"), text);
    }

    /// Make every call of `kind` fail.
    fn fail(&self, kind: &str) {
        std::fs::write(self.gh.join(format!("{kind}.fail")), "").unwrap();
    }

    fn file(&self, name: &str, text: &str) {
        let tmp = self.gh.join("answer.tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(tmp, self.gh.join(name)).unwrap();
    }

    /// The calls of `kind` the stand-in answered.
    fn calls(&self, kind: &str) -> usize {
        std::fs::read_to_string(self.gh.join("calls"))
            .map(|t| t.lines().filter(|l| *l == kind).count())
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

    /// The bodies of the messages to `recipient` that are `LIKE` `like`, oldest first.
    fn bodies(&self, recipient: &str, like: &str) -> Vec<String> {
        let db = self.db();
        let mut stmt = db
            .prepare("SELECT body FROM messages WHERE recipient = ?1 AND body LIKE ?2 ORDER BY id")
            .unwrap();
        stmt.query_map([recipient, like], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
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

    /// The one notice to the secretary of a failed CI of pull request 5.
    fn failed_notice(&self) -> String {
        let like = format!("CI failed for {}#5 %", self.repo);
        let bodies = self.bodies("secretary", &like);
        assert_eq!(bodies.len(), 1, "{bodies:?}");
        bodies.into_iter().next().unwrap()
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

fn green(name: &str) -> Value {
    json!({"name": name, "status": "completed", "conclusion": "success"})
}

fn url(repo: &str, id: i64) -> String {
    format!("https://github.com/{repo}/actions/runs/9/job/{id}")
}

/// A failed check run of GitHub Actions: its id is the job's.
fn actions_failed(repo: &str, name: &str, id: i64) -> Value {
    json!({"id": id, "name": name, "status": "completed", "conclusion": "failure",
           "app": "github-actions", "url": url(repo, id)})
}

fn step(number: i64, name: &str, conclusion: &str, from: &str, to: &str) -> Value {
    json!({"name": name, "number": number, "conclusion": conclusion,
           "started_at": from, "completed_at": to})
}

/// A log line at `sec` seconds past 10:00 on 2026-11-23, the `n`th of that second.
fn line(sec: u32, n: u32, text: &str) -> String {
    format!("2026-11-23T10:00:{sec:02}.{n:07}Z {text}\n")
}

/// The job's steps of the fixture: a lint step allowed to fail (it reports success), a format
/// step, the failed test step (its start written with an offset: 19:00:30+09:00 is 10:00:30Z)
/// and the post step.
fn fixture_steps() -> Value {
    json!([
        step(
            1,
            "Set up job",
            "success",
            "2026-11-23T10:00:00Z",
            "2026-11-23T10:00:05Z"
        ),
        step(
            2,
            "Lint",
            "success",
            "2026-11-23T10:00:10Z",
            "2026-11-23T10:00:12Z"
        ),
        step(
            3,
            "Format",
            "success",
            "2026-11-23T10:00:20Z",
            "2026-11-23T10:00:22Z"
        ),
        step(
            4,
            "Run tests",
            "failure",
            "2026-11-23T19:00:30+09:00",
            "2026-11-23T10:00:40Z"
        ),
        step(
            5,
            "Post job cleanup.",
            "success",
            "2026-11-23T10:00:50Z",
            "2026-11-23T10:00:51Z"
        ),
    ])
}

/// The fixture log: timestamped lines, ANSI colour, a continued failure in an earlier step,
/// diagnostics after the first annotation of the failed step, and post steps after it.
fn fixture_log() -> String {
    let mut s = String::new();
    s += &line(1, 0, "Current runner version: '2.330.0'");
    s += &line(10, 0, "##[group]Run cargo clippy");
    s += &line(10, 1, "##[endgroup]");
    s += &line(11, 0, "flaky_lint FAILED (allowed)");
    s += &line(20, 0, "##[group]Run cargo fmt --check");
    s += &line(20, 1, "##[endgroup]");
    s += &line(21, 0, "Diff in src/lib.rs: none");
    s += &line(30, 0, "##[group]Run cargo test --locked");
    s += &line(30, 1, "\u{1b}[36;1mcargo test --locked\u{1b}[0m");
    s += &line(30, 2, "##[endgroup]");
    for i in 1..=200u32 {
        s += &line(31 + i / 40, i, &format!("test filler_{i} ... ok"));
    }
    s += &line(36, 0, "---- the_table_matches stdout ----");
    s += &line(
        36,
        1,
        "\u{1b}[31mthread 'the_table_matches' panicked at tests/spec_matches_code.rs:40:5:\u{1b}[0m",
    );
    s += &line(36, 2, "assertion `left == right` failed");
    s += &line(36, 3, "  left: 3");
    s += &line(36, 4, " right: 4");
    for i in 1..=50u32 {
        s += &line(37, i, &format!("test after_{i} ... ok"));
    }
    s += &line(
        38,
        0,
        "test result: \u{1b}[31mFAILED\u{1b}[0m. 1 passed; 1 failed",
    );
    s += &line(
        38,
        1,
        "##[error]test failed, to rerun pass `--test spec_matches_code`",
    );
    s += &line(39, 0, "error: 1 target failed:");
    s += &line(39, 1, "    `--test spec_matches_code`");
    s += &line(40, 0, "##[error]Process completed with exit code 101.");
    s += &line(50, 0, "##[group]Run actions/cache");
    s += &line(50, 1, "Post job cleanup.");
    s += &line(51, 0, "Cleaning up orphan processes");
    s
}

/// One failed Actions job `test` (501) and a passed `fmt` at A, with the fixture's steps and log,
/// recorded as pull request 5.
fn failed_test_job(org: &Org) {
    org.answer(
        A,
        "check-runs",
        &[actions_failed(&org.repo, "test", 501), green("fmt")],
    );
    org.job(501, fixture_steps());
    org.log(501, &fixture_log());
    org.pr("opened", 5, A);
}

fn the_notice_starts_as_a_failed_ci_notice(org: &Org, body: &str) {
    let head = format!(
        "CI failed for {}#5 at aaaaaaa: 1 of 2 checks failed.",
        org.repo
    );
    assert!(body.starts_with(&head), "{body}");
}

#[test]
fn a_failed_ci_notice_carries_the_failing_steps_log() {
    let org = Org::new("log");
    failed_test_job(&org);
    let _d = org.start();
    wait("the watch to conclude", || org.status_of(A) == "completed");

    assert_eq!(
        org.rows(),
        vec![(A.into(), "completed".into(), Some("failed".into()))]
    );
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["status"], json!("failed"));
    assert_eq!(done[0]["fail_count"], json!(1));
    assert_eq!(done[0]["total_checks"], json!(2));
    let mut keys: Vec<&str> = done[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "fail_count",
            "head_oid",
            "pending_count",
            "pr",
            "probe_attempts",
            "repo",
            "status",
            "total_checks"
        ]
    );

    let body = org.failed_notice();
    the_notice_starts_as_a_failed_ci_notice(&org, &body);
    for want in [
        "`test`".to_string(),
        url(&org.repo, 501),
        "step `Run tests`".into(),
        "---- the_table_matches stdout ----".into(),
        "panicked at tests/spec_matches_code.rs:40:5:".into(),
        "assertion `left == right` failed".into(),
        "test result: FAILED. 1 passed; 1 failed".into(),
        "##[error]test failed".into(),
        "error: 1 target failed:".into(),
        "##[error]Process completed with exit code 101.".into(),
    ] {
        assert!(body.contains(&want), "missing {want:?} in:\n{body}");
    }
    for not in [
        "2026-11-23T",
        "\u{1b}",
        "flaky_lint FAILED (allowed)",
        "Run cargo fmt",
        "Post job cleanup",
        "test filler_100 ... ok",
    ] {
        assert!(!body.contains(not), "{not:?} in:\n{body}");
    }
    assert!(
        body.chars().count() <= BODY_LIMIT,
        "{}",
        body.chars().count()
    );
    assert_eq!(org.calls("job"), 1);
    assert_eq!(org.calls("logs"), 1);
}

#[test]
fn a_parked_worker_gets_the_same_log() {
    let org = Org::new("parked");
    failed_test_job(&org);
    org.db()
        .execute_batch(
            "UPDATE runs SET status = 'parked' WHERE task_id = 't1';
             INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 's1', 'fake', 'b', '/w', 'verified', 'c1');
             INSERT INTO pending (run_id, kind, question, status)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'question', 'q', 'escalated');",
        )
        .unwrap();
    let _d = org.start();
    wait("the watch to conclude", || org.status_of(A) == "completed");

    let body = org.failed_notice();
    let like = format!("CI failed for {}#5 %", org.repo);
    let worker = org.bodies("w-t1", &like);
    assert_eq!(worker, vec![body], "the worker's notice is the secretary's");
    assert_eq!(
        org.count(
            "SELECT count(*) FROM events WHERE kind = 'run_unparked'
             AND json_extract(payload_json, '$.reason') = 'ci_failed'"
        ),
        1
    );
}

#[test]
fn an_unreadable_log_leaves_the_failed_notice_with_the_reason() {
    let org = Org::new("unread");
    org.fail("logs");
    failed_test_job(&org);
    let _d = org.start();
    wait("the watch to conclude", || org.status_of(A) == "completed");

    assert_eq!(
        org.rows(),
        vec![(A.into(), "completed".into(), Some("failed".into()))]
    );
    let done = org.events("ci_completed");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0]["status"], json!("failed"));
    assert_eq!(org.events("ci_watch_failed").len(), 0);
    let body = org.failed_notice();
    the_notice_starts_as_a_failed_ci_notice(&org, &body);
    assert!(body.contains("Failed job `test`"), "{body}");
    assert!(body.contains("its log could not be read"), "{body}");
}

#[test]
fn no_log_is_read_for_a_check_outside_actions_or_a_passed_head() {
    // (a) A failed check of another app: named, no log read.
    let ext = Org::new("ext");
    ext.answer(
        A,
        "check-runs",
        &[
            json!({"id": 7, "name": "ext", "status": "completed", "conclusion": "failure",
                   "app": "other-ci", "url": "https://ci.example.com/7"}),
            green("fmt"),
        ],
    );
    ext.pr("opened", 5, A);
    // (b) A passed Actions job: no log read.
    let pass = Org::new("pass");
    pass.answer(
        A,
        "check-runs",
        &[
            json!({"id": 501, "name": "test", "status": "completed", "conclusion": "success",
                 "app": "github-actions", "url": url("o/pass", 501)}),
        ],
    );
    pass.job(501, fixture_steps());
    pass.log(501, &fixture_log());
    pass.pr("opened", 5, A);

    let _a = ext.start();
    let _b = pass.start();
    wait("both watches to conclude", || {
        ext.status_of(A) == "completed" && pass.status_of(A) == "completed"
    });

    let body = ext.failed_notice();
    assert!(body.contains("Failed check `ext`"), "{body}");
    assert!(body.contains("makibad reads no log for it"), "{body}");
    assert_eq!((ext.calls("job"), ext.calls("logs")), (0, 0));

    let passed = pass.bodies("secretary", "CI passed for o/pass#5 %");
    assert_eq!(passed.len(), 1, "{passed:?}");
    assert!(!passed[0].contains("Failed job"), "{}", passed[0]);
    assert_eq!((pass.calls("job"), pass.calls("logs")), (0, 0));
}

#[test]
fn two_failed_jobs_with_long_lines_stay_within_the_limit() {
    let org = Org::new("long");
    org.answer(
        A,
        "check-runs",
        &[
            actions_failed(&org.repo, "a", 501),
            actions_failed(&org.repo, "b", 502),
        ],
    );
    for (id, job) in [(501, "a"), (502, "b")] {
        org.job(
            id,
            json!([step(
                1,
                "Run tests",
                "failure",
                "2026-11-23T10:00:30Z",
                "2026-11-23T10:00:40Z"
            )]),
        );
        let mut log = line(30, 0, "##[group]Run cargo test");
        log += &line(
            31,
            0,
            &format!(
                "thread 't{job}' panicked at src/{job}.rs:1:1: {}",
                "P".repeat(460)
            ),
        );
        for i in 1..=50u32 {
            log += &line(32 + i / 10, i, &format!("tail_{i} {}", "x".repeat(990)));
        }
        log += &line(39, 0, "##[error]Process completed with exit code 101.");
        org.log(id, &log);
    }
    org.pr("opened", 5, A);
    let _d = org.start();
    wait("the watch to conclude", || org.status_of(A) == "completed");

    let body = org.failed_notice();
    let (at_a, at_b) = (
        body.find("Failed job `a`")
            .unwrap_or_else(|| panic!("{body}")),
        body.find("Failed job `b`")
            .unwrap_or_else(|| panic!("{body}")),
    );
    assert!(at_a < at_b, "{body}");
    let (part_a, part_b) = (&body[at_a..at_b], &body[at_b..]);
    for (part, job) in [(part_a, "a"), (part_b, "b")] {
        assert!(
            part.chars().count() <= 1500,
            "job {job}: {} characters",
            part.chars().count()
        );
        let panic = format!("thread 't{job}' panicked at src/{job}.rs:1:1:");
        assert!(part.contains(&panic), "{part}");
        assert!(
            part.contains("##[error]Process completed with exit code 101."),
            "{part}"
        );
    }
    assert!(!body.contains(&"P".repeat(201)), "a panic line is cut");
    assert!(!body.contains(&"x".repeat(201)), "a tail line is cut");
    assert!(
        body.chars().count() <= BODY_LIMIT,
        "{}",
        body.chars().count()
    );
}

#[test]
fn a_job_with_no_failed_step_shows_the_end_of_its_log() {
    let org = Org::new("nostep");
    org.answer(
        A,
        "check-runs",
        &[actions_failed(&org.repo, "test", 501), green("fmt")],
    );
    org.job(
        501,
        json!([step(
            1,
            "Run tests",
            "success",
            "2026-11-23T10:00:30Z",
            "2026-11-23T10:00:40Z"
        )]),
    );
    let mut log = line(30, 0, "##[group]Run cargo test");
    for i in 1..=30u32 {
        log += &line(31, i, &format!("test filler_{i} ... ok"));
    }
    log += &line(
        59,
        0,
        "##[error]The job running on runner r1 has exceeded the maximum execution time",
    );
    log += &line(59, 1, "the very last line of the log");
    org.log(501, &log);
    org.pr("opened", 5, A);
    let _d = org.start();
    wait("the watch to conclude", || org.status_of(A) == "completed");

    let body = org.failed_notice();
    assert!(
        body.contains("GitHub names no failed step; the end of the log"),
        "{body}"
    );
    assert!(body.contains("the very last line of the log"), "{body}");
}
