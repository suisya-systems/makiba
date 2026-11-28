//! Issue 592 (docs/design/acceptance-approval.md 4.4 (1)-(4), build step 4): the person's
//! `merge approve` signs the acceptance approval, makibad records `acceptance_approved`, and the
//! next request at the same head and method lands on that one approval.
//!
//! - A head whose only failed check is `acceptance`, failed by construction, gives one pending
//!   merge entry under both modes; the entry holds the approval text of note section 4.2 byte for
//!   byte, with GitHub's spelling of the repository and the entry's own id, and shows it.
//! - `approve` of that entry needs the person's signature (made with `makiba::cli::sign_approval`,
//!   the CLI's own function); it records `acceptance_approved` with the comment the secretary
//!   posts, tells the secretary to post it and rerun, and lands nothing.
//! - Once the acceptance check passes on a rerun, a request at that head by that method lands on
//!   the approval (`merge_attempted` approved by the person); another method, another head, a
//!   newer approval of another method, a new warning, a missing acceptance pass, a change under
//!   `.github/` on the base, or any later attempt at the head does not.
//!
//! The fixture is that of tests/merge_approve.rs (a local origin publishing `refs/pull/<n>/head`,
//! the project's clone, the door in process), with a stand-in `gh` (made through
//! tests/common/exe.rs) that also serves the job log and the head's check surface. Each test runs
//! its body under `MAKIBA_MERGE_APPROVAL_MODE` `acceptance` and then `human`; one lock serializes
//! the tests, as the mode is this process's environment. The outcomes of attempts are written into
//! the ledger as the merge pass writes them; nothing is sent to GitHub.

mod common;

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key, sha256_hex};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::{DoorError, ErrorKind, Role};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once, OnceLock};

const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
const T1: &str = "2026-11-28T03:00:00Z";
const T2: &str = "2026-11-28T04:00:00Z";
const ACTIONS: i64 = 15368;
const STAMP: &str = "2026-11-28T03:04:23.8170690Z";
const ANSWER: &str = "changed tests/answer.rs";
const SECOND: &str = "ignored tests/second.rs 1";

/// The stand-in for `gh`. The case's folder is named by the repository in the arguments
/// (`--repo o/<name>`, `repos/o/<name>/...`). Every call is logged in `calls`. `pr view <n>`
/// prints `view.<n>`; a job's log prints `log.<id>`; the check runs of the pull request's
/// workflow runs, the check surface, the commit statuses, the pull request's workflow runs and
/// all workflow runs of a commit print `<sha>.checks`, `.surface`, `.status`, `.runs` and
/// `.going`. Any other call, a landing among them, is logged `unexpected call` and fails.
const STAND_IN: &str = r#"#!/bin/sh
name=""; prev=""; ep=""
for a in "$@"; do
  if [ "$prev" = "--repo" ]; then name=${a#*/}; fi
  case "$a" in
    repos/*) ep=$a; r=${a#repos/}; r=${r#*/}; name=${r%%/*} ;;
  esac
  prev="$a"
done
dir="$FAKE_GH_DIR/$name"
echo "$*" >> "$dir/calls"
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then cat "$dir/view.$3"; exit 0; fi
case "$*" in
  *"-X PUT"*|*enqueuePullRequest*|*graphql*) echo "unexpected call" >> "$dir/calls"; exit 2 ;;
esac
case "$ep" in
  */actions/jobs/*/logs) id=${ep#*/actions/jobs/}; id=${id%%/*}; cat "$dir/log.$id"; exit 0 ;;
  *check-runs?filter=all*) kind=checks ;;
  *check-runs*) kind=surface ;;
  */status*) kind=status ;;
  *event=pull_request*) kind=runs ;;
  */actions/runs*) kind=going ;;
  *) echo "unexpected call" >> "$dir/calls"; exit 2 ;;
esac
case "$ep" in
  */commits/*) sha=${ep#*/commits/}; sha=${sha%%/*} ;;
  *) sha=${ep#*head_sha=}; sha=${sha%%&*} ;;
esac
if [ -e "$dir/$sha.$kind" ]; then cat "$dir/$sha.$kind"; fi
exit 0
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();
/// The mode is this process's environment: one test body at a time.
static MODE: Mutex<()> = Mutex::new(());

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
            std::env::set_var(makiba::daemon::gate::READ_WAIT_ENV, "0");
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

/// `body` once under each merge approval mode, with a case name of its own for each.
fn both(name: &str, body: impl Fn(&str)) {
    let _held = MODE.lock().unwrap_or_else(|e| e.into_inner());
    init();
    for (mode, tag) in [("acceptance", "a"), ("human", "h")] {
        // SAFETY: every test body runs under the lock `MODE`.
        unsafe { std::env::set_var("MAKIBA_MERGE_APPROVAL_MODE", mode) };
        body(&format!("{name}-{tag}"));
    }
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

/// One pull request's world: an origin, the project's clone, `state.db`, the door, the key.
struct Case {
    tmp: tempfile::TempDir,
    /// The repository as makiba names it (`o/<case name>`, lower case).
    repo: String,
    /// The repository as GitHub spells it in the pull request's url (an upper-case letter).
    github: String,
    origin: PathBuf,
    gh: PathBuf,
    db: PathBuf,
    door: Door,
    secretary: String,
    person: String,
    key: PathBuf,
    test_commit: String,
    head: String,
}

impl Case {
    /// PR 7 of task `t1`. The base names the check `acceptance`; the test commit declares
    /// `tests/answer.rs` and `tests/second.rs` and failed CI; the fix follows. With `edit`, a last
    /// commit edits the declared `tests/answer.rs`, and at that head `acceptance` (job 501) failed
    /// with the record `changed tests/answer.rs` while `tests` passed, CI failed there. Without it,
    /// the head is the fix and every check passed there. The worker's diff review passed at head.
    fn new(name: &str, edit: bool) -> Case {
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
        let mut up = name.to_string();
        up[..1].make_ascii_uppercase();
        let key = tmp.path().join("approval.key");
        let made = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "t", "-f"])
            .arg(&key)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let mut c = Case {
            tmp,
            repo: format!("o/{name}"),
            github: format!("o/{up}"),
            origin,
            gh,
            db,
            door,
            secretary,
            person,
            key,
            test_commit: String::new(),
            head: String::new(),
        };
        c.commit(
            "base",
            &[
                ("src/lib.rs", "pub fn answer() -> u32 { 41 }\n"),
                (".github/acceptance-check", "acceptance\n"),
                (".github/workflows/ci.yml", "on: pull_request\n"),
                ("scripts/acceptance-check.sh", "#!/bin/sh\necho base\n"),
                ("tests/common/x.rs", "pub fn x() {}\n"),
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
            "test\n\nAcceptance-Test: tests/answer.rs\nAcceptance-Test: tests/second.rs",
            &[
                (
                    "tests/answer.rs",
                    "#[test]\nfn a() { assert_eq!(x(), 42); }\n",
                ),
                ("tests/second.rs", "#[test]\nfn b() {}\n"),
            ],
        );
        c.ci_at(
            &c.test_commit.clone(),
            &[
                ("acceptance", "failure", 401, T1),
                ("tests", "failure", 402, T1),
            ],
        );
        c.ci_row("failed", &c.test_commit.clone());
        c.commit("fix", &[("src/lib.rs", "pub fn answer() -> u32 { 42 }\n")]);
        if edit {
            c.commit(
                "fix the declared test",
                &[(
                    "tests/answer.rs",
                    "#[test]\nfn a() { assert_eq!(answer(), 42); }\n",
                )],
            );
        }
        c.head = c.git(&["rev-parse", "HEAD"]);
        c.publish_at(7, &c.head.clone());
        if edit {
            c.ci_at(
                &c.head.clone(),
                &[
                    ("acceptance", "failure", 501, T1),
                    ("tests", "success", 502, T1),
                ],
            );
            c.log(501, &[ANSWER]);
            c.ci_row("failed", &c.head.clone());
        } else {
            c.ci_at(
                &c.head.clone(),
                &[
                    ("acceptance", "success", 501, T1),
                    ("tests", "success", 502, T1),
                ],
            );
            c.ci_row("passed", &c.head.clone());
        }
        c.watched(7, &c.head.clone());
        c.reviewed(&c.head.clone());
        c
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.origin, args)
    }

    fn commit(&self, msg: &str, files: &[(&str, &str)]) -> String {
        for (path, content) in files {
            let dest = self.origin.join(path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(&dest, content).unwrap();
        }
        self.git(&["add", "--all"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// A commit on the base branch `main` (the pull request's branch is left as it is).
    fn on_base(&self, msg: &str, files: &[(&str, &str)]) {
        self.git(&["checkout", "-q", "main"]);
        self.commit(msg, files);
        self.git(&["checkout", "-q", "feature"]);
    }

    /// Publish `head` as PR `pr`'s head; `gh pr view` names the repository as GitHub spells it.
    fn publish_at(&self, pr: i64, head: &str) {
        self.git(&["update-ref", &format!("refs/pull/{pr}/head"), head]);
        let view = json!({"number": pr, "url": format!("https://github.com/{}/pull/{pr}", self.github),
                          "baseRefName": "main", "headRefOid": head});
        std::fs::write(self.gh.join(format!("view.{pr}")), view.to_string()).unwrap();
    }

    fn answer(&self, sha: &str, kind: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.gh.join(format!("{sha}.{kind}")), text).unwrap();
    }

    /// CI at `sha`: each `(name, conclusion, id, completed_at)` a completed check run of the pull
    /// request's workflow run (workflow 1, suite 1, base C). The check surface shows the latest
    /// run of each name.
    fn ci_at(&self, sha: &str, runs: &[(&str, &str, i64, &str)]) {
        let checks: Vec<Value> = runs
            .iter()
            .map(|(name, c, id, at)| {
                json!({"app": ACTIONS, "name": name, "conclusion": c, "completed_at": at,
                       "suite": 1, "id": id})
            })
            .collect();
        self.answer(sha, "checks", &checks);
        let mut latest: std::collections::BTreeMap<&str, (&str, &str, i64)> = Default::default();
        for (name, c, id, at) in runs {
            if latest.get(name).is_none_or(|(t, _, _)| at > t) {
                latest.insert(name, (at, c, *id));
            }
        }
        let surface: Vec<Value> = latest
            .iter()
            .map(|(name, (_, c, id))| {
                json!({"name": name, "status": "completed", "conclusion": c, "id": id,
                       "app": "github-actions"})
            })
            .collect();
        self.answer(sha, "surface", &surface);
        self.answer(
            sha,
            "runs",
            &[json!({"wf": 1, "suite": 1, "created": T1, "bases": [C]})],
        );
        self.answer(sha, "going", &[json!({"status": "completed"})]);
        self.answer(sha, "status", &[]);
    }

    /// Job `id`'s log in downloaded form: a byte order mark, each line after a runner timestamp,
    /// the checker's canonical lines for `records`.
    fn log(&self, id: i64, records: &[&str]) {
        let mut text = String::from("\u{feff}");
        text.push_str(&format!("{STAMP} running the acceptance check\n"));
        for r in records {
            text.push_str(&format!("{STAMP} acceptance-check: approvable {r}\n"));
        }
        text.push_str(&format!("{STAMP} ACCEPTANCE: fail\n"));
        std::fs::write(self.gh.join(format!("log.{id}")), text).unwrap();
    }

    /// The acceptance check passed on a rerun at head (a later run), and CI passed there.
    fn rerun_passed(&self) {
        self.ci_at(
            &self.head,
            &[
                ("acceptance", "failure", 501, T1),
                ("acceptance", "success", 503, T2),
                ("tests", "success", 502, T1),
            ],
        );
        self.ci_row("passed", &self.head);
    }

    /// A `ci_completed` row of PR 7, written as the PR watch writes it.
    fn ci_row(&self, status: &str, head: &str) {
        self.event(
            "ci_completed",
            json!({"repo": self.repo, "pr": 7, "head_oid": head, "status": status,
                   "fail_count": 0, "pending_count": 0, "total_checks": 2, "probe_attempts": 1}),
        );
    }

    fn event(&self, kind: &str, payload: Value) -> i64 {
        let w = StateWriter::open(&self.db).unwrap();
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap()
    }

    /// The code task `t1` and a watch tying it to PR `pr` at `head`.
    fn watched(&self, pr: i64, head: &str) {
        let w = StateWriter::open(&self.db).unwrap();
        let run: i64 = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = 't1'", [], |r| r.get(0))
            .unwrap();
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
    }

    /// A passed diff review of `head` by t1's worker.
    fn reviewed(&self, head: &str) {
        let answer = json!({"verdict": "passed", "kind": "diff", "reader": "codex", "round": 1,
                            "allowed": 3, "head": head,
                            "findings": {"blocker": 0, "major": 0, "minor": 0}});
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

    fn tool(&self, name: &str, args: Value) -> Result<Value, DoorError> {
        self.door
            .handle(Some(&self.secretary), name, args, None, true)
    }

    /// The secretary's `merge` request for PR 7 at `head` by `method`.
    fn request_at(&self, head: &str, method: &str) -> Result<Value, DoorError> {
        self.tool(
            "merge",
            json!({"action": "request", "repo": self.repo, "pr": 7, "head": head, "method": method}),
        )
    }

    fn request(&self, method: &str) -> Result<Value, DoorError> {
        self.request_at(&self.head, method)
    }

    fn person(&self, args: Value) -> Result<Value, DoorError> {
        self.door
            .handle(Some(&self.person), "merge", args, None, false)
    }

    /// What `makiba merge approve <id>` prints, read as the CLI reads it.
    fn printed(&self, id: i64) -> String {
        makiba::status_local::merge_entry_text(&self.db, id)
            .unwrap_or_else(|e| panic!("entry {id}: {e}"))
    }

    fn context(&self, id: i64) -> Value {
        let text: String = self
            .conn()
            .query_row(
                "SELECT context_json FROM pending WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// `(status, answer, answered_by)` of pending entry `id`.
    fn entry(&self, id: i64) -> (String, Option<String>, Option<String>) {
        self.conn()
            .query_row(
                "SELECT status, answer, answered_by FROM pending WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    fn open_entries(&self) -> i64 {
        self.conn()
            .query_row(
                "SELECT count(*) FROM pending WHERE kind = 'merge' AND status = 'open'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The signature `makiba merge approve <id>` sends: the entry's approval text, which the
    /// printed text shows, signed with the approval key by the CLI's own function.
    fn signature(&self, id: i64) -> String {
        let text = makiba::status_local::merge_entry_approval(&self.db, id)
            .unwrap()
            .unwrap_or_else(|| panic!("entry {id} asks no approval"));
        assert!(self.printed(id).contains(&text), "{}", self.printed(id));
        makiba::cli::sign_approval(&self.key, &text).unwrap()
    }

    /// The person's approval of entry `id` as printed now, signed.
    fn approve_signed(&self, id: i64) -> Result<Value, DoorError> {
        let sig = self.signature(id);
        self.person(json!({"action": "approve", "id": id,
                           "shown": sha256_hex(&self.printed(id)), "signature": sig}))
    }

    /// The person's approval of entry `id` as printed now, with no signature.
    fn approve_plain(&self, id: i64) -> Result<Value, DoorError> {
        self.person(json!({"action": "approve", "id": id, "shown": sha256_hex(&self.printed(id))}))
    }

    fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

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

    fn calls(&self) -> String {
        std::fs::read_to_string(self.gh.join("calls")).unwrap_or_default()
    }

    /// The approval text of note section 4.2 for entry `id` and `records`, built here.
    fn expected(&self, id: i64, records: &[&str]) -> String {
        let set: String = records.iter().map(|r| format!("{r}\n")).collect();
        format!(
            "makiba-acceptance-approval v1\nrepo {}\npr 7\nhead {}\nfailures {}\npending {id}\n{set}",
            self.github,
            self.head,
            sha256_hex(&set)
        )
    }

    /// The first ask: the secretary's request, then the person's signed approval. The entry.
    fn approved(&self) -> i64 {
        let n = pending_id(self.request("squash"));
        let v = self
            .approve_signed(n)
            .unwrap_or_else(|e| panic!("approve: {e:?}"));
        assert_eq!(v["status"], json!("approved"), "{v}");
        n
    }
}

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

fn warnings(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w.as_str().map(String::from))
        .collect()
}

fn approved_in(id: i64, records: &[&str]) -> String {
    format!(
        "acceptance failed by construction, approved in pending {id}: {}",
        records.join("; ")
    )
}

// ---- 1. the ask ------------------------------------------------------------------------------------

#[test]
fn a_failed_acceptance_where_every_other_check_passed_gives_one_pending_entry() {
    both("ask", |name| {
        let c = Case::new(name, true);
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("pending"), "{v}");
        assert!(
            warnings(&v).contains(&format!("acceptance fails by construction: {ANSWER}")),
            "{v}"
        );
        let n = v["pending_id"].as_i64().unwrap();
        assert_eq!(c.open_entries(), 1);
        let ctx = c.context(n);
        assert_eq!(ctx["approval"], json!(c.expected(n, &[ANSWER])), "{ctx}");
        let text = ctx["text"].as_str().unwrap();
        assert!(text.contains(&c.expected(n, &[ANSWER])), "{text}");
        assert_eq!(ctx["text_sum"], json!(sha256_hex(text)));
        assert_eq!(c.count("merge_attempted"), 0);
        assert_eq!(c.count("acceptance_approved"), 0);
        assert_eq!(pending_id(c.request("squash")), n);
        assert_eq!(c.open_entries(), 1);
        assert!(!c.calls().contains("unexpected call"), "{}", c.calls());
    });
}

#[test]
fn another_failed_check_is_refused() {
    both("other", |name| {
        let c = Case::new(name, true);
        c.ci_at(
            &c.head,
            &[
                ("acceptance", "failure", 501, T1),
                ("tests", "failure", 502, T1),
            ],
        );
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("refused"), "{v}");
        assert!(
            v["reason"]
                .as_str()
                .unwrap()
                .starts_with("no passed ci_completed"),
            "{v}"
        );
        assert_eq!(c.open_entries(), 0);
        assert_eq!(c.count("merge_attempted"), 0);
        assert_eq!(c.count("acceptance_approved"), 0);
    });
}

// ---- 2. the answer -----------------------------------------------------------------------------------

#[test]
fn approve_records_the_event_and_tells_the_secretary() {
    both("answer", |name| {
        let c = Case::new(name, true);
        let n = pending_id(c.request("squash"));
        let shown = sha256_hex(&c.printed(n));
        refused_as(
            c.person(json!({"action": "approve", "id": n, "shown": shown})),
            ErrorKind::Usage,
        );
        refused_as(
            c.person(json!({"action": "approve", "id": n, "shown": shown,
                            "signature": "not a signature"})),
            ErrorKind::Usage,
        );
        assert_eq!(c.entry(n).0, "open");
        assert_eq!(c.count("acceptance_approved"), 0);

        let sig = c.signature(n);
        let v = c
            .person(json!({"action": "approve", "id": n, "shown": shown, "signature": sig}))
            .unwrap_or_else(|e| panic!("approve: {e:?}"));
        assert_eq!(v["status"], json!("approved"), "{v}");
        assert_eq!(v["pending_id"], json!(n), "{v}");
        let text = c.expected(n, &[ANSWER]);
        let comment = format!("{text}---\n{sig}");
        assert_eq!(v["comment"], json!(comment), "{v}");
        let events = c.events("acceptance_approved");
        assert_eq!(events.len(), 1, "{events:?}");
        let (id, e) = &events[0];
        assert_eq!(v["event"], json!(id), "{v}");
        assert_eq!(e["repo"], json!(c.repo), "{e}");
        assert_eq!(e["github_repo"], json!(c.github), "{e}");
        assert_eq!(e["pr"], json!(7), "{e}");
        assert_eq!(e["head"], json!(c.head), "{e}");
        assert_eq!(e["method"], json!("squash"), "{e}");
        assert_eq!(
            e["failures"],
            json!(sha256_hex(&format!("{ANSWER}\n"))),
            "{e}"
        );
        assert_eq!(e["set"], json!([ANSWER]), "{e}");
        assert_eq!(e["warnings"], json!([]), "{e}");
        assert_eq!(e["pending_id"], json!(n), "{e}");
        assert_eq!(e["comment"], json!(comment), "{e}");

        // The comment verifies as the acceptance job verifies it: the lines before the first
        // line `---` against the lines after it, with the key's allowed-signers line.
        let (head, tail) = comment.split_once("\n---\n").unwrap();
        let dir = c.tmp.path();
        std::fs::write(dir.join("text"), format!("{head}\n")).unwrap();
        std::fs::write(dir.join("sig"), tail).unwrap();
        std::fs::write(
            dir.join("signers"),
            format!("{}\n", makiba::cli::approval_signer_line(&c.key).unwrap()),
        )
        .unwrap();
        let verified = Command::new("ssh-keygen")
            .args([
                "-Y",
                "verify",
                "-I",
                "makiba-owner",
                "-n",
                "makiba-acceptance-approval",
            ])
            .arg("-f")
            .arg(dir.join("signers"))
            .arg("-s")
            .arg(dir.join("sig"))
            .stdin(std::fs::File::open(dir.join("text")).unwrap())
            .output()
            .unwrap();
        assert!(verified.status.success(), "{verified:?}");

        assert_eq!(
            c.entry(n),
            (
                "answered".into(),
                Some("approved".into()),
                Some("person".into())
            )
        );
        assert_eq!(c.count("merge_attempted"), 0);
        let told: Vec<String> = c
            .notices()
            .into_iter()
            .filter(|t| t.contains(&comment))
            .collect();
        assert_eq!(told.len(), 1, "{:?}", c.notices());
        assert!(told[0].contains("rerun"), "{}", told[0]);
        assert!(!c.calls().contains("unexpected call"), "{}", c.calls());
        assert!(!c.calls().contains("-X PUT"), "{}", c.calls());
        assert!(!c.calls().contains("enqueuePullRequest"), "{}", c.calls());
    });
}

#[test]
fn a_stale_shown_writes_nothing() {
    both("stale", |name| {
        let c = Case::new(name, true);
        let n = pending_id(c.request("squash"));
        let sig = c.signature(n);
        refused_as(
            c.person(
                json!({"action": "approve", "id": n, "shown": sha256_hex("another text"),
                            "signature": sig}),
            ),
            ErrorKind::Stale,
        );
        assert_eq!(c.count("acceptance_approved"), 0);
        assert_eq!(c.count("merge_attempted"), 0);
        assert_eq!(c.entry(n).0, "open");
    });
}

// ---- 3. the landing -----------------------------------------------------------------------------------

#[test]
fn the_next_request_at_the_same_head_and_method_lands_on_the_approval() {
    both("land", |name| {
        let c = Case::new(name, true);
        let n = c.approved();
        c.rerun_passed();
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("accepted"), "{v}");
        let attempts = c.events("merge_attempted");
        assert_eq!(attempts.len(), 1, "{attempts:?}");
        assert_eq!(attempts[0].1["approved_by"], json!("person"));
        assert_eq!(attempts[0].1["pending_id"], json!(n));
        assert_eq!(c.open_entries(), 0);
        refused_as(c.request("squash"), ErrorKind::Conflict);
        assert_eq!(c.count("merge_attempted"), 1);
        assert!(!c.calls().contains("unexpected call"), "{}", c.calls());
    });
}

#[test]
fn another_head_or_method_has_no_approval() {
    both("method", |name| {
        let c = Case::new(name, true);
        let n = c.approved();
        c.rerun_passed();
        let v = c
            .request("merge")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("pending"), "{v}");
        assert!(warnings(&v).contains(&approved_in(n, &[ANSWER])), "{v}");
        assert_eq!(c.count("merge_attempted"), 0);

        // A new head: no approval there, and condition (a) refuses the edited declared test.
        let h2 = c.commit(
            "more",
            &[("src/lib.rs", "pub fn answer() -> u32 { 42 } // more\n")],
        );
        c.publish_at(7, &h2);
        c.ci_at(
            &h2,
            &[
                ("acceptance", "success", 601, T2),
                ("tests", "success", 602, T2),
            ],
        );
        c.pr_pushed(&h2);
        c.ci_row("passed", &h2);
        c.reviewed(&h2);
        let v = c
            .request_at(&h2, "squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("refused"), "{v}");
        let why = v["reason"].as_str().unwrap();
        assert!(
            why.contains("protected paths changed") && why.contains("tests/answer.rs"),
            "{v}"
        );
        assert_eq!(c.count("merge_attempted"), 0);
    });
    both("newest", |name| {
        let c = Case::new(name, true);
        c.approved();
        // Before the rerun, the log names a second record; an ask by merge, signed: A2.
        c.log(501, &[ANSWER, SECOND]);
        let m = pending_id(c.request("merge"));
        assert_eq!(
            c.context(m)["approval"],
            json!(c.expected(m, &[ANSWER, SECOND]))
        );
        let v = c
            .approve_signed(m)
            .unwrap_or_else(|e| panic!("approve: {e:?}"));
        assert_eq!(v["status"], json!("approved"), "{v}");
        c.rerun_passed();
        // The older approval by squash is not the newest of the head: it authorizes nothing.
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("pending"), "{v}");
        assert!(
            warnings(&v).contains(&approved_in(m, &[ANSWER, SECOND])),
            "{v}"
        );
        assert_eq!(c.count("merge_attempted"), 0);
        let v = c
            .request("merge")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("accepted"), "{v}");
        let attempts = c.events("merge_attempted");
        assert_eq!(attempts.len(), 1, "{attempts:?}");
        assert_eq!(attempts[0].1["pending_id"], json!(m));
    });
}

#[test]
fn a_new_warning_makes_a_new_entry() {
    both("warned", |name| {
        let c = Case::new(name, true);
        let n = c.approved();
        c.rerun_passed();
        c.on_base(
            "helper",
            &[("tests/common/x.rs", "pub fn x() -> u32 { 1 }\n")],
        );
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("pending"), "{v}");
        assert_ne!(v["pending_id"], json!(n), "{v}");
        assert!(
            warnings(&v)
                .iter()
                .any(|w| w.starts_with("test helpers the base changed")),
            "{v}"
        );
        assert_eq!(c.count("merge_attempted"), 0);
    });
}

#[test]
fn an_approval_is_spent_by_any_later_attempt_at_its_head() {
    both("spent", |name| {
        let c = Case::new(name, true);
        let n = c.approved();
        c.rerun_passed();
        c.on_base(
            "helper",
            &[("tests/common/x.rs", "pub fn x() -> u32 { 1 }\n")],
        );
        let b = pending_id(c.request("squash"));
        let v = c
            .approve_plain(b)
            .unwrap_or_else(|e| panic!("approve: {e:?}"));
        assert_eq!(v["status"], json!("accepted"), "{v}");
        let attempts = c.events("merge_attempted");
        assert_eq!(attempts.len(), 1, "{attempts:?}");
        assert_eq!(attempts[0].1["pending_id"], json!(b));
        c.outcome("merge_failed", attempts[0].0, json!({}));
        c.on_base(
            "revert the helper",
            &[("tests/common/x.rs", "pub fn x() {}\n")],
        );
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("pending"), "{v}");
        assert!(warnings(&v).contains(&approved_in(n, &[ANSWER])), "{v}");
        assert_eq!(c.count("merge_attempted"), 1);
    });
}

#[test]
fn the_landing_needs_the_acceptance_check_itself() {
    both("itself", |name| {
        let c = Case::new(name, true);
        c.approved();
        // CI passed at head, but acceptance did not: `tests` failed at the test commit and
        // succeeded at head, so the fallback of condition (b) would pass the head.
        c.ci_row("passed", &c.head);
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("refused"), "{v}");
        assert!(
            v["reason"]
                .as_str()
                .unwrap()
                .contains("did not succeed at head"),
            "{v}"
        );
        assert_eq!(c.count("merge_attempted"), 0);
        c.rerun_passed();
        c.on_base("workflow", &[(".github/extra.yml", "x: 1\n")]);
        let v = c
            .request("squash")
            .unwrap_or_else(|e| panic!("merge: {e:?}"));
        assert_eq!(v["status"], json!("refused"), "{v}");
        assert!(v["reason"].as_str().unwrap().contains(".github/"), "{v}");
        assert_eq!(c.count("merge_attempted"), 0);
    });
}

// ---- 4. the retry ---------------------------------------------------------------------------------------

#[test]
fn after_an_attempt_ends_the_next_request_asks_again() {
    both("retry", |name| {
        let c = Case::new(name, true);
        let n = c.approved();
        c.rerun_passed();
        assert_eq!(c.request("squash").unwrap()["status"], json!("accepted"));
        let first = c.events("merge_attempted")[0].0;
        c.outcome("merge_failed", first, json!({}));
        let p = pending_id(c.request("squash"));
        assert!(
            c.printed(p).contains(&approved_in(n, &[ANSWER])),
            "{}",
            c.printed(p)
        );
        assert_eq!(c.count("merge_attempted"), 1);
        let v = c
            .approve_plain(p)
            .unwrap_or_else(|e| panic!("approve: {e:?}"));
        assert_eq!(v["status"], json!("accepted"), "{v}");
        let attempts = c.events("merge_attempted");
        assert_eq!(attempts.len(), 2, "{attempts:?}");
        assert_eq!(attempts[1].1["pending_id"], json!(p));

        // The same after the queue took the attempt and dropped it.
        c.outcome("merge_enqueued", attempts[1].0, json!({}));
        c.dequeued(attempts[1].0);
        let q = pending_id(c.request("squash"));
        let v = c
            .approve_plain(q)
            .unwrap_or_else(|e| panic!("approve: {e:?}"));
        assert_eq!(v["status"], json!("accepted"), "{v}");
        let attempts = c.events("merge_attempted");
        assert_eq!(attempts.len(), 3, "{attempts:?}");
        assert_eq!(attempts[2].1["pending_id"], json!(q));
    });
}

#[test]
fn an_attempt_still_in_the_queue_is_not_retried() {
    both("queued", |name| {
        let c = Case::new(name, true);
        c.approved();
        c.rerun_passed();
        assert_eq!(c.request("squash").unwrap()["status"], json!("accepted"));
        let first = c.events("merge_attempted")[0].0;
        c.outcome("merge_unknown", first, json!({"observed": "enqueued"}));
        refused_as(c.request("squash"), ErrorKind::Conflict);
        assert_eq!(c.open_entries(), 0);
        assert_eq!(c.count("merge_attempted"), 1);
        c.dequeued(first);
        pending_id(c.request("squash"));
        assert_eq!(c.count("merge_attempted"), 1);
    });
}

// ---- 5. an open entry made before acceptance failed -------------------------------------------------------

#[test]
fn an_open_entry_takes_the_approval_when_acceptance_fails_after_it() {
    both("before", |name| {
        let c = Case::new(name, false);
        c.on_base(
            "helper",
            &[("tests/common/x.rs", "pub fn x() -> u32 { 1 }\n")],
        );
        let e = pending_id(c.request("squash"));
        assert!(c.context(e)["approval"].is_null(), "{}", c.context(e));
        let old = c.printed(e);

        // A rerun of acceptance fails by construction: the test in tests/second.rs is ignored.
        c.ci_at(
            &c.head,
            &[
                ("acceptance", "success", 501, T1),
                ("acceptance", "failure", 504, T2),
                ("tests", "success", 502, T1),
            ],
        );
        c.log(504, &[SECOND]);
        c.ci_row("failed", &c.head);
        let v = c
            .request("squash")
            .unwrap_or_else(|err| panic!("merge: {err:?}"));
        assert_eq!(v["pending_id"], json!(e), "{v}");
        assert!(
            warnings(&v).contains(&format!("acceptance fails by construction: {SECOND}")),
            "{v}"
        );
        assert_eq!(c.context(e)["approval"], json!(c.expected(e, &[SECOND])));
        assert_ne!(c.printed(e), old);
        let sig = c.signature(e);
        refused_as(
            c.person(
                json!({"action": "approve", "id": e, "shown": sha256_hex(&old),
                            "signature": sig}),
            ),
            ErrorKind::Stale,
        );
        assert_eq!(c.count("acceptance_approved"), 0);
        let v = c
            .approve_signed(e)
            .unwrap_or_else(|err| panic!("approve: {err:?}"));
        assert_eq!(v["status"], json!("approved"), "{v}");
        assert_eq!(c.count("acceptance_approved"), 1);
        assert_eq!(c.count("merge_attempted"), 0);
    });
}

impl Case {
    /// `pr pushed` of PR 7 at `head` by the secretary.
    fn pr_pushed(&self, head: &str) {
        self.tool(
            "pr",
            json!({"state": "pushed", "repo": self.repo, "pr": 7, "head": head, "task": "t1"}),
        )
        .unwrap_or_else(|e| panic!("pr pushed: {e:?}"));
    }

    /// The outcome `kind` of attempt `attempt`, written as the merge pass writes it.
    fn outcome(&self, kind: &str, attempt: i64, extra: Value) {
        let mut p = json!({"repo": self.repo, "pr": 7, "head": self.head, "attempt": attempt});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        self.event(kind, p);
    }

    /// The queue dropped attempt `attempt`, written as the PR watch writes it.
    fn dequeued(&self, attempt: i64) {
        self.event(
            "merge_dequeued",
            json!({"repo": self.repo, "pr": 7, "head": self.head, "attempt": attempt,
                   "why": "left the queue", "reason": null}),
        );
    }
}
