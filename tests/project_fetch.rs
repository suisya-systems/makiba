//! Issue 184: the verb `project`, by which the secretary registers a project and has `makibad`
//! fetch its repository.
//!
//! `makiba daemon` runs as a child process on the stand-in base, with no Unix socket. The
//! repository is a bare one under a scratch folder that `MAKIBA_GIT_URL` points at, so
//! `OWNER/NAME` becomes `file://<root>/remote/OWNER/NAME.git` and nothing reaches the network.
//! The person's setting `allowed_repo_owners` and the agents' keys are written before `makibad`
//! starts; while it runs, this test only calls the agents' port and reads `state.db` read-only,
//! waiting on the rows `makibad` wrote (Issue 181).

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

struct Org {
    root: PathBuf,
    port: u16,
    secretary: String,
    worker: String,
    daemon: Child,
    _tmp: tempfile::TempDir,
}

impl Drop for Org {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
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

impl Org {
    /// An organization whose person allowed the owner `acme`, with the repositories
    /// `acme/widget` and `other/widget` (one commit on `main`) under the scratch remote, and a
    /// running `makibad` on the stand-in base.
    fn start() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        for d in [
            "state",
            "bin",
            "home",
            "remote/acme",
            "remote/other",
            "work",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let claude = root.join("bin/claude");
        std::fs::write(&claude, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        for owner in ["acme", "other"] {
            let bare = root.join(format!("remote/{owner}/widget.git"));
            git(
                &root,
                &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
            );
        }
        let work = root.join("work");
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("README"), "one\n").unwrap();
        git(&work, &["add", "README"]);
        git(&work, &["commit", "-q", "-m", "one"]);
        for owner in ["acme", "other"] {
            let bare = root.join(format!("remote/{owner}/widget.git"));
            git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
        }

        // Before makibad starts: the person's setting, and the keys of a secretary and a worker.
        let db = root.join("state/state.db");
        let w = StateWriter::open(&db).unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let worker = issue_key(&w, Role::Worker, "w-t0", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        let set = json!({"action": "set", "key": "allowed_repo_owners", "value": "acme"});
        let r = door.handle(Some(&person), "config", set, None, false);
        assert!(r.is_ok(), "config set allowed_repo_owners: {r:?}");
        drop(door);

        let mut c = Command::new(MAKIBA);
        c.env("PATH", &path)
            .env("HOME", root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("MAKIBA_STATE_DIR", root.join("state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("MAKIBA_SHARE_DIR", root.join("share"))
            .env("MAKIBA_PERSON_KEY", root.join("elsewhere/person.key"))
            .env("MAKIBA_DOCS_DIR", root.join("docs"))
            .env("MAKIBA_WORKERS_DIR", root.join("workers"))
            .env("MAKIBA_REPOS_DIR", root.join("repos"))
            .env("MAKIBA_KNOWLEDGE_DIR", root.join("knowledge"))
            .env(
                "MAKIBA_GIT_URL",
                format!("file://{}", root.join("remote").display()),
            )
            .env("MAKIBA_PORT", port.to_string())
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_MAX_CONCURRENT_WORKERS", "4");
        for v in [
            "MAKIBA_FAKE_AGENT",
            "MAKIBA_LAUNCH_AGENT",
            "MAKIBA_SECRETARY",
            "MAKIBA_SECRETARY_REMOTE_CONTROL",
            "MAKIBA_GH",
        ] {
            c.env_remove(v);
        }
        let daemon = c
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let org = Org {
            root,
            port,
            secretary,
            worker,
            daemon,
            _tmp: tmp,
        };
        let until = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < until, "the agents' door did not open");
            std::thread::sleep(Duration::from_millis(50));
        }
        org
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.root.join("state/state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn repos(&self) -> PathBuf {
        self.root.join("repos")
    }

    /// A tool call on the agents' port: whether it is an error, and the JSON it carries.
    fn tool(&self, key: &str, verb: &str, args: Value) -> (bool, Value) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": verb, "arguments": args}})
        .to_string();
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut all = String::new();
        s.read_to_string(&mut all).unwrap();
        let reply: Value = serde_json::from_str(all.split_once("\r\n\r\n").unwrap().1).unwrap();
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or("null");
        (
            result["isError"] == json!(true),
            serde_json::from_str(text).unwrap_or(Value::Null),
        )
    }

    /// The secretary's `project`, accepted; then wait until `makibad` finished that command and
    /// return its row's status and result.
    fn project(&self, slug: &str, repo: &str) -> (String, Value) {
        let before: i64 = self
            .db()
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM commands WHERE verb = 'project'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let (err, answer) = self.tool(
            &self.secretary,
            "project",
            json!({"slug": slug, "repo": repo}),
        );
        assert!(!err, "project {slug} {repo}: {answer}");
        let until = Instant::now() + Duration::from_secs(120);
        loop {
            let row: Option<(String, Option<String>)> = self
                .db()
                .query_row(
                    "SELECT status, result_json FROM commands
                     WHERE verb = 'project' AND actor = 'secretary' AND id > ?1
                       AND status IN ('done', 'failed')
                     ORDER BY id LIMIT 1",
                    [before],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .unwrap();
            if let Some((status, result)) = row {
                // The stored answer line: `{"ok": true, "result": {...}}` or `{"ok": false, "error": {...}}`.
                let line: Value = result
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or(Value::Null);
                let result = match line.get("result") {
                    Some(r) => r.clone(),
                    None => line,
                };
                return (status, result);
            }
            assert!(
                Instant::now() < until,
                "project {slug} {repo} did not finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The secretary's `dispatch` of a code task; then wait until `makibad` launched its worker
    /// (the launch's stage), and return that launch's working folder.
    fn dispatch(&self, task: &str, project: &str) -> PathBuf {
        let (err, answer) = self.tool(
            &self.secretary,
            "dispatch",
            json!({"kind": "code", "task_id": task, "title": "Fix", "brief": "Fix it.",
                   "project": project}),
        );
        assert!(!err, "dispatch {task}: {answer}");
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let row: Option<(String, String, Option<String>)> = self
                .db()
                .query_row(
                    "SELECT l.stage, l.worktree, l.failed_reason
                     FROM run_launch l JOIN runs r ON r.id = l.run_id WHERE r.task_id = ?1",
                    [task],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .unwrap();
            if let Some((stage, worktree, failed)) = row {
                assert_ne!(stage, "failed", "the launch of {task} failed: {failed:?}");
                if ["launched", "dialog_passed", "verified"].contains(&stage.as_str()) {
                    return PathBuf::from(worktree);
                }
            }
            assert!(Instant::now() < until, "{task} was not launched");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The project row of `slug`: its origin and default branch.
    fn project_row(&self, slug: &str) -> Option<(Option<String>, Option<String>)> {
        self.db()
            .query_row(
                "SELECT origin_url, default_branch FROM projects WHERE slug = ?1",
                [slug],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    /// A new commit on `main` of the remote `acme/widget`; its id.
    fn advance_remote(&self) -> String {
        let work = self.root.join("work");
        std::fs::write(work.join("README"), "two\n").unwrap();
        git(&work, &["commit", "-q", "-am", "two"]);
        let bare = self.root.join("remote/acme/widget.git");
        git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
        git(&work, &["rev-parse", "HEAD"])
    }
}

/// Points 1 and 2: with no clone, the verb leaves one and a project row, and a dispatch then
/// launches from it; called again once the clone exists, it fetches (the clone is the same one)
/// and the next dispatch starts at the new commit.
#[test]
fn the_secretary_registers_a_project_and_dispatches_into_it_then_fetches_a_newer_base() {
    let org = Org::start();
    let clone = org.repos().join("widget");
    assert!(!clone.exists());
    let first = git(&org.root.join("work"), &["rev-parse", "HEAD"]);

    let (status, result) = org.project("widget", "acme/widget");
    assert_eq!(status, "done", "{result}");
    assert_eq!(result["action"], "clone", "{result}");
    assert!(
        clone.join(".git").is_dir(),
        "no clone at {}",
        clone.display()
    );
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), first);
    let (origin, branch) = org.project_row("widget").expect("no project row of widget");
    assert_eq!(origin.as_deref(), Some("https://github.com/acme/widget"));
    assert_eq!(branch.as_deref(), Some("main"));

    let worktree = org.dispatch("t1", "widget");
    assert_eq!(worktree, org.root.join("workers/widget/t1"));
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), first);

    // A file only this clone has: a second clone would not carry it.
    let marker = clone.join(".git/makiba-test-marker");
    std::fs::write(&marker, "the first clone\n").unwrap();
    let second = org.advance_remote();

    let (status, result) = org.project("widget", "acme/widget");
    assert_eq!(status, "done", "{result}");
    assert_eq!(result["action"], "fetch", "{result}");
    assert!(marker.exists(), "the clone was made again");
    assert_eq!(git(&clone, &["rev-parse", "origin/main"]), second);
    assert_eq!(git(&clone, &["rev-parse", "main"]), second);

    let worktree = org.dispatch("t2", "widget");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), second);
}

/// Point 3: a worker's key cannot call it, and the refusal is recorded.
#[test]
fn a_workers_key_cannot_call_project() {
    let org = Org::start();
    let (err, answer) = org.tool(
        &org.worker,
        "project",
        json!({"slug": "widget", "repo": "acme/widget"}),
    );
    assert!(err, "{answer}");
    assert_eq!(answer["kind"], "denied", "{answer}");
    let rejected: i64 = org
        .db()
        .query_row(
            "SELECT COUNT(*) FROM commands
             WHERE verb = 'project' AND actor = 'w-t0' AND status = 'rejected' AND error_kind = 'denied'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rejected, 1);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!org.repos().join("widget").exists());
    assert!(org.project_row("widget").is_none());
}

/// Point 4: a repository outside what the person allowed (an owner not listed, a URL instead of
/// `OWNER/NAME`, no repository at all) is refused at once, and nothing is cloned.
#[test]
fn a_repository_outside_the_allowed_owners_is_refused_and_nothing_is_cloned() {
    let org = Org::start();
    let (err, answer) = org.tool(
        &org.secretary,
        "project",
        json!({"slug": "widget", "repo": "other/widget"}),
    );
    assert!(err, "{answer}");
    assert_eq!(answer["kind"], "denied", "{answer}");

    for repo in [
        json!("https://example.invalid/acme/widget.git"),
        json!("file:///etc"),
        json!("../acme/widget"),
        json!("acme/widget/extra"),
        Value::Null,
    ] {
        let mut args = json!({"slug": "widget"});
        if !repo.is_null() {
            args["repo"] = repo.clone();
        }
        let (err, answer) = org.tool(&org.secretary, "project", args);
        assert!(err, "{repo}: {answer}");
        assert_eq!(answer["kind"], "usage", "{repo}: {answer}");
    }
    let accepted: i64 = org
        .db()
        .query_row(
            "SELECT COUNT(*) FROM commands
             WHERE verb = 'project' AND status IN ('accepted', 'done', 'failed')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(accepted, 0, "a refused request was carried out");
    std::thread::sleep(Duration::from_millis(500));
    let left: Vec<_> = std::fs::read_dir(org.repos())
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(
        left.is_empty(),
        "something was made in the repositories folder: {left:?}"
    );
    assert!(org.project_row("widget").is_none());
}
