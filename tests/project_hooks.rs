//! Issue 236: a project's own git hooks run after the secret scan. In a code run that `makibad`
//! prepares, the worker's commit runs the scanner first (Issue 136) and then the hook of the same
//! name that the project clone would have run without `makibad`: the clone's configured
//! `core.hooksPath` (absolute, relative to the work tree, or `~/`), or else its `.git/hooks`. A
//! refusing project hook refuses the commit; a secret is refused before the project hook runs; a
//! project hook that stages a secret is refused by a second scan. Hook names the scanner does not
//! use are forwarded too. `makibad` stops setting the clone's own hooks path (and unsets it where
//! it is still the scanner's), says in its log that the project's hooks are forwarded, and its
//! own git (the `push` verb, the merge gate's fetch) runs no project hook.
//!
//! `makibad` runs as its own process (`MAKIBA_DOOR=none`, the stand-in base, the `fake` agent),
//! its standard error kept in a file; the door runs in this process on the same `state.db`. The
//! remote is a local bare repository: no network. Global and system git configuration are not
//! read. The token-shaped test data is joined from parts at run time, so that no file in the
//! repository holds a line a scanner would flag.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::gate;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

fn git_out(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Write an executable shell hook `dir/name` whose body is `body`.
fn hook(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    common::exe::write(&path, format!("#!/bin/sh\n{body}\n"));
}

/// A hook body that appends its working folder to the marker `marks/label`, reads its input and
/// exits with `code`.
fn marking(marks: &Path, label: &str, code: i32) -> String {
    format!(
        "pwd >> '{}'\ncat > /dev/null\nexit {code}",
        marks.join(label).display()
    )
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The clone `repos/p` with one commit on `main`, pushed to the bare `origin.git` and fetched
/// back; a state folder with a queued code run of `task` and its `planned` launch; the
/// secretary's key; a folder for markers.
struct Org {
    _dir: tempfile::TempDir,
    root: PathBuf,
    clone: PathBuf,
    origin: PathBuf,
    marks: PathBuf,
    task: String,
    secretary: String,
}

impl Org {
    fn new(task: &str) -> Org {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let clone = root.join("repos/p");
        std::fs::create_dir_all(&clone).unwrap();
        std::fs::write(clone.join("README.md"), "readme\n").unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["add", "README.md"]);
        git(&clone, &["commit", "-q", "-m", "first"]);
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &clone,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&clone, &["push", "-q", "origin", "main"]);
        git(&clone, &["fetch", "-q", "origin"]);
        let marks = root.join("marks");
        std::fs::create_dir_all(&marks).unwrap();
        std::fs::create_dir_all(root.join("tmp")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'code', 'fake', 'b', ?3)",
                rusqlite::params![
                    task,
                    format!("spawn-{task}"),
                    root.join("workers/p").join(task).to_string_lossy()
                ],
            )
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        Org {
            _dir: dir,
            root,
            clone,
            origin,
            marks,
            task: task.into(),
            secretary,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// The secret scanner's folder, as `makibad` names it.
    fn scanner(&self) -> PathBuf {
        self.state().join("git-hooks")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("makibad.log")).unwrap_or_default()
    }

    /// `makibad`, its home `home/`, its standard error in `makibad.log`.
    fn start(&self) -> Daemon {
        let log = std::fs::File::create(self.root.join("makibad.log")).unwrap();
        Daemon(
            makibad::daemon(&self.root)
                .env("MAKIBA_STATE_DIR", self.state())
                .env("MAKIBA_DOOR", "none")
                .env("MAKIBA_PORT", "0")
                .env("MAKIBA_PERSON_KEY", self.root.join("key/person.key"))
                .env("MAKIBA_BASE", "stub")
                .env("MAKIBA_WORKERS_DIR", self.root.join("workers"))
                .env("MAKIBA_REPOS_DIR", self.root.join("repos"))
                .env("MAKIBA_DOCS_DIR", self.root.join("docs"))
                .env("TMPDIR", self.root.join("tmp"))
                .env("HOME", self.root.join("home"))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        )
    }

    fn conn(&self) -> Connection {
        let c = Connection::open_with_flags(
            self.state().join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c
    }

    /// Wait until the launch is past `settings_ready`'s write; its workplace.
    fn prepared(&self) -> PathBuf {
        let until = Instant::now() + WAIT;
        loop {
            let (stage, worktree, why): (String, String, Option<String>) = self
                .conn()
                .query_row(
                    "SELECT stage, worktree, failed_reason FROM run_launch
                     WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                    [&self.task],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert!(stage != "failed", "the launch failed: {why:?}");
            if !matches!(stage.as_str(), "planned" | "worktree_ready") {
                return std::fs::canonicalize(worktree).unwrap();
            }
            assert!(Instant::now() < until, "still {stage}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The value of `key` in the run's own configuration, if any.
    fn run_config(&self, key: &str) -> Option<String> {
        let file = self.state().join("git").join(&self.task).join("config");
        let out = git_out(
            &self.root,
            &["config", "-f", &file.to_string_lossy(), "--get", key],
        );
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn mark(&self, label: &str) -> Option<String> {
        std::fs::read_to_string(self.marks.join(label)).ok()
    }

    /// The status of the push command once `makibad` has finished it, and the notices.
    fn pushed(&self) -> (String, Vec<String>) {
        let sql = "SELECT status FROM commands WHERE verb = 'push'
                   AND json_extract(args_json, '$.task') = ?1 ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        loop {
            let c = self.conn();
            let s: Option<String> = c
                .query_row(sql, [&self.task], |r| r.get(0))
                .optional()
                .unwrap();
            if let Some(s) = s.filter(|s| s != "accepted") {
                let mut q = c
                    .prepare(
                        "SELECT body FROM messages WHERE kind = 'notice' AND sender = 'makibad'
                         ORDER BY id",
                    )
                    .unwrap();
                let notices = q
                    .query_map([], |r| r.get(0))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                return (s, notices);
            }
            assert!(Instant::now() < until, "the push is not finished");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn head(wt: &Path) -> String {
    git(wt, &["rev-parse", "HEAD"])
}

/// Stage `text` as `name`, commit it with `message`; the output of the commit.
fn try_commit(wt: &Path, name: &str, text: &str, message: &str) -> Output {
    std::fs::write(wt.join(name), text).unwrap();
    git(wt, &["add", "--", name]);
    git_out(wt, &["commit", "-q", "-m", message])
}

/// Unstage and delete `name` after a refused commit, so the next case starts clean.
fn drop_file(wt: &Path, name: &str) {
    git(wt, &["reset", "-q", "--", name]);
    std::fs::remove_file(wt.join(name)).unwrap();
}

/// A line shaped like an AWS access key id, joined at run time.
fn token() -> String {
    format!("key = {}{}QWERTYUIOPASDFG7", "AK", "IA")
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Cases 1-3, the Issue's definition of done: the clone's `.git/hooks/pre-commit` runs after the
/// scan for a commit without a secret, refuses the commit when it refuses, and does not run for a
/// commit with a secret, which the scan refuses.
#[test]
fn a_project_pre_commit_hook_runs_after_the_scan_and_a_secret_is_refused_before_it() {
    let org = Org::new("ph1");
    hook(
        &org.clone.join(".git/hooks"),
        "pre-commit",
        &marking(&org.marks, "pre-commit", 0),
    );
    let _d = org.start();
    let wt = org.prepared();
    let before = head(&wt);

    // 3. A secret: refused by the scan, naming the shape; the project hook did not run.
    let out = try_commit(&wt, "secret.txt", &format!("{}\n", token()), "a secret");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a commit with a secret passed");
    assert!(err.contains("an AWS access key id"), "{err}");
    assert_eq!(head(&wt), before);
    assert_eq!(
        org.mark("pre-commit"),
        None,
        "the project hook ran before the scan refused"
    );
    drop_file(&wt, "secret.txt");

    // 1. A plain commit: lands, and the project hook ran in the workplace.
    let out = try_commit(&wt, "plain.txt", "plain\n", "a plain file");
    assert_ok(&out, "a plain commit");
    assert_ne!(head(&wt), before);
    let ran = org
        .mark("pre-commit")
        .expect("the project's pre-commit did not run");
    assert_eq!(ran.trim(), wt.to_string_lossy(), "{ran}");

    // 2. The project hook refuses: the commit is refused, HEAD stays.
    hook(
        &org.clone.join(".git/hooks"),
        "pre-commit",
        &marking(&org.marks, "refusing", 3),
    );
    let before = head(&wt);
    let out = try_commit(&wt, "second.txt", "second\n", "a second file");
    assert!(
        !out.status.success(),
        "a commit the project hook refused landed"
    );
    assert_eq!(head(&wt), before);
    assert!(
        org.mark("refusing").is_some(),
        "the refusing hook did not run"
    );
}

/// Case 4: an absolute `core.hooksPath` the clone had before preparation is the project's.
#[test]
fn an_absolute_hooks_path_of_the_clone_is_forwarded_to() {
    let org = Org::new("ph4");
    let theirs = org.root.join("person-hooks");
    hook(&theirs, "pre-commit", &marking(&org.marks, "absolute", 0));
    git(
        &org.clone,
        &["config", "core.hooksPath", &theirs.to_string_lossy()],
    );
    let _d = org.start();
    let wt = org.prepared();
    assert_eq!(
        org.run_config("makiba.projectHooksPath").as_deref(),
        Some(&*theirs.to_string_lossy())
    );
    assert_ok(
        &try_commit(&wt, "plain.txt", "plain\n", "a plain file"),
        "a plain commit",
    );
    assert!(
        org.mark("absolute").is_some(),
        "the configured hook did not run"
    );
}

/// Case 5: a relative `core.hooksPath` names hooks tracked in the project; the workplace's run.
#[test]
fn a_relative_hooks_path_runs_the_hook_tracked_in_the_workplace() {
    let org = Org::new("ph5");
    hook(
        &org.clone.join(".githooks"),
        "pre-commit",
        &marking(&org.marks, "relative", 0),
    );
    git(&org.clone, &["add", ".githooks/pre-commit"]);
    git(&org.clone, &["commit", "-q", "-m", "tracked hooks"]);
    git(&org.clone, &["push", "-q", "origin", "main"]);
    git(&org.clone, &["fetch", "-q", "origin"]);
    git(&org.clone, &["config", "core.hooksPath", ".githooks"]);
    let _d = org.start();
    let wt = org.prepared();
    assert!(wt.join(".githooks/pre-commit").is_file());
    assert_ok(
        &try_commit(&wt, "plain.txt", "plain\n", "a plain file"),
        "a plain commit",
    );
    let ran = org.mark("relative").expect("the tracked hook did not run");
    assert_eq!(ran.trim(), wt.to_string_lossy(), "{ran}");
}

/// Case 6: a hook name the scanner does not use is forwarded.
#[test]
fn a_post_commit_hook_of_the_project_is_forwarded() {
    let org = Org::new("ph6");
    hook(
        &org.clone.join(".git/hooks"),
        "post-commit",
        &marking(&org.marks, "post-commit", 0),
    );
    let _d = org.start();
    let wt = org.prepared();
    assert_ok(
        &try_commit(&wt, "plain.txt", "plain\n", "a plain file"),
        "a plain commit",
    );
    assert!(
        org.mark("post-commit").is_some(),
        "the project's post-commit did not run"
    );
}

/// Case 7: a project hook that stages a secret after the scan is refused by the second scan.
#[test]
fn a_secret_a_project_hook_stages_is_refused_by_the_second_scan() {
    let org = Org::new("ph7");
    hook(
        &org.clone.join(".git/hooks"),
        "pre-commit",
        "printf '%s%s\\n' 'key = AK' 'IAQWERTYUIOPASDFG7' > sneaky.txt && git add sneaky.txt",
    );
    let _d = org.start();
    let wt = org.prepared();
    let before = head(&wt);
    let out = try_commit(&wt, "plain.txt", "plain\n", "a plain file");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a secret staged by the project hook landed"
    );
    assert!(err.contains("sneaky.txt"), "{err}");
    assert_eq!(head(&wt), before);
}

/// Case 8 (and D7): the clone's hooks path is not the scanner's after preparation; one `makibad`
/// set before this change is unset, and the log says a local value it replaced is lost.
#[test]
fn the_clone_keeps_no_hooks_path_of_the_scanner() {
    let org = Org::new("ph8");
    git(
        &org.clone,
        &["config", "core.hooksPath", &org.scanner().to_string_lossy()],
    );
    let _d = org.start();
    org.prepared();
    let now = git_out(&org.clone, &["config", "--get-all", "core.hooksPath"]);
    let now = String::from_utf8_lossy(&now.stdout);
    assert!(
        !now.contains(&*org.scanner().to_string_lossy()),
        "the clone's hooks path is still the scanner's: {now}"
    );
    let log = org.log();
    assert!(log.contains("cannot be recovered"), "{log}");
}

/// Case 9: `makibad`'s own git runs no project hook: the `push` verb (the handing of the run's
/// head into the clone and the push to the remote) fires neither the clone's receive-side hooks
/// nor its `pre-push` or `reference-transaction`.
#[test]
fn the_push_verb_runs_no_project_hook() {
    let org = Org::new("ph9");
    let _d = org.start();
    let wt = org.prepared();
    assert_ok(
        &try_commit(&wt, "work.txt", "work\n", "the worker's commit"),
        "the worker's commit",
    );
    let hooks = org.clone.join(".git/hooks");
    for name in [
        "pre-push",
        "reference-transaction",
        "pre-receive",
        "update",
        "post-receive",
        "post-update",
    ] {
        hook(&hooks, name, &marking(&org.marks, name, 0));
    }
    let door = Door::new(
        StateWriter::open(&org.state().join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );
    let r = door.handle(
        Some(&org.secretary),
        "push",
        json!({"task": org.task}),
        None,
        true,
    );
    assert!(r.is_ok(), "{r:?}");
    let (status, notices) = org.pushed();
    assert_eq!(status, "done", "{notices:?}");
    assert_eq!(
        git(
            &org.origin,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/makiba/{}", org.task)
            ]
        ),
        head(&wt)
    );
    let fired: Vec<String> = std::fs::read_dir(&org.marks)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        fired.is_empty(),
        "makibad's git ran project hooks: {fired:?}"
    );
}

/// Case 10: the log says the project's own hooks were found and run after the scan.
#[test]
fn the_log_says_the_projects_own_hooks_are_forwarded() {
    let org = Org::new("ph10");
    let hooks = org.clone.join(".git/hooks");
    hook(&hooks, "pre-commit", "exit 0");
    let _d = org.start();
    org.prepared();
    let log = org.log();
    let line = log
        .lines()
        .find(|l| l.contains("run after the secret scan"))
        .unwrap_or_else(|| panic!("no forwarding line in the log: {log}"));
    assert!(line.contains("ph10"), "{line}");
    assert!(line.contains(&*hooks.to_string_lossy()), "{line}");
    assert!(line.contains("pre-commit"), "{line}");
}

/// Case 11: `core.hooksPath=~/...` is expanded as git does, with `makibad`'s home.
#[test]
fn a_hooks_path_under_the_home_is_expanded() {
    let org = Org::new("ph11");
    let theirs = org.root.join("home/hk");
    hook(&theirs, "pre-commit", &marking(&org.marks, "home", 0));
    git(&org.clone, &["config", "core.hooksPath", "~/hk"]);
    let _d = org.start();
    let wt = org.prepared();
    assert_eq!(
        org.run_config("makiba.projectHooksPath").as_deref(),
        Some(&*theirs.to_string_lossy())
    );
    assert_ok(
        &try_commit(&wt, "plain.txt", "plain\n", "a plain file"),
        "a plain commit",
    );
    assert!(
        org.mark("home").is_some(),
        "the hook under the home did not run"
    );
}

// ---- case 9b: the merge gate ---------------------------------------------------------------------

const GATE_KEY: &str = "236236236236236236236236236236236236236236236236236236236236ab12";

/// `pr view <n> --repo o/<name> ...` prints `<name>/view.<n>`; any other call fails.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  name=${5#*/}; cat "$FAKE_GH_DIR/$name/view.$3"; exit 0
fi
echo "unexpected call: $*" >&2; exit 2
"#;

/// The gate's folder: the `gh` stand-in and the repositories folder, set once for this process
/// (the door reads them here; every `makibad` above is given its own).
fn gate_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let script = home.join("gh");
        common::exe::write(&script, STAND_IN);
        std::fs::create_dir_all(home.join("repos")).unwrap();
        // SAFETY: set once; the variables are read by this process's door only.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", &home);
            std::env::set_var("MAKIBA_REPOS_DIR", home.join("repos"));
            std::env::set_var(gate::MODE_ENV, "human");
        }
        home
    })
}

/// Case 9b: the merge gate's fetch in the clone runs no project `reference-transaction`.
#[test]
fn the_merge_gates_fetch_runs_no_project_hook() {
    let home = gate_home();
    let name = "gatehooks";
    let tmp = tempfile::tempdir().unwrap();
    let served = tmp.path().join("served");
    std::fs::create_dir_all(&served).unwrap();
    git(&served, &["init", "-q", "-b", "main"]);
    std::fs::write(served.join("a.txt"), "a\n").unwrap();
    git(&served, &["add", "a.txt"]);
    git(&served, &["commit", "-q", "-m", "base"]);
    let clone = home.join("repos").join(name);
    git(
        &home.join("repos"),
        &["clone", "-q", &served.to_string_lossy(), name],
    );
    let origin_url = format!("git@github.com:o/{name}.git");
    git(&clone, &["remote", "set-url", "origin", &origin_url]);
    git(
        &clone,
        &[
            "config",
            &format!("url.{}.insteadOf", served.display()),
            &origin_url,
        ],
    );
    git(&served, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(served.join("b.txt"), "b\n").unwrap();
    git(&served, &["add", "b.txt"]);
    git(&served, &["commit", "-q", "-m", "change"]);
    let pr_head = git(&served, &["rev-parse", "HEAD"]);
    git(&served, &["update-ref", "refs/pull/7/head", &pr_head]);
    git(&served, &["checkout", "-q", "main"]);
    let gh = home.join(name);
    std::fs::create_dir_all(&gh).unwrap();
    let view = json!({"number": 7, "url": format!("https://github.com/o/{name}/pull/7"),
                      "baseRefName": "main", "headRefOid": pr_head});
    std::fs::write(gh.join("view.7"), view.to_string()).unwrap();

    let marks = tmp.path().join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    hook(
        &clone.join(".git/hooks"),
        "reference-transaction",
        &marking(&marks, "reference-transaction", 0),
    );

    let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
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
    w.append_event(&NewEvent {
        kind: "ci_completed",
        actor: Some("makibad"),
        payload: Some(
            &json!({"repo": format!("o/{name}"), "pr": 7, "head_oid": pr_head,
                    "status": "passed", "fail_count": 0, "pending_count": 0,
                    "total_checks": 1, "probe_attempts": 1}),
        ),
        ..Default::default()
    })
    .unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
    door.set_gate_key(GATE_KEY);
    let v: Value = door
        .handle(
            Some(GATE_KEY),
            "gate",
            json!({"repo": format!("o/{name}"), "pr": 7, "head": pr_head}),
            None,
            false,
        )
        .unwrap_or_else(|e| panic!("gate: {e:?}"));
    assert_eq!(v["allow"], json!(true), "{v}");
    let fetched = git(
        &clone,
        &["rev-parse", &format!("refs/makiba/gate/o/{name}/7/head")],
    );
    assert_eq!(fetched, pr_head);
    assert!(
        !marks.join("reference-transaction").exists(),
        "the gate's fetch ran the project's reference-transaction hook"
    );
}
