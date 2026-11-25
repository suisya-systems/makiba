//! Issue 476 (docs/design/merge-queue-watch.md): the PR watch tells when a pull request leaves the
//! merge queue without landing, whoever queued it.
//!
//! - Per repository, one GraphQL read (the membership read, aliases `p<i>` for the numbers
//!   `-F n<i>=<number>`) asks whether each followed open pull request with no act of makibad and
//!   no open hand stay is in the queue; one read in it gets one `merge_queue_entered` (`attempt`
//!   null), and from then on Issue 434's reader follows it.
//! - A hand stay that leaves the queue open (the newest queue item of the timeline a removal) gets
//!   one `merge_dequeued` (`attempt` null, `reason` GitHub's) and one notice naming the pull
//!   request, its head and the reason; one read out of the queue and then merged gets none.
//! - A restart writes nothing again; a failed attempt of makibad does not end a hand stay; a
//!   membership read delivered after the exit still gives one entry and one exit; makibad's own
//!   queue act is never entered again, and its `merge_dequeued` names the reason too.
//!
//! The PR watch's pass runs in this process on a state database of its own (makibad is not
//! started). `gh` is a stand-in script (`MAKIBA_GH`) that answers `gh pr view`, the state read
//! over GraphQL (`-F number=<n>`, answered in `data.repository.pullRequest`) and the membership
//! read (`-F n<i>=<n>`, answered in `data.repository.p<i>`) from one file per pull request, answers
//! the CI reads of any head with one check still running, and logs every call per repository.
//! When `mhold-<n>` exists, the next membership read naming pull request `<n>` takes its answer at
//! its start and prints it only once `mgo-<n>` exists. GitHub's timeline items and their `reason`
//! (merge-act.md section 10, claim 5) are those of the stand-in: [unverified].

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
const LEFT: &str = "left the merge queue without landing";

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<r> --json ...: the file pr-<n>.json of the repository's
# folder. gh api graphql ... -f owner=<o> -f name=<r> -F number=<n>: the same file in
# data.repository.pullRequest. gh api graphql ... -F n<i>=<n> ...: each file in
# data.repository.p<i> (null when absent). gh api --paginate repos/<o>/<r>/...: the CI reads of a
# head, one check still running. Every call is logged whole, one line, in the repository's folder.
all="$*"
repo=; num=; members=
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
        n[0-9]*=*) members="$members ${a}" ;;
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
  "api graphql"*)
    if [ -z "$num" ] && [ -n "$members" ]; then
      out=; sep=; waits=
      for m in $members; do
        key=${m%%=*}; n=${m#*=}
        f="$dir/pr-$n.json"
        if [ -e "$f" ]; then body=$(cat "$f"); else body=null; fi
        out="$out$sep\"p${key#n}\":$body"; sep=,
        if mv "$dir/mhold-$n" "$dir/mheld-$n" 2>/dev/null; then waits="$waits $n"; fi
      done
      for n in $waits; do
        while [ ! -e "$dir/mgo-$n" ]; do sleep 0.02; done
      done
      printf '{"data":{"repository":{%s}}}\n' "$out"
      exit 0
    fi ;;
esac
case "$all" in
  "pr view"*|"api graphql"*)
    f="$dir/pr-$num.json"
    if [ ! -e "$f" ]; then echo "not found" >&2; exit 1; fi
    body=$(cat "$f")
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

/// The next membership read naming `repo#pr` takes its answer at its start and waits for
/// `release`.
fn hold_membership(home: &Path, repo: &str, pr: i64) {
    std::fs::write(answers(home, repo).join(format!("mhold-{pr}")), "").unwrap();
}

/// Whether the held membership read of `repo#pr` has begun.
fn membership_held(home: &Path, repo: &str, pr: i64) -> bool {
    answers(home, repo).join(format!("mheld-{pr}")).exists()
}

fn release_membership(home: &Path, repo: &str, pr: i64) {
    std::fs::write(answers(home, repo).join(format!("mgo-{pr}")), "").unwrap();
}

/// The calls the stand-in logged for `repo`.
fn calls(home: &Path, repo: &str) -> Vec<String> {
    std::fs::read_to_string(answers(home, repo).join("calls"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// GitHub's time `secs` seconds before now.
fn ago(secs: u64) -> String {
    let t = std::time::SystemTime::now() - Duration::from_secs(secs);
    let s = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let (days, rest) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    // The civil date of `days` since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// An addition to the queue at `at`.
fn added(at: &str) -> Value {
    json!({"__typename": "AddedToMergeQueueEvent", "createdAt": at})
}

/// A removal from the queue at `at`, with GitHub's `reason`.
fn removed(at: &str, reason: &str) -> Value {
    json!({"__typename": "RemovedFromMergeQueueEvent", "createdAt": at, "reason": reason})
}

/// A pull request read open at `head`, in the merge queue or not, with its queue timeline.
fn open(pr: i64, head: &str, queued: bool, timeline: &[Value]) -> Value {
    json!({"number": pr, "state": "OPEN", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": "T", "mergeable": "MERGEABLE",
           "mergeStateStatus": "CLEAN", "isDraft": false, "isInMergeQueue": queued,
           "mergeQueueEntry": if queued { json!({"id": "MQE_9", "state": "QUEUED", "position": 1}) }
                              else { Value::Null },
           "timelineItems": {"nodes": timeline}})
}

/// A pull request read merged by `owner` at `head` with the merge commit `merge`.
fn merged(pr: i64, head: &str, merge: &str, timeline: &[Value]) -> Value {
    json!({"number": pr, "state": "MERGED", "mergedAt": MERGED_AT, "mergedBy": {"login": "owner"},
           "headRefOid": head, "mergeCommit": {"oid": merge}, "title": "T",
           "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN", "isDraft": false,
           "isInMergeQueue": false, "mergeQueueEntry": null,
           "timelineItems": {"nodes": timeline}})
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

    /// An open run of `task` whose pull request `repo#pr` is recorded at `head`.
    fn follow(&self, task: &str, repo: &str, pr: i64, head: &str) {
        self.w
            .conn()
            .execute(
                "INSERT INTO runs (task_id, project_id, pattern, title, status)
                 VALUES (?1, ?2, 'B', ?1, 'in_use')",
                rusqlite::params![task, self.project],
            )
            .unwrap();
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
    fn outcome(&self, task: &str, kind: &str, attempt: i64, more: Value) {
        let mut payload = json!({"attempt": attempt});
        for (k, v) in more.as_object().unwrap() {
            payload[k] = v.clone();
        }
        self.event(kind, "makibad", task, payload);
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

    /// Run until `task` has its `merge_queue_entered`.
    fn until_entered(&mut self, task: &str) {
        self.pass_until("the entry to the queue", |o| {
            !o.events("merge_queue_entered", task).is_empty()
        });
    }

    /// Run until `task` has its `merge_dequeued`.
    fn until_dequeued(&mut self, task: &str) {
        self.pass_until("the exit from the queue", |o| {
            !o.events("merge_dequeued", task).is_empty()
        });
    }
}

/// A pull request the person queued, read in the queue, then out of it still open with a removal
/// (`FAILED_CHECKS`) after its addition: one entry, one exit naming the reason, one notice.
#[test]
fn a_pull_request_queued_by_hand_that_leaves_open_gets_one_dequeue_and_one_notice() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/q1", "1".repeat(40));
    org.follow("t1", repo, 1, &head);
    answer(&home, repo, 1, &open(1, &head, true, &[added(&ago(30))]));
    org.until_entered("t1");

    answer(
        &home,
        repo,
        1,
        &open(
            1,
            &head,
            false,
            &[added(&ago(30)), removed(&ago(10), "FAILED_CHECKS")],
        ),
    );
    org.until_dequeued("t1");
    org.pass_for(TEN_INTERVALS);

    let entered = org.events("merge_queue_entered", "t1");
    assert_eq!(entered.len(), 1, "{entered:?}");
    assert_eq!(entered[0]["attempt"], Value::Null, "{entered:?}");
    assert_eq!(entered[0]["head"], json!(head), "{entered:?}");
    let left = org.events("merge_dequeued", "t1");
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["why"], "left the queue", "{left:?}");
    assert_eq!(left[0]["attempt"], Value::Null, "{left:?}");
    assert_eq!(left[0]["head"], json!(head), "{left:?}");
    assert_eq!(left[0]["reason"], "FAILED_CHECKS", "{left:?}");
    let told = org.notices_naming(LEFT);
    assert_eq!(told.len(), 1, "{told:?}");
    for want in ["o/q1#1", head.as_str(), "FAILED_CHECKS"] {
        assert!(told[0].contains(want), "{want:?} not in {told:?}");
    }
}

/// A pull request the person queued, read out of the queue with no removal for ten intervals (it
/// is landing), then read merged: no exit and no notice of one, and the landing is `hand`.
#[test]
fn a_pull_request_read_out_of_the_queue_then_merged_is_not_dequeued() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/q2", "2".repeat(40));
    org.follow("t2", repo, 2, &head);
    answer(&home, repo, 2, &open(2, &head, true, &[added(&ago(30))]));
    org.until_entered("t2");

    answer(&home, repo, 2, &open(2, &head, false, &[added(&ago(30))]));
    org.pass_for(TEN_INTERVALS);
    answer(
        &home,
        repo,
        2,
        &merged(2, &head, &"a".repeat(40), &[added(&ago(30))]),
    );
    org.pass_until("the landing", |o| !o.events("pr_merged", "t2").is_empty());
    org.pass_for(TEN_INTERVALS);

    assert_eq!(org.events("merge_queue_entered", "t2").len(), 1);
    let left = org.events("merge_dequeued", "t2");
    assert!(left.is_empty(), "{left:?}");
    let told = org.notices_naming("left the merge queue");
    assert!(told.is_empty(), "{told:?}");
    let landed = org.events("pr_merged", "t2");
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert_eq!(landed[0]["merged_via"], "hand", "{landed:?}");
}

/// After a restart (a new PR watch on the same ledger and the same answers) nothing is written
/// again: an exit already recorded is not recorded again, and a stay still open is not entered
/// again.
#[test]
fn a_restart_writes_nothing_again() {
    let home = init();
    let mut org = Org::new();
    let repo = "o/q3";
    let (ha, hb) = ("3".repeat(40), "4".repeat(40));
    org.follow("t3a", repo, 3, &ha);
    org.follow("t3b", repo, 4, &hb);
    answer(&home, repo, 3, &open(3, &ha, true, &[added(&ago(30))]));
    answer(&home, repo, 4, &open(4, &hb, true, &[added(&ago(30))]));
    org.until_entered("t3a");
    org.until_entered("t3b");
    answer(
        &home,
        repo,
        3,
        &open(
            3,
            &ha,
            false,
            &[added(&ago(30)), removed(&ago(10), "FAILED_CHECKS")],
        ),
    );
    org.until_dequeued("t3a");

    drop(std::mem::replace(&mut org.watch, PrWatch::from_env()));
    org.pass_for(TEN_INTERVALS);

    assert_eq!(org.events("merge_queue_entered", "t3a").len(), 1);
    assert_eq!(org.events("merge_dequeued", "t3a").len(), 1);
    assert_eq!(org.events("merge_queue_entered", "t3b").len(), 1);
    let left = org.events("merge_dequeued", "t3b");
    assert!(left.is_empty(), "{left:?}");
    let told = org.notices_naming(LEFT);
    assert_eq!(told.len(), 1, "{told:?}");
}

/// Two followed open pull requests of one repository, neither in the queue: one membership read
/// names both, and nothing is entered.
#[test]
fn the_membership_of_a_repositorys_pull_requests_is_one_read() {
    let home = init();
    let mut org = Org::new();
    let repo = "o/q4";
    let (ha, hb) = ("5".repeat(40), "6".repeat(40));
    org.follow("t4a", repo, 41, &ha);
    org.follow("t4b", repo, 42, &hb);
    answer(&home, repo, 41, &open(41, &ha, false, &[]));
    answer(&home, repo, 42, &open(42, &hb, false, &[]));

    let both = |c: &String| {
        c.starts_with("api graphql")
            && ((c.contains("n0=41") && c.contains("n1=42"))
                || (c.contains("n0=42") && c.contains("n1=41")))
    };
    org.pass_until("one membership read of both", |_| {
        calls(&home, repo).iter().any(both)
    });
    org.pass_for(TEN_INTERVALS);
    assert!(org.events("merge_queue_entered", "t4a").is_empty());
    assert!(org.events("merge_queue_entered", "t4b").is_empty());
}

/// makibad's own queue act is followed by Issue 434's reader, never entered as a hand stay, and its
/// `merge_dequeued` and notice name GitHub's reason.
#[test]
fn makibads_own_queue_act_is_not_entered_again_and_its_dequeue_names_the_reason() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/q5", "7".repeat(40));
    org.follow("t5", repo, 5, &head);
    let a = org.attempt("t5", repo, 5, &head);
    org.outcome(
        "t5",
        "merge_enqueued",
        a,
        json!({"entry": {"id": "MQE_9", "state": "QUEUED", "position": 1}}),
    );
    let add = added(&org.time_of(a, "+2 seconds"));
    answer(
        &home,
        repo,
        5,
        &open(5, &head, true, std::slice::from_ref(&add)),
    );
    org.pass_for(TEN_INTERVALS);
    answer(
        &home,
        repo,
        5,
        &open(
            5,
            &head,
            false,
            &[
                add,
                removed(&org.time_of(a, "+3 seconds"), "MERGE_CONFLICT"),
            ],
        ),
    );
    org.until_dequeued("t5");
    org.pass_for(TEN_INTERVALS);

    let entered = org.events("merge_queue_entered", "t5");
    assert!(entered.is_empty(), "{entered:?}");
    let left = org.events("merge_dequeued", "t5");
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["attempt"], json!(a), "{left:?}");
    assert_eq!(left[0]["reason"], "MERGE_CONFLICT", "{left:?}");
    let told = org.notices_naming(LEFT);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("MERGE_CONFLICT"), "{told:?}");
}

/// A hand stay outlives an attempt of makibad that failed before sending: the pull request is still
/// in the person's queue entry, and its exit is told.
#[test]
fn a_hand_stay_outlives_a_failed_attempt() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/q6", "8".repeat(40));
    org.follow("t6", repo, 6, &head);
    answer(&home, repo, 6, &open(6, &head, true, &[added(&ago(30))]));
    org.until_entered("t6");
    let a = org.attempt("t6", repo, 6, &head);
    org.outcome(
        "t6",
        "merge_failed",
        a,
        json!({"sent": false, "exit": null, "http_status": null, "stderr": "", "state": null,
               "why": "already in the merge queue"}),
    );

    answer(
        &home,
        repo,
        6,
        &open(
            6,
            &head,
            false,
            &[added(&ago(30)), removed(&ago(10), "FAILED_CHECKS")],
        ),
    );
    org.until_dequeued("t6");
    org.pass_for(TEN_INTERVALS);

    assert_eq!(org.events("merge_queue_entered", "t6").len(), 1);
    let left = org.events("merge_dequeued", "t6");
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["attempt"], Value::Null, "{left:?}");
    assert_eq!(left[0]["why"], "left the queue", "{left:?}");
    let told = org.notices_naming(LEFT);
    assert_eq!(told.len(), 1, "{told:?}");
    for want in ["o/q6#6", "FAILED_CHECKS"] {
        assert!(told[0].contains(want), "{want:?} not in {told:?}");
    }
}

/// A membership read that took its in-queue answer and is delivered only after the pull request
/// left the queue (its addition and removal more than a minute before the delivery) still gives
/// one entry, then one exit and one notice.
#[test]
fn a_membership_read_delivered_after_the_exit_gives_one_entry_and_one_exit() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/q7", "9".repeat(40));
    answer(&home, repo, 7, &open(7, &head, true, &[added(&ago(150))]));
    hold_membership(&home, repo, 7);
    org.follow("t7", repo, 7, &head);
    org.pass_until("the held membership read begun", |_| {
        membership_held(&home, repo, 7)
    });

    answer(
        &home,
        repo,
        7,
        &open(
            7,
            &head,
            false,
            &[added(&ago(150)), removed(&ago(90), "FAILED_CHECKS")],
        ),
    );
    release_membership(&home, repo, 7);
    org.until_entered("t7");
    org.until_dequeued("t7");
    org.pass_for(TEN_INTERVALS);

    assert_eq!(org.events("merge_queue_entered", "t7").len(), 1);
    let left = org.events("merge_dequeued", "t7");
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["why"], "left the queue", "{left:?}");
    assert_eq!(org.notices_naming(LEFT).len(), 1);
}
