//! Issue 388: a pull request merged or closed outside makiba is read by makibad's PR watch.
//!
//! - A merge is recorded (`pr_merged` with who merged it, the head it merged and how: `gate`
//!   when the gate allowed that head, else `hand`), the watches of the pull request end
//!   (`ci_watch_failed`, reason `merged`) and a hand merge is told to the secretary.
//! - A watch started after the merge is ended too.
//! - A close outside makiba is recorded, ends the watches and is told; a reopen is recorded and
//!   told.
//! - A run outside a range is read for a day after it closed, not after.
//! - The belt reads no pull request's state any more.
//!
//! The PR watch's pass (and the belt's, for the last test) runs in this process on a state
//! database of its own (makibad is not started). `gh` is a stand-in script (`MAKIBA_GH`) that
//! answers `gh pr view` from a file per pull request, answers the CI reads of any head with one
//! check still running, logs every call per repository and refuses anything else.

mod common;

use makiba::daemon::belt::Belt;
use makiba::daemon::prwatch::{self, PrWatch};
use makiba::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);
const MERGED_AT: &str = "2026-11-20T00:00:00Z";

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<n> --json ...: the file pr-<n>.json of the repository's
# folder, else a failure. gh api --paginate repos/<o>/<n>/...: the CI reads of a head, one check
# still running. Every call is logged whole, one line, in the repository's folder (calls).
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
echo "$all" >> "$dir/calls"
case "$all" in
  "pr view"*)
    f="$dir/pr-$num.json"
    if [ -e "$f" ]; then cat "$f"; else echo "not found" >&2; exit 1; fi ;;
  *check-runs*) echo '{"name":"ci","status":"in_progress","conclusion":null}' ;;
  */status*) ;;
  *actions/runs*) echo in_progress ;;
  *) echo "unexpected call: $all" >&2; exit 2 ;;
esac
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The folder the tests share (the stand-in, its answers and an empty folder of clones), with the
/// environment set once before any test starts a thread.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        for d in ["answers", "repos"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "50");
            std::env::set_var("MAKIBA_LANDING_RETRY_MS", "0");
            std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
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

/// The stand-in answers `repo#pr` with `v` (written whole, then renamed into place).
fn answer(home: &Path, repo: &str, pr: i64, v: &Value) {
    let dir = answers(home, repo);
    let tmp = dir.join(format!("pr-{pr}.json.tmp"));
    std::fs::write(&tmp, v.to_string()).unwrap();
    std::fs::rename(&tmp, dir.join(format!("pr-{pr}.json"))).unwrap();
}

/// A pull request read open at `head`.
fn open(head: &str) -> Value {
    json!({"state": "OPEN", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": "T"})
}

/// A pull request read closed unmerged at `head`.
fn closed(head: &str) -> Value {
    json!({"state": "CLOSED", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": "T"})
}

/// A pull request read merged by `owner` at `head` with the merge commit `merge`.
fn merged(head: &str, merge: &str) -> Value {
    json!({"state": "MERGED", "mergedAt": MERGED_AT, "mergedBy": {"login": "owner"},
           "headRefOid": head, "mergeCommit": {"oid": merge}, "title": "T"})
}

/// The calls the stand-in logged for `repo`.
fn calls(home: &Path, repo: &str) -> Vec<String> {
    std::fs::read_to_string(answers(home, repo).join("calls"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// How many logged calls of `repo` contain `what`.
fn count_calls(home: &Path, repo: &str, what: &str) -> usize {
    calls(home, repo)
        .iter()
        .filter(|c| c.contains(what))
        .count()
}

fn hand_notice(repo: &str, pr: i64, task: &str, merge: &str) -> String {
    format!(
        "Pull request {repo}#{pr} of task {task} was merged by hand outside makiba's gate (by owner, at {MERGED_AT}, merge commit {}): run the retro and close the task.",
        &merge[..7]
    )
}

/// A state database with one project, and a PR watch on it.
struct Org {
    _tmp: tempfile::TempDir,
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
            _tmp: tmp,
            w,
            watch: PrWatch::from_env(),
            project,
        }
    }

    /// A run of `task` with `status`, in `range` when given, closed at `completed_at` (an SQLite
    /// time modifier from now) when given.
    fn run(&self, task: &str, status: &str, range: Option<i64>, completed_ago: Option<&str>) {
        self.w
            .conn()
            .execute(
                "INSERT INTO runs (task_id, project_id, pattern, title, status, range_id,
                                   completed_at)
                 VALUES (?1, ?2, 'B', ?1, ?3, ?4,
                         CASE WHEN ?5 IS NULL THEN NULL
                              ELSE strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?5) END)",
                rusqlite::params![task, self.project, status, range, completed_ago],
            )
            .unwrap();
    }

    /// An ended range of the project.
    fn ended_range(&self) -> i64 {
        self.w
            .conn()
            .execute(
                "INSERT INTO ranges (project_id, at_once, budget_tasks, brief_head, proposed_by,
                                     status, ended_reason, ended_at)
                 VALUES (?1, 1, 5, 'head', 'secretary', 'ended', 'person',
                         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [self.project],
            )
            .unwrap();
        self.w.conn().last_insert_rowid()
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

    /// A verdict of the gate on `repo#pr` at `head`.
    fn gate(&self, task: &str, repo: &str, pr: i64, head: &str, allow: bool) {
        self.event(
            "acceptance_gate",
            "gate",
            task,
            json!({"repo": repo, "pr": pr, "base": "main", "head": head,
                   "result": if allow { "passed" } else { "rejected" }, "reason": "r",
                   "mode": "acceptance", "allow": allow}),
        );
    }

    /// The payloads of every event `kind` of `task`, oldest first.
    fn events(&self, kind: &str, task: &str) -> Vec<Value> {
        let mut q = self
            .w
            .conn()
            .prepare(
                "SELECT e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = ?1 AND r.task_id = ?2 ORDER BY e.id",
            )
            .unwrap();
        q.query_map([kind, task], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }

    /// How many events `kind` the ledger has, of any run.
    fn kind_count(&self, kind: &str) -> i64 {
        self.w
            .conn()
            .query_row("SELECT count(*) FROM events WHERE kind = ?1", [kind], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// The bodies of the notices to the secretary, oldest first.
    fn notices(&self) -> Vec<String> {
        let mut q = self
            .w
            .conn()
            .prepare(
                "SELECT body FROM messages WHERE recipient = 'secretary' AND kind = 'notice'
                 ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|b| b.unwrap())
            .collect()
    }

    fn notices_naming(&self, what: &str) -> Vec<String> {
        self.notices()
            .into_iter()
            .filter(|b| b.contains(what))
            .collect()
    }

    fn watch_status(&self, repo: &str, pr: i64, head: &str) -> String {
        self.w
            .conn()
            .query_row(
                "SELECT status FROM pr_watches WHERE repo = ?1 AND pr = ?2 AND head_oid = ?3",
                rusqlite::params![repo, pr, head],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Run the PR watch's pass until `ok` holds; panics after `WAIT`.
    fn pass_until(&mut self, what: &str, mut ok: impl FnMut(&Org) -> bool) {
        let until = Instant::now() + WAIT;
        loop {
            self.watch.pass(&self.w).unwrap();
            if ok(self) {
                return;
            }
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
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

/// A reviewed run's pull request merged in GitHub's web UI: `pr_merged` says who merged it, the
/// head and `hand`; its CI watch ends with the reason `merged`; the secretary is told once, and
/// the pull request is not read again.
#[test]
fn a_hand_merge_of_a_reviewed_runs_pull_request_is_recorded_ended_and_told() {
    let home = init();
    let mut org = Org::new();
    let (repo, head, merge) = ("o/h1", "1".repeat(40), "b".repeat(40));
    org.run("t1", "review", None, None);
    answer(&home, repo, 7, &open(&head));
    org.register("pr_opened", "t1", repo, 7, &head);

    org.pass_until("the pull request and its CI read", |_| {
        count_calls(&home, repo, "pr view 7") > 0
            && count_calls(&home, repo, &format!("commits/{head}/check-runs")) > 0
    });
    assert!(org.events("pr_merged", "t1").is_empty(), "read open");
    assert_eq!(org.watch_status(repo, 7, &head), "watching");

    answer(&home, repo, 7, &merged(&head, &merge));
    org.pass_until("the merge", |o| !o.events("pr_merged", "t1").is_empty());
    assert_eq!(
        org.events("pr_merged", "t1"),
        vec![
            json!({"repo": repo, "pr": 7, "title": "T", "merged_at": MERGED_AT,
                    "merge_commit": merge, "task_id": "t1", "merged_by": "owner",
                    "head_oid": head, "merged_via": "hand"})
        ]
    );
    assert_eq!(org.watch_status(repo, 7, &head), "failed");
    assert_eq!(
        org.events("ci_watch_failed", "t1"),
        vec![json!({"repo": repo, "pr": 7, "head_oid": head, "reason": "merged"})]
    );
    let told = vec![hand_notice(repo, 7, "t1", &merge)];
    assert_eq!(org.notices(), told);

    let reads = count_calls(&home, repo, "pr view 7");
    org.pass_for(Duration::from_millis(500));
    assert_eq!(org.events("pr_merged", "t1").len(), 1, "recorded once");
    assert_eq!(org.events("ci_watch_failed", "t1").len(), 1, "ended once");
    assert_eq!(org.notices(), told, "told once");
    assert_eq!(
        count_calls(&home, repo, "pr view 7"),
        reads,
        "a merged pull request is not read again"
    );
}

/// A head recorded for a pull request already read merged: its watch is ended with the reason
/// `merged`, and nothing else is recorded or told again.
#[test]
fn a_watch_started_after_the_merge_is_ended_too() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1, h2, merge) = ("o/h2", "1".repeat(40), "2".repeat(40), "c".repeat(40));
    org.run("t2", "in_use", None, None);
    answer(&home, repo, 8, &merged(&h1, &merge));
    org.register("pr_opened", "t2", repo, 8, &h1);
    org.pass_until("the merge", |o| !o.events("pr_merged", "t2").is_empty());

    org.register("fix_pushed", "t2", repo, 8, &h2);
    org.pass_until("the watch of the later head ended", |o| {
        o.watch_status(repo, 8, &h2) != "watching"
    });
    assert_eq!(org.watch_status(repo, 8, &h2), "failed");
    assert!(
        org.events("ci_watch_failed", "t2")
            .contains(&json!({"repo": repo, "pr": 8, "head_oid": h2, "reason": "merged"})),
        "{:?}",
        org.events("ci_watch_failed", "t2")
    );
    let ci_reads = count_calls(&home, repo, &format!("commits/{h2}/check-runs"));
    org.pass_for(Duration::from_millis(300));
    assert_eq!(org.events("pr_merged", "t2").len(), 1);
    assert_eq!(org.notices_naming("o/h2#8").len(), 1);
    assert_eq!(
        count_calls(&home, repo, &format!("commits/{h2}/check-runs")),
        ci_reads,
        "the ended watch reads nothing"
    );
}

/// The gate allowed the very head that was merged (its repository spelled in another case): the
/// merge is the gate's, and the secretary is not told.
#[test]
fn a_merge_the_gate_allowed_at_that_head_is_the_gates_and_is_not_told() {
    let home = init();
    let mut org = Org::new();
    let (repo, head, merge) = ("o/h3", "3".repeat(40), "d".repeat(40));
    org.run("t3", "in_use", None, None);
    answer(&home, repo, 9, &merged(&head, &merge));
    org.register("pr_opened", "t3", repo, 9, &head);
    org.gate("t3", "O/H3", 9, &head, true);

    org.pass_until("the merge", |o| !o.events("pr_merged", "t3").is_empty());
    let m = &org.events("pr_merged", "t3")[0];
    assert_eq!(m["merged_via"], "gate");
    assert_eq!(m["merged_by"], "owner");
    assert_eq!(m["head_oid"], json!(head));
    assert_eq!(org.watch_status(repo, 9, &head), "failed");
    assert_eq!(
        org.events("ci_watch_failed", "t3"),
        vec![json!({"repo": repo, "pr": 9, "head_oid": head, "reason": "merged"})]
    );
    org.pass_for(Duration::from_millis(300));
    assert!(
        org.notices_naming("o/h3#9").is_empty(),
        "{:?}",
        org.notices()
    );
}

/// An allowance of another head of the pull request, a refusal of the merged head and an
/// allowance of another pull request at that head do not make the merge the gate's.
#[test]
fn an_allowance_of_another_head_does_not_make_a_merge_the_gates() {
    let home = init();
    let mut org = Org::new();
    let (repo, h1, h2, merge) = ("o/h4", "4".repeat(40), "5".repeat(40), "e".repeat(40));
    org.run("t4", "in_use", None, None);
    answer(&home, repo, 10, &merged(&h2, &merge));
    org.register("pr_opened", "t4", repo, 10, &h2);
    org.gate("t4", repo, 10, &h1, true);
    org.gate("t4", repo, 10, &h2, false);
    org.gate("t4", repo, 11, &h2, true);

    org.pass_until("the merge", |o| !o.events("pr_merged", "t4").is_empty());
    assert_eq!(org.events("pr_merged", "t4")[0]["merged_via"], "hand");
    assert_eq!(
        org.notices_naming("o/h4#10"),
        vec![hand_notice(repo, 10, "t4", &merge)]
    );
}

/// A pull request closed unmerged outside makiba: `pr_closed`, its watch ended with the reason
/// `closed` and the secretary told, once; read open again, `pr_reopened` and told.
#[test]
fn a_close_outside_makiba_is_recorded_ended_and_told() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/h5", "6".repeat(40));
    org.run("t5", "in_use", None, None);
    answer(&home, repo, 12, &closed(&head));
    org.register("pr_opened", "t5", repo, 12, &head);

    org.pass_until("the close", |o| !o.events("pr_closed", "t5").is_empty());
    assert_eq!(
        org.events("pr_closed", "t5"),
        vec![json!({"repo": repo, "pr": 12, "title": "T", "task_id": "t5"})]
    );
    assert_eq!(org.watch_status(repo, 12, &head), "failed");
    assert_eq!(
        org.events("ci_watch_failed", "t5"),
        vec![json!({"repo": repo, "pr": 12, "head_oid": head, "reason": "closed"})]
    );
    assert!(org.events("pr_merged", "t5").is_empty());
    let closed_notice = "Pull request o/h5#12 of task t5 was closed without a merge outside makiba; if it is reopened, record its head with makiba pr pushed to watch its CI again.";
    assert_eq!(org.notices_naming(closed_notice), vec![closed_notice]);
    org.pass_for(Duration::from_millis(300));
    assert_eq!(org.events("pr_closed", "t5").len(), 1, "recorded once");
    assert_eq!(org.notices_naming(closed_notice).len(), 1, "told once");

    answer(&home, repo, 12, &open(&head));
    org.pass_until("the reopen", |o| !o.events("pr_reopened", "t5").is_empty());
    assert_eq!(
        org.events("pr_reopened", "t5"),
        vec![json!({"repo": repo, "pr": 12, "task_id": "t5"})]
    );
    let reopened = "Pull request o/h5#12 of task t5 was reopened outside makiba; record its head with makiba pr pushed to watch its CI again.";
    assert_eq!(org.notices_naming(reopened), vec![reopened]);
}

/// A run outside every range is read for a day after it closed: one closed an hour ago has its
/// merge recorded, one closed two days ago is not read.
#[test]
fn a_non_range_run_closed_within_a_day_is_read_and_one_closed_long_ago_is_not() {
    let home = init();
    let mut org = Org::new();
    let repo = "o/h6";
    let (ha, hb, ma, mb) = (
        "7".repeat(40),
        "8".repeat(40),
        "f".repeat(40),
        "a".repeat(40),
    );
    org.run("t6a", "completed", None, Some("-1 hours"));
    org.run("t6b", "completed", None, Some("-2 days"));
    answer(&home, repo, 13, &merged(&ha, &ma));
    answer(&home, repo, 14, &merged(&hb, &mb));
    org.register("pr_opened", "t6a", repo, 13, &ha);
    org.register("pr_opened", "t6b", repo, 14, &hb);

    org.pass_until("the recent run's merge", |o| {
        !o.events("pr_merged", "t6a").is_empty()
    });
    org.pass_for(Duration::from_millis(500));
    assert_eq!(org.events("pr_merged", "t6a").len(), 1);
    assert!(org.events("pr_merged", "t6b").is_empty());
    let calls = calls(&home, repo);
    assert!(
        !calls.iter().any(|c| c.contains("pr view 14")),
        "the run closed long ago is not read: {calls:?}"
    );
}

/// The belt reads no pull request's state: not a completed range task's, not an open run's, not
/// that of a task a design-only run waits for. The PR watch reads and records them.
#[test]
fn the_belt_reads_no_pull_request_state() {
    let home = init();
    let mut org = Org::new();
    let repo = "o/h7";
    let range = org.ended_range();
    let (h15, h16, h17) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
    org.run("t7", "completed", Some(range), Some("-1 hours"));
    org.run("t7c", "in_use", None, None);
    org.run("t7d", "completed", Some(range), Some("-1 hours"));
    org.run("t7w", "in_use", Some(range), None);
    org.event(
        "belt_dispatched",
        "makibad",
        "t7w",
        json!({"range_id": range, "issue": 2, "task_id": "t7w", "design_only": true,
               "waits_for": [1], "waits_tasks": ["t7d"]}),
    );
    answer(&home, repo, 15, &merged(&h15, &"1".repeat(40)));
    answer(&home, repo, 16, &closed(&h16));
    answer(&home, repo, 17, &merged(&h17, &"2".repeat(40)));
    org.register("pr_opened", "t7", repo, 15, &h15);
    org.register("pr_opened", "t7c", repo, 16, &h16);
    org.register("pr_opened", "t7d", repo, 17, &h17);

    let mut belt = Belt::from_env(home.join("repos"));
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        belt.pass(&org.w).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(belt);
    let calls = calls(&home, repo);
    assert!(calls.is_empty(), "the belt read: {calls:?}");
    for kind in ["pr_merged", "pr_closed", "pr_reopened"] {
        assert_eq!(org.kind_count(kind), 0, "{kind}");
    }

    org.pass_until("the PR watch's records", |o| {
        !o.events("pr_merged", "t7").is_empty()
            && !o.events("pr_merged", "t7d").is_empty()
            && !o.events("pr_closed", "t7c").is_empty()
    });
}
