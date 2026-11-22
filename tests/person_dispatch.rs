//! Issue 143: the person's `dispatch`, brought forward for the first real worker.
//!
//! The daemon's half is asserted through the agents' port with the person key (the same handler
//! as the person's socket, and it runs where a Unix socket cannot be made). The person's own
//! command, `makiba dispatch`, is run over the socket by the last test.
//!
//! `makibad` runs on the stand-in base. The launch is `claude_code` unless the test asks for the
//! test agent with `MAKIBA_LAUNCH_AGENT=fake` (spec section 2); the tests that look at the
//! `queued` run keep capacity at 0 so no pass takes it while they look.

mod common;

use common::makibad;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const BRIEF: &str = "Write the release notes.\nSecond line, with a quote: \"x\".\n";

struct Org {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    daemon: Child,
}

impl Org {
    fn env(root: &std::path::Path, port: u16, c: &mut Command, agent: &str, capacity: &str) {
        c.env("HOME", root.join("home"))
            .env("MAKIBA_STATE_DIR", root.join("state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("MAKIBA_DOCS_DIR", root.join("docs"))
            .env("MAKIBA_WORKERS_DIR", root.join("workers"))
            .env("MAKIBA_PORT", port.to_string())
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_MAX_CONCURRENT_WORKERS", capacity);
        for v in [
            "MAKIBA_DOOR",
            "MAKIBA_PERSON_KEY",
            "MAKIBA_SHARE_DIR",
            "MAKIBA_FAKE_AGENT",
            "MAKIBA_LAUNCH_AGENT",
        ] {
            c.env_remove(v);
        }
        if !agent.is_empty() {
            c.env("MAKIBA_LAUNCH_AGENT", agent);
        }
    }

    /// `agent`: the value of `MAKIBA_LAUNCH_AGENT`, none when empty. `socket`: whether the
    /// person's socket is opened (`MAKIBA_DOOR=none` otherwise).
    fn start(agent: &str, capacity: &str, socket: bool) -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["home", "state", "config", "data", "run"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut c = makibad::daemon(&root);
        c.with(|c| Org::env(&root, port, c, agent, capacity));
        if !socket {
            c.env("MAKIBA_DOOR", "none");
        }
        let daemon = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let org = Org {
            _tmp: tmp,
            root,
            port,
            daemon,
        };
        let until = Instant::now() + Duration::from_secs(15);
        while !(TcpStream::connect(("127.0.0.1", port)).is_ok() && org.key_file().exists()) {
            assert!(Instant::now() < until, "the door did not open");
            std::thread::sleep(Duration::from_millis(50));
        }
        org
    }

    fn key_file(&self) -> PathBuf {
        self.root.join("config/makiba/person.key")
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.root.join("state/state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn text(&self, sql: &str) -> String {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// The tool `dispatch` with the person key: whether it is an error, and the JSON it carries.
    fn dispatch(&self, args: Value) -> (bool, Value) {
        let key = std::fs::read_to_string(self.key_file()).unwrap();
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "dispatch", "arguments": args}})
        .to_string();
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            key.trim(),
            body.len()
        )
        .unwrap();
        let mut all = String::new();
        s.read_to_string(&mut all).unwrap();
        let reply: Value = serde_json::from_str(all.split_once("\r\n\r\n").unwrap().1).unwrap();
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }

    /// The person's `makiba`, as a person runs it.
    fn cli(&self, args: &[&str]) -> Output {
        let mut c = Command::new(MAKIBA);
        Org::env(&self.root, self.port, &mut c, "", "0");
        c.args(args).stdin(Stdio::null()).output().unwrap()
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn document(task: &str) -> Value {
    json!({"kind": "document", "task_id": task, "title": "Notes", "brief": BRIEF})
}

/// The four points of the Issue, as far as the daemon goes without the test agent.
#[test]
fn a_document_dispatch_is_accepted_and_a_code_one_without_a_project_and_a_repeat_are_not() {
    let org = Org::start("", "0", false);

    let (err, answer) = org.dispatch(document("doc-1"));
    assert!(!err, "{answer}");
    assert_eq!(answer["task_id"], "doc-1");
    assert_eq!(answer["status"], "queued");
    assert_eq!(
        org.text("SELECT status FROM commands WHERE verb = 'dispatch'"),
        "accepted"
    );
    assert_eq!(
        org.count("SELECT count(*) FROM runs WHERE status = 'queued'"),
        1
    );
    assert_eq!(org.count("SELECT count(*) FROM run_launch"), 1);
    assert_eq!(org.text("SELECT brief FROM run_launch"), BRIEF);
    assert_eq!(org.text("SELECT agent FROM run_launch"), "claude_code");
    assert_eq!(org.text("SELECT kind FROM run_launch"), "document");
    assert_eq!(org.text("SELECT stage FROM run_launch"), "planned");
    assert_eq!(
        org.count("SELECT count(*) FROM events WHERE kind = 'delegate_sent'"),
        1
    );

    let (err, answer) = org
        .dispatch(json!({"kind": "code", "task_id": "code-1", "title": "Fix", "brief": "Fix it."}));
    assert!(err);
    assert_eq!(answer["kind"], "usage", "{answer}");

    let (err, answer) = org.dispatch(document("doc-1"));
    assert!(err);
    assert_eq!(answer["kind"], "conflict", "{answer}");
    assert_eq!(
        org.count(
            "SELECT count(*) FROM commands WHERE status = 'rejected' AND error_kind = 'conflict'"
        ),
        1
    );
    assert_eq!(org.count("SELECT count(*) FROM runs"), 1);
    assert_eq!(org.count("SELECT count(*) FROM run_launch"), 1);
}

/// A daemon pass takes the run, with the test agent asked for by the daemon's own environment.
#[test]
fn a_pass_takes_the_dispatched_run_with_the_test_agent_asked_for() {
    let org = Org::start("fake", "4", false);
    let (err, answer) = org.dispatch(document("doc-2"));
    assert!(!err, "{answer}");
    assert_eq!(org.text("SELECT agent FROM run_launch"), "fake");
    let until = Instant::now() + Duration::from_secs(15);
    while org.count(
        "SELECT count(*) FROM events WHERE kind = 'run_taken'
         AND json_extract(payload_json, '$.task_id') = 'doc-2'",
    ) == 0
    {
        assert!(Instant::now() < until, "the run was not taken");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        org.text(
            "SELECT json_extract(payload_json, '$.spawn_id') FROM events WHERE kind = 'run_taken'"
        ),
        org.text("SELECT spawn_id FROM run_launch")
    );
}

/// The person's command reads the brief file itself and sends its content over the socket.
/// This one needs a Unix socket.
#[test]
fn the_persons_command_sends_the_brief_files_content() {
    let org = Org::start("", "0", true);
    let brief = org.root.join("brief.md");
    std::fs::write(&brief, BRIEF).unwrap();
    let brief = brief.to_str().unwrap();
    let run = |kind: &str, task: &str, more: &[&str]| {
        let mut args = vec![
            "dispatch",
            "--kind",
            kind,
            "--task-id",
            task,
            "--title",
            "T",
            "--brief-file",
            brief,
        ];
        args.extend_from_slice(more);
        org.cli(&args)
    };
    let ok = run("document", "doc-3", &[]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(org.text("SELECT brief FROM run_launch"), BRIEF);
    assert_eq!(run("code", "code-3", &[]).status.code(), Some(2));
    assert_eq!(run("document", "doc-3", &[]).status.code(), Some(5));
    assert_eq!(org.count("SELECT count(*) FROM runs"), 1);
}
