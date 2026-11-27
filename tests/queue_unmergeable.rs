//! Issue 575 (docs/design/queue-unmergeable.md): makibad picks up a pull request whose merge queue
//! entry turns `UNMERGEABLE`.
//!
//! - A queue act whose entry is `UNMERGEABLE` is taken out of the queue once (the
//!   `dequeuePullRequest` mutation, recorded before it is sent), with `merge_dequeued` (`why`
//!   `unmergeable`) and one notice; it is not told as having left the queue without landing.
//! - It waits while an entry ahead of it is still queued (the `merge` verb refuses it meanwhile),
//!   is updated once that entry has landed, and its new head is told "can be queued" once its CI
//!   passed. When the entry ahead left without landing, the update merges nothing and the same head
//!   is told "can be queued" again.
//! - A pull request queued by hand is taken out only when `MAKIBA_UNMERGEABLE_HAND` is `dequeue`.
//!
//! The world, as `tests/queue_ready.rs`: `makibad` runs in this process on a state database of its
//! own, per test, with one project whose repository sits under a temporary repositories folder
//! with a local bare `origin`, and one `review` code run whose worktree is the run's own
//! repository and whose branch is at origin. Landings are commits pushed to origin's `main` from a
//! clone of its own. `gh` is a stand-in script (`MAKIBA_GH`) answering from files per pull request
//! (its state, whether it is in the queue, its entry's state and position, its timeline): `gh pr
//! view`, every CI read (passed, finished), and every `gh api graphql` call: a read of one pull
//! request, the membership read (`p<i>`/`n<i>`), the queue of a base (`mergeQueue(`), and the
//! dequeue mutation, which takes the pull request out of the queue and adds a removal to its
//! timeline. No network. The tests run one at a time (one lock), since they share the process's
//! environment.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::prwatch;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
/// Ten intervals of the PR watch (`MAKIBA_PRWATCH_INTERVAL_MS`).
const TEN_INTERVALS: Duration = Duration::from_millis(500);
const READY: &str = "can be queued";
const HAND: &str = "MAKIBA_UNMERGEABLE_HAND";

const STAND_IN: &str = r#"#!/bin/sh
# Answers from $FAKE_GH_DIR/<owner>_<name>/pr-<n>/: origin and branch (the head is that branch at
# origin) or head; state (default OPEN); status (mergeStateStatus, default CLEAN); queue (in or
# out, default out); entry_state and position (of the queue entry); timeline (one JSON node per
# line). Every call is logged whole, one line, in the repository's folder (calls).
all="$*"
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
fail() { echo "$1" >&2; exit 2; }
head_of() {
  f=$1
  if [ -e "$f/origin" ]; then
    git --git-dir="$(cat "$f/origin")" rev-parse --verify "refs/heads/$(cat "$f/branch")"
  else
    cat "$f/head"
  fi
}
val() { if [ -e "$1" ]; then cat "$1"; else echo "$2"; fi; }
pr_json() {
  # pr_json <dir of the repository> <owner> <name> <n>
  f="$1/pr-$4"
  if [ ! -d "$f" ]; then echo null; return; fi
  state=$(val "$f/state" OPEN)
  head=$(head_of "$f") || exit 1
  if [ "$(val "$f/queue" out)" = in ]; then
    inq=true
    entry=$(printf '{"id":"MQE_%s","state":"%s","position":%s}' "$4" "$(val "$f/entry_state" QUEUED)" "$(val "$f/position" 1)")
  else
    inq=false; entry=null
  fi
  if [ -e "$f/timeline" ]; then nodes=$(paste -sd, "$f/timeline"); else nodes=; fi
  if [ "$state" = MERGED ]; then
    merged='"mergedAt":"2026-11-27T00:00:00Z","mergedBy":{"login":"o"},"mergeCommit":{"oid":"9999999999999999999999999999999999999999"}'
  else
    merged='"mergedAt":null,"mergedBy":null,"mergeCommit":null'
  fi
  printf '{"id":"PR_%s_%s_%s","number":%s,"state":"%s",%s,"headRefOid":"%s","title":"T","mergeable":"MERGEABLE","mergeStateStatus":"%s","isDraft":false,"isMergeQueueEnabled":true,"baseRefName":"main","isInMergeQueue":%s,"mergeQueueEntry":%s,"timelineItems":{"nodes":[%s]}}' \
    "$2" "$3" "$4" "$4" "$state" "$merged" "$head" "$(val "$f/status" CLEAN)" "$inq" "$entry" "$nodes"
}
case "$1 $2" in
  "pr view")
    num=$3; shift 3; repo=
    while [ $# -gt 0 ]; do
      case "$1" in
        --repo) repo=${2#github.com/}; shift 2 ;;
        --json) shift 2 ;;
        *) fail "unexpected argument: $1" ;;
      esac
    done
    dir="$FAKE_GH_DIR/${repo%%/*}_${repo#*/}"
    mkdir -p "$dir"; echo "$all" >> "$dir/calls"
    f="$dir/pr-$num"
    [ -e "$f/origin" ] || { echo "not found" >&2; exit 1; }
    head=$(head_of "$f") || exit 1
    printf '{"state":"%s","mergedAt":null,"mergedBy":null,"headRefOid":"%s","mergeCommit":null,"title":"T","mergeable":"MERGEABLE","mergeStateStatus":"%s","isDraft":false}\n' "$(val "$f/state" OPEN)" "$head" "$(val "$f/status" CLEAN)"
    exit 0 ;;
  "api --paginate")
    rest=${3#repos/}; owner=${rest%%/*}; rest=${rest#*/}
    dir="$FAKE_GH_DIR/${owner}_${rest%%/*}"
    mkdir -p "$dir"; echo "$all" >> "$dir/calls"
    case "$all" in
      *check-runs*) echo '{"name":"ci","status":"completed","conclusion":"success"}' ;;
      */status*) ;;
      *actions/runs*) echo completed ;;
      *) fail "unexpected call: $all" ;;
    esac
    exit 0 ;;
  "api graphql") ;;
  *) fail "unexpected call: $all" ;;
esac
shift 2
query=; owner=; name=; number=; id=; nums=
while [ $# -gt 0 ]; do
  case "$1" in
    --hostname) shift 2 ;;
    -f|-F)
      case "$2" in
        query=*) query=${2#query=} ;;
        owner=*) owner=${2#owner=} ;;
        name=*) name=${2#name=} ;;
        number=*) number=${2#number=} ;;
        id=*) id=${2#id=} ;;
        n[0-9]*=*) nums="$nums ${2%%=*}:${2#*=}" ;;
        *) ;;
      esac
      shift 2 ;;
    *) fail "unexpected argument: $1" ;;
  esac
done
case "$query" in
  *dequeuePullRequest*)
    # id = PR_<owner>_<name>_<n>
    rest=${id#PR_}; owner=${rest%%_*}; rest=${rest#*_}; name=${rest%_*}; n=${rest##*_}
    dir="$FAKE_GH_DIR/${owner}_${name}"
    [ -d "$dir/pr-$n" ] || fail "no pull request of the id $id"
    echo "$all" >> "$dir/calls"
    echo out > "$dir/pr-$n/queue"
    printf '{"__typename":"RemovedFromMergeQueueEvent","createdAt":"%s","reason":"MANUAL"}\n' "$now" >> "$dir/pr-$n/timeline"
    printf '{"data":{"dequeuePullRequest":{"mergeQueueEntry":{"id":"MQE_%s"}}}}\n' "$n"
    exit 0 ;;
esac
[ -n "$owner" ] && [ -n "$name" ] || fail "no repository: $all"
dir="$FAKE_GH_DIR/${owner}_${name}"
mkdir -p "$dir"; echo "$all" >> "$dir/calls"
case "$query" in
  *mergeQueue\(*)
    nodes=
    for f in "$dir"/pr-*; do
      [ -d "$f" ] || continue
      [ "$(val "$f/queue" out)" = in ] || continue
      n=${f##*/pr-}
      node=$(printf '{"position":%s,"pullRequest":{"number":%s}}' "$(val "$f/position" 1)" "$n")
      if [ -n "$nodes" ]; then nodes="$nodes,$node"; else nodes=$node; fi
    done
    printf '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[%s]}}}}}\n' "$nodes" ;;
  *p0:*)
    out=
    for pair in $nums; do
      alias=p${pair%%:*}; alias=p${alias#pn}
      part=$(printf '"%s":%s' "$alias" "$(pr_json "$dir" "$owner" "$name" "${pair#*:}")")
      if [ -n "$out" ]; then out="$out,$part"; else out=$part; fi
    done
    printf '{"data":{"repository":{%s}}}\n' "$out" ;;
  *pullRequest\(number:\$number\)*)
    [ -n "$number" ] || fail "no number: $all"
    printf '{"data":{"repository":{"pullRequest":%s}}}\n' "$(pr_json "$dir" "$owner" "$name" "$number")" ;;
  *) fail "unexpected query: $all" ;;
esac
"#;

static HOME: OnceLock<PathBuf> = OnceLock::new();
static INIT: Once = Once::new();
/// One test at a time: each runs a makibad of its own in this process.
static LOCK: Mutex<()> = Mutex::new(());

/// The folder the tests share (the stand-in, its answers, the repositories and workers folders),
/// with the environment set once before any test starts a thread.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        for d in ["repos", "workers", "docs", "answers"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "50");
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::remove_var(HAND);
        }
        makibad::process_knowledge();
        HOME.set(root).unwrap();
    });
    HOME.get().unwrap().clone()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A commit in `dir` writing `text` to `name` (folders made); its id.
fn commit_in(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", &format!("change {name}")]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Whether `a` is an ancestor of (or is) `b` in `dir`.
fn holds(dir: &Path, a: &str, b: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", a, b])
        .status()
        .unwrap()
        .success()
}

/// GitHub's time now, as the stand-in writes it.
fn now_github() -> String {
    let out = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One project, its repository and origin, one `review` code run with its pull request recorded,
/// and (once started) a makibad of its own.
struct World {
    state: PathBuf,
    /// The project's repository (`$MAKIBA_REPOS_DIR/<slug>`).
    repo: PathBuf,
    origin: PathBuf,
    /// A clone of origin that lands commits on its `main`.
    author: PathBuf,
    worktree: PathBuf,
    task: String,
    branch: String,
    /// `o/<slug>`.
    name: String,
    pr: i64,
    secretary: String,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl World {
    /// The project `slug` with `main` at one commit holding `shared.txt`.
    fn new(slug: &str, pr: i64) -> World {
        let home = init();
        let top = home.join("worlds").join(slug);
        std::fs::create_dir_all(&top).unwrap();
        let repo = home.join("repos").join(slug);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        commit_in(&repo, "shared.txt", "base\n");
        let origin = top.join("origin.git");
        git(&top, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &origin,
            &["fetch", "-q", &repo.to_string_lossy(), "main:main"],
        );
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["fetch", "-q", "origin"]);
        let author = top.join("author");
        git(
            &top,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &author.to_string_lossy(),
            ],
        );
        let state = top.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let task = format!("{slug}-task");
        let branch = format!("makiba/{task}");
        let worktree = home.join("workers").join(slug).join(&task);
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        World {
            state,
            repo,
            origin,
            author,
            worktree,
            task,
            branch,
            name: format!("o/{slug}"),
            pr,
            secretary,
            stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
        }
    }

    /// The stand-in's folder of pull request `pr` of this repository.
    fn answers(&self, pr: i64) -> PathBuf {
        let dir = init()
            .join("answers")
            .join(self.name.replace('/', "_"))
            .join(format!("pr-{pr}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The stand-in's answer `file` of pull request `pr` set to `text`.
    fn set(&self, pr: i64, file: &str, text: &str) {
        std::fs::write(self.answers(pr).join(file), text).unwrap();
    }

    /// Pull request `pr` in the queue at `position`, its entry in `entry_state`, with an addition
    /// to the queue on its timeline now.
    fn queued(&self, pr: i64, position: i64, entry_state: &str) {
        self.set(pr, "queue", "in");
        self.set(pr, "position", &position.to_string());
        self.set(pr, "entry_state", entry_state);
        self.set(
            pr,
            "timeline",
            &format!(
                "{{\"__typename\":\"AddedToMergeQueueEvent\",\"createdAt\":\"{}\"}}\n",
                now_github()
            ),
        );
    }

    /// A pull request no run follows, open at a head of its own, in the queue at `position`.
    fn other_in_queue(&self, pr: i64, position: i64) {
        self.set(pr, "head", &format!("{pr}").repeat(40)[..40]);
        self.set(pr, "state", "OPEN");
        self.queued(pr, position, "AWAITING_CHECKS");
    }

    /// A landing on origin's `main` writing `text` to `path`; its id.
    fn land(&self, path: &str, text: &str) -> String {
        git(&self.author, &["pull", "-q", "--ff-only", "origin", "main"]);
        let id = commit_in(&self.author, path, text);
        git(&self.author, &["push", "-q", "origin", "main"]);
        id
    }

    /// The run's worktree from the project's `main`, one commit writing `text` to `path`, its
    /// branch pushed to origin, the run `review` with its code launch, and the pull request
    /// recorded at that head with the stand-in reading `CLEAN`. The head.
    fn fork(&self, path: &str, text: &str) -> String {
        let state = &self.state;
        makiba::daemon::run_repo::create(
            &self.repo,
            &makiba::daemon::run_repo::dir(&state.join("settings"), &self.task),
            &self.worktree,
            &self.branch,
            "main",
            &state.join("git-hooks"),
        )
        .unwrap();
        let head = commit_in(&self.worktree, path, text);
        git(
            &self.origin,
            &[
                "fetch",
                "-q",
                &self.worktree.to_string_lossy(),
                &format!("{}:refs/heads/{}", self.branch, self.branch),
            ],
        );
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        let run = w
            .upsert_run(&RunUpsert {
                task_id: &self.task,
                project_slug: self.name.split('/').nth(1).unwrap(),
                status: Some("review"),
                branch: Some(&self.branch),
                ..Default::default()
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, ?2, 'code', 'fake', 'the brief', ?3, 'verified')",
                rusqlite::params![
                    run,
                    format!("{}-s1", self.task),
                    self.worktree.to_string_lossy()
                ],
            )
            .unwrap();
        self.set(self.pr, "origin", &self.origin.to_string_lossy());
        self.set(self.pr, "branch", &self.branch);
        self.set(self.pr, "status", "CLEAN");
        self.set(self.pr, "state", "OPEN");
        prwatch::register(&w, "pr_opened", &self.task, &self.name, self.pr, &head).unwrap();
        head
    }

    /// makibad's queue act of the pull request at `head`, as the door's `merge` and the merge pass
    /// write it (an attempt of an earlier start, ended `merge_enqueued`): the attempt's id.
    fn act(&self, head: &str) -> i64 {
        let w = StateWriter::open(&self.state.join("state.db")).unwrap();
        let event = |kind: &str, payload: Value| {
            w.append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(&payload),
                run_task_id: Some(&self.task),
                ..Default::default()
            })
            .unwrap()
        };
        let attempt = event(
            "merge_attempted",
            json!({"repo": self.name, "pr": self.pr, "head": head, "method": "squash",
                   "gate_event": null, "command": null, "approved_by": null,
                   "boot": "0".repeat(32), "task_id": self.task}),
        );
        event(
            "merge_enqueued",
            json!({"attempt": attempt,
                   "entry": {"id": format!("MQE_{}", self.pr), "state": "QUEUED", "position": 2}}),
        );
        attempt
    }

    /// makibad started, once a Resync newer than any before is handled (`base_resynced`), so the
    /// door's writes cannot race its start writes (Issue 181).
    fn start(&mut self) {
        let before = self.count("SELECT count(*) FROM events WHERE kind = 'base_resynced'");
        self.stop = Arc::new(AtomicBool::new(false));
        let (dir, flag) = (self.state.clone(), self.stop.clone());
        self.daemon = Some(std::thread::spawn(move || {
            makibad::run_until(&dir, &|| Box::new(Stub::new()), flag)
        }));
        let until = Instant::now() + WAIT;
        while self.count("SELECT count(*) FROM events WHERE kind = 'base_resynced'") <= before {
            assert!(
                Instant::now() < until,
                "makibad did not handle its first Resync"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn read(&self) -> Connection {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.read().query_row(sql, [], |r| r.get(0)).unwrap_or(0)
    }

    /// The `queue_readiness` payloads of `head`, oldest first.
    fn readiness_of(&self, head: &str) -> Vec<Value> {
        self.payloads("queue_readiness")
            .into_iter()
            .filter(|p| p["head_oid"].as_str() == Some(head))
            .collect()
    }

    fn payloads(&self, kind: &str) -> Vec<Value> {
        let c = self.read();
        let mut q = c
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap().unwrap_or_default()).unwrap_or(Value::Null))
            .collect()
    }

    /// makibad's notices to the secretary, oldest first.
    fn notices(&self) -> Vec<String> {
        let c = self.read();
        let mut q = c
            .prepare(
                "SELECT body FROM messages WHERE kind = 'notice' AND sender = 'makibad'
                   AND recipient = 'secretary' ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The `update` commands: actor, status and args.
    fn updates(&self) -> Vec<(String, String, Value)> {
        let c = self.read();
        let mut q = c
            .prepare(
                "SELECT actor, status, args_json FROM commands WHERE verb = 'update' ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// The stand-in's logged calls of this repository that send the dequeue mutation.
    fn dequeue_calls(&self) -> Vec<String> {
        let log = init()
            .join("answers")
            .join(self.name.replace('/', "_"))
            .join("calls");
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("dequeuePullRequest"))
            .map(str::to_string)
            .collect()
    }

    /// The head of the branch at origin.
    fn origin_head(&self) -> String {
        git(
            &self.origin,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", self.branch),
            ],
        )
    }

    /// The door's `merge` request of the pull request at `head`, as the secretary.
    fn merge_request(&self, head: &str) -> makiba::daemon::door::Response {
        let door = Door::new(
            StateWriter::open(&self.state.join("state.db")).unwrap(),
            Arc::new(Stub::new()),
            "fake".into(),
        );
        door.handle(
            Some(&self.secretary),
            "merge",
            json!({"repo": self.name, "pr": self.pr, "head": head}),
            None,
            true,
        )
    }

    /// Wait until `done` holds, or panic with what makibad wrote.
    fn until(&self, what: &str, done: impl Fn(&World) -> bool) {
        let until = Instant::now() + WAIT;
        while !done(self) {
            assert!(
                Instant::now() < until,
                "{what} not seen after {WAIT:?}; queue_readiness: {:?}; dequeue requests: {:?}; \
                 ended: {:?}; merge_dequeued: {:?}; updates: {:?}; notices: {:#?}",
                self.payloads("queue_readiness"),
                self.payloads("queue_dequeue_requested"),
                self.payloads("queue_dequeue_ended"),
                self.payloads("merge_dequeued"),
                self.updates(),
                self.notices()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(d) = self.daemon.take() {
            let _ = d.join();
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop();
    }
}

fn short(head: &str) -> &str {
    &head[..7]
}

/// The newest `queue_readiness` of `head` is the wait for the entries ahead.
fn waiting(w: &World, head: &str) -> bool {
    w.readiness_of(head).last().is_some_and(|p| {
        p["verdict"] == "update" && p["reason"] == "unmergeable" && p["waiting"] == true
    })
}

/// The design's case 1: makibad's queue act turns `UNMERGEABLE` behind PR 2 (which no run
/// follows); it is taken out once, waits while PR 2 is queued, is updated once PR 2 has landed,
/// and its new head is told "can be queued" once its CI passed.
#[test]
fn an_unmergeable_queue_act_is_dequeued_once_updated_after_the_entry_ahead_lands_and_ready_again() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("qa", 1);
    let h1 = w.fork("a.txt", "a\n");
    w.queued(1, 2, "UNMERGEABLE");
    w.other_in_queue(2, 1);
    let attempt = w.act(&h1);
    w.start();

    w.until("the wait for the entry ahead", |w| waiting(w, &h1));
    std::thread::sleep(TEN_INTERVALS);
    let calls = w.dequeue_calls();
    let requested = w.payloads("queue_dequeue_requested");
    let ended = w.payloads("queue_dequeue_ended");
    let dequeued = w.payloads("merge_dequeued");
    let waited = w.readiness_of(&h1);
    let updates_while = w.updates();
    let answer = w.merge_request(&h1);
    let attempts = w.payloads("merge_attempted");

    assert_eq!(calls.len(), 1, "one dequeue sent: {calls:#?}");
    assert!(
        calls[0].contains("PR_o_qa_1"),
        "the dequeue names the pull request's node id: {calls:#?}"
    );
    assert_eq!(requested.len(), 1, "one request: {requested:?}");
    assert_eq!(ended.len(), 1, "one outcome: {ended:?}");
    assert_eq!(ended[0]["outcome"], "dequeued", "{ended:?}");
    assert_eq!(ended[0]["ahead"], json!([2]), "{ended:?}");
    assert_eq!(dequeued.len(), 1, "one merge_dequeued: {dequeued:?}");
    assert_eq!(dequeued[0]["why"], "unmergeable", "{dequeued:?}");
    assert_eq!(dequeued[0]["attempt"], json!(attempt), "{dequeued:?}");
    assert_eq!(
        dequeued[0]["head"].as_str(),
        Some(h1.as_str()),
        "{dequeued:?}"
    );

    let newest = waited.last().expect("a queue_readiness of H1");
    assert_eq!(newest["verdict"], "update", "{newest}");
    assert_eq!(newest["reason"], "unmergeable", "{newest}");
    assert_eq!(newest["waiting"], true, "{newest}");
    assert_eq!(newest["waits_for"], json!([2]), "{newest}");
    assert!(
        updates_while.is_empty(),
        "no update while PR 2 is queued: {updates_while:?}"
    );
    let e = answer.expect_err("a head waiting for the entries ahead is not queued again");
    assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
    assert!(e.message.contains("not ready for the queue"), "{e:?}");
    assert_eq!(attempts.len(), 1, "no attempt added: {attempts:?}");

    // PR 2 lands.
    let l1 = w.land("ahead.txt", "ahead\n");
    w.set(2, "queue", "out");
    w.set(2, "state", "MERGED");
    w.until("a ready verdict of a new head", |w| {
        w.payloads("queue_readiness")
            .iter()
            .any(|p| p["verdict"] == "ready" && p["head_oid"].as_str() != Some(&h1))
    });
    let h2 = w.origin_head();
    let check = |w: &World, when: &str| {
        let notices = w.notices();
        let updates = w.updates();
        assert!(
            notices.iter().any(|n| n.contains("out of the merge queue")
                && n.contains("o/qa#1")
                && n.contains("#2")),
            "{when}: the dequeue is told: {notices:#?}"
        );
        assert!(
            !notices
                .iter()
                .any(|n| n.contains("left the merge queue without landing")),
            "{when}: makibad's own dequeue told as a leaving: {notices:#?}"
        );
        assert_eq!(updates.len(), 1, "{when}: one update: {updates:?}");
        assert_eq!(updates[0].0, "makibad", "{when}: {updates:?}");
        assert_eq!(updates[0].1, "done", "{when}: {updates:?}; {notices:#?}");
        assert_ne!(h2, h1, "{when}: origin's branch moved");
        assert!(holds(&w.origin, &l1, &h2), "{when}: {h2} does not hold L1");
        assert!(holds(&w.origin, &h1, &h2), "{when}: {h2} does not hold H1");
        let pushed = w.payloads("fix_pushed");
        assert!(
            pushed
                .iter()
                .any(|p| p["head_oid"].as_str() == Some(h2.as_str())),
            "{when}: the new head {h2} is recorded: {pushed:?}"
        );
        let ready = format!("{}#{} at {} {READY}", w.name, w.pr, short(&h2));
        assert_eq!(
            notices.iter().filter(|n| n.contains(&ready)).count(),
            1,
            "{when}: the new head is told ready once: {notices:#?}"
        );
        assert!(
            w.readiness_of(&h2).iter().any(|p| p["verdict"] == "ready"),
            "{when}: {:?}",
            w.readiness_of(&h2)
        );
        assert_eq!(w.dequeue_calls().len(), 1, "{when}: one dequeue sent");
        assert_eq!(
            w.payloads("merge_dequeued").len(),
            1,
            "{when}: one merge_dequeued"
        );
    };
    check(&w, "once the new head is ready");
    std::thread::sleep(TEN_INTERVALS);
    check(&w, "ten intervals later");
    w.stop();
}

/// The design's case 2: a pull request queued by hand whose entry turns `UNMERGEABLE` is left to
/// the owner unless `MAKIBA_UNMERGEABLE_HAND` is `dequeue`.
#[test]
fn an_unmergeable_entry_queued_by_hand_is_left_to_the_owner_unless_turned_on() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("qb", 3);
    w.fork("b.txt", "b\n");
    w.queued(3, 1, "UNMERGEABLE");
    w.start();

    w.until("the stay queued by hand", |w| {
        !w.payloads("merge_queue_entered").is_empty()
    });
    std::thread::sleep(TEN_INTERVALS);
    let entered = w.payloads("merge_queue_entered");
    let calls = w.dequeue_calls();
    let requested = w.payloads("queue_dequeue_requested");
    let dequeued = w.payloads("merge_dequeued");
    w.stop();
    assert_eq!(entered.len(), 1, "{entered:?}");
    assert!(calls.is_empty(), "dequeued with the switch off: {calls:#?}");
    assert!(requested.is_empty(), "{requested:?}");
    assert!(dequeued.is_empty(), "{dequeued:?}");

    // SAFETY: this file's tests run one at a time (`LOCK`), and the makibad above has stopped.
    unsafe { std::env::set_var(HAND, "dequeue") };
    w.start();
    w.until("the dequeue of the stay", |w| {
        !w.payloads("merge_dequeued").is_empty()
    });
    std::thread::sleep(TEN_INTERVALS);
    let calls = w.dequeue_calls();
    let dequeued = w.payloads("merge_dequeued");
    w.stop();
    // SAFETY: as above.
    unsafe { std::env::remove_var(HAND) };
    assert_eq!(calls.len(), 1, "one dequeue sent: {calls:#?}");
    assert_eq!(dequeued.len(), 1, "{dequeued:?}");
    assert_eq!(dequeued[0]["why"], "unmergeable", "{dequeued:?}");
    assert_eq!(dequeued[0]["attempt"], Value::Null, "{dequeued:?}");
}

/// The design's case 3: the entry ahead leaves the queue without landing and `main` does not move:
/// the update merges nothing, and the same head is told "can be queued" again.
#[test]
fn a_dequeued_head_whose_entry_ahead_left_without_landing_is_ready_again_at_the_same_head() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("qc", 4);
    let h1 = w.fork("c.txt", "c\n");
    w.queued(4, 2, "UNMERGEABLE");
    w.other_in_queue(5, 1);
    w.act(&h1);
    w.start();

    w.until("the wait for the entry ahead", |w| waiting(w, &h1));
    w.set(5, "queue", "out");
    let ready = format!("{}#{} at {} {READY}", w.name, w.pr, short(&h1));
    w.until("the head told ready again", |w| {
        w.notices().iter().any(|n| n.contains(&ready))
    });
    std::thread::sleep(TEN_INTERVALS);
    let updates = w.updates();
    let pushed = w.payloads("fix_pushed");
    let notices = w.notices();
    let decided = w.readiness_of(&h1);
    w.stop();

    assert_eq!(updates.len(), 1, "one update: {updates:?}");
    assert_eq!(updates[0].1, "done", "{updates:?}; {notices:#?}");
    assert!(pushed.is_empty(), "no new head recorded: {pushed:?}");
    assert_eq!(w.origin_head(), h1, "origin's branch moved");
    assert_eq!(
        notices.iter().filter(|n| n.contains(&ready)).count(),
        1,
        "told ready once: {notices:#?}"
    );
    assert_eq!(
        decided.last().map(|p| p["verdict"].clone()),
        Some(json!("ready")),
        "{decided:?}"
    );
}
