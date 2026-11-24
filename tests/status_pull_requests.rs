//! Issue 405: `makiba status --json` lists the pull requests of open runs and those merged today.
//!
//! - Every pull request of a run not closed (its current one) and every pull request merged
//!   today is one entry of `pull_requests[]`: `repo`, `pr`, `task_id`, `title`, `head_oid`, `ci`
//!   (`running`, `failed`, `passed`, or null with no result at that head), `state` (`open`,
//!   `closed`, `merged`), `merged_via`, `merged_by` and `merge_state`.
//! - The title and the merge state of an open pull request come from what the PR watch's state
//!   read saw (`pr_read`), written once per change of head, title or merge state, also across a
//!   restart; the merge state is only that of the head shown.
//! - A CI result counts for its repository, number and head only, and not across a close until
//!   the head is recorded again.
//!
//! The PR watch's pass runs in this process on a state database of its own (makibad is not
//! started), and the read is `status_local::read` of that database, the object `--json` prints.
//! `gh` is a stand-in script (`MAKIBA_GH`) that answers `gh pr view` from a file per pull request
//! and the CI reads of any head from the repository's file `ci` (`running`, `success` or
//! `failure`, running when absent), and refuses anything else.

mod common;

use makiba::daemon::prwatch::{self, PrWatch};
use makiba::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);
/// About ten intervals of the PR watch (50 ms each).
const TEN_INTERVALS: Duration = Duration::from_millis(600);
const TITLE: &str = "Add the widget";

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<n> --json ...: the file pr-<n>.json of the repository's
# folder, else a failure. gh api --paginate repos/<o>/<n>/...: the CI reads of a head, as the
# repository's file ci says (running when absent).
all="$*"
case "$1 $2" in
  "pr view")
    num=$3; shift 3; repo=
    while [ $# -gt 0 ]; do
      case "$1" in
        --repo) repo=${2#github.com/}; shift 2 ;;
        --json) shift 2 ;;
        *) echo "unexpected argument: $1" >&2; exit 2 ;;
      esac
    done ;;
  "api --paginate")
    path=$3
    rest=${path#repos/}
    owner=${rest%%/*}; rest=${rest#*/}
    repo="$owner/${rest%%/*}" ;;
  *) echo "unexpected call: $all" >&2; exit 2 ;;
esac
case "$repo" in */*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
dir="$FAKE_GH_DIR/${repo%%/*}_${repo#*/}"
mkdir -p "$dir"
ci=running
if [ -e "$dir/ci" ]; then ci=$(cat "$dir/ci"); fi
case "$all" in
  "pr view"*)
    f="$dir/pr-$num.json"
    if [ -e "$f" ]; then cat "$f"; else echo "not found" >&2; exit 1; fi ;;
  *check-runs*)
    case "$ci" in
      success) echo '{"name":"ci","status":"completed","conclusion":"success"}' ;;
      failure) echo '{"name":"ci","status":"completed","conclusion":"failure"}' ;;
      *) echo '{"name":"ci","status":"in_progress","conclusion":null}' ;;
    esac ;;
  */status*) ;;
  *actions/runs*)
    case "$ci" in
      running) echo in_progress ;;
      *) echo completed ;;
    esac ;;
  *) echo "unexpected call: $all" >&2; exit 2 ;;
esac
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The folder the tests share (the stand-in and its answers), with the environment set once
/// before any test starts a thread.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        std::fs::create_dir_all(root.join("answers")).unwrap();
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "50");
            std::env::set_var("MAKIBA_PRWATCH_BUDGET_MS", "3000");
            std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
            std::env::set_var("MAKIBA_PERSON_KEY", root.join("no-person.key"));
            std::env::set_var("MAKIBA_GATE_KEY", root.join("no-gate.key"));
            std::env::set_var("MAKIBA_GOALS_DIR", root.join("goals"));
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
}

/// The stand-in's folder of `repo` (`o/n`).
fn answers(home: &Path, repo: &str) -> PathBuf {
    let dir = home.join("answers").join(repo.replace('/', "_"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `text` to `name` of the folder of `repo`, whole, then renamed into place.
fn put(home: &Path, repo: &str, name: &str, text: &str) {
    let dir = answers(home, repo);
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, dir.join(name)).unwrap();
}

/// The stand-in answers `repo#pr` with `v`.
fn answer(home: &Path, repo: &str, pr: i64, v: &Value) {
    put(home, repo, &format!("pr-{pr}.json"), &v.to_string());
}

/// The stand-in answers the CI reads of `repo`: `running`, `success` or `failure`.
fn ci(home: &Path, repo: &str, what: &str) {
    put(home, repo, "ci", what);
}

/// A pull request read open at `head` with the merge state `status` and `title`.
fn open(head: &str, status: &str, title: &str) -> Value {
    json!({"state": "OPEN", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": title, "mergeable": "MERGEABLE",
           "mergeStateStatus": status, "isDraft": false})
}

/// A pull request read closed unmerged at `head`.
fn closed(head: &str, title: &str) -> Value {
    json!({"state": "CLOSED", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": title, "mergeable": "MERGEABLE",
           "mergeStateStatus": "CLEAN", "isDraft": false})
}

/// A pull request read merged by `owner` at `head`, at `at`.
fn merged(head: &str, at: &str) -> Value {
    json!({"state": "MERGED", "mergedAt": at, "mergedBy": {"login": "owner"},
           "headRefOid": head, "mergeCommit": {"oid": "c".repeat(40)}, "title": TITLE,
           "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN", "isDraft": false})
}

/// A state database with one project, and a PR watch on it.
struct Org {
    tmp: tempfile::TempDir,
    w: StateWriter,
    watch: PrWatch,
    project: i64,
}

impl Org {
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
        let project = w
            .ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        Org {
            tmp,
            w,
            watch: PrWatch::from_env(),
            project,
        }
    }

    /// A run of `task` with `status`.
    fn run(&self, task: &str, status: &str) {
        self.w
            .conn()
            .execute(
                "INSERT INTO runs (task_id, project_id, pattern, title, status)
                 VALUES (?1, ?2, 'B', ?1, ?3)",
                rusqlite::params![task, self.project, status],
            )
            .unwrap();
    }

    fn register(&self, kind: &str, task: &str, repo: &str, pr: i64, head: &str) {
        prwatch::register(&self.w, kind, task, repo, pr, head).unwrap();
    }

    fn event(&self, kind: &str, actor: &str, task: &str, payload: Value) {
        self.w
            .append_event(&NewEvent {
                kind,
                actor: Some(actor),
                payload: Some(&payload),
                run_task_id: Some(task),
                ..Default::default()
            })
            .unwrap();
    }

    /// SQLite's `now` with `modifier`, as an ISO time to the second.
    fn time(&self, modifier: &str) -> String {
        self.w
            .conn()
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
                [modifier],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// How many events `kind` of `repo` the ledger has.
    fn count(&self, kind: &str, repo: &str) -> i64 {
        self.w
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = ?1
                   AND json_extract(payload_json, '$.repo') = ?2",
                [kind, repo],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The object `makiba status --json` prints for this database.
    fn status(&self) -> Value {
        makiba::status_local::read(&self.tmp.path().join("state.db")).unwrap()
    }

    /// The entries of `pull_requests[]` of `repo` (any case).
    fn entries(&self, repo: &str) -> Vec<Value> {
        let v = self.status();
        v["pull_requests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| {
                e["repo"]
                    .as_str()
                    .is_some_and(|r| r.eq_ignore_ascii_case(repo))
            })
            .cloned()
            .collect()
    }

    /// The entry of `repo#pr`, if listed.
    fn entry(&self, repo: &str, pr: i64) -> Option<Value> {
        self.entries(repo)
            .into_iter()
            .find(|e| e["pr"] == json!(pr))
    }

    /// The entry of `repo#pr`, which must be listed.
    fn must(&self, repo: &str, pr: i64) -> Value {
        self.entry(repo, pr).unwrap_or_else(|| {
            panic!(
                "no pull_requests entry for {repo}#{pr}: {}",
                self.status()["pull_requests"]
            )
        })
    }

    /// Whether the entry of `repo#pr` has `field` equal to `want`.
    fn has(&self, repo: &str, pr: i64, field: &str, want: &Value) -> bool {
        self.entry(repo, pr).is_some_and(|e| &e[field] == want)
    }

    /// Run the PR watch's pass until `ok` holds; panics after `WAIT`.
    fn pass_until(&mut self, what: &str, mut ok: impl FnMut(&Org) -> bool) {
        let until = Instant::now() + WAIT;
        loop {
            self.watch.pass(&self.w).unwrap();
            if ok(self) {
                return;
            }
            assert!(
                Instant::now() < until,
                "timed out waiting for {what}: {}",
                self.status()["pull_requests"]
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Run the pass until the entry of `repo#pr` has `field` equal to `want`.
    fn until(&mut self, repo: &str, pr: i64, field: &str, want: Value) {
        let what = format!("{repo}#{pr} {field} = {want}");
        self.pass_until(&what, |o| o.has(repo, pr, field, &want));
    }

    /// Run the PR watch's pass for `d`.
    fn pass_for(&mut self, d: Duration) {
        let until = Instant::now() + d;
        while Instant::now() < until {
            self.watch.pass(&self.w).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// An open pull request whose checks run: its title and merge state as GitHub gives them.
#[test]
fn an_open_pull_request_with_checks_running_shows_its_title_and_merge_state() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1) = ("o/s1", "1".repeat(40));
    ci(&home, repo, "running");
    answer(&home, repo, 5, &open(&h1, "BLOCKED", TITLE));
    org.run("t1", "review");
    org.register("pr_opened", "t1", repo, 5, &h1);
    org.pass_until("the title and merge state", |o| {
        o.has(repo, 5, "title", &json!(TITLE)) && o.has(repo, 5, "merge_state", &json!("blocked"))
    });
    assert_eq!(
        org.must(repo, 5),
        json!({"repo": repo, "pr": 5, "task_id": "t1", "title": TITLE, "head_oid": h1,
               "ci": "running", "state": "open", "merged_via": null, "merged_by": null,
               "merge_state": "blocked"})
    );
}

/// Failed checks read failed; the pull request is still open.
#[test]
fn failed_checks_read_failed() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1) = ("o/s2", "2".repeat(40));
    ci(&home, repo, "failure");
    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.run("t2", "review");
    org.register("pr_opened", "t2", repo, 5, &h1);
    org.pass_until("the failed CI", |o| o.count("ci_completed", repo) > 0);
    let e = org.must(repo, 5);
    assert_eq!(e["ci"], json!("failed"), "{e}");
    assert_eq!(e["state"], json!("open"), "{e}");
}

/// Passed checks read passed, and a clean pull request reads ready.
#[test]
fn passed_checks_read_passed_and_ready() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1) = ("o/s3", "3".repeat(40));
    ci(&home, repo, "success");
    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.run("t3", "review");
    org.register("pr_opened", "t3", repo, 5, &h1);
    org.pass_until("passed and ready", |o| {
        o.has(repo, 5, "ci", &json!("passed")) && o.has(repo, 5, "merge_state", &json!("ready"))
    });
    let e = org.must(repo, 5);
    assert_eq!(e["ci"], json!("passed"), "{e}");
    assert_eq!(e["merge_state"], json!("ready"), "{e}");
}

/// A head recorded after the last read: its merge state is not that of the older head.
#[test]
fn a_new_head_has_no_merge_state_until_read() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1, h2) = ("o/s4", "4".repeat(40), "5".repeat(40));
    ci(&home, repo, "success");
    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.run("t4", "review");
    org.register("pr_opened", "t4", repo, 5, &h1);
    org.pass_until("passed and ready", |o| {
        o.has(repo, 5, "ci", &json!("passed")) && o.has(repo, 5, "merge_state", &json!("ready"))
    });

    org.register("fix_pushed", "t4", repo, 5, &h2);
    let e = org.must(repo, 5);
    assert_eq!(e["head_oid"], json!(h2), "{e}");
    assert_eq!(e["ci"], json!("running"), "{e}");
    assert_eq!(e["merge_state"], Value::Null, "{e}");

    answer(&home, repo, 5, &open(&h2, "BEHIND", TITLE));
    org.until(repo, 5, "merge_state", json!("behind"));
}

/// A close outside makiba, then a reopen: the CI result of before the close does not count until
/// the head is recorded again, and the title is the newest read.
#[test]
fn a_close_outside_makiba_then_a_reopen() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1) = ("o/s5", "6".repeat(40));
    ci(&home, repo, "success");
    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.run("t5", "review");
    org.register("pr_opened", "t5", repo, 5, &h1);
    org.until(repo, 5, "ci", json!("passed"));

    answer(&home, repo, 5, &closed(&h1, "Renamed"));
    org.until(repo, 5, "state", json!("closed"));
    let e = org.must(repo, 5);
    assert_eq!(e["ci"], Value::Null, "{e}");
    assert_eq!(e["title"], json!("Renamed"), "{e}");

    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.pass_until("the reopen and its read", |o| {
        o.count("pr_reopened", repo) == 1 && o.has(repo, 5, "title", &json!(TITLE))
    });
    let e = org.must(repo, 5);
    assert_eq!(e["state"], json!("open"), "{e}");
    assert_eq!(e["ci"], Value::Null, "{e}");

    org.register("fix_pushed", "t5", repo, 5, &h1);
    let e = org.must(repo, 5);
    assert_eq!(e["ci"], json!("passed"), "{e}");
}

/// A pull request merged by hand today is listed as such, also once its run is closed.
#[test]
fn a_hand_merge_today_is_listed_after_its_run_is_closed() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1) = ("o/s6", "7".repeat(40));
    ci(&home, repo, "running");
    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.run("t6", "review");
    org.register("pr_opened", "t6", repo, 5, &h1);
    org.until(repo, 5, "title", json!(TITLE));

    let now = org.time("+0 seconds");
    answer(&home, repo, 5, &merged(&h1, &now));
    org.pass_until("the merge", |o| o.count("pr_merged", repo) == 1);
    let e = org.must(repo, 5);
    assert_eq!(e["state"], json!("merged"), "{e}");
    assert_eq!(e["merged_via"], json!("hand"), "{e}");
    assert_eq!(e["merged_by"], json!("owner"), "{e}");
    assert_eq!(e["merge_state"], Value::Null, "{e}");
    assert_eq!(e["title"], json!(TITLE), "{e}");
    assert_eq!(e["head_oid"], json!(h1), "{e}");
    assert_eq!(e["task_id"], json!("t6"), "{e}");

    org.w
        .conn()
        .execute(
            "UPDATE runs SET status = 'completed',
               completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE task_id = 't6'",
            [],
        )
        .unwrap();
    assert_eq!(org.must(repo, 5), e, "still listed once its run is closed");
}

/// A merge the gate allowed at that head reads `gate`.
#[test]
fn a_gate_merge_reads_gate() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1) = ("o/s7", "8".repeat(40));
    ci(&home, repo, "running");
    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.run("t7", "review");
    org.register("pr_opened", "t7", repo, 5, &h1);
    org.event(
        "acceptance_gate",
        "gate",
        "t7",
        json!({"repo": repo, "pr": 5, "base": "main", "head": h1, "result": "passed",
               "reason": "r", "mode": "acceptance", "allow": true}),
    );
    let now = org.time("+0 seconds");
    answer(&home, repo, 5, &merged(&h1, &now));
    org.pass_until("the merge", |o| o.count("pr_merged", repo) == 1);
    let e = org.must(repo, 5);
    assert_eq!(e["state"], json!("merged"), "{e}");
    assert_eq!(e["merged_via"], json!("gate"), "{e}");
}

/// A merge of two days ago, a run with no pull request and a pull request a run has replaced
/// are not listed.
#[test]
fn an_older_merge_and_runs_without_a_pull_request_are_not_listed() {
    let _home = init();
    let org = Org::new();
    org.run("old", "completed");
    let then = org.time("-2 days");
    org.event(
        "pr_merged",
        "makibad",
        "old",
        json!({"repo": "o/s8x", "pr": 3, "title": "Old", "merged_at": then,
               "merge_commit": "d".repeat(40), "task_id": "old", "merged_by": "owner",
               "head_oid": "9".repeat(40), "merged_via": "hand"}),
    );
    org.run("bare", "in_use");
    org.run("two", "review");
    org.register("pr_opened", "two", "o/s8", 11, &"a".repeat(40));
    org.register("pr_opened", "two", "o/s8", 12, &"b".repeat(40));

    let v = org.status();
    let all = v["pull_requests"]
        .as_array()
        .unwrap_or_else(|| panic!("no pull_requests array: {v}"));
    assert!(
        all.iter()
            .all(|e| e["task_id"] != json!("old") && e["task_id"] != json!("bare")),
        "{v}"
    );
    assert!(org.entries("o/s8x").is_empty(), "{v}");
    let s8 = org.entries("o/s8");
    assert_eq!(s8.len(), 1, "{v}");
    assert_eq!(s8[0]["pr"], json!(12), "{v}");
    assert_eq!(s8[0]["task_id"], json!("two"), "{v}");
}

/// One head in two repositories and two pull requests: a CI result is that of its repository,
/// number and head only.
#[test]
fn a_ci_result_is_of_its_repository_number_and_head_only() {
    let home = init();
    let mut org = Org::new();
    let h = "e".repeat(40);
    let (failing, running) = ("o/s9a", "o/s9b");
    ci(&home, failing, "failure");
    ci(&home, running, "running");
    answer(&home, failing, 1, &open(&h, "CLEAN", TITLE));
    answer(&home, running, 1, &open(&h, "CLEAN", TITLE));
    org.run("a", "review");
    org.run("b", "review");
    org.register("pr_opened", "a", failing, 1, &h);
    org.register("pr_opened", "b", running, 1, &h);
    org.pass_until("the failed CI", |o| o.count("ci_completed", failing) == 1);
    assert_eq!(org.must(failing, 1)["ci"], json!("failed"));
    assert_eq!(org.must(running, 1)["ci"], json!("running"));

    // Another pull request of the failing repository at the same head, whose watch ended
    // without a result: no result of its own.
    org.run("c", "review");
    org.register("pr_opened", "c", failing, 2, &h);
    org.w
        .conn()
        .execute(
            "UPDATE pr_watches SET status = 'failed',
               finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE repo = ?1 AND pr = 2",
            [failing],
        )
        .unwrap();
    let e = org.must(failing, 2);
    assert_eq!(e["ci"], Value::Null, "{e}");
    assert_eq!(e["task_id"], json!("c"), "{e}");
}

/// The same read is recorded once, also after a restart; a change of the merge state is
/// recorded again.
#[test]
fn an_unchanged_read_is_recorded_once_also_after_a_restart() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1) = ("o/s10", "f".repeat(40));
    ci(&home, repo, "running");
    answer(&home, repo, 5, &open(&h1, "CLEAN", TITLE));
    org.run("t10", "review");
    org.register("pr_opened", "t10", repo, 5, &h1);
    org.pass_until("the first read", |o| o.count("pr_read", repo) >= 1);
    org.pass_for(TEN_INTERVALS);
    assert_eq!(org.count("pr_read", repo), 1, "one read recorded");

    org.watch = PrWatch::from_env();
    org.pass_for(TEN_INTERVALS);
    assert_eq!(org.count("pr_read", repo), 1, "not again after a restart");

    answer(&home, repo, 5, &open(&h1, "DIRTY", TITLE));
    org.pass_until("the change", |o| o.count("pr_read", repo) == 2);
    assert_eq!(org.must(repo, 5)["merge_state"], json!("conflict"));
}
