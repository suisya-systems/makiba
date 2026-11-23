//! Issue 385: what the ledger records of a run closed with `--abandon` whose pull request lands,
//! and of a relaunch `makiba up` adds while `makibad` is stopped.
//!
//! - A run closed with `--abandon` (of a range or not) is followed by the PR watch for a day after
//!   its close, unless its pull request was read closed: a merge then is recorded (`pr_merged`),
//!   and a hand merge is told to the secretary. A run abandoned longer ago is not read.
//! - `makiba up`'s relaunch is the person's, not `makibad`'s: its `run_taken` has the actor
//!   `person`.
//!
//! The PR watch's pass runs in this process on a state database of its own (makibad is not
//! started). `gh` is a stand-in script (`MAKIBA_GH`) that answers `gh pr view` from a file per
//! pull request, answers the CI reads of any head with one check still running, logs every call
//! per repository and refuses anything else.

use makiba::daemon::prwatch::{self, PrWatch};
use makiba::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
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

/// The folder the tests share (the stand-in and its answers), with the environment set once
/// before any test starts a thread.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(root.join("answers")).unwrap();
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "50");
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

/// A pull request read merged by `owner` at `head` with the merge commit `merge`.
fn merged(head: &str, merge: &str) -> Value {
    json!({"state": "MERGED", "mergedAt": MERGED_AT, "mergedBy": {"login": "owner"},
           "headRefOid": head, "mergeCommit": {"oid": merge}, "title": "T"})
}

/// How many logged calls of `repo` contain `what`.
fn count_calls(home: &Path, repo: &str, what: &str) -> usize {
    std::fs::read_to_string(answers(home, repo).join("calls"))
        .unwrap_or_default()
        .lines()
        .filter(|c| c.contains(what))
        .count()
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

    /// A run of `task` with `status`, in `range` when given, closed at `completed_ago` (an SQLite
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

    /// A verdict of the gate on `repo#pr` at `head` that allows it.
    fn gate_allows(&self, task: &str, repo: &str, pr: i64, head: &str) {
        self.event(
            "acceptance_gate",
            "gate",
            task,
            json!({"repo": repo, "pr": pr, "base": "main", "head": head, "result": "passed",
                   "reason": "r", "mode": "acceptance", "allow": true}),
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

    /// The bodies of the notices to the secretary that contain `what`.
    fn notices_naming(&self, what: &str) -> Vec<String> {
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

/// A range task closed with `--abandon` whose pull request is merged (the gate allowed that
/// head): its landing is recorded once, as the gate's, its watch ends and nothing is told.
#[test]
fn a_range_run_closed_with_abandon_whose_pull_request_merges_gets_pr_merged() {
    let home = init();
    let mut org = Org::new();
    let (repo, head, merge) = ("o/a1", "1".repeat(40), "a".repeat(40));
    let range = org.ended_range();
    org.run("r1-1", "abandoned", Some(range), Some("-1 minutes"));
    answer(&home, repo, 5, &merged(&head, &merge));
    org.register("pr_opened", "r1-1", repo, 5, &head);
    org.gate_allows("r1-1", repo, 5, &head);

    org.pass_until("the merge of the abandoned range run", |o| {
        !o.events("pr_merged", "r1-1").is_empty()
    });
    org.pass_for(Duration::from_millis(300));
    let m = org.events("pr_merged", "r1-1");
    assert_eq!(m.len(), 1, "recorded once: {m:?}");
    assert_eq!(m[0]["repo"], repo);
    assert_eq!(m[0]["pr"], 5);
    assert_eq!(m[0]["head_oid"], json!(head));
    assert_eq!(m[0]["merged_via"], "gate");
    assert_eq!(org.watch_status(repo, 5, &head), "failed");
    assert!(org.notices_naming("o/a1#5").is_empty());
}

/// A run outside a range closed with `--abandon` an hour ago, its pull request then merged by
/// hand: the landing is recorded as a hand merge and the secretary is told once.
#[test]
fn a_run_outside_a_range_closed_with_abandon_and_merged_by_hand_gets_pr_merged_and_is_told() {
    let home = init();
    let mut org = Org::new();
    let (repo, head, merge) = ("o/a2", "2".repeat(40), "b".repeat(40));
    org.run("t2", "abandoned", None, Some("-1 hours"));
    answer(&home, repo, 6, &merged(&head, &merge));
    org.register("pr_opened", "t2", repo, 6, &head);

    org.pass_until("the merge of the abandoned run", |o| {
        !o.events("pr_merged", "t2").is_empty()
    });
    org.pass_for(Duration::from_millis(300));
    let m = org.events("pr_merged", "t2");
    assert_eq!(m.len(), 1, "recorded once: {m:?}");
    assert_eq!(m[0]["merged_via"], "hand");
    assert_eq!(org.notices_naming("o/a2#6").len(), 1);
}

/// A run closed with `--abandon` more than a day ago, of a range or not, is not read.
#[test]
fn a_run_closed_with_abandon_more_than_a_day_ago_is_not_read() {
    let home = init();
    let mut org = Org::new();
    let (r3, r4) = ("o/a3", "o/a4");
    let (head, merge) = ("3".repeat(40), "c".repeat(40));
    let range = org.ended_range();
    org.run("r1-3", "abandoned", Some(range), Some("-2 days"));
    org.run("t4", "abandoned", None, Some("-2 days"));
    answer(&home, r3, 7, &merged(&head, &merge));
    answer(&home, r4, 8, &merged(&head, &merge));
    org.register("pr_opened", "r1-3", r3, 7, &head);
    org.register("pr_opened", "t4", r4, 8, &head);

    org.pass_for(Duration::from_millis(500));
    assert_eq!(count_calls(&home, r3, "pr view"), 0);
    assert_eq!(count_calls(&home, r4, "pr view"), 0);
    assert_eq!(org.kind_count("pr_merged"), 0);
}

/// A run closed with `--abandon` whose pull request was read closed is not read again.
#[test]
fn a_run_closed_with_abandon_whose_pull_request_was_read_closed_is_not_read() {
    let home = init();
    let mut org = Org::new();
    let (repo, head, merge) = ("o/a5", "5".repeat(40), "e".repeat(40));
    let range = org.ended_range();
    org.run("r1-5", "abandoned", Some(range), Some("-1 minutes"));
    answer(&home, repo, 9, &merged(&head, &merge));
    org.register("pr_opened", "r1-5", repo, 9, &head);
    org.event(
        "pr_closed",
        "makibad",
        "r1-5",
        json!({"repo": repo, "pr": 9, "title": "T", "task_id": "r1-5"}),
    );

    org.pass_for(Duration::from_millis(500));
    assert_eq!(count_calls(&home, repo, "pr view 9"), 0);
    assert_eq!(org.kind_count("pr_merged"), 0);
}

/// `makiba up` relaunches a suspended run before it starts makibad: the `run_taken` it writes is
/// the person's, naming the new launch.
#[test]
fn makiba_up_records_its_relaunch_as_the_persons() {
    let org = Org::new();
    org.run("t6", "suspended", None, None);
    org.w
        .conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree,
                                     conversation_id, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = 't6'), 'first-spawn', 'code', 'fake',
                     'brief', '/nowhere', 'conversation-1', 'verified')",
            [],
        )
        .unwrap();

    let resumed = makiba::up::resume(&org.w).unwrap();
    assert_eq!(resumed, vec!["t6".to_string()]);

    let newest: String = org
        .w
        .conn()
        .query_row(
            "SELECT spawn_id FROM run_launch WHERE run_id = (SELECT id FROM runs WHERE task_id = 't6')
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(newest, "first-spawn");
    let taken: Vec<(String, String)> = {
        let mut q = org
            .w
            .conn()
            .prepare(
                "SELECT e.actor, e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'run_taken' AND r.task_id = 't6' ORDER BY e.id",
            )
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|x| x.unwrap())
            .collect()
    };
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].0, "person");
    let payload: Value = serde_json::from_str(&taken[0].1).unwrap();
    assert_eq!(payload["spawn_id"], json!(newest));
}
