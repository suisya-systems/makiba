//! Issue 573 (docs/design/queue-overlap.md): makibad holds a followed pull request from the merge
//! queue while an earlier followed one that conflicts with it is queued or ready, and starts its
//! update itself when that one lands.
//!
//! - Two pull requests conflict when `git merge-tree` of their heads conflicts; changes to other
//!   lines of the same file do not count.
//! - A conflicting pull request queued (by the person, in the web UI) or told ready before it holds
//!   the later one: its head gets `queue_readiness` `hold` (reason `overlap`, `holds` naming the
//!   earlier one) and one notice that it is held, never "can be queued"; the merge verb refuses it.
//! - When the earlier one lands, the held head gets `update` (reason `overlap_landed`) with an
//!   `update` command of makibad; a same-lines conflict makes that update fail naming the file,
//!   and the worker's merge is the new head, told ready.
//! - A head told ready is held when the person queues a conflicting one first; a held head is still
//!   updated by a landing that changes what the checks check.
//!
//! The world: `makibad` runs in this process on a state database of its own, per test, with one
//! project whose repository sits under a temporary repositories folder with a local bare `origin`,
//! and `review` code runs whose worktrees are their own repositories (`run_repo::create`) and whose
//! branches are at origin. Landings are commits pushed to origin's `main` from a clone of its own.
//! `gh` is a stand-in script (`MAKIBA_GH`) that answers `gh pr view`, the state read over GraphQL
//! (`-F number=<n>`) and the membership read (`-F n<i>=<n>`) from a folder per pull request: `OPEN`
//! at the head of the branch at origin, the merge state of the file `status`, in the merge queue
//! while the file `queued` exists, and `MERGED` with the merge commit of the file `merged` once it
//! exists; every CI read answers passed and finished. No network. The tests run one at a time (one
//! lock), since they share the process's environment.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::prwatch;
use makiba::db::writer::{RunUpsert, StateWriter};
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
const HELD: &str = "is held from the merge queue";
const NO_LONGER: &str = "is no longer ready for the queue";
const SHARED: &str = "docs/shared.md";

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo github.com/<o>/<r> --json ...: the pull request of the folder pr-<n> of the
# repository's folder. gh api graphql ... -f owner=<o> -f name=<r> -F number=<n>: the same in
# data.repository.pullRequest. gh api graphql ... -F n<i>=<n> ...: each in data.repository.p<i>.
# A pull request is OPEN at the head of its branch at origin (the files origin and branch), with
# the merge state of the file status, in the merge queue while the file queued exists, and MERGED
# with the merge commit of the file merged (and the head of the file merged-head) once it exists.
# gh api --paginate repos/<o>/<r>/...: the CI reads of a head, all passed and finished. Every call
# is logged whole, one line, in the repository's folder (calls).
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
body() {
  f="$dir/pr-$1"
  if [ ! -e "$f/origin" ]; then return 1; fi
  if [ -e "$f/merged" ]; then
    printf '{"number":%s,"state":"MERGED","mergedAt":"2026-11-27T00:00:00Z","mergedBy":{"login":"owner"},"headRefOid":"%s","mergeCommit":{"oid":"%s"},"title":"T","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","isDraft":false,"isInMergeQueue":false,"mergeQueueEntry":null,"timelineItems":{"nodes":[]}}' \
      "$1" "$(cat "$f/merged-head")" "$(cat "$f/merged")"
    return 0
  fi
  head=$(git --git-dir="$(cat "$f/origin")" rev-parse --verify "refs/heads/$(cat "$f/branch")") || return 1
  if [ -e "$f/queued" ]; then q=true; e='{"id":"MQE_1","state":"QUEUED","position":1}'; else q=false; e=null; fi
  printf '{"number":%s,"state":"OPEN","mergedAt":null,"mergedBy":null,"headRefOid":"%s","mergeCommit":null,"title":"T","mergeable":"MERGEABLE","mergeStateStatus":"%s","isDraft":false,"isInMergeQueue":%s,"mergeQueueEntry":%s,"timelineItems":{"nodes":[]}}' \
    "$1" "$head" "$(cat "$f/status")" "$q" "$e"
}
case "$all" in
  "api graphql"*)
    if [ -z "$num" ] && [ -n "$members" ]; then
      out=; sep=
      for m in $members; do
        key=${m%%=*}; n=${m#*=}
        b=$(body "$n") || b=null
        out="$out$sep\"p${key#n}\":$b"; sep=,
      done
      printf '{"data":{"repository":{%s}}}\n' "$out"
      exit 0
    fi
    b=$(body "$num") || { echo "not found" >&2; exit 1; }
    printf '{"data":{"repository":{"pullRequest":%s}}}\n' "$b" ;;
  "pr view"*)
    b=$(body "$num") || { echo "not found" >&2; exit 1; }
    printf '%s\n' "$b" ;;
  *check-runs*) echo '{"name":"ci","status":"completed","conclusion":"success"}' ;;
  */status*) ;;
  *actions/runs*) echo completed ;;
  *) echo "unexpected call: $all" >&2; exit 2 ;;
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
        }
        makibad::process_knowledge();
        HOME.set(root).unwrap();
    });
    HOME.get().unwrap().clone()
}

const GIT_CONFIG: &[&str] = &[
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
];

/// git in `dir`, which must succeed; its trimmed output.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(GIT_CONFIG)
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

/// Whether git in `dir` succeeds (its output dropped).
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(GIT_CONFIG)
        .args(args)
        .output()
        .unwrap()
        .status
        .success()
}

/// The shared document: ten lines, `line <n>`, with line `n` replaced by `text` when given.
fn shared(changes: &[(usize, &str)]) -> String {
    (1..=10)
        .map(|n| {
            let line = changes
                .iter()
                .find(|(at, _)| *at == n)
                .map_or_else(|| format!("line {n}"), |(_, t)| (*t).to_string());
            format!("{line}\n")
        })
        .collect()
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
    git_ok(dir, &["merge-base", "--is-ancestor", a, b])
}

fn short(head: &str) -> &str {
    &head[..7]
}

/// One run of the world: its task, branch, worktree and pull request.
struct Run {
    task: String,
    branch: String,
    pr: i64,
}

/// One project, its repository and origin, its runs, and (once started) a makibad of its own.
struct World {
    state: PathBuf,
    /// The project's repository (`$MAKIBA_REPOS_DIR/<slug>`).
    repo: PathBuf,
    origin: PathBuf,
    /// A clone of origin that lands commits on its `main`.
    author: PathBuf,
    slug: String,
    /// `o/<slug>`.
    name: String,
    secretary: String,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl World {
    /// The project `slug` with `main` at one commit (M0) holding the shared document.
    fn new(slug: &str) -> World {
        let home = init();
        let top = home.join("worlds").join(slug);
        std::fs::create_dir_all(&top).unwrap();
        let repo = home.join("repos").join(slug);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        commit_in(&repo, SHARED, &shared(&[]));
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
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        World {
            state,
            repo,
            origin,
            author,
            slug: slug.to_string(),
            name: format!("o/{slug}"),
            secretary,
            stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
        }
    }

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.state.join("state.db")).unwrap()
    }

    /// The stand-in's folder of pull request `pr`.
    fn answers(&self, pr: i64) -> PathBuf {
        init()
            .join("answers")
            .join(self.name.replace('/', "_"))
            .join(format!("pr-{pr}"))
    }

    /// The run `<slug>-<name>`: its worktree from the project's `main` (M0), one commit writing the
    /// shared document with line `line` set to `text`, its branch pushed to origin, the run
    /// `review` with its code launch, and its pull request `pr` recorded at that head, read
    /// `CLEAN`. The run and the head.
    fn fork(&self, name: &str, pr: i64, line: usize, text: &str) -> (Run, String) {
        let task = format!("{}-{name}", self.slug);
        let branch = format!("makiba/{task}");
        let worktree = init().join("workers").join(&self.slug).join(&task);
        let state = &self.state;
        makiba::daemon::run_repo::create(
            &self.repo,
            &makiba::daemon::run_repo::dir(&state.join("settings"), &task),
            &worktree,
            &branch,
            "main",
            &state.join("git-hooks"),
        )
        .unwrap();
        let head = commit_in(&worktree, SHARED, &shared(&[(line, text)]));
        git(
            &self.origin,
            &[
                "fetch",
                "-q",
                &worktree.to_string_lossy(),
                &format!("{branch}:refs/heads/{branch}"),
            ],
        );
        let w = self.writer();
        let run = w
            .upsert_run(&RunUpsert {
                task_id: &task,
                project_slug: &self.slug,
                status: Some("review"),
                branch: Some(&branch),
                ..Default::default()
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, ?2, 'code', 'fake', 'the brief', ?3, 'verified')",
                rusqlite::params![run, format!("{task}-s1"), worktree.to_string_lossy()],
            )
            .unwrap();
        let answers = self.answers(pr);
        std::fs::create_dir_all(&answers).unwrap();
        std::fs::write(
            answers.join("origin"),
            self.origin.to_string_lossy().as_bytes(),
        )
        .unwrap();
        std::fs::write(answers.join("branch"), &branch).unwrap();
        std::fs::write(answers.join("status"), "CLEAN").unwrap();
        prwatch::register(&w, "pr_opened", &task, &self.name, pr, &head).unwrap();
        (Run { task, branch, pr }, head)
    }

    /// The person queues `pr` in the web UI: the stand-in reads it in the merge queue.
    fn queue(&self, pr: i64) {
        std::fs::write(self.answers(pr).join("queued"), "").unwrap();
    }

    /// The pull request of `r` lands: a merge commit of its branch on origin's `main`, and the
    /// stand-in reads it `MERGED` with that commit. The merge commit.
    fn land_pr(&self, r: &Run, head: &str) -> String {
        git(&self.author, &["checkout", "-q", "main"]);
        git(&self.author, &["pull", "-q", "--ff-only", "origin", "main"]);
        git(&self.author, &["fetch", "-q", "origin", &r.branch]);
        git(
            &self.author,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                &format!("Merge pull request #{}", r.pr),
                "FETCH_HEAD",
            ],
        );
        let merge = git(&self.author, &["rev-parse", "HEAD"]);
        git(&self.author, &["push", "-q", "origin", "main"]);
        let answers = self.answers(r.pr);
        std::fs::write(answers.join("merged-head"), head).unwrap();
        let _ = std::fs::remove_file(answers.join("queued"));
        std::fs::write(answers.join("merged"), &merge).unwrap();
        merge
    }

    /// A landing on origin's `main` writing `text` to `path`; its id.
    fn land(&self, path: &str, text: &str) -> String {
        git(&self.author, &["checkout", "-q", "main"]);
        git(&self.author, &["pull", "-q", "--ff-only", "origin", "main"]);
        let id = commit_in(&self.author, path, text);
        git(&self.author, &["push", "-q", "origin", "main"]);
        id
    }

    /// The worker's merge of origin's `main` into the branch of `r`, its conflict in the shared
    /// document resolved with `resolved`, pushed to origin's branch. The new head.
    fn worker_merge(&self, r: &Run, resolved: &str) -> String {
        git(&self.author, &["checkout", "-q", "main"]);
        git(&self.author, &["pull", "-q", "--ff-only", "origin", "main"]);
        git(
            &self.author,
            &[
                "fetch",
                "-q",
                "origin",
                &format!("+refs/heads/{0}:refs/remotes/origin/{0}", r.branch),
            ],
        );
        git(
            &self.author,
            &[
                "checkout",
                "-q",
                "-B",
                "work",
                &format!("refs/remotes/origin/{}", r.branch),
            ],
        );
        // The merge conflicts in the shared document; the worker resolves it.
        let _ = git_ok(&self.author, &["merge", "-q", "--no-edit", "main"]);
        std::fs::write(self.author.join(SHARED), resolved).unwrap();
        git(&self.author, &["add", SHARED]);
        git(&self.author, &["commit", "-q", "--no-edit"]);
        let head = git(&self.author, &["rev-parse", "HEAD"]);
        git(
            &self.author,
            &[
                "push",
                "-q",
                "origin",
                &format!("work:refs/heads/{}", r.branch),
            ],
        );
        git(&self.author, &["checkout", "-q", "main"]);
        head
    }

    /// makibad started, once its first Resync is handled (`base_resynced`), so the door's writes
    /// cannot race its start writes (Issue 181).
    fn start(&mut self) {
        let (dir, flag) = (self.state.clone(), self.stop.clone());
        self.daemon = Some(std::thread::spawn(move || {
            makibad::run_until(&dir, &|| Box::new(Stub::new()), flag)
        }));
        let c = self.read();
        let until = Instant::now() + WAIT;
        while c
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'base_resynced'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            == 0
        {
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

    fn door(&self) -> Door {
        Door::new(self.writer(), Arc::new(Stub::new()), "fake".into())
    }

    /// Every event `kind`: its id and payload, oldest first.
    fn events(&self, kind: &str) -> Vec<(i64, Value)> {
        let c = self.read();
        let mut q = c
            .prepare("SELECT id, payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (id, p) = row.unwrap();
            (
                id,
                serde_json::from_str(&p.unwrap_or_default()).unwrap_or(Value::Null),
            )
        })
        .collect()
    }

    /// The `queue_readiness` events of `head`: id and payload.
    fn readiness_of(&self, head: &str) -> Vec<(i64, Value)> {
        self.events("queue_readiness")
            .into_iter()
            .filter(|(_, p)| p["head_oid"].as_str() == Some(head))
            .collect()
    }

    /// The payloads of `kind` naming pull request `pr` of this world.
    fn of_pr(&self, kind: &str, pr: i64) -> Vec<Value> {
        self.events(kind)
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| p["pr"].as_i64() == Some(pr))
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

    /// How many notices contain every one of `parts`.
    fn told(&self, parts: &[&str]) -> usize {
        self.notices()
            .iter()
            .filter(|n| parts.iter().all(|p| n.contains(p)))
            .count()
    }

    /// The `update` commands of `task`: actor, status and args.
    fn updates_of(&self, task: &str) -> Vec<(String, String, Value)> {
        let c = self.read();
        let mut q = c
            .prepare(
                "SELECT actor, status, args_json FROM commands WHERE verb = 'update'
                   AND json_extract(args_json, '$.task') = ?1 ORDER BY id",
            )
            .unwrap();
        q.query_map([task], |r| {
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

    /// The head of `branch` at origin.
    fn origin_head(&self, branch: &str) -> String {
        git(
            &self.origin,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        )
    }

    /// Wait until `done` holds, or panic with what makibad wrote.
    fn until(&self, what: &str, done: impl Fn(&World) -> bool) {
        let until = Instant::now() + WAIT;
        while !done(self) {
            assert!(
                Instant::now() < until,
                "{what} not seen after {WAIT:?}; queue_readiness: {:?}; notices: {:#?}",
                self.events("queue_readiness"),
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

/// The design's case 1: PR 2 conflicts with PR 1, which the person queued: it is held, the merge
/// verb refuses it, and once PR 1 lands makibad updates it; the update conflicts in the same lines
/// and names the file, and the worker's merge is told ready.
#[test]
fn a_pull_request_that_conflicts_with_a_queued_one_is_held_and_updated_when_it_lands() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pa");
    let (one, h1) = w.fork("one", 1, 3, "one");
    w.start();
    let ready1 = format!("{}#1 at {} {READY}", w.name, short(&h1));
    w.until("PR 1 told ready", |w| w.told(&[&ready1]) > 0);
    assert_eq!(w.told(&[&ready1]), 1, "{:#?}", w.notices());
    w.queue(1);
    w.until("PR 1 entered the queue", |w| {
        !w.of_pr("merge_queue_entered", 1).is_empty()
    });
    assert_eq!(w.of_pr("merge_queue_entered", 1).len(), 1);

    let (two, h2) = w.fork("two", 2, 3, "two");
    w.until("a decision of PR 2's head", |w| {
        !w.readiness_of(&h2).is_empty()
    });
    let held = format!("{}#2 at {} {HELD}", w.name, short(&h2));
    let ready2 = format!("{}#2 at {} {READY}", w.name, short(&h2));
    let check_held = |w: &World, when: &str| {
        let decided = w.readiness_of(&h2);
        assert_eq!(decided.len(), 1, "{when}: one decision of h2: {decided:?}");
        let d = &decided[0].1;
        assert_eq!(d["verdict"], "hold", "{when}: {d}");
        assert_eq!(d["reason"], "overlap", "{when}: {d}");
        assert_eq!(
            d["holds"],
            json!([{"pr": 1, "task": one.task, "head": h1, "queued": true,
                    "files": [SHARED]}]),
            "{when}: {d}"
        );
        assert_eq!(
            w.told(&[&held, SHARED, &format!("{}#1", w.name)]),
            1,
            "{when}: one held notice: {:#?}",
            w.notices()
        );
        assert_eq!(w.told(&[&ready2]), 0, "{when}: {:#?}", w.notices());
        assert!(
            w.updates_of(&two.task).is_empty(),
            "{when}: {:?}",
            w.updates_of(&two.task)
        );
    };
    check_held(&w, "after the first judgement");
    std::thread::sleep(TEN_INTERVALS);
    check_held(&w, "ten intervals later");

    let answer = w.door().handle(
        Some(&w.secretary),
        "merge",
        json!({"repo": w.name, "pr": 2, "head": h2}),
        None,
        true,
    );
    let e = answer.expect_err("the merge of a held head is refused");
    assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
    assert!(e.message.contains("not ready for the queue"), "{e:?}");
    assert!(
        w.of_pr("merge_attempted", 2).is_empty(),
        "{:?}",
        w.of_pr("merge_attempted", 2)
    );

    w.land_pr(&one, &h1);
    w.until("PR 1's landing recorded", |w| {
        !w.of_pr("pr_merged", 1).is_empty()
    });
    w.until("the update of PR 2 finished", |w| {
        w.updates_of(&two.task)
            .iter()
            .any(|(_, s, _)| s != "accepted")
    });
    let decided = w.readiness_of(&h2);
    let lifted: Vec<&(i64, Value)> = decided
        .iter()
        .filter(|(_, p)| p["verdict"] == "update")
        .collect();
    assert_eq!(lifted.len(), 1, "{decided:?}");
    let (event, d) = lifted[0];
    assert_eq!(d["reason"], "overlap_landed", "{d}");
    assert_eq!(d["after"]["pr"], 1, "{d}");
    let updates = w.updates_of(&two.task);
    assert_eq!(updates.len(), 1, "{updates:?}");
    let (actor, status, args) = &updates[0];
    assert_eq!(actor, "makibad", "{updates:?}");
    assert_eq!(args["task"].as_str(), Some(two.task.as_str()), "{args}");
    assert_eq!(args["readiness"].as_i64(), Some(*event), "{args}");
    assert_eq!(status, "failed", "{updates:?}; {:#?}", w.notices());
    assert!(
        w.told(&[&format!("conflicts in {SHARED}")]) > 0,
        "the conflict is told: {:#?}",
        w.notices()
    );
    assert_eq!(w.origin_head(&two.branch), h2, "origin's branch moved");

    let h3 = w.worker_merge(&two, &shared(&[(3, "one two")]));
    w.door()
        .handle(
            Some(&w.secretary),
            "pr",
            json!({"state": "pushed", "repo": w.name, "pr": 2, "head": h3, "task": two.task}),
            None,
            true,
        )
        .expect("the worker's head is recorded");
    let ready3 = format!("{}#2 at {} {READY}", w.name, short(&h3));
    w.until("the worker's head told ready", |w| w.told(&[&ready3]) > 0);
    let decided3 = w.readiness_of(&h3);
    w.stop();
    assert_eq!(w.told(&[&ready3]), 1, "{:#?}", w.notices());
    assert!(
        decided3.iter().any(
            |(_, p)| p["verdict"] == "ready" && p["holds"].as_array().is_none_or(Vec::is_empty)
        ),
        "{decided3:?}"
    );
}

/// The design's case 2: PR 4 changes another line of the same file as PR 3, which is queued: the
/// two merge, so PR 4 is not held.
#[test]
fn a_pull_request_that_changes_other_lines_of_the_same_file_is_not_held() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pb");
    let (_three, h3) = w.fork("three", 3, 2, "three");
    w.start();
    let ready3 = format!("{}#3 at {} {READY}", w.name, short(&h3));
    w.until("PR 3 told ready", |w| w.told(&[&ready3]) > 0);
    w.queue(3);
    w.until("PR 3 entered the queue", |w| {
        !w.of_pr("merge_queue_entered", 3).is_empty()
    });

    let (_four, h4) = w.fork("four", 4, 9, "four");
    w.until("a decision of PR 4's head", |w| {
        !w.readiness_of(&h4).is_empty()
    });
    std::thread::sleep(TEN_INTERVALS);
    let decided = w.readiness_of(&h4);
    let ready4 = format!("{}#4 at {} {READY}", w.name, short(&h4));
    let told = w.told(&[&ready4]);
    w.stop();
    assert_eq!(told, 1, "{:#?}", w.notices());
    assert!(
        decided.iter().any(|(_, p)| p["verdict"] == "ready"),
        "{decided:?}"
    );
    assert!(
        !decided.iter().any(|(_, p)| p["verdict"] == "hold"),
        "{decided:?}"
    );
}

/// The design's case 3: PR 5 is told ready and PR 6 is held behind it; then the person queues PR 6
/// by hand, so PR 5 is no longer ready and is held behind PR 6 (a new conflicting peer while the
/// base has not moved).
#[test]
fn a_head_told_ready_is_held_when_the_person_queues_a_conflicting_one_first() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pc");
    let (five, h5) = w.fork("five", 5, 3, "five");
    w.start();
    let ready5 = format!("{}#5 at {} {READY}", w.name, short(&h5));
    w.until("PR 5 told ready", |w| w.told(&[&ready5]) > 0);

    let (six, h6) = w.fork("six", 6, 3, "six");
    w.until("a decision of PR 6's head", |w| {
        !w.readiness_of(&h6).is_empty()
    });
    let decided6 = w.readiness_of(&h6);
    assert_eq!(decided6.len(), 1, "{decided6:?}");
    let d6 = &decided6[0].1;
    assert_eq!(d6["verdict"], "hold", "{d6}");
    assert_eq!(
        d6["holds"],
        json!([{"pr": 5, "task": five.task, "head": h5, "queued": false,
                "files": [SHARED]}]),
        "{d6}"
    );

    w.queue(6);
    w.until("PR 5 held", |w| {
        w.readiness_of(&h5)
            .iter()
            .any(|(_, p)| p["verdict"] == "hold")
    });
    let decided5 = w.readiness_of(&h5);
    let no_longer = format!("{}#5 at {} {NO_LONGER}", w.name, short(&h5));
    let told_no_longer = w.told(&[&no_longer]);
    let told_ready = w.told(&[&ready5]);
    w.stop();
    let hold: Vec<&Value> = decided5
        .iter()
        .map(|(_, p)| p)
        .filter(|p| p["verdict"] == "hold")
        .collect();
    assert_eq!(hold.len(), 1, "{decided5:?}");
    assert_eq!(hold[0]["watched"], true, "{}", hold[0]);
    assert_eq!(
        hold[0]["holds"],
        json!([{"pr": 6, "task": six.task, "head": h6, "queued": true,
                "files": [SHARED]}]),
        "{}",
        hold[0]
    );
    assert!(told_no_longer > 0, "{:#?}", w.notices());
    assert_eq!(told_ready, 1, "{:#?}", w.notices());
}

/// The design's case 4: a held head is still watched against its base: a landing under `tools/`
/// updates it, as it would a ready head.
#[test]
fn a_held_head_is_updated_by_a_landing_that_changes_what_the_checks_check() {
    let _one = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut w = World::new("pd");
    let (_seven, h7) = w.fork("seven", 7, 3, "seven");
    w.start();
    let ready7 = format!("{}#7 at {} {READY}", w.name, short(&h7));
    w.until("PR 7 told ready", |w| w.told(&[&ready7]) > 0);
    let (eight, h8) = w.fork("eight", 8, 3, "eight");
    w.until("PR 8 held", |w| {
        w.readiness_of(&h8)
            .iter()
            .any(|(_, p)| p["verdict"] == "hold")
    });

    let l1 = w.land("tools/check.py", "print('checked')\n");
    w.until("PR 8's head updated", |w| {
        w.readiness_of(&h8)
            .iter()
            .any(|(_, p)| p["verdict"] == "update")
            && w.updates_of(&eight.task)
                .iter()
                .any(|(_, s, _)| s != "accepted")
    });
    let decided = w.readiness_of(&h8);
    let updates = w.updates_of(&eight.task);
    let new = w.origin_head(&eight.branch);
    let pushed: Vec<Value> = w
        .events("fix_pushed")
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| p["pr"].as_i64() == Some(8))
        .collect();
    w.stop();

    let update: Vec<&Value> = decided
        .iter()
        .map(|(_, p)| p)
        .filter(|p| p["verdict"] == "update")
        .collect();
    assert_eq!(update.len(), 1, "{decided:?}");
    assert_eq!(update[0]["reason"], "checks_changed", "{}", update[0]);
    assert!(
        update[0]["landings"]
            .as_array()
            .is_some_and(
                |ls| ls.iter().any(|l| l["sha"].as_str() == Some(l1.as_str())
                    && l["paths"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|x| x == "tools/check.py")))
            ),
        "L1 {l1} with tools/check.py is named: {}",
        update[0]
    );
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0].0, "makibad", "{updates:?}");
    assert_eq!(updates[0].1, "done", "{updates:?}; {:#?}", w.notices());
    assert!(
        holds(&w.origin, &l1, &new),
        "origin's branch at {new} does not hold L1 {l1}"
    );
    assert!(
        pushed
            .iter()
            .any(|p| p["head_oid"].as_str() == Some(new.as_str())),
        "the new head {new} is recorded: {pushed:?}"
    );
}
