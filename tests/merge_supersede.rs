//! Issue 432, after the diff review: a pending merge superseded by an act that runs at once is
//! withdrawn, and an approval whose entry the person ended while its gate ran answers stale
//! whatever the gate said, and tells nobody the entry stays open.
//!
//! The fixture is the one of `tests/merge_approve.rs` (that file is the declared acceptance test
//! and is not changed), under `human`.

// The fixture is a copy of the declared test's; some of its helpers are not used here.
#![allow(dead_code)]

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key, sha256_hex};
use makiba::daemon::merge::Merge;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::{DoorError, ErrorKind, Role};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
const T1: &str = "2026-11-05T01:00:00Z";
const ACTIONS: i64 = 15368;
const MERGE_SHA: &str = "abcdefabcdefabcdefabcdefabcdefabcdef0123";

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";
const TEST: &str = "from calc import add\n\n\ndef test_add():\n    assert add(2, 2) == 4\n";
const TRAILER: &str = "\n\nAcceptance-Test: tests/test_calc.py";

/// The stand-in for `gh`. The case's folder is named by the repository in the arguments
/// (`--repo o/<name>`, `repos/o/<name>/...`, `name=<name>`, or the node id `id=PR_<name>`). Every
/// call is logged in `calls`, one line each. `pr view <n>` prints `view.<n>`; the check runs and
/// workflow runs of a commit print `<sha>.check-runs` / `<sha>.runs`; the REST merge prints
/// `put.out` and exits with `put.exit`; the enqueue mutation prints `enqueue.out` and exits with
/// `enqueue.exit`; any other GraphQL call is the read and prints `read.json`. After the merge or
/// the enqueue, `read.after.json` (when there is one) becomes the read: GitHub's state moved.
const STAND_IN: &str = r#"#!/bin/sh
name=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--repo" ]; then name=${a#*/}; fi
  case "$a" in
    repos/*) r=${a#repos/}; r=${r#*/}; name=${r%%/*} ;;
    name=*) name=${a#name=} ;;
    id=PR_*) name=${a#id=PR_} ;;
  esac
  prev="$a"
done
dir="$FAKE_GH_DIR/$name"
echo "$*" >> "$dir/calls"
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  if [ -e "$dir/hold" ]; then
    touch "$dir/held"; n=0
    while [ -e "$dir/hold" ] && [ "$n" -lt 400 ]; do sleep 0.05; n=$((n+1)); done
  fi
  cat "$dir/view.$3"; exit 0
fi
moved() { if [ -e "$dir/read.after.json" ]; then cat "$dir/read.after.json" > "$dir/read.json"; fi; }
case "$*" in
  *check-runs*) oid=${3#*/commits/}; oid=${oid%%/*}
    if [ -e "$dir/$oid.check-runs" ]; then cat "$dir/$oid.check-runs"; fi; exit 0 ;;
  *actions/runs*) oid=${3#*head_sha=}; oid=${oid%%&*}
    if [ -e "$dir/$oid.runs" ]; then cat "$dir/$oid.runs"; fi; exit 0 ;;
  *enqueuePullRequest*) cat "$dir/enqueue.out"; moved; exit "$(cat "$dir/enqueue.exit")" ;;
  *"-X PUT"*) cat "$dir/put.out"; moved; exit "$(cat "$dir/put.exit")" ;;
  *graphql*) cat "$dir/read.json"; exit 0 ;;
esac
echo "unexpected call: $*" >&2; exit 2
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The stand-in's folder and the repositories folder. The environment is set once.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        common::exe::write(&script, STAND_IN);
        std::fs::create_dir_all(home.path().join("repos")).unwrap();
        // SAFETY: set once, before this file's tests read the environment.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path());
            std::env::set_var("MAKIBA_REPOS_DIR", home.path().join("repos"));
            std::env::set_var("MAKIBA_MERGE_APPROVAL_MODE", "human");
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One pull request's world: an origin, the project's clone, `state.db`, the door and the pass.
struct Case {
    _tmp: tempfile::TempDir,
    name: String,
    repo: String,
    origin: PathBuf,
    gh: PathBuf,
    db: PathBuf,
    door: Door,
    secretary: String,
    person: String,
    base: String,
    /// The test commit.
    test_commit: String,
    /// The pull request's head, judged.
    head: String,
}

impl Case {
    /// PR 7 of task `t1` at head H: the test commit failed CI, H passed it, the worker's last diff
    /// review passed at H, and the run is open, so the gate allows H under `acceptance`. GitHub
    /// reads the pull request open at H on a branch with no merge queue, and the REST merge lands. This
    /// binary runs under `human`, so the gate allows H with no warning and the person decides.
    fn new(name: &str) -> Case {
        let home = init();
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git_in(&origin, &["init", "-q", "-b", "main"]);
        let gh = home.join(name);
        std::fs::create_dir_all(&gh).unwrap();
        let state = tmp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let db = state.join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: name,
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT OR IGNORE INTO projects (slug, display_name) VALUES (?1, ?1)",
                [name],
            )
            .unwrap();
        w.conn()
            .execute(
                "UPDATE projects SET origin_url = ?2 WHERE slug = ?1",
                [name, &format!("https://github.com/o/{name}")],
            )
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        let mut c = Case {
            _tmp: tmp,
            name: name.into(),
            repo: format!("o/{name}"),
            origin,
            gh,
            db,
            door,
            secretary,
            person,
            base: String::new(),
            test_commit: String::new(),
            head: String::new(),
        };
        c.base = c.commit(
            "base",
            &[
                ("calc.py", Some(BUGGY)),
                ("pyproject.toml", Some("[project]\nname = 'x'\n")),
                (".github/workflows/ci.yml", Some("on: push\n")),
            ],
        );
        let clone = home.join("repos").join(name);
        assert!(!clone.exists(), "the case name {name} is used twice");
        git_in(
            &home.join("repos"),
            &["clone", "-q", c.origin.to_str().unwrap(), name],
        );
        let url = format!("https://github.com/o/{name}");
        git_in(&clone, &["remote", "set-url", "origin", &url]);
        git_in(
            &clone,
            &[
                "config",
                &format!("url.{}.insteadOf", c.origin.display()),
                &url,
            ],
        );
        c.git(&["checkout", "-q", "-b", "feature"]);
        c.test_commit = c.commit(
            &format!("test: failing test{TRAILER}"),
            &[("tests/test_calc.py", Some(TEST))],
        );
        c.commit("fix", &[("calc.py", Some(FIXED))]);
        c.head = c.ci_all(7);
        c.reviewed(7, &c.head);
        c.read(&c.head, "OPEN", false, false);
        c.put(
            "HTTP/2.0 200 OK\nContent-Type: application/json; charset=utf-8\n\n",
            0,
        );
        c
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
    }

    fn commit(&self, msg: &str, files: &[(&str, Option<&str>)]) -> String {
        for (path, content) in files {
            let dest = self.origin.join(path);
            match content {
                None => std::fs::remove_file(&dest).unwrap(),
                Some(text) => {
                    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    std::fs::write(&dest, text).unwrap();
                }
            }
        }
        self.git(&["add", "--all"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Publish `head` as PR `pr`'s head and let the stand-in answer `gh pr view` for it.
    fn publish_at(&self, pr: i64, head: &str) {
        self.git(&["update-ref", &format!("refs/pull/{pr}/head"), head]);
        let view = json!({"number": pr, "url": format!("file://{}/pull/{pr}", self.origin.display()),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(self.gh.join(format!("view.{pr}")), view.to_string()).unwrap();
    }

    fn answer(&self, sha: &str, kind: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.gh.join(format!("{sha}.{kind}")), text).unwrap();
    }

    /// A `ci_completed` row, written while no makibad runs.
    fn ci(&self, pr: i64, status: &str, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        w.append_event(&NewEvent {
            kind: "ci_completed",
            actor: Some("makibad"),
            payload: Some(
                &json!({"repo": self.repo, "pr": pr, "head_oid": head, "status": status,
                                   "fail_count": 0, "pending_count": 0, "total_checks": 1,
                                   "probe_attempts": 1}),
            ),
            ..Default::default()
        })
        .unwrap();
    }

    /// Publish HEAD as PR `pr`: every commit of the PR but its head failed the job `test`, the
    /// head passed it, all on base C. Its head.
    fn ci_all(&self, pr: i64) -> String {
        let head = self.git(&["rev-parse", "HEAD"]);
        self.publish_at(pr, &head);
        let range = format!("{}..{head}", self.base);
        for sha in self.git(&["rev-list", "--reverse", &range]).lines() {
            let ok = sha == head;
            let conclusion = if ok { "success" } else { "failure" };
            self.answer(
                sha,
                "check-runs",
                &[
                    json!({"app": ACTIONS, "name": "test", "conclusion": conclusion, "completed_at": T1, "suite": 1}),
                ],
            );
            self.answer(
                sha,
                "runs",
                &[json!({"wf": 1, "suite": 1, "created": T1, "bases": [C]})],
            );
            if !ok {
                self.ci(pr, "failed", sha);
            }
        }
        self.ci(pr, "passed", &head);
        head
    }

    /// The code task `t1` has a passed diff review of `head`, and a watch ties it to PR `pr`.
    fn reviewed(&self, pr: i64, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = 't1'", [], |r| r.get(0))
            .unwrap();
        let answer = json!({"verdict": "passed", "kind": "diff", "reader": "codex", "round": 1,
                            "allowed": 3, "head": head,
                            "findings": {"blocker": 0, "major": 0, "minor": 0}});
        let t = |sql: &str, p: &[&dyn rusqlite::ToSql]| w.conn().execute(sql, p).unwrap();
        t(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES (?1, 't1-s1', 'code', 'fake', 'b', '/nonexistent', 'verified')",
            &[&run],
        );
        t(
            "INSERT INTO pr_watches (repo, pr, head_oid, run_id) VALUES (?1, ?2, ?3, ?4)",
            &[&self.repo, &pr, &head, &run],
        );
        t(
            "INSERT INTO commands (actor, verb, status, result_json) VALUES (?1, 'review', 'done', ?2)",
            &[
                &makiba::spec::worker_agent_id("t1"),
                &json!({"result": answer}).to_string(),
            ],
        );
    }

    /// The GraphQL read of PR 7 as `file` (`read.json`, or `read.after.json`: the read once the
    /// act was sent).
    fn read_file(&self, file: &str, head: &str, state: &str, queue: bool, queued: bool) {
        let merged = state == "MERGED";
        let v = json!({"data": {"repository": {"pullRequest": {
            "id": format!("PR_{}", self.name),
            "state": state,
            "headRefOid": head,
            "isMergeQueueEnabled": queue,
            "isInMergeQueue": queued,
            "mergeQueueEntry": if queued { json!({"id": "MQE_1", "state": "QUEUED", "position": 1}) } else { Value::Null },
            "mergeCommit": if merged { json!({"oid": MERGE_SHA}) } else { Value::Null },
            "mergedAt": if merged { json!(T1) } else { Value::Null },
        }}}});
        std::fs::write(self.gh.join(file), v.to_string()).unwrap();
    }

    fn read(&self, head: &str, state: &str, queue: bool, queued: bool) {
        self.read_file("read.json", head, state, queue, queued);
    }

    /// The REST merge's answer: `head` (the status line and headers) and the body, and the exit.
    fn put(&self, head: &str, exit: i32) {
        let body = json!({"sha": MERGE_SHA, "merged": true, "message": "Pull Request successfully merged"});
        self.put_raw(&format!("{head}{body}\n"), exit);
    }

    fn put_raw(&self, out: &str, exit: i32) {
        std::fs::write(self.gh.join("put.out"), out).unwrap();
        std::fs::write(self.gh.join("put.exit"), exit.to_string()).unwrap();
    }

    /// The door's answer to the secretary's tool `name` with `args`.
    fn tool(&self, name: &str, args: Value) -> Result<Value, DoorError> {
        self.door
            .handle(Some(&self.secretary), name, args, None, true)
    }

    /// The secretary's `merge` request for PR 7 at `head`.
    fn request(&self, head: &str) -> Result<Value, DoorError> {
        self.tool(
            "merge",
            json!({"action": "request", "repo": self.repo, "pr": 7, "head": head}),
        )
    }

    fn pr(&self, state: &str, pr: i64, head: &str) -> Result<Value, DoorError> {
        self.tool(
            "pr",
            json!({"state": state, "repo": self.repo, "pr": pr, "head": head, "task": "t1"}),
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.gh.join("calls")).unwrap_or_default()
    }

    fn lines_with(&self, needle: &str) -> Vec<String> {
        self.calls()
            .lines()
            .filter(|l| l.contains(needle))
            .map(String::from)
            .collect()
    }

    fn puts(&self) -> Vec<String> {
        self.lines_with("-X PUT")
    }

    fn enqueues(&self) -> Vec<String> {
        self.lines_with("enqueuePullRequest")
    }

    fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    /// `(id, payload)` of every event of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<(i64, Value)> {
        let db = self.conn();
        let mut st = db
            .prepare("SELECT id, payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        st.query_map([kind], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| {
            let (id, t) = r.unwrap();
            (id, serde_json::from_str(&t).unwrap())
        })
        .collect()
    }

    fn count(&self, kind: &str) -> usize {
        self.events(kind).len()
    }

    /// The statuses of the `merge` commands, oldest first.
    fn merge_commands(&self) -> Vec<String> {
        let db = self.conn();
        let mut st = db
            .prepare("SELECT status FROM commands WHERE verb = 'merge' ORDER BY id")
            .unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The notices makibad wrote to the secretary, oldest first.
    fn notices(&self) -> Vec<String> {
        let db = self.conn();
        let mut st = db
            .prepare(
                "SELECT body FROM messages WHERE sender = 'makibad' AND recipient = 'secretary'
                 AND kind = 'notice' ORDER BY id",
            )
            .unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Whether every `merge_attempted` has an outcome.
    fn settled(&self) -> bool {
        let outcomes: Vec<i64> = [
            "merge_done",
            "merge_enqueued",
            "merge_failed",
            "merge_unknown",
        ]
        .iter()
        .flat_map(|k| self.events(k))
        .filter_map(|(_, v)| v["attempt"].as_i64())
        .collect();
        self.events("merge_attempted")
            .iter()
            .all(|(id, _)| outcomes.contains(id))
    }

    /// What the passes may change: the stand-in's calls, the events and the commands.
    fn snapshot(&self) -> (String, i64, Vec<String>) {
        let n: i64 = self
            .conn()
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        (self.calls(), n, self.merge_commands())
    }

    /// The pass, until every attempt has its outcome (at most 30 s), then five more passes that
    /// must change nothing.
    fn run_passes(&self, m: &mut Merge) {
        let w = StateWriter::open(&self.db).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            m.pass(&w).unwrap();
            if self.settled() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no outcome within 30 s; calls:\n{}",
                self.calls()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let before = self.snapshot();
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(50));
            m.pass(&w).unwrap();
        }
        assert_eq!(
            self.snapshot(),
            before,
            "five more passes changed something"
        );
    }
}

impl Case {
    /// The door's answer to the person's `merge` with `args` (the person's key, as `makiba` sends it).
    fn person(&self, args: Value) -> Result<Value, DoorError> {
        self.door
            .handle(Some(&self.person), "merge", args, None, false)
    }

    fn request_with(&self, pr: i64, head: &str, method: &str) -> Result<Value, DoorError> {
        self.tool(
            "merge",
            json!({"action": "request", "repo": self.repo, "pr": pr, "head": head, "method": method}),
        )
    }

    /// The person's approval of entry `id` with the sum of `printed`.
    fn approve(&self, id: i64, printed: &str) -> Result<Value, DoorError> {
        self.person(json!({"action": "approve", "id": id, "shown": sha256_hex(printed)}))
    }

    /// What `makiba merge approve <id>` prints, read as the CLI reads it.
    fn printed(&self, id: i64) -> String {
        makiba::status_local::merge_entry_text(&self.db, id)
            .unwrap_or_else(|e| panic!("entry {id}: {e}"))
    }

    /// `(kind, status, answer, answered_by, task)` of pending entry `id`.
    #[allow(clippy::type_complexity)]
    fn entry(
        &self,
        id: i64,
    ) -> (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        self.conn()
            .query_row(
                "SELECT p.kind, p.status, p.answer, p.answered_by, r.task_id
                 FROM pending p LEFT JOIN runs r ON r.id = p.run_id WHERE p.id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap()
    }

    fn status_of(&self, id: i64) -> String {
        self.entry(id).1
    }

    /// The number of entries of kind `merge` with `status`.
    fn merge_entries(&self, status: &str) -> i64 {
        self.conn()
            .query_row(
                "SELECT count(*) FROM pending WHERE kind = 'merge' AND status = ?1",
                [status],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// makibad's merge pass, run until settled and five more times; nothing may have been sent.
    fn nothing_sent(&self) {
        let mut m = Merge::new();
        self.run_passes(&mut m);
        assert!(self.puts().is_empty(), "{}", self.calls());
        assert!(self.enqueues().is_empty(), "{}", self.calls());
    }

    /// A newer diff review of `head` by t1's worker, passed with one blocker.
    fn review_with_blocker(&self, head: &str) {
        let answer = json!({"verdict": "passed", "kind": "diff", "reader": "codex", "round": 2,
                            "allowed": 3, "head": head,
                            "findings": {"blocker": 1, "major": 0, "minor": 0}});
        self.conn()
            .execute(
                "INSERT INTO commands (actor, verb, status, result_json) VALUES (?1, 'review', 'done', ?2)",
                rusqlite::params![
                    makiba::spec::worker_agent_id("t1"),
                    json!({"result": answer}).to_string()
                ],
            )
            .unwrap();
    }
}

/// The entry of a `pending` answer.
fn pending_id(r: Result<Value, DoorError>) -> i64 {
    let v = r.unwrap_or_else(|e| panic!("merge: {e:?}"));
    assert_eq!(v["status"], json!("pending"), "{v}");
    v["pending_id"]
        .as_i64()
        .unwrap_or_else(|| panic!("no pending_id: {v}"))
}

fn refused_as(r: Result<Value, DoorError>, kind: ErrorKind) -> DoorError {
    match r {
        Err(e) if e.kind == kind => e,
        other => panic!("not {kind:?}: {other:?}"),
    }
}

// ---- a direct request supersedes the entry ----------------------------------------------------------

#[test]
fn an_attempt_the_person_asks_directly_withdraws_the_pending_entry() {
    let c = Case::new("superseded");
    let h = c.head.clone();
    let n = pending_id(c.request(&h));
    let text = c.printed(n);
    let v = c
        .person(json!({"action": "request", "repo": c.repo, "pr": 7, "head": h}))
        .unwrap_or_else(|e| panic!("merge: {e:?}"));
    assert_eq!(v["status"], json!("accepted"), "{v}");
    assert_eq!(c.status_of(n), "withdrawn");
    assert_eq!(c.merge_entries("open"), 0);
    let entry = format!("entry {n}");
    assert!(
        c.notices().iter().any(|t| t.contains(&entry)),
        "{:?}",
        c.notices()
    );
    // Once the attempt has ended, the entry cannot make a second one.
    let mut m = Merge::new();
    c.run_passes(&mut m);
    match c.approve(n, &text) {
        Err(e) if e.kind == ErrorKind::Stale => {}
        other => panic!("not stale: {other:?}"),
    }
    assert_eq!(c.count("merge_attempted"), 1);
}

// ---- an entry ended while the approval's gate ran, and the gate refused ---------------------------

#[test]
fn an_approval_whose_entry_ended_while_a_refusing_gate_ran_is_stale() {
    let c = Case::new("ended-and-refused");
    let h = c.head.clone();
    let n = pending_id(c.request(&h));
    let text = c.printed(n);
    let hold = c.gh.join("hold");
    let held = c.gh.join("held");
    std::fs::write(&hold, "").unwrap();
    let approved = std::thread::scope(|s| {
        let a = s.spawn(|| c.approve(n, &text));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !held.exists() {
            assert!(Instant::now() < deadline, "the approval's gate never ran");
            std::thread::sleep(Duration::from_millis(20));
        }
        c.person(json!({"action": "refuse", "id": n}))
            .unwrap_or_else(|e| panic!("refuse: {e:?}"));
        // The CI result changes while the gate runs, so the gate refuses.
        c.ci(7, "failed", &h);
        std::fs::remove_file(&hold).unwrap();
        a.join().unwrap()
    });
    match approved {
        Err(e) if e.kind == ErrorKind::Stale => {}
        other => panic!("not stale: {other:?}"),
    }
    assert_eq!(c.count("merge_attempted"), 0);
    assert!(
        !c.notices().iter().any(|t| t.contains("stays open")),
        "{:?}",
        c.notices()
    );
}
