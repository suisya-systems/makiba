//! Issue 431 (docs/design/merge-act.md, build Issue 1): makibad's landing verb and its records.
//!
//! The secretary asks with the door verb `merge`; the acceptance gate runs inside it, and on an
//! allowance the attempt (`merge_attempted`) is committed with the gate's row before anything is
//! sent to GitHub. makibad's pass `daemon::merge::Merge` then runs the act once (the REST landing
//! bound to the judged head, or the merge-queue enqueue bound to it), records one outcome with the
//! command's end and a notice, and never sends an attempt again; an attempt of an earlier start is
//! only read and recorded as observed, with the actor unknown.
//!
//! As in `tests/acceptance_gate.rs`: a local origin publishes `refs/pull/<n>/head`, the project's
//! clone sits in the repositories folder, and `gh` is a stand-in script (`MAKIBA_GH`) that answers
//! from files of the case's folder and logs every call. The door is called in process, as the
//! secretary's tool; no makibad loop runs, the test calls the pass itself. Every case runs under
//! `merge_approval_mode` `acceptance` (this binary's environment).
//!
//! GitHub's own answers (merge-act.md section 10, claims 1-4) are those of the stand-in: they are
//! [unverified] here and confirmed at the first real landing.

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::merge::{self, Merge};
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
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then cat "$dir/view.$3"; exit 0; fi
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
            std::env::set_var("MAKIBA_MERGE_APPROVAL_MODE", "acceptance");
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
    base: String,
    /// The test commit.
    test_commit: String,
    /// The pull request's head, judged.
    head: String,
}

impl Case {
    /// PR 7 of task `t1` at head H: the test commit failed CI, H passed it, the worker's last diff
    /// review passed at H, and the run is open, so the gate allows H under `acceptance`. GitHub
    /// reads the pull request open at H on a branch with no merge queue, and the REST merge lands.
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

    fn enqueue(&self, out: &str, exit: i32) {
        std::fs::write(self.gh.join("enqueue.out"), out).unwrap();
        std::fs::write(self.gh.join("enqueue.exit"), exit.to_string()).unwrap();
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

    /// An event written as makibad or a test writes it, outside the door.
    fn append(&self, kind: &str, task: Option<&str>, payload: Value) -> i64 {
        let w = StateWriter::open(&self.db).unwrap();
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: task,
            ..Default::default()
        })
        .unwrap()
    }
}

fn conflict(r: Result<Value, DoorError>) -> DoorError {
    match r {
        Err(e) if e.kind == ErrorKind::Conflict => e,
        other => panic!("not a conflict: {other:?}"),
    }
}

// ---- 1. an allowed request lands at the judged head ----------------------------------------------

#[test]
fn an_allowed_merge_is_run_by_makibad_at_the_judged_head() {
    let c = Case::new("landed");
    let h = c.head.clone();
    let v = c.request(&h).unwrap_or_else(|e| panic!("merge: {e:?}"));
    assert_eq!(v["status"], json!("accepted"), "{v}");
    assert_eq!(v["head"], json!(h), "{v}");
    assert_eq!(v["method"], json!("squash"), "{v}");
    let attempt = v["attempt"].as_i64().expect("an attempt number");

    let gates = c.events("acceptance_gate");
    assert_eq!(gates.len(), 1, "{gates:?}");
    let (gate_id, gate) = &gates[0];
    assert_eq!(gate["allow"], json!(true), "{gate}");
    assert_eq!(gate["via"], json!("merge"), "{gate}");

    let attempts = c.events("merge_attempted");
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    let (id, a) = &attempts[0];
    assert_eq!(*id, attempt);
    assert_eq!(a["head"], json!(h));
    assert_eq!(a["method"], json!("squash"));
    assert_eq!(a["repo"], json!(c.repo));
    assert_eq!(a["pr"], json!(7));
    assert_eq!(a["gate_event"], json!(gate_id), "{a}");
    assert_eq!(a["boot"], json!(merge::boot()), "{a}");
    assert!(id > gate_id, "the attempt is written after the gate's row");
    assert_eq!(c.merge_commands(), ["accepted"]);

    // Nothing is sent before the pass.
    assert!(c.puts().is_empty(), "{}", c.calls());
    assert!(c.enqueues().is_empty(), "{}", c.calls());

    let mut m = Merge::new();
    c.run_passes(&mut m);

    let puts = c.puts();
    assert_eq!(puts.len(), 1, "{}", c.calls());
    let put = &puts[0];
    assert!(
        put.contains(&format!("repos/{}/pulls/7/merge", c.repo)),
        "{put}"
    );
    assert!(put.contains(&format!("sha={h}")), "{put}");
    assert!(put.contains("merge_method=squash"), "{put}");
    assert!(c.enqueues().is_empty(), "{}", c.calls());

    let done = c.events("merge_done");
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0].1["attempt"], json!(attempt));
    assert_eq!(done[0].1["merge_commit"], json!(MERGE_SHA));
    assert_eq!(c.count("merge_failed"), 0);
    assert_eq!(c.count("merge_unknown"), 0);
    assert_eq!(c.merge_commands(), ["done"]);
    let notices = c.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("merged") && notices[0].contains("#7") && notices[0].contains(&h),
        "{}",
        notices[0]
    );
}

// ---- 2. a refused request sends nothing ---------------------------------------------------------

#[test]
fn a_refused_merge_sends_nothing() {
    let c = Case::new("refused");
    let h = c.head.clone();
    c.ci(7, "failed", &h);
    let v = c.request(&h).unwrap_or_else(|e| panic!("merge: {e:?}"));
    assert_eq!(v["status"], json!("refused"), "{v}");
    assert_eq!(v["verdict"], json!("rejected"), "{v}");
    assert_eq!(c.merge_commands(), ["rejected"]);
    let gates = c.events("acceptance_gate");
    assert_eq!(gates.len(), 1, "{gates:?}");
    assert_eq!(gates[0].1["allow"], json!(false));
    assert_eq!(gates[0].1["via"], json!("merge"));
    assert_eq!(c.count("merge_attempted"), 0);

    let mut m = Merge::new();
    c.run_passes(&mut m);
    assert!(c.puts().is_empty(), "{}", c.calls());
    assert!(c.enqueues().is_empty(), "{}", c.calls());
    for k in [
        "merge_attempted",
        "merge_done",
        "merge_enqueued",
        "merge_failed",
        "merge_unknown",
        "merge_dequeued",
    ] {
        assert_eq!(c.count(k), 0, "{k}");
    }
}

// ---- 3. an answer that cannot be read is unknown, never failed -----------------------------------

#[test]
fn a_502_answer_is_recorded_unknown_not_failed() {
    let c = Case::new("bad-gateway");
    let h = c.head.clone();
    c.put_raw(
        "HTTP/2.0 502 Bad Gateway\nContent-Type: text/html\n\n<html><body>502 Bad Gateway</body></html>\n",
        1,
    );
    let v = c.request(&h).unwrap_or_else(|e| panic!("merge: {e:?}"));
    assert_eq!(v["status"], json!("accepted"), "{v}");
    let attempt = v["attempt"].as_i64().unwrap();

    let mut m = Merge::new();
    c.run_passes(&mut m);
    assert_eq!(c.puts().len(), 1, "{}", c.calls());
    let unknown = c.events("merge_unknown");
    assert_eq!(unknown.len(), 1, "{unknown:?}");
    assert_eq!(unknown[0].1["attempt"], json!(attempt));
    assert_eq!(unknown[0].1["observed"], json!("open"), "{}", unknown[0].1);
    assert_eq!(unknown[0].1["actor"], json!("unknown"));
    assert_eq!(c.count("merge_failed"), 0);
    assert_eq!(c.count("merge_done"), 0);
    assert_eq!(c.merge_commands(), ["failed"]);
    let notices = c.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("cannot tell"), "{}", notices[0]);
}

// ---- 4. a definitive rejection is failed --------------------------------------------------------

#[test]
fn a_definitive_rejection_is_recorded_failed() {
    let c = Case::new("not-allowed");
    let h = c.head.clone();
    c.put_raw(
        "HTTP/2.0 405 Method Not Allowed\nContent-Type: application/json\n\n{\"message\":\"Pull Request is not mergeable\"}\n",
        1,
    );
    let v = c.request(&h).unwrap_or_else(|e| panic!("merge: {e:?}"));
    let attempt = v["attempt"].as_i64().unwrap();

    let mut m = Merge::new();
    c.run_passes(&mut m);
    assert_eq!(c.puts().len(), 1, "{}", c.calls());
    let failed = c.events("merge_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].1["attempt"], json!(attempt));
    assert_eq!(failed[0].1["sent"], json!(true), "{}", failed[0].1);
    assert_eq!(failed[0].1["http_status"], json!(405), "{}", failed[0].1);
    assert_eq!(c.count("merge_unknown"), 0);
    assert_eq!(c.count("merge_done"), 0);
    assert_eq!(c.merge_commands(), ["failed"]);
    assert_eq!(c.notices().len(), 1);
}

// ---- 5. an attempt of an earlier start is read, never sent ---------------------------------------

#[test]
fn an_attempt_of_an_earlier_start_is_reconciled_and_not_sent_again() {
    let c = Case::new("restarted");
    let h = c.head.clone();
    // What an earlier makibad committed before it stopped: the allowance, the accepted command and
    // the attempt, with no outcome.
    let gate = c.append(
        "acceptance_gate",
        Some("t1"),
        json!({"repo": c.repo, "pr": 7, "base": C, "head": h, "result": "passed",
               "reason": "ok", "mode": "acceptance", "allow": true, "task": "t1", "via": "merge"}),
    );
    let command: i64 = {
        let db = c.conn();
        db.execute(
            "INSERT INTO commands (actor, verb, args_json, status, result_json)
             VALUES ('secretary', 'merge', ?1, 'accepted', '{}')",
            [json!({"action": "request", "repo": c.repo, "pr": 7, "head": h}).to_string()],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    let attempt = c.append(
        "merge_attempted",
        Some("t1"),
        json!({"repo": c.repo, "pr": 7, "head": h, "method": "squash", "gate_event": gate,
               "command": command, "approved_by": null, "boot": "an-earlier-start",
               "task_id": "t1"}),
    );
    // GitHub shows it merged at that head.
    c.read(&h, "MERGED", false, false);

    let mut m = Merge::new();
    c.run_passes(&mut m);
    assert!(c.puts().is_empty(), "{}", c.calls());
    assert!(c.enqueues().is_empty(), "{}", c.calls());
    let unknown = c.events("merge_unknown");
    assert_eq!(unknown.len(), 1, "{unknown:?}");
    assert_eq!(unknown[0].1["attempt"], json!(attempt));
    assert_eq!(
        unknown[0].1["observed"],
        json!("merged"),
        "{}",
        unknown[0].1
    );
    assert_eq!(unknown[0].1["actor"], json!("unknown"));
    assert_eq!(c.count("merge_done"), 0);
    assert_eq!(c.merge_commands(), ["failed"]);
    assert_eq!(c.notices().len(), 1, "{:?}", c.notices());
}

// ---- 6. after an enqueue, a second request is refused until the queue act ends --------------------

#[test]
fn after_an_enqueue_a_second_request_is_refused_until_the_queue_act_ends() {
    let c = Case::new("queued");
    let h = c.head.clone();
    c.read(&h, "OPEN", true, false);
    c.read_file("read.after.json", &h, "OPEN", true, true);
    c.enqueue(
        &json!({"data": {"enqueuePullRequest": {"mergeQueueEntry": {"id": "MQE_1", "state": "QUEUED", "position": 1}}}})
            .to_string(),
        0,
    );
    let v = c.request(&h).unwrap_or_else(|e| panic!("merge: {e:?}"));
    assert_eq!(v["status"], json!("accepted"), "{v}");
    let mut m = Merge::new();
    c.run_passes(&mut m);
    let enqueues = c.enqueues();
    assert_eq!(enqueues.len(), 1, "{}", c.calls());
    assert!(enqueues[0].contains(&h), "{}", enqueues[0]);
    assert!(
        enqueues[0].contains(&format!("PR_{}", c.name)),
        "{}",
        enqueues[0]
    );
    assert!(c.puts().is_empty(), "{}", c.calls());
    assert_eq!(c.count("merge_enqueued"), 1);
    assert_eq!(c.merge_commands(), ["done"]);

    // A second request at the same head: the queue act has not ended.
    conflict(c.request(&h));
    assert_eq!(c.count("merge_attempted"), 1);

    // Another pull request cannot replace the run's current one while its act awaits its landing.
    c.publish_at(8, &c.test_commit);
    let opened = c.count("pr_opened");
    conflict(c.pr("opened", 8, &c.test_commit));
    assert_eq!(c.count("pr_opened"), opened);
    let pushed = c.count("fix_pushed");
    conflict(c.pr("pushed", 8, &c.test_commit));
    assert_eq!(c.count("fix_pushed"), pushed);

    // The landing ends the queue act. (Written with no run, so the run still follows PR 7 and the
    // request reaches the act's check rather than the watch's.)
    c.append(
        "pr_merged",
        None,
        json!({"repo": c.repo, "pr": 7, "head_oid": h}),
    );
    if let Err(e) = c.request(&h) {
        assert_ne!(e.kind, ErrorKind::Conflict, "{e:?}");
    }
}

// ---- 7. [R2] a landing recorded before the outcome ends the act ----------------------------------

#[test]
fn a_landing_recorded_before_the_outcome_ends_the_act() {
    let c = Case::new("landed-first");
    let h = c.head.clone();
    let v = c.request(&h).unwrap_or_else(|e| panic!("merge: {e:?}"));
    let attempt = v["attempt"].as_i64().unwrap();
    // The PR watch reads the landing before the act's outcome is written.
    let landed = c.append(
        "pr_merged",
        Some("t1"),
        json!({"repo": c.repo, "pr": 7, "head_oid": h, "merged_via": "hand", "task_id": "t1"}),
    );
    assert!(landed > attempt);
    let mut m = Merge::new();
    c.run_passes(&mut m);
    let done = c.events("merge_done");
    assert_eq!(done.len(), 1, "{done:?}");
    assert!(done[0].0 > landed, "the outcome comes after the landing");
    // The act has ended: the run may record another pull request.
    c.publish_at(8, &c.test_commit);
    let v = c
        .pr("opened", 8, &c.test_commit)
        .unwrap_or_else(|e| panic!("pr opened: {e:?}"));
    assert!(v["watch_id"].is_i64(), "{v}");
}

// ---- 8. [R4] a new head ends an unknown enqueued act ---------------------------------------------

#[test]
fn a_new_head_ends_an_unknown_enqueued_act() {
    let c = Case::new("queued-unknown");
    let h = c.head.clone();
    c.read(&h, "OPEN", true, false);
    c.read_file("read.after.json", &h, "OPEN", true, true);
    // The enqueue's answer cannot be read; the read after it shows the pull request queued.
    c.enqueue("", 1);
    c.request(&h).unwrap_or_else(|e| panic!("merge: {e:?}"));
    let mut m = Merge::new();
    c.run_passes(&mut m);
    let unknown = c.events("merge_unknown");
    assert_eq!(unknown.len(), 1, "{unknown:?}");
    assert_eq!(
        unknown[0].1["observed"],
        json!("enqueued"),
        "{}",
        unknown[0].1
    );
    assert_eq!(c.count("merge_dequeued"), 0);

    // A new head of the same pull request is recorded: the entry was bound to the old one.
    let h2 = c.commit("more", &[("README.md", Some("more\n"))]);
    c.publish_at(7, &h2);
    c.pr("pushed", 7, &h2)
        .unwrap_or_else(|e| panic!("pr pushed: {e:?}"));
    let dequeued = c.events("merge_dequeued");
    assert_eq!(dequeued.len(), 1, "{dequeued:?}");
    assert_eq!(dequeued[0].1["why"], json!("head recorded"));
    assert_eq!(dequeued[0].1["attempt"], unknown[0].1["attempt"]);
}

// ---- 9. [R5] a pull request the watch does not follow is refused ---------------------------------

#[test]
fn a_pull_request_the_watch_does_not_follow_is_refused() {
    let c = Case::new("unfollowed");
    let h = c.head.clone();
    c.conn()
        .execute(
            "UPDATE runs SET status = 'completed', completed_at = '2026-01-01T00:00:00Z'
             WHERE task_id = 't1'",
            [],
        )
        .unwrap();
    conflict(c.request(&h));
    assert_eq!(c.count("merge_attempted"), 0);
    assert!(c.puts().is_empty() && c.enqueues().is_empty());
}
