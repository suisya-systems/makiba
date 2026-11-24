//! Issue 461 (docs/design/landing-test-commits.md): a landing that changes `tools/`, `.github/`
//! or `tests/common/` names every open run of the project with a pushed test commit, to the
//! secretary and to each such worker, so that the worker runs the repository's checks over its
//! declared tests in its own sandbox before its fix.
//!
//! PR 454's case: its test commit declared a test that made an executable in its own process,
//! and PR 437 then landed `tools/check_test_executables.py`, which fails such a test. The declared
//! test could not change, and nothing said so until the fix head's CI failed. Here the same
//! landing names the run, once, and a landing that changes none of the three folders names none.
//!
//! `makibad` runs in this process on the stub base (as tests/pr_open_test_commit.rs does for its
//! push tests). The repositories are built as in tests/pr_open_test_commit.rs: a project's clone
//! sits in the repositories folder with the GitHub URL as its origin, mapped to a local
//! repository with `url.<local>.insteadOf`, and that repository publishes each pull request's
//! `refs/pull/<n>/head`. Its HEAD stays detached, so that moving `main` there is a landing and
//! touches no files. `gh` is a stand-in (`MAKIBA_GH`) that answers `pr view` from a file per pull
//! request, answers the CI reads of any head with one check still running, logs every call and
//! refuses anything else. No network and no socket.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
const MERGED_AT: &str = "2026-11-20T00:00:00Z";

const STAND_IN: &str = r#"#!/bin/sh
# gh pr view <n> --repo [github.com/]<o>/<n> --json ...: the file pr-<n>.json of the repository's
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

/// The declared test of PR 454's kind: it makes an executable in its own process. It is a file of
/// the fixture's repository, never compiled here.
const EXE_MADE: &str = concat!(
    "use std::fs;\n",
    "use std::os::unix::fs::PermissionsExt;\n",
    "#[test]\n",
    "fn makes_a_script() {\n",
    // not an executable: the text of a fixture file, never compiled or run
    "    fs::copy(\"a\", \"b\").unwrap();\n",
    "    let mut p = fs::metadata(\"b\").unwrap().permissions();\n",
    // not an executable: the text of a fixture file, never compiled or run
    "    p.set_mode(0o755);\n",
    "    fs::set_permissions(\"b\", p).unwrap();\n",
    "}\n",
);

const CLEAN_RS: &str = "#[test]\nfn adds() {\n    assert_eq!(2 + 2, 4);\n}\n";

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The folder the test shares (the stand-in, its answers, the clones, the workplaces), with the
/// environment set once before any thread starts.
fn init() -> PathBuf {
    INIT.call_once(|| {
        makibad::process_knowledge();
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        for d in ["answers", "repos", "workers", "docs", "origins", "state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "200");
            std::env::set_var("MAKIBA_PRWATCH_BUDGET_MS", "1000");
            std::env::set_var("MAKIBA_LANDING_RETRY_MS", "300");
            std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A pull request's answer: open, or merged with `merge`.
fn view(slug: &str, pr: i64, head: &str, merge: Option<&str>) -> Value {
    let mut v = json!({"number": pr, "url": format!("https://github.com/o/{slug}/pull/{pr}"),
                       "baseRefName": "main", "headRefOid": head, "title": "T",
                       "state": "OPEN", "mergedAt": null, "mergedBy": null, "mergeCommit": null,
                       "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "isDraft": false});
    if let Some(m) = merge {
        v["state"] = json!("MERGED");
        v["mergedAt"] = json!(MERGED_AT);
        v["mergedBy"] = json!({"login": "owner"});
        v["mergeCommit"] = json!({"oid": m});
    }
    v
}

/// One project `o/<slug>`: its local origin (HEAD detached) and its clone.
struct Project {
    slug: String,
    origin: PathBuf,
    answers: PathBuf,
}

impl Project {
    fn new(home: &Path, slug: &str) -> Project {
        let origin = home.join("origins").join(slug);
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("README.md"), "base\n").unwrap();
        std::fs::create_dir_all(origin.join("src")).unwrap();
        std::fs::write(origin.join("src").join("lib.rs"), "// base\n").unwrap();
        git(&origin, &["add", "README.md", "src/lib.rs"]);
        git(&origin, &["commit", "-q", "-m", "base"]);
        git(&origin, &["checkout", "-q", "--detach"]);
        let url = format!("https://github.com/o/{slug}");
        git(
            &home.join("repos"),
            &["clone", "-q", origin.to_str().unwrap(), slug],
        );
        let clone = home.join("repos").join(slug);
        git(&clone, &["remote", "set-url", "origin", &url]);
        git(
            &clone,
            &[
                "config",
                &format!("url.{}.insteadOf", origin.display()),
                &url,
            ],
        );
        let answers = home.join("answers").join(format!("o_{slug}"));
        std::fs::create_dir_all(&answers).unwrap();
        Project {
            slug: slug.into(),
            origin,
            answers,
        }
    }

    fn main(&self) -> String {
        git(&self.origin, &["rev-parse", "refs/heads/main"])
    }

    /// A branch `branch` from `main` with one commit per `(message, files)`; the commits' ids.
    fn branch(&self, branch: &str, commits: &[(&str, &[(&str, &str)])]) -> Vec<String> {
        git(&self.origin, &["checkout", "-q", "-B", branch, "main"]);
        let mut ids = Vec::new();
        for (msg, files) in commits {
            for (path, text) in *files {
                let dest = self.origin.join(path);
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::write(&dest, text).unwrap();
                git(&self.origin, &["add", path]);
            }
            git(&self.origin, &["commit", "-q", "--allow-empty", "-m", msg]);
            ids.push(git(&self.origin, &["rev-parse", "HEAD"]));
        }
        git(&self.origin, &["checkout", "-q", "--detach"]);
        ids
    }

    /// Publish `head` as pull request `pr`'s head, read open.
    fn publish(&self, pr: i64, head: &str) {
        git(
            &self.origin,
            &["update-ref", &format!("refs/pull/{pr}/head"), head],
        );
        self.answer(pr, &view(&self.slug, pr, head, None));
    }

    fn answer(&self, pr: i64, v: &Value) {
        let tmp = self.answers.join(format!("pr-{pr}.json.tmp"));
        std::fs::write(&tmp, v.to_string()).unwrap();
        std::fs::rename(&tmp, self.answers.join(format!("pr-{pr}.json"))).unwrap();
    }

    /// The merge commit of `branch` onto `onto`, made without moving `main`.
    fn merge_commit(&self, onto: &str, branch: &str) -> String {
        git(&self.origin, &["checkout", "-q", "--detach", onto]);
        git(
            &self.origin,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                &format!("Merge {branch}"),
                branch,
            ],
        );
        let id = git(&self.origin, &["rev-parse", "HEAD"]);
        git(&self.origin, &["checkout", "-q", "--detach"]);
        id
    }

    /// `main` moves to `commit`: the landing at the origin.
    fn put_on_main(&self, commit: &str) {
        git(&self.origin, &["update-ref", "refs/heads/main", commit]);
    }

    /// How many times the stand-in read `pr view <pr>` of this project.
    fn views(&self, pr: i64) -> usize {
        std::fs::read_to_string(self.answers.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with(&format!("pr view {pr} ")))
            .count()
    }
}

/// makibad's stop flag and thread.
type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

/// The organization: one state database, its door, the secretary, and makibad in this process.
struct Org {
    home: PathBuf,
    state: PathBuf,
    door: Door,
    secretary: String,
    daemon: Option<Daemon>,
}

impl Drop for Org {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Org {
    fn new() -> Org {
        let home = init();
        let state = home.join("state").join("org");
        std::fs::create_dir_all(&state).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        Org {
            home,
            state,
            door: Door::new(w, Arc::new(Stub::new()), "fake".into()),
            secretary,
            daemon: None,
        }
    }

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.state.join("state.db")).unwrap()
    }

    fn conn(&self) -> Connection {
        Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn project(&self, slug: &str) -> Project {
        let p = Project::new(&self.home, slug);
        let w = self.writer();
        w.ensure_project(slug, None, Some(&format!("https://github.com/o/{slug}")))
            .unwrap();
        p
    }

    /// An open run of `task` in project `p` on `branch`, in `review`: its worker reported done
    /// with its commit pushed (a test commit's run waits there for its CI). A run `in_use` with no
    /// terminal of the stub base would be read gone at makibad's start.
    fn run(&self, p: &Project, task: &str, branch: &str) {
        self.writer()
            .upsert_run(&RunUpsert {
                task_id: task,
                project_slug: &p.slug,
                title: Some(task),
                status: Some("review"),
                branch: Some(branch),
                ..Default::default()
            })
            .unwrap();
    }

    /// A failed CI run of `o/<slug>#pr` at `head`, as the PR watch records it.
    fn failed_ci(&self, slug: &str, pr: i64, head: &str) {
        self.writer()
            .append_event(&NewEvent {
                kind: "ci_completed",
                actor: Some("makibad"),
                payload: Some(
                    &json!({"repo": format!("o/{slug}"), "pr": pr, "head_oid": head,
                                      "status": "failed", "fail_count": 1, "pending_count": 0,
                                      "total_checks": 1, "probe_attempts": 1}),
                ),
                ..Default::default()
            })
            .unwrap();
    }

    /// `pr opened` of `o/<slug>#pr` at `head` for `task`, as the secretary records it.
    fn opened(&self, p: &Project, pr: i64, head: &str, task: &str) {
        p.publish(pr, head);
        let r = self.door.handle(
            Some(&self.secretary),
            "pr",
            json!({"state": "opened", "repo": format!("o/{}", p.slug), "pr": pr, "head": head,
                   "task": task}),
            None,
            false,
        );
        assert!(r.is_ok(), "pr opened {}#{pr} at {head}: {r:?}", p.slug);
    }

    /// A run of `task` whose pull request `pr` holds `commits`, recorded opened at the last one.
    /// The commits' ids.
    fn with_pr(
        &self,
        p: &Project,
        task: &str,
        pr: i64,
        commits: &[(&str, &[(&str, &str)])],
    ) -> Vec<String> {
        let branch = format!("makiba/{task}");
        self.run(p, task, &branch);
        let ids = p.branch(&branch, commits);
        if ids.len() > 1 {
            // A head past the test commit is recorded only after a failed run at it (Issue 307).
            self.failed_ci(&p.slug, pr, &ids[0]);
        }
        self.opened(p, pr, ids.last().unwrap(), task);
        ids
    }

    /// The landing of `task`'s pull request `pr` changing `files`: its merge commit, made on
    /// `onto`. With `now`, `main` is moved to it before the pull request is read merged.
    fn landing(
        &self,
        p: &Project,
        task: &str,
        pr: i64,
        onto: &str,
        files: &[(&str, &str)],
        now: bool,
    ) -> String {
        let ids = self.with_pr(p, task, pr, &[("feat: the change", files)]);
        let merge = p.merge_commit(onto, &format!("makiba/{task}"));
        if now {
            p.put_on_main(&merge);
        }
        p.answer(pr, &view(&p.slug, pr, &ids[0], Some(&merge)));
        merge
    }

    fn start(&mut self) {
        let before = self.count("SELECT count(*) FROM events WHERE kind = 'base_resynced'");
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, flag) = (self.state.clone(), stop.clone());
        let d =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(Stub::new()), flag));
        self.daemon = Some((stop, d));
        wait("makibad's first Resync", || {
            self.count("SELECT count(*) FROM events WHERE kind = 'base_resynced'") > before
        });
    }

    fn stop(&mut self) {
        if let Some((stop, d)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = d.join();
        }
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn().query_row(sql, [], |r| r.get(0)).unwrap_or(0)
    }

    /// The payloads of every `landing_scanned`, oldest first.
    fn scans(&self) -> Vec<Value> {
        let c = self.conn();
        let mut q = c
            .prepare("SELECT payload_json FROM events WHERE kind = 'landing_scanned' ORDER BY id")
            .unwrap();
        q.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }

    /// The `landing_scanned` of the merge commit `merge`, if written.
    fn scan_of(&self, merge: &str) -> Vec<Value> {
        self.scans()
            .into_iter()
            .filter(|s| s["merge_commit"] == json!(merge))
            .collect()
    }

    /// The bodies of makibad's notices to `recipient`, oldest first.
    fn notices(&self, recipient: &str) -> Vec<String> {
        let c = self.conn();
        let mut q = c
            .prepare(
                "SELECT body FROM messages WHERE kind = 'notice' AND sender = 'makibad'
                 AND recipient = ?1 ORDER BY id",
            )
            .unwrap();
        q.query_map([recipient], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The notices to the secretary of a landing that name `task`.
    fn secretary_naming(&self, task: &str) -> Vec<String> {
        self.notices("secretary")
            .into_iter()
            .filter(|b| b.contains("landed") && b.contains(task))
            .collect()
    }

    /// The notices of a landing to the worker of `task`.
    fn worker_told(&self, task: &str) -> Vec<String> {
        self.notices(&format!("w-{task}"))
            .into_iter()
            .filter(|b| b.contains("landed"))
            .collect()
    }
}

/// Several passes of makibad: the PR watch reads `pr` of `p` three more times.
fn passes(p: &Project, pr: i64) {
    let seen = p.views(pr);
    wait("three more reads of the pull request", || {
        p.views(pr) >= seen + 3
    });
}

/// The task ids a scan names.
fn named(scan: &Value) -> Vec<String> {
    scan["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("no tasks[] in {scan}"))
        .iter()
        .map(|t| t["task_id"].as_str().unwrap_or("?").to_string())
        .collect()
}

/// PR 454's case and its neighbours: a landing that changes `tools/`, `.github/` or
/// `tests/common/` names, once, every open run of the project with a pushed test commit (to the
/// secretary and to the worker); a run without one, a closed run, the landed run itself and a run
/// of another project are not named; a restart names nobody twice; a landing that changes none
/// of the three folders names nobody; two landings between scans are read together.
#[test]
fn a_landing_that_may_change_the_checks_names_each_open_test_commit_once() {
    let mut org = Org::new();
    let p = org.project("p461");
    let q = org.project("q461");
    let base = p.main();

    // A: PR 454's kind of test commit, the only commit of its pull request.
    let a = org.with_pr(
        &p,
        "t461-a",
        1,
        &[(
            "test: the script test\n\nAcceptance-Test: tests/exe_made.rs",
            &[("tests/exe_made.rs", EXE_MADE)],
        )],
    );
    let m = a[0].clone();
    // C: an open pull request without a test commit.
    org.with_pr(
        &p,
        "t461-c",
        3,
        &[("feat: work", &[("src/c.rs", "// c\n")])],
    );
    // D: a test commit, but the run closed before the landing.
    org.with_pr(
        &p,
        "t461-d",
        4,
        &[(
            "test: d\n\nAcceptance-Test: tests/d.rs",
            &[("tests/d.rs", CLEAN_RS)],
        )],
    );
    org.writer()
        .conn()
        .execute(
            "UPDATE runs SET status = 'completed',
                    completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE task_id = 't461-d'",
            [],
        )
        .unwrap();
    // Q: a test commit in another project.
    org.with_pr(
        &q,
        "t461-q",
        1,
        &[(
            "test: q\n\nAcceptance-Test: tests/q.rs",
            &[("tests/q.rs", EXE_MADE)],
        )],
    );
    // B: the landed run has a test commit too, then the commit that adds the lint.
    let tool = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/check_test_executables.py"),
    )
    .unwrap();
    let b = org.with_pr(
        &p,
        "t461-b",
        2,
        &[
            (
                "test: b\n\nAcceptance-Test: tests/b.rs",
                &[("tests/b.rs", CLEAN_RS)],
            ),
            (
                "feat: the lint",
                &[("tools/check_test_executables.py", tool.as_str())],
            ),
        ],
    );

    org.start();

    // 1. Before any landing nothing names A.
    passes(&p, 1);
    assert!(
        org.scans().is_empty(),
        "a scan before any landing: {:?}",
        org.scans()
    );
    assert!(org.secretary_naming("t461-a").is_empty());
    assert!(org.worker_told("t461-a").is_empty());

    // The landing of B, which adds tools/check_test_executables.py (PR 437's).
    let l = p.merge_commit(&base, "makiba/t461-b");
    p.put_on_main(&l);
    p.answer(2, &view("p461", 2, &b[1], Some(&l)));
    wait("the scan of the landing", || !org.scan_of(&l).is_empty());
    passes(&p, 1);
    let scans = org.scan_of(&l);
    assert_eq!(scans.len(), 1, "one scan of the landing: {scans:?}");
    let s = &scans[0];
    assert_eq!(s["base_head"], json!(l), "{s}");
    assert_eq!(
        s["matched"],
        json!(["tools/check_test_executables.py"]),
        "{s}"
    );
    assert_eq!(named(s), vec!["t461-a".to_string()], "{s}");
    let ta = &s["tasks"][0];
    assert_eq!(ta["test_commit"], json!(m), "{s}");
    assert_eq!(ta["declared"], json!(["tests/exe_made.rs"]), "{s}");

    let told = org.secretary_naming("t461-a");
    assert_eq!(
        told.len(),
        1,
        "one notice to the secretary naming A: {told:?}"
    );
    for want in [
        &m[..7],
        "tests/exe_made.rs",
        "tools/check_test_executables.py",
    ] {
        assert!(
            told[0].contains(want),
            "the notice names {want}: {}",
            told[0]
        );
    }
    let worker = org.worker_told("t461-a");
    assert_eq!(worker.len(), 1, "one notice to A's worker: {worker:?}");
    for want in [&m[..7], "tests/exe_made.rs", &l[..7], "in your sandbox"] {
        assert!(
            worker[0].contains(want),
            "the worker's notice names {want}: {}",
            worker[0]
        );
    }

    // 2. C (no test commit), D (closed), B (the landed run) and Q (another project) are not
    // named, and their workers are not told.
    for task in ["t461-b", "t461-c", "t461-d", "t461-q"] {
        assert!(org.worker_told(task).is_empty(), "{task}'s worker was told");
        assert!(
            !told[0].contains(task),
            "the notice names {task}: {}",
            told[0]
        );
    }

    // 3. A restart names nobody twice.
    org.stop();
    org.start();
    passes(&p, 1);
    assert_eq!(org.scan_of(&l).len(), 1, "{:?}", org.scans());
    assert_eq!(org.secretary_naming("t461-a").len(), 1);
    assert_eq!(org.worker_told("t461-a").len(), 1);

    // 4. A landing that changes only README.md names nobody.
    let l2 = org.landing(&p, "t461-r2", 12, &l, &[("README.md", "two\n")], true);
    wait("the scan of L2", || !org.scan_of(&l2).is_empty());
    passes(&p, 1);
    let s2 = org.scan_of(&l2);
    assert_eq!(s2.len(), 1, "{s2:?}");
    assert_eq!(s2[0]["matched"], json!([]), "{}", s2[0]);
    assert_eq!(s2[0]["tasks"], json!([]), "{}", s2[0]);
    assert_eq!(org.secretary_naming("t461-a").len(), 1);
    assert_eq!(org.worker_told("t461-a").len(), 1);

    // 5. A landing under tests/common/, then one under .github/: each names A.
    let l3 = org.landing(
        &p,
        "t461-r3",
        13,
        &l2,
        &[("tests/common/helper.rs", "// helper\n")],
        true,
    );
    wait("the scan of L3", || !org.scan_of(&l3).is_empty());
    let s3 = org.scan_of(&l3);
    assert_eq!(
        s3[0]["matched"],
        json!(["tests/common/helper.rs"]),
        "{}",
        s3[0]
    );
    assert_eq!(named(&s3[0]), vec!["t461-a".to_string()], "{}", s3[0]);
    wait("A's worker told of L3", || {
        org.worker_told("t461-a").len() == 2
    });
    let l4 = org.landing(
        &p,
        "t461-r4",
        14,
        &l3,
        &[(".github/workflows/extra.yml", "name: extra\n")],
        true,
    );
    wait("the scan of L4", || !org.scan_of(&l4).is_empty());
    let s4 = org.scan_of(&l4);
    assert_eq!(
        s4[0]["matched"],
        json!([".github/workflows/extra.yml"]),
        "{}",
        s4[0]
    );
    assert_eq!(named(&s4[0]), vec!["t461-a".to_string()], "{}", s4[0]);
    wait("A's worker told of L4", || {
        org.worker_told("t461-a").len() == 3
    });
    passes(&p, 1);
    assert_eq!(org.secretary_naming("t461-a").len(), 3);
    assert_eq!(org.worker_told("t461-a").len(), 3);

    // 6. Two landings between scans are read together, from the tip scanned last.
    let l5 = org.landing(
        &p,
        "t461-r5",
        15,
        &l4,
        &[("tools/new_check.py", "print('ok')\n")],
        false,
    );
    let l6 = org.landing(&p, "t461-r6", 16, &l5, &[("README.md", "six\n")], false);
    let merged = |task: &str| {
        org.count(&format!(
            "SELECT count(*) FROM events e JOIN runs r ON r.id = e.run_id
             WHERE e.kind = 'pr_merged' AND r.task_id = '{task}'"
        )) > 0
    };
    wait("both merges read", || {
        merged("t461-r5") && merged("t461-r6")
    });
    passes(&p, 1);
    assert!(
        org.scan_of(&l5).is_empty() && org.scan_of(&l6).is_empty(),
        "{:?}",
        org.scans()
    );
    p.put_on_main(&l6);
    wait("the scan of L6", || !org.scan_of(&l6).is_empty());
    passes(&p, 1);
    assert!(org.scan_of(&l5).is_empty(), "{:?}", org.scans());
    let s6 = org.scan_of(&l6);
    assert_eq!(s6.len(), 1, "{s6:?}");
    assert_eq!(s6[0]["from"], json!(l4), "{}", s6[0]);
    assert_eq!(s6[0]["matched"], json!(["tools/new_check.py"]), "{}", s6[0]);
    assert_eq!(named(&s6[0]), vec!["t461-a".to_string()], "{}", s6[0]);
    assert_eq!(org.worker_told("t461-a").len(), 4);

    // 7. No scan of project p names the run of project q.
    for s in org.scans() {
        assert!(!named(&s).contains(&"t461-q".to_string()), "{s}");
    }
    assert!(org.worker_told("t461-q").is_empty());
}
