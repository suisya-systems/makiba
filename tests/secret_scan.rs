//! Issue 136: no secret goes into the git history (line L-K12 of `docs/design/lines.md`). A
//! worktree prepared by `makibad` refuses a commit that adds a line shaped like a token, in a file
//! or in the message; a benign commit lands; the hooks path is absolute, outside the worktree and
//! denied for write in the run's settings; a missing scanner refuses with the path it misses; and
//! the line in `lines.md` names the hook, which the binary knows.
//!
//! The token-shaped test data is joined from two halves at run time, so that no file in the
//! repository holds a line a scanner would flag.

use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

fn git_out(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
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

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A state folder and a project repository with one commit on `main`, under one temporary
/// folder, and a code run queued with its `planned` launch.
struct Org {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    conn: Connection,
}

impl Org {
    fn new(task: &str) -> Org {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repos/p");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("README.md"), "readme\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
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
        drop(w);
        let conn = Connection::open(state.join("state.db")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'code', 'fake', 'b', ?3)",
            rusqlite::params![
                task,
                format!("spawn-{task}"),
                root.join("workers/p").join(task).to_string_lossy()
            ],
        )
        .unwrap();
        Org {
            _dir: dir,
            root,
            repo,
            conn,
        }
    }

    fn start(&self, program: &Path) -> Daemon {
        Daemon(
            Command::new(program)
                .arg("daemon")
                .env("MAKIBA_STATE_DIR", self.root.join("state"))
                .env("MAKIBA_DOOR", "none")
                .env("MAKIBA_PORT", "0")
                .env("MAKIBA_PERSON_KEY", self.root.join("key/person.key"))
                .env("MAKIBA_BASE", "stub")
                .env("MAKIBA_WORKERS_DIR", self.root.join("workers"))
                .env("MAKIBA_REPOS_DIR", self.root.join("repos"))
                .env("MAKIBA_DOCS_DIR", self.root.join("docs"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    /// Wait until the task's launch is `settings_ready`; its worktree.
    fn prepared(&self, task: &str) -> PathBuf {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let (stage, worktree, why): (String, String, Option<String>) = self
                .conn
                .query_row(
                    "SELECT stage, worktree, failed_reason FROM run_launch
                     WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                    [task],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            if stage == "settings_ready" {
                return PathBuf::from(worktree);
            }
            assert!(stage != "failed", "the launch failed: {why:?}");
            assert!(Instant::now() < until, "still {stage}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// One token-shaped line per shape of the scanner's list, each joined from two halves.
fn token_lines() -> Vec<(&'static str, String)> {
    let join = |a: &str, b: &str| format!("{a}{b}");
    vec![
        ("aws", join("key = AK", "IAQWERTYUIOPASDFG7")),
        (
            "github",
            join("token: gh", &format!("p_{}", &"a1B2c3D4e5".repeat(4)[..36])),
        ),
        (
            "github-pat",
            join("github_", &format!("pat_{}", &"x9_Y8z7W6v".repeat(9)[..82])),
        ),
        (
            "private-key",
            join("-----BEGIN OPENSSH PRIV", "ATE KEY-----"),
        ),
        (
            "anthropic",
            join("sk-a", "nt-api03-abcdefghijklmnopqrstuvwxyz0123"),
        ),
        ("slack", join("xo", "xb-1234567890-abcdefghijkl")),
    ]
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

#[test]
fn a_commit_adding_a_token_shaped_line_is_refused_in_a_prepared_worktree() {
    let org = Org::new("t1");
    let _d = org.start(Path::new(MAKIBA));
    let wt = org.prepared("t1");
    let before = head(&wt);

    for (shape, line) in token_lines() {
        let name = format!("{shape}.txt");
        let out = try_commit(&wt, &name, &format!("ordinary\n{line}\n"), "add a file");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{shape}: the commit was not refused");
        assert!(
            err.contains(&name),
            "{shape}: the refusal names no file: {err}"
        );
        assert!(
            !err.contains(&line),
            "{shape}: the refusal prints the token: {err}"
        );
        assert_eq!(head(&wt), before, "{shape}: something was committed");
        drop_file(&wt, &name);
    }

    // A file git takes for binary (a NUL byte) is read too.
    let (_, line) = &token_lines()[0];
    let out = try_commit(&wt, "blob.bin", &format!("\0\0{line}\0"), "a blob");
    assert!(!out.status.success(), "a token in a binary file passed");
    assert_eq!(head(&wt), before);
    drop_file(&wt, "blob.bin");

    // A token in the commit message is refused as well.
    let out = try_commit(&wt, "plain.txt", "plain\n", &format!("note {line}"));
    assert!(!out.status.success(), "a token in the message passed");
    assert_eq!(head(&wt), before);

    // A benign commit lands.
    let out = git_out(&wt, &["commit", "-q", "-m", "a plain file"]);
    assert!(
        out.status.success(),
        "a benign commit was refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(head(&wt), before);
}

#[test]
fn the_hooks_path_is_absolute_outside_the_worktree_and_denied_for_write() {
    let org = Org::new("t2");
    let _d = org.start(Path::new(MAKIBA));
    let wt = org.prepared("t2");
    let hooks = git(&wt, &["config", "--get", "core.hooksPath"]);
    // The clone's own hooks path is the person's (Issue 236): makibad sets none there.
    assert!(
        !git_out(&org.repo, &["config", "--get", "core.hooksPath"])
            .status
            .success()
    );
    let hooks = PathBuf::from(hooks);
    assert!(hooks.is_absolute(), "{}", hooks.display());
    assert!(!hooks.starts_with(&wt), "{}", hooks.display());
    for name in [
        "pre-commit",
        "pre-merge-commit",
        "pre-applypatch",
        "commit-msg",
    ] {
        assert!(
            hooks.join(name).is_file(),
            "no {name} in {}",
            hooks.display()
        );
    }
    let settings = std::fs::read_to_string(org.root.join("state/settings/t2.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&settings).unwrap();
    let deny = v["sandbox"]["filesystem"]["denyWrite"].as_array().unwrap();
    assert!(
        deny.iter().any(|d| d.as_str() == hooks.to_str()),
        "the hooks path is not denied for write: {settings}"
    );
}

#[test]
fn a_missing_scanner_refuses_the_commit_and_names_its_path() {
    let org = Org::new("t3");
    // makibad runs from a copy, which is removed once the worktree is prepared.
    let bin = org.root.join("bin/makiba");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::copy(MAKIBA, &bin).unwrap();
    let wt = {
        let _d = org.start(&bin);
        org.prepared("t3")
    };
    std::fs::remove_file(&bin).unwrap();
    let before = head(&wt);
    let out = try_commit(&wt, "plain.txt", "plain\n", "a plain file");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a commit passed with no scanner");
    assert!(
        err.contains(&*bin.to_string_lossy()),
        "the refusal does not name the missing path: {err}"
    );
    assert_eq!(head(&wt), before);
}

#[test]
fn the_line_in_lines_md_names_the_scanner_and_the_binary_knows_it() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design/lines.md"))
            .unwrap();
    let row = doc
        .lines()
        .find(|l| l.starts_with("| L-K12 "))
        .expect("lines.md has no row L-K12");
    assert!(row.contains("`hook pre-commit`"), "{row}");
    assert!(!row.contains("Open: #136"), "{row}");
    assert!(row.contains("#140") && row.contains("#231"), "{row}");

    // The binary knows the hook: it does not answer with its usage line.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(MAKIBA)
        .args(["hook", "pre-commit"])
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("usage:"), "{err}");
    // Outside a repository it cannot scan, so it refuses.
    assert!(!out.status.success(), "{err}");
}
