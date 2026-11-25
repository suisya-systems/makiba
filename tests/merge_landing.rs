//! Issue 434 (docs/design/merge-act.md section 7, build Issue 4): the PR watch classes a landing
//! from makibad's record of the act, never from an `acceptance_gate` row, and follows the merge
//! queue.
//!
//! - `merged_via` is `gate` for a `merge_done` at the merged head, and for a `merge_enqueued` whose
//!   landing the pull request's timeline shows uninterrupted (one addition to the queue after the
//!   attempt, no removal); `unknown` for a `merge_unknown`, and for a queued landing the timeline
//!   shows re-added; `hand` for an allowance alone.
//! - A run whose pull request has an act awaiting its landing is read whatever its status or age.
//! - A state read is held while the attempt is open, and a read begun before the act's outcome was
//!   recorded is dropped: a delayed read at the old head does not end a queue act.
//! - A queue act ends (`merge_dequeued`) when the pull request leaves the queue with a removal on
//!   its timeline (two reads), when another head is read, or when it is read closed; a read with
//!   no entry and no removal does not end it.
//!
//! The PR watch's pass runs in this process on a state database of its own (makibad is not
//! started). The act's records are written as makibad's merge verb and merge pass write them.
//! `gh` is a stand-in script (`MAKIBA_GH`) that answers `gh pr view` and `gh api graphql` from one
//! file per pull request (GraphQL's answer wraps it in `data.repository.pullRequest`), answers
//! the CI reads of any head with one check still running, and logs every call per repository.
//! When `hold-<n>` exists, the next read of pull request `<n>` takes its answer at its start and
//! prints it only once `go-<n>` exists. GitHub's timeline items (merge-act.md section 10, claims
//! 5-6) are those of the stand-in: [unverified].

mod common;

use makiba::daemon::prwatch::{self, PrWatch};
use makiba::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);
const MERGED_AT: &str = "2026-11-20T00:00:00Z";
/// Ten intervals of the PR watch (`MAKIBA_PRWATCH_INTERVAL_MS`).
const TEN_INTERVALS: Duration = Duration::from_millis(500);

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<r> --json ...: the file pr-<n>.json of the repository's
# folder. gh api graphql -f query=... -f owner=<o> -f name=<r> -F number=<n>: the same file in
# data.repository.pullRequest. gh api --paginate repos/<o>/<r>/...: the CI reads of a head, one
# check still running. Every call is logged whole, one line, in the repository's folder (calls).
all="$*"
repo=; num=
case "$1 $2" in
  "pr view")
    num=$3; shift 3
    while [ $# -gt 0 ]; do
      case "$1" in
        --repo) repo=${2#github.com/}; shift 2 ;;
        --json) shift 2 ;;
        *) echo "unexpected argument: $1" >&2; exit 2 ;;
      esac
    done ;;
  "api graphql")
    owner=; name=
    for a in "$@"; do
      case "$a" in
        owner=*) owner=${a#owner=} ;;
        name=*) name=${a#name=} ;;
        number=*) num=${a#number=} ;;
      esac
    done
    repo="$owner/$name" ;;
  "api --paginate")
    path=$3
    rest=${path#repos/}
    owner=${rest%%/*}; rest=${rest#*/}
    repo="$owner/${rest%%/*}" ;;
  *) echo "unexpected call: $all" >&2; exit 2 ;;
esac
case "$repo" in ?*/?*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
dir="$FAKE_GH_DIR/${repo%%/*}_${repo#*/}"
mkdir -p "$dir"
echo "$all" >> "$dir/calls"
case "$all" in
  "pr view"*|"api graphql"*)
    f="$dir/pr-$num.json"
    if [ ! -e "$f" ]; then echo "not found" >&2; exit 1; fi
    body=$(cat "$f")
    if mv "$dir/hold-$num" "$dir/held-$num" 2>/dev/null; then
      while [ ! -e "$dir/go-$num" ]; do sleep 0.02; done
    fi
    case "$all" in
      "pr view"*) printf '%s\n' "$body" ;;
      *) printf '{"data":{"repository":{"pullRequest":%s}}}\n' "$body" ;;
    esac ;;
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
        common::exe::write(&script, STAND_IN);
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

/// The next read of `repo#pr` takes its answer at its start and waits for `release`.
fn hold(home: &Path, repo: &str, pr: i64) {
    std::fs::write(answers(home, repo).join(format!("hold-{pr}")), "").unwrap();
}

/// Whether the held read of `repo#pr` has begun.
fn held(home: &Path, repo: &str, pr: i64) -> bool {
    answers(home, repo).join(format!("held-{pr}")).exists()
}

fn release(home: &Path, repo: &str, pr: i64) {
    std::fs::write(answers(home, repo).join(format!("go-{pr}")), "").unwrap();
}

/// A timeline item of the queue: `kind` `AddedToMergeQueueEvent` or `RemovedFromMergeQueueEvent`.
fn item(kind: &str, at: &str) -> Value {
    json!({"__typename": kind, "createdAt": at})
}

/// A pull request read open at `head`, in the merge queue or not, with its queue timeline.
fn open(head: &str, queued: bool, timeline: &[Value]) -> Value {
    json!({"state": "OPEN", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": "T", "mergeable": "MERGEABLE",
           "mergeStateStatus": "CLEAN", "isDraft": false, "isInMergeQueue": queued,
           "mergeQueueEntry": if queued { json!({"id": "MQE_1", "state": "QUEUED", "position": 1}) }
                              else { Value::Null },
           "timelineItems": {"nodes": timeline}})
}

/// A pull request read closed unmerged at `head`.
fn closed(head: &str) -> Value {
    json!({"state": "CLOSED", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": "T", "isInMergeQueue": false, "mergeQueueEntry": null,
           "timelineItems": {"nodes": []}})
}

/// A pull request read merged by `owner` at `head` with the merge commit `merge`.
fn merged(head: &str, merge: &str, timeline: &[Value]) -> Value {
    json!({"state": "MERGED", "mergedAt": MERGED_AT, "mergedBy": {"login": "owner"},
           "headRefOid": head, "mergeCommit": {"oid": merge}, "title": "T",
           "isInMergeQueue": false, "mergeQueueEntry": null,
           "timelineItems": {"nodes": timeline}})
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

/// How many state reads of `repo` were made, in either form.
fn state_reads(home: &Path, repo: &str) -> usize {
    count_calls(home, repo, "pr view") + count_calls(home, repo, "api graphql")
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

    /// A run of `task` with `status`, closed at `completed_ago` (an SQLite time modifier from
    /// now) when given.
    fn run(&self, task: &str, status: &str, completed_ago: Option<&str>) {
        self.w
            .conn()
            .execute(
                "INSERT INTO runs (task_id, project_id, pattern, title, status, completed_at)
                 VALUES (?1, ?2, 'B', ?1, ?3,
                         CASE WHEN ?4 IS NULL THEN NULL
                              ELSE strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4) END)",
                rusqlite::params![task, self.project, status, completed_ago],
            )
            .unwrap();
    }

    fn register(&self, task: &str, repo: &str, pr: i64, head: &str) {
        prwatch::register(&self.w, "pr_opened", task, repo, pr, head).unwrap();
    }

    fn event(&self, kind: &str, actor: &str, task: &str, payload: Value) -> i64 {
        self.w
            .append_event(&NewEvent {
                kind,
                actor: Some(actor),
                payload: Some(&payload),
                run_task_id: Some(task),
                ..Default::default()
            })
            .unwrap()
    }

    /// An allowance of the gate on `repo#pr` at `head`.
    fn allow(&self, task: &str, repo: &str, pr: i64, head: &str) {
        self.event(
            "acceptance_gate",
            "gate",
            task,
            json!({"repo": repo, "pr": pr, "base": "main", "head": head, "result": "passed",
                   "reason": "r", "mode": "acceptance", "allow": true}),
        );
    }

    /// makibad's attempt to land `repo#pr` at `head`, as the door's `merge` writes it: its id.
    fn attempt(&self, task: &str, repo: &str, pr: i64, head: &str) -> i64 {
        self.event(
            "merge_attempted",
            "makibad",
            task,
            json!({"repo": repo, "pr": pr, "head": head, "method": "squash",
                   "gate_event": null, "command": null, "approved_by": null,
                   "boot": "0".repeat(32), "task_id": task}),
        )
    }

    /// The outcome `kind` of `attempt`, as makibad's merge pass writes it, with `more`.
    fn outcome(&self, task: &str, kind: &str, attempt: i64, more: Value) -> i64 {
        let mut payload = json!({"attempt": attempt});
        for (k, v) in more.as_object().unwrap() {
            payload[k] = v.clone();
        }
        self.event(kind, "makibad", task, payload)
    }

    fn done(&self, task: &str, attempt: i64) {
        self.outcome(
            task,
            "merge_done",
            attempt,
            json!({"merge_commit": "9".repeat(40), "answer": "Pull Request successfully merged"}),
        );
    }

    fn enqueued(&self, task: &str, attempt: i64) {
        self.outcome(
            task,
            "merge_enqueued",
            attempt,
            json!({"entry": {"id": "MQE_1", "state": "QUEUED", "position": 1}}),
        );
    }

    /// The ledger time of event `id`, moved by `shift` (an SQLite modifier), as GitHub writes a
    /// time.
    fn time_of(&self, id: i64, shift: &str) -> String {
        self.w
            .conn()
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', occurred_at, ?2) FROM events WHERE id = ?1",
                rusqlite::params![id, shift],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The payloads of every event `kind` of `task`, oldest first.
    fn events(&self, kind: &str, task: &str) -> Vec<Value> {
        self.events_with_ids(kind, task)
            .into_iter()
            .map(|(_, v)| v)
            .collect()
    }

    fn events_with_ids(&self, kind: &str, task: &str) -> Vec<(i64, Value)> {
        let mut q = self
            .w
            .conn()
            .prepare(
                "SELECT e.id, e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = ?1 AND r.task_id = ?2 ORDER BY e.id",
            )
            .unwrap();
        q.query_map([kind, task], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|p| {
            let (id, text) = p.unwrap();
            (id, serde_json::from_str(&text).unwrap())
        })
        .collect()
    }

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

    /// The one `pr_merged` of `task`, once written.
    fn landing(&mut self, task: &str) -> Value {
        self.pass_until("the landing", |o| !o.events("pr_merged", task).is_empty());
        let all = self.events("pr_merged", task);
        assert_eq!(all.len(), 1, "{all:?}");
        all[0].clone()
    }
}

/// An allowance of the gate at the merged head, with no act of makibad: `hand`, and the
/// `pr_merged` names no attempt.
#[test]
fn an_allowance_alone_gives_hand() {
    let home = init();
    let mut org = Org::new();
    let (repo, head, merge) = ("o/l1", "1".repeat(40), "a".repeat(40));
    org.run("t1", "in_use", None);
    answer(&home, repo, 1, &merged(&head, &merge, &[]));
    org.register("t1", repo, 1, &head);
    org.allow("t1", repo, 1, &head);

    let m = org.landing("t1");
    assert_eq!(m["merged_via"], "hand", "{m}");
    assert!(m.get("attempt").is_none(), "{m}");
}

/// makibad's own merge (`merge_done` at the merged head) is `gate` and names its attempt; a merge
/// whose answer could not be read (`merge_unknown` observed `merged`) is `unknown`. Each notice
/// says which.
#[test]
fn makibads_direct_merge_gives_gate_and_an_unknown_outcome_gives_unknown() {
    let home = init();
    let mut org = Org::new();
    let repo = "o/l2";
    let (ha, hb, ma, mb) = (
        "2".repeat(40),
        "3".repeat(40),
        "b".repeat(40),
        "c".repeat(40),
    );
    org.run("t2a", "in_use", None);
    org.run("t2b", "in_use", None);
    org.register("t2a", repo, 2, &ha);
    org.register("t2b", repo, 3, &hb);
    let a = org.attempt("t2a", repo, 2, &ha);
    org.done("t2a", a);
    let b = org.attempt("t2b", repo, 3, &hb);
    org.outcome(
        "t2b",
        "merge_unknown",
        b,
        json!({"observed": "merged", "actor": "unknown", "state": null,
               "why": "GitHub answered 502"}),
    );
    answer(&home, repo, 2, &merged(&ha, &ma, &[]));
    answer(&home, repo, 3, &merged(&hb, &mb, &[]));

    let m = org.landing("t2a");
    assert_eq!(m["merged_via"], "gate", "{m}");
    assert_eq!(m["attempt"], json!(a), "{m}");
    let m = org.landing("t2b");
    assert_eq!(m["merged_via"], "unknown", "{m}");
    assert_eq!(m["attempt"], json!(b), "{m}");

    org.pass_for(Duration::from_millis(200));
    let gate = org.notices_naming("o/l2#2 ");
    assert_eq!(gate.len(), 1, "{gate:?}");
    assert!(gate[0].contains("landed (makibad's merge)"), "{gate:?}");
    let unknown = org.notices_naming("o/l2#3 ");
    assert_eq!(unknown.len(), 1, "{unknown:?}");
    assert!(
        unknown[0].contains("cannot tell whether its attempt or another merge landed it"),
        "{unknown:?}"
    );
}

/// A queued landing whose timeline shows one addition after makibad's attempt and no removal is
/// `gate`; one whose timeline shows the entry removed and added again is `unknown`. Both were read
/// through GitHub's GraphQL API, which carries the queue.
#[test]
fn an_uninterrupted_queued_landing_gives_gate_and_a_re_added_one_unknown() {
    let home = init();
    let mut org = Org::new();
    let repo = "o/l3";
    let (ha, hb, ma, mb) = (
        "4".repeat(40),
        "5".repeat(40),
        "d".repeat(40),
        "e".repeat(40),
    );
    org.run("t3a", "in_use", None);
    org.run("t3b", "in_use", None);
    org.register("t3a", repo, 4, &ha);
    org.register("t3b", repo, 5, &hb);
    let a = org.attempt("t3a", repo, 4, &ha);
    org.enqueued("t3a", a);
    let b = org.attempt("t3b", repo, 5, &hb);
    org.enqueued("t3b", b);
    let once = [item(
        "AddedToMergeQueueEvent",
        &org.time_of(a, "+2 seconds"),
    )];
    let again = [
        item("AddedToMergeQueueEvent", &org.time_of(b, "+2 seconds")),
        item("RemovedFromMergeQueueEvent", &org.time_of(b, "+3 seconds")),
        item("AddedToMergeQueueEvent", &org.time_of(b, "+4 seconds")),
    ];
    answer(&home, repo, 4, &merged(&ha, &ma, &once));
    answer(&home, repo, 5, &merged(&hb, &mb, &again));

    let m = org.landing("t3a");
    assert_eq!(m["merged_via"], "gate", "{m}");
    assert_eq!(m["attempt"], json!(a), "{m}");
    let m = org.landing("t3b");
    assert_eq!(m["merged_via"], "unknown", "{m}");
    assert!(
        count_calls(&home, repo, "api graphql") > 0,
        "{:?}",
        calls(&home, repo)
    );
}

/// A run completed two days ago is no longer followed by its age, but while its pull request has
/// an act awaiting its landing (a direct merge recorded, or a queue entry) it is still read, and
/// its landing recorded. One whose act ended without a landing (`merge_failed`) is not read.
#[test]
fn a_completed_run_older_than_a_day_is_still_read_while_an_act_awaits_its_landing() {
    let home = init();
    let mut org = Org::new();
    let (direct, queued, failed) = ("o/l4a", "o/l4b", "o/l4c");
    let (ha, hb, hc) = ("6".repeat(40), "7".repeat(40), "8".repeat(40));
    for t in ["t4a", "t4b", "t4c"] {
        org.run(t, "completed", Some("-2 days"));
    }
    org.register("t4a", direct, 6, &ha);
    org.register("t4b", queued, 7, &hb);
    org.register("t4c", failed, 8, &hc);
    let a = org.attempt("t4a", direct, 6, &ha);
    org.done("t4a", a);
    let b = org.attempt("t4b", queued, 7, &hb);
    org.enqueued("t4b", b);
    let c = org.attempt("t4c", failed, 8, &hc);
    org.outcome(
        "t4c",
        "merge_failed",
        c,
        json!({"sent": true, "exit": 1, "http_status": 405, "stderr": "", "state": null,
               "why": "405: not mergeable"}),
    );
    let once = [item(
        "AddedToMergeQueueEvent",
        &org.time_of(b, "+2 seconds"),
    )];
    answer(&home, direct, 6, &merged(&ha, &"f".repeat(40), &[]));
    answer(&home, queued, 7, &merged(&hb, &"a".repeat(40), &once));
    answer(&home, failed, 8, &merged(&hc, &"b".repeat(40), &[]));

    assert_eq!(org.landing("t4a")["merged_via"], "gate");
    assert_eq!(org.landing("t4b")["merged_via"], "gate");
    org.pass_for(TEN_INTERVALS);
    assert!(org.events("pr_merged", "t4c").is_empty());
    assert_eq!(state_reads(&home, failed), 0, "{:?}", calls(&home, failed));
}

/// While makibad's attempt has no outcome, a read of the pull request merged is not written (it
/// would be `hand`, the watch having read it before the act's own answer was recorded); once the
/// outcome is recorded, the next read records the landing as makibad's.
#[test]
fn a_merged_read_while_the_attempt_is_open_is_held() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/l5", "9".repeat(40));
    org.run("t5", "in_use", None);
    org.register("t5", repo, 9, &head);
    let a = org.attempt("t5", repo, 9, &head);
    answer(&home, repo, 9, &merged(&head, &"c".repeat(40), &[]));

    org.pass_until("the pull request read", |_| state_reads(&home, repo) > 0);
    org.pass_for(TEN_INTERVALS);
    assert!(
        org.events("pr_merged", "t5").is_empty(),
        "{:?}",
        org.events("pr_merged", "t5")
    );

    org.done("t5", a);
    let m = org.landing("t5");
    assert_eq!(m["merged_via"], "gate", "{m}");
    assert_eq!(m["attempt"], json!(a), "{m}");
}

/// A read begun while the enqueue's outcome was not yet recorded, showing the pull request at an
/// older head and out of the queue, comes back after `merge_enqueued`: it is dropped, so it ends
/// nothing, and the queued landing is still makibad's.
#[test]
fn a_delayed_read_at_the_old_head_does_not_end_a_queue_act() {
    let home = init();
    let mut org = Org::new();
    let (repo, old, head) = ("o/l6", "a".repeat(40), "b".repeat(40));
    org.run("t6", "in_use", None);
    org.register("t6", repo, 10, &head);
    let a = org.attempt("t6", repo, 10, &head);
    answer(&home, repo, 10, &open(&old, false, &[]));
    hold(&home, repo, 10);

    org.pass_until("the held read begun", |_| held(&home, repo, 10));
    org.enqueued("t6", a);
    let once = [item(
        "AddedToMergeQueueEvent",
        &org.time_of(a, "+2 seconds"),
    )];
    answer(&home, repo, 10, &open(&head, true, &once));
    release(&home, repo, 10);
    org.pass_for(TEN_INTERVALS);
    assert!(
        org.events("merge_dequeued", "t6").is_empty(),
        "{:?}",
        org.events("merge_dequeued", "t6")
    );

    answer(&home, repo, 10, &merged(&head, &"d".repeat(40), &once));
    let m = org.landing("t6");
    assert_eq!(m["merged_via"], "gate", "{m}");
}

/// A queue act ends with one `merge_dequeued` and one notice each: when the pull request is read
/// out of the queue at its head with a removal on its timeline (`left the queue`), when another
/// head is read (`head moved`), and when it is read closed (`closed`, before its `pr_closed`).
#[test]
fn a_queue_entry_removed_writes_one_dequeue_and_one_notice() {
    let home = init();
    let mut org = Org::new();
    let repo = "o/l7";
    let (ha, hb, hb2, hc) = (
        "c".repeat(40),
        "d".repeat(40),
        "e".repeat(40),
        "f".repeat(40),
    );
    for t in ["t7a", "t7b", "t7c"] {
        org.run(t, "in_use", None);
    }
    org.register("t7a", repo, 11, &ha);
    org.register("t7b", repo, 12, &hb);
    org.register("t7c", repo, 13, &hc);
    let a = org.attempt("t7a", repo, 11, &ha);
    org.enqueued("t7a", a);
    let b = org.attempt("t7b", repo, 12, &hb);
    org.enqueued("t7b", b);
    let c = org.attempt("t7c", repo, 13, &hc);
    org.enqueued("t7c", c);
    let removed = [
        item("AddedToMergeQueueEvent", &org.time_of(a, "+2 seconds")),
        item("RemovedFromMergeQueueEvent", &org.time_of(a, "+3 seconds")),
    ];
    answer(&home, repo, 11, &open(&ha, false, &removed));
    answer(&home, repo, 12, &open(&hb2, false, &[]));
    answer(&home, repo, 13, &closed(&hc));

    org.pass_until("the three queue acts ended", |o| {
        ["t7a", "t7b", "t7c"]
            .iter()
            .all(|t| !o.events("merge_dequeued", t).is_empty())
    });
    org.pass_for(TEN_INTERVALS);

    let left = org.events("merge_dequeued", "t7a");
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["why"], "left the queue", "{left:?}");
    assert_eq!(left[0]["attempt"], json!(a), "{left:?}");
    let told = org.notices_naming("left the merge queue without landing");
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("o/l7#11"), "{told:?}");

    let moved = org.events("merge_dequeued", "t7b");
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(moved[0]["why"], "head moved", "{moved:?}");
    assert_eq!(moved[0]["attempt"], json!(b), "{moved:?}");

    let ended = org.events_with_ids("merge_dequeued", "t7c");
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].1["why"], "closed", "{ended:?}");
    assert_eq!(ended[0].1["attempt"], json!(c), "{ended:?}");
    let closes = org.events_with_ids("pr_closed", "t7c");
    assert_eq!(closes.len(), 1, "{closes:?}");
    assert!(ended[0].0 < closes[0].0, "{ended:?} {closes:?}");
}

/// A pull request read out of the queue at its head with no removal on its timeline (it may be
/// on its way to landing) keeps its queue act: nothing ends, and the landing is makibad's.
#[test]
fn a_queue_absence_without_a_removal_keeps_the_act() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/l8", "1".repeat(39) + "2");
    org.run("t8", "in_use", None);
    org.register("t8", repo, 14, &head);
    let a = org.attempt("t8", repo, 14, &head);
    org.enqueued("t8", a);
    let once = [item(
        "AddedToMergeQueueEvent",
        &org.time_of(a, "+2 seconds"),
    )];
    answer(&home, repo, 14, &open(&head, false, &once));

    org.pass_until("the pull request read", |_| state_reads(&home, repo) > 2);
    org.pass_for(TEN_INTERVALS * 2);
    assert!(
        org.events("merge_dequeued", "t8").is_empty(),
        "{:?}",
        org.events("merge_dequeued", "t8")
    );
    assert!(org.notices_naming("left the merge queue").is_empty());

    answer(&home, repo, 14, &merged(&head, &"3".repeat(40), &once));
    let m = org.landing("t8");
    assert_eq!(m["merged_via"], "gate", "{m}");
}
