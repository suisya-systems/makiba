//! Issue 227: `makiba status` lists the registered projects, and the secretary's tool `status`
//! returns the same list.
//!
//! The first case runs `makiba daemon` as a child process on the stand-in base, with no Unix
//! socket, and registers `acme/widget` from a bare repository under a scratch folder that
//! `MAKIBA_GIT_URL` points at, so nothing reaches the network. The second runs no `makibad`.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn git(dir: &Path, args: &[&str]) {
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
}

/// The person's `makiba status` (no door: it reads `state.db` itself).
fn status(state: &Path, json: bool) -> Output {
    let mut c = Command::new(MAKIBA);
    c.arg("status");
    if json {
        c.arg("--json");
    }
    c.env("MAKIBA_STATE_DIR", state)
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE", "stub")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn projects_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("projects:"))
        .collect()
}

/// The entry of `slug` in a `projects` array.
fn entry<'a>(v: &'a Value, slug: &str) -> Option<&'a Value> {
    v["projects"]
        .as_array()
        .unwrap_or_else(|| panic!("no projects array: {v}"))
        .iter()
        .find(|p| p["slug"] == json!(slug))
}

/// A tool call on the agents' port: whether it is an error, and the JSON it carries.
fn tool(port: u16, key: &str, verb: &str, args: Value) -> (bool, Value) {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": verb, "arguments": args}})
    .to_string();
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
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

#[test]
fn status_says_when_no_project_is_registered_and_names_a_registered_one_with_its_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let root: PathBuf = std::fs::canonicalize(tmp.path()).unwrap();
    for d in ["state", "home", "remote/acme", "work"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let bare = root.join("remote/acme/widget.git");
    git(
        &root,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let work = root.join("work");
    git(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("README"), "one\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-q", "-m", "one"]);
    git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);

    let state = root.join("state");
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
    let set = json!({"action": "set", "key": "allowed_repo_owners", "value": "acme"});
    assert!(
        door.handle(Some(&person), "config", set, None, false)
            .is_ok()
    );
    drop(door);

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut c = makibad::daemon(&root);
    c.env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("MAKIBA_STATE_DIR", &state)
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
        .env("MAKIBA_BASE", "stub");
    for v in [
        "MAKIBA_FAKE_AGENT",
        "MAKIBA_LAUNCH_AGENT",
        "MAKIBA_SECRETARY",
        "MAKIBA_SECRETARY_REMOTE_CONTROL",
        "MAKIBA_GH",
    ] {
        c.env_remove(v);
    }
    let _daemon = Daemon(
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(15);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < until, "the agents' door did not open");
        std::thread::sleep(Duration::from_millis(50));
    }

    // No project yet: one line says so, and the tool's list is empty.
    let text = stdout(&status(&state, false));
    let lines = projects_lines(&text);
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].contains("none"), "{text}");
    let (err, before) = tool(port, &secretary, "status", json!({}));
    assert!(!err, "{before}");
    assert_eq!(before["projects"], json!([]), "{before}");

    // The secretary registers acme/widget; wait until makibad finished that command.
    let (err, answer) = tool(
        port,
        &secretary,
        "project",
        json!({"slug": "widget", "repo": "acme/widget"}),
    );
    assert!(!err, "{answer}");
    let db = || {
        Connection::open_with_flags(state.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    };
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let done: Option<String> = db()
            .query_row(
                "SELECT status FROM commands WHERE verb = 'project'
                   AND status IN ('done', 'failed') ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        if let Some(s) = done {
            assert_eq!(s, "done", "the project was not fetched");
            break;
        }
        assert!(Instant::now() < until, "the project did not finish");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The command names it with its repository.
    let text = stdout(&status(&state, false));
    assert!(
        text.lines()
            .any(|l| l.contains("widget:") && l.contains("acme/widget")),
        "{text}"
    );
    assert!(!text.contains("none registered"), "{text}");
    // So does the tool, and the person's --json.
    let (err, after) = tool(port, &secretary, "status", json!({}));
    assert!(!err, "{after}");
    let e = entry(&after, "widget").unwrap_or_else(|| panic!("no widget: {after}"));
    assert_eq!(e["repo"], json!("acme/widget"), "{after}");
    let local: Value = serde_json::from_slice(&status(&state, true).stdout).unwrap();
    let e = entry(&local, "widget").unwrap_or_else(|| panic!("no widget: {local}"));
    assert_eq!(e["repo"], json!("acme/widget"), "{local}");
}

#[test]
fn status_lists_the_active_projects_by_slug_when_makibad_never_ran() {
    let tmp = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
    w.ensure_project("zeta", None, Some("https://github.com/Acme/Zeta"))
        .unwrap();
    w.ensure_project("alpha", None, None).unwrap();
    w.ensure_project("gone", None, Some("https://github.com/acme/gone"))
        .unwrap();
    w.conn()
        .execute(
            "UPDATE projects SET status = 'archived' WHERE slug = 'gone'",
            [],
        )
        .unwrap();
    drop(w);

    let out = status(tmp.path(), false);
    let text = stdout(&out);
    assert!(!out.status.success(), "no makibad, yet exit 0: {text}");
    let zeta = text
        .lines()
        .position(|l| l.contains("zeta:") && l.contains("Acme/Zeta"))
        .unwrap_or_else(|| panic!("no zeta with its repository: {text}"));
    let alpha = text
        .lines()
        .position(|l| l.contains("alpha:"))
        .unwrap_or_else(|| panic!("no alpha: {text}"));
    assert!(alpha < zeta, "not by slug: {text}");
    assert!(
        !text.contains("gone:"),
        "an archived project is listed: {text}"
    );
}
