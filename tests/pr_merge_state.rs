//! Issue 387: a green CI notice says whether the pull request can merge now.
//!
//! - The PR watch's state read (Issue 388, one call per pull request) also reads `mergeable`,
//!   `mergeStateStatus` and `isDraft`; no other call is added.
//! - The notice of a passed CI ends with the merge state of that head: behind its base, a
//!   conflict, blocked, a draft, mergeable now, or not known yet. An UNKNOWN or unreadable state
//!   does not hold the notice for good; an unfollowed pull request is said to be unread.
//! - A change of the merge state of a green head (main moved under it) is told once, also after
//!   makibad restarted following a "not known yet" notice.
//!
//! The PR watch's pass runs in this process on a state database of its own (makibad is not
//! started). `gh` is a stand-in script (`MAKIBA_GH`) that answers `gh pr view` from a file per
//! pull request, answers the CI reads of any head as passed, logs every call per repository and
//! refuses anything else.

mod common;

use makiba::daemon::prwatch::{self, PrWatch};
use makiba::db::writer::StateWriter;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);
/// Ten and more intervals of the PR watch (50 ms each).
const TEN_INTERVALS: Duration = Duration::from_millis(700);
const FIELDS: &str = "--json state,mergedAt,mergedBy,headRefOid,mergeCommit,title,mergeable,mergeStateStatus,isDraft";
const CHANGED: &str = "The merge state of ";

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<n> --json ...: the file pr-<n>.json of the repository's
# folder, else a failure. gh api --paginate repos/<o>/<n>/...: the CI reads of a head, all
# passed and finished. Every call is logged whole, one line, in the repository's folder (calls).
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
  *check-runs*) echo '{"name":"ci","status":"completed","conclusion":"success"}' ;;
  */status*) ;;
  *actions/runs*) echo completed ;;
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

/// The stand-in answers `repo#5` with `v` (written whole, then renamed into place).
fn answer(home: &Path, repo: &str, v: &Value) {
    let dir = answers(home, repo);
    let tmp = dir.join("pr-5.json.tmp");
    std::fs::write(&tmp, v.to_string()).unwrap();
    std::fs::rename(&tmp, dir.join("pr-5.json")).unwrap();
}

/// A pull request read open at `head` with the merge state `status`.
fn open(head: &str, status: &str, draft: bool) -> Value {
    let mergeable = match status {
        "DIRTY" => "CONFLICTING",
        "UNKNOWN" => "UNKNOWN",
        _ => "MERGEABLE",
    };
    json!({"state": "OPEN", "mergedAt": null, "mergedBy": null, "headRefOid": head,
           "mergeCommit": null, "title": "T", "mergeable": mergeable,
           "mergeStateStatus": status, "isDraft": draft})
}

/// The calls the stand-in logged for `repo`.
fn calls(home: &Path, repo: &str) -> Vec<String> {
    std::fs::read_to_string(answers(home, repo).join("calls"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
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

    /// A run of `task` with `status`, and its pull request `repo#5` opened at `head`.
    fn opened(&self, task: &str, status: &str, repo: &str, head: &str) {
        self.w
            .conn()
            .execute(
                "INSERT INTO runs (task_id, project_id, pattern, title, status)
                 VALUES (?1, ?2, 'B', ?1, ?3)",
                rusqlite::params![task, self.project, status],
            )
            .unwrap();
        prwatch::register(&self.w, "pr_opened", task, repo, 5, head).unwrap();
    }

    /// Whether a `ci_completed` `passed` of `head` is recorded.
    fn passed(&self, head: &str) -> bool {
        self.w
            .conn()
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'ci_completed'
                   AND json_extract(payload_json, '$.head_oid') = ?1
                   AND json_extract(payload_json, '$.status') = 'passed')",
                [head],
                |r| r.get(0),
            )
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

    /// The notices that start with `prefix`.
    fn starting(&self, prefix: &str) -> Vec<String> {
        self.notices()
            .into_iter()
            .filter(|b| b.starts_with(prefix))
            .collect()
    }

    /// The one CI-passed notice of `repo#5` at `head`.
    fn passed_notice(&self, repo: &str, head: &str) -> String {
        let n = self.starting(&passed_prefix(repo, head));
        assert_eq!(n.len(), 1, "one CI-passed notice of {repo} expected: {n:?}");
        n[0].clone()
    }

    /// The merge-state change notices of `repo#5` at `head`.
    fn changes(&self, repo: &str, head: &str) -> Vec<String> {
        self.starting(&format!("{CHANGED}{repo}#5 at {}", &head[..7]))
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

    /// Run the pass until the CI-passed notice of `repo#5` at `head` is written.
    fn until_passed_notice(&mut self, repo: &str, head: &str) -> String {
        let prefix = passed_prefix(repo, head);
        self.pass_until("the CI-passed notice", |o| {
            o.passed(head) && !o.starting(&prefix).is_empty()
        });
        self.passed_notice(repo, head)
    }
}

fn passed_prefix(repo: &str, head: &str) -> String {
    format!("CI passed for {repo}#5 at {}:", &head[..7])
}

/// The Issue's case: CI passed on a head that is behind its base, and the notice says so, and
/// that being behind alone is no reason to update it (Issue 475). The merge state comes from the
/// state read alone, with its fields; no other call is made.
#[test]
fn behind_head_passed_notice_names_it_behind() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/behind", "1".repeat(40));
    answer(&home, repo, &open(&head, "BEHIND", false));
    org.opened("t1", "review", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    for want in ["behind its base", "(BEHIND)", "no reason to update"] {
        assert!(body.contains(want), "{want:?} not in {body:?}");
    }
    let calls = calls(&home, repo);
    assert!(
        calls.iter().any(|c| c.starts_with("pr view 5")),
        "{calls:?}"
    );
    for c in &calls {
        if c.starts_with("pr view") {
            assert!(c.contains(FIELDS), "the state read lacks the fields: {c}");
        } else if c.starts_with("api graphql") {
            // The membership read of the merge queue (Issue 476).
            assert!(
                c.contains("isInMergeQueue"),
                "a GraphQL call other than it: {c}"
            );
        } else {
            assert!(
                c.starts_with("api --paginate repos/")
                    && ["check-runs", "/status", "actions/runs"]
                        .iter()
                        .any(|k| c.contains(k)),
                "a call other than the state and CI reads: {c}"
            );
        }
    }
}

/// CLEAN, DIRTY and BLOCKED are each named in the notice.
#[test]
fn clean_dirty_blocked_are_named() {
    let home = init();
    let mut org = Org::new();
    let cases = [
        ("o/clean", "2", "CLEAN", vec!["mergeable now (CLEAN)"]),
        (
            "o/dirty",
            "3",
            "DIRTY",
            vec!["conflicts with its base", "(DIRTY)"],
        ),
        ("o/blocked", "4", "BLOCKED", vec!["(BLOCKED)"]),
    ];
    for (i, (repo, c, status, _)) in cases.iter().enumerate() {
        let head = c.repeat(40);
        answer(&home, repo, &open(&head, status, false));
        org.opened(&format!("t{i}"), "review", repo, &head);
    }
    for (repo, c, _, wants) in &cases {
        let head = c.repeat(40);
        let body = org.until_passed_notice(repo, &head);
        for want in wants {
            assert!(body.contains(want), "{want:?} not in {body:?}");
        }
        assert!(!body.contains("behind"), "{body:?}");
    }
}

/// A draft is named a draft, whatever its merge state reads.
#[test]
fn a_draft_is_named_even_when_clean() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/draft", "5".repeat(40));
    answer(&home, repo, &open(&head, "CLEAN", true));
    org.opened("t1", "review", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    assert!(body.contains("is a draft"), "{body:?}");
    assert!(body.contains("(DRAFT)"), "{body:?}");
    assert!(!body.contains("mergeable now"), "{body:?}");
}

/// A state GitHub has not computed does not hold the notice for good; once it is computed, the
/// secretary is told once.
#[test]
fn unknown_does_not_hold_the_notice_forever() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/unknown", "6".repeat(40));
    answer(&home, repo, &open(&head, "UNKNOWN", false));
    org.opened("t1", "review", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    assert!(body.contains("(UNKNOWN)"), "{body:?}");

    answer(&home, repo, &open(&head, "BEHIND", false));
    org.pass_until("the merge state change", |o| {
        !o.changes(repo, &head).is_empty()
    });
    org.pass_for(TEN_INTERVALS);
    let changes = org.changes(repo, &head);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(changes[0].contains("(BEHIND)"), "{changes:?}");
}

/// A state that cannot be read does not hold the notice.
#[test]
fn an_unread_state_does_not_hold_the_notice() {
    let _home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/unread", "7".repeat(40));
    org.opened("t1", "review", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    assert!(body.contains("(not read)"), "{body:?}");
    assert!(body.contains("tells you"), "{body:?}");
    assert!(org.passed(&head));
}

/// A pull request makibad does not follow is said to be unread, with the read to make.
#[test]
fn an_unfollowed_pull_request_is_said_so() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/unfollowed", "8".repeat(40));
    answer(&home, repo, &open(&head, "BEHIND", false));
    org.opened("t1", "abandoned", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    assert!(body.contains("(not followed)"), "{body:?}");
    assert!(
        body.contains("gh pr view 5 --repo o/unfollowed --json mergeStateStatus"),
        "{body:?}"
    );
    let calls = calls(&home, repo);
    assert!(!calls.iter().any(|c| c.starts_with("pr view")), "{calls:?}");
}

/// Main moves under a green head: the secretary is told once that it is behind now, and an
/// UNKNOWN read afterwards is no change.
#[test]
fn main_moving_under_a_green_head_is_told_once() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/moved", "9".repeat(40));
    answer(&home, repo, &open(&head, "CLEAN", false));
    org.opened("t1", "review", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    assert!(body.contains("(CLEAN)"), "{body:?}");

    answer(&home, repo, &open(&head, "BEHIND", false));
    org.pass_until("the merge state change", |o| {
        !o.changes(repo, &head).is_empty()
    });
    org.pass_for(TEN_INTERVALS);
    let changes = org.changes(repo, &head);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(changes[0].contains("(BEHIND)"), "{changes:?}");
    assert!(changes[0].contains("behind its base"), "{changes:?}");

    answer(&home, repo, &open(&head, "UNKNOWN", false));
    org.pass_for(TEN_INTERVALS);
    assert_eq!(org.changes(repo, &head).len(), 1);

    let all = org.notices();
    let first_passed = all
        .iter()
        .position(|b| b.starts_with(&passed_prefix(repo, &head)))
        .unwrap();
    let first_change = all.iter().position(|b| b.starts_with(CHANGED)).unwrap();
    assert!(first_passed < first_change, "{all:?}");
}

/// A read of another head is not the merge state of the watched head.
#[test]
fn a_read_of_another_head_is_not_this_heads_state() {
    let home = init();
    let mut org = Org::new();
    let (repo, head, other) = ("o/otherhead", "a".repeat(40), "b".repeat(40));
    answer(&home, repo, &open(&other, "BEHIND", false));
    org.opened("t1", "review", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    assert!(body.contains("(not read)"), "{body:?}");
    assert!(!body.contains("(BEHIND)"), "{body:?}");
    org.pass_for(TEN_INTERVALS);
    assert!(org.starting(CHANGED).is_empty(), "{:?}", org.notices());
}

/// makibad restarts after a "not known yet" notice: the state read afterwards is still told.
#[test]
fn a_restart_after_an_unknown_notice_still_tells_the_known_state() {
    let home = init();
    let mut org = Org::new();
    let (repo, head) = ("o/restart", "c".repeat(40));
    answer(&home, repo, &open(&head, "UNKNOWN", false));
    org.opened("t1", "review", repo, &head);

    let body = org.until_passed_notice(repo, &head);
    assert!(body.contains("(UNKNOWN)"), "{body:?}");

    org.watch = PrWatch::from_env();
    answer(&home, repo, &open(&head, "CLEAN", false));
    org.pass_until("the merge state change", |o| {
        !o.changes(repo, &head).is_empty()
    });
    org.pass_for(TEN_INTERVALS);
    let changes = org.changes(repo, &head);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(changes[0].contains("(CLEAN)"), "{changes:?}");
}
