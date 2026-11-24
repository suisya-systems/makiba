//! M6-1 (Issue 42): the secretary's commands.
//!
//! Three parts. The door's half runs in this process on a scratch `state.db` with the stand-in
//! base, through `Door::handle` with `tool` set, which is the handler the MCP server calls for a
//! tool: every verb of the secretary, `dispatch`'s rows, the role check, `peek` and the answer to a
//! prompt. `status` and the exit codes of spec section 3 run the real binary, `makiba daemon` on
//! the stand-in base, and the person's `makiba`. The last part compares the clap definition of
//! the person's command with the verb table of spec section 12, read from `docs/design/spec.md`.

mod common;

use clap::CommandFactory;
use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{Base, LaunchSpec};
use makiba::daemon::door::{Door, Response, issue_key, sha256_hex};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// What the worker's terminal shows on the stand-in base.
const SCREEN: &[&str] = &["first line of the worker", "Allow edit of src/x.rs? (y/n)"];
/// A prompt that is on the worker's screen, and one that is not.
const SHOWN_PROMPT: &str = "Allow edit of src/x.rs? (y/n)";
const GONE_PROMPT: &str = "Allow running rm? (y/n)";

/// The secretary's verbs, spec section 4.
const SECRETARY_VERBS: &[&str] = &[
    "dispatch", "close", "answer", "pr", "pending", "peek", "status", "send", "show", "renew",
    "inbox",
];

fn spec_md() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design/spec.md"))
        .unwrap()
}

// ---- the door in this process ------------------------------------------------------------------

struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    door: Door,
    secretary: String,
    worker: String,
}

impl Org {
    /// Run `t1` (`in_use`) with a worker key and a tagged terminal showing `SCREEN`; run `t3`
    /// (`queued`) with no terminal.
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        let w = StateWriter::open(&db).unwrap();
        let base = Arc::new(Stub::scripted(SCREEN));
        let mut worker = String::new();
        for (task, status) in [("t1", "in_use"), ("t3", "queued")] {
            let run = w
                .upsert_run(&RunUpsert {
                    task_id: task,
                    project_slug: "p",
                    status: Some(status),
                    ..Default::default()
                })
                .unwrap();
            if task == "t1" {
                worker = issue_key(&w, Role::Worker, "w-t1", Some(run), None).unwrap();
                base.launch(&LaunchSpec {
                    argv: vec!["sleep".into(), "60".into()],
                    cwd: "/".into(),
                    name: "w-t1".into(),
                    tags: BTreeMap::from([
                        ("makiba.role".to_string(), "worker".to_string()),
                        ("makiba.task_id".to_string(), "t1".to_string()),
                    ]),
                    env: BTreeMap::new(),
                    unset: vec![],
                })
                .unwrap();
            }
        }
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, base, "fake".into());
        Org {
            _tmp: tmp,
            db,
            door,
            secretary,
            worker,
        }
    }

    /// A tool call: what the MCP server does for `tools/call`.
    fn tool(&self, key: &str, verb: &str, args: Value) -> Response {
        self.door.handle(Some(key), verb, args, None, true)
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    fn count(&self, sql: &str, p: impl rusqlite::Params) -> i64 {
        self.db().query_row(sql, p, |r| r.get(0)).unwrap()
    }

    /// A pending entry of kind `prompt` for `t1` with this prompt text.
    fn prompt(&self, text: &str) -> i64 {
        let c = Connection::open(&self.db).unwrap();
        c.execute(
            "INSERT INTO pending (run_id, kind, question, prompt_text, prompt_sum)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'prompt', 'permission', ?1, 'sum')",
            [text],
        )
        .unwrap();
        c.last_insert_rowid()
    }
}

fn kind_of(r: &Response) -> Option<ErrorKind> {
    r.as_ref().err().map(|e| e.kind)
}

#[test]
fn each_verb_called_as_the_secretarys_tool_is_recorded_and_returns_a_result() {
    let org = Org::new();
    let s = org.secretary.as_str();
    let head = "a".repeat(40);
    let added = org
        .tool(
            s,
            "pending",
            json!({"action": "add", "task": "t1", "question": "Use the old API?"}),
        )
        .unwrap();
    let sent = org
        .tool(s, "send", json!({"to": "w-t1", "text": "hello"}))
        .unwrap();
    let calls: Vec<(&str, Value)> = vec![
        (
            "dispatch",
            json!({"kind": "document", "task_id": "d1", "title": "Notes", "brief": "Write notes."}),
        ),
        (
            "answer",
            json!({"id": added["id"], "answer": "yes", "note": "keep it short"}),
        ),
        (
            "pr",
            json!({"state": "opened", "repo": "o/n", "pr": 3, "head": head, "task": "t1"}),
        ),
        ("pending", json!({"action": "list"})),
        ("peek", json!({"task_id": "t1"})),
        ("status", json!({})),
        ("show", json!({"id": sent["id"]})),
        ("renew", json!({"handover": "The state of things."})),
        ("inbox", json!({})),
        ("close", json!({"task_id": "t3", "abandon": true})),
    ];
    let mut results = BTreeMap::new();
    for (verb, args) in calls {
        let r = org.tool(s, verb, args);
        results.insert(verb, r);
    }
    for verb in SECRETARY_VERBS {
        assert!(
            org.count(
                "SELECT COUNT(*) FROM commands WHERE actor = 'secretary' AND verb = ?1",
                [verb]
            ) >= 1,
            "{verb} is not recorded in commands"
        );
    }
    for (verb, r) in &results {
        match *verb {
            // Built in M6-5: accepted, and carried out by makibad.
            "renew" => {
                let ok = r.as_ref().ok();
                assert_eq!(ok, Some(&json!({"renewal": "accepted"})), "{r:?}");
            }
            _ => assert!(r.is_ok(), "{verb}: {r:?}"),
        }
    }
    // `dispatch`, `close` and `renew` return at `accepted`, the rest `done`.
    let status = |verb: &str| -> String {
        org.db()
            .query_row(
                "SELECT status FROM commands WHERE actor = 'secretary' AND verb = ?1
                 ORDER BY id DESC LIMIT 1",
                [verb],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(status("dispatch"), "accepted");
    assert_eq!(status("close"), "accepted");
    assert_eq!(status("renew"), "accepted");
    for verb in [
        "answer", "pr", "pending", "peek", "status", "send", "show", "inbox",
    ] {
        assert_eq!(status(verb), "done", "{verb}");
    }
    // What the results say.
    let peek = results["peek"].as_ref().unwrap();
    assert_eq!(peek["lines"], json!(SCREEN), "{peek}");
    let answered = results["answer"].as_ref().unwrap();
    assert_eq!(answered["status"], json!("answered"), "{answered}");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM messages WHERE sender = 'secretary' AND recipient = 'w-t1'
               AND body LIKE '%yes%keep it short%'",
            []
        ),
        1
    );
    let listed = results["pending"].as_ref().unwrap();
    assert_eq!(
        listed["entries"],
        json!([]),
        "the answered entry is not listed"
    );
    assert!(
        results["status"].as_ref().unwrap()["makibad"].is_object(),
        "{:?}",
        results["status"]
    );
}

#[test]
fn dispatch_creates_a_queued_run_its_launch_row_with_the_brief_and_kind_and_delegate_sent() {
    let org = Org::new();
    let brief = "Fix the parser.\nSecond line, \"quoted\".\n";
    let r = org
        .tool(
            &org.secretary,
            "dispatch",
            json!({"kind": "code", "task_id": "c1", "title": "Parser", "brief": brief,
                   "project": "p"}),
        )
        .unwrap();
    assert_eq!(r["status"], json!("queued"), "{r}");
    let (status, kind, launch_brief): (String, String, String) = org
        .db()
        .query_row(
            "SELECT r.status, l.kind, l.brief FROM runs r JOIN run_launch l ON l.run_id = r.id
             WHERE r.task_id = 'c1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), kind.as_str(), launch_brief.as_str()),
        ("queued", "code", brief)
    );
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM events WHERE kind = 'delegate_sent'
               AND json_extract(payload_json, '$.task_id') = 'c1' AND actor = 'secretary'",
            []
        ),
        1
    );
}

#[test]
fn dispatch_of_code_needs_a_project_and_a_document_does_not() {
    let org = Org::new();
    let r = org.tool(
        &org.secretary,
        "dispatch",
        json!({"kind": "code", "task_id": "c2", "title": "T", "brief": "B"}),
    );
    assert_eq!(kind_of(&r), Some(ErrorKind::Usage), "{r:?}");
    assert_eq!(
        org.count("SELECT COUNT(*) FROM runs WHERE task_id = 'c2'", []),
        0
    );
    assert_eq!(
        org.count("SELECT COUNT(*) FROM commands WHERE verb = 'dispatch'", []),
        0,
        "a usage error is not recorded"
    );
    let r = org.tool(
        &org.secretary,
        "dispatch",
        json!({"kind": "document", "task_id": "d2", "title": "T", "brief": "B"}),
    );
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM run_launch WHERE kind = 'document'
               AND run_id = (SELECT id FROM runs WHERE task_id = 'd2')",
            []
        ),
        1
    );
}

#[test]
fn a_workers_key_cannot_peek_or_dispatch() {
    let org = Org::new();
    let peek = org.tool(&org.worker, "peek", json!({"task_id": "t1"}));
    assert_eq!(kind_of(&peek), Some(ErrorKind::Denied), "{peek:?}");
    let dispatch = org.tool(
        &org.worker,
        "dispatch",
        json!({"kind": "document", "task_id": "d3", "title": "T", "brief": "B"}),
    );
    assert_eq!(kind_of(&dispatch), Some(ErrorKind::Denied), "{dispatch:?}");
    for verb in ["peek", "dispatch"] {
        assert_eq!(
            org.count(
                "SELECT COUNT(*) FROM commands WHERE actor = 'w-t1' AND verb = ?1
                   AND status = 'rejected' AND error_kind = 'denied'",
                [verb]
            ),
            1,
            "{verb}"
        );
    }
    assert_eq!(
        org.count("SELECT COUNT(*) FROM runs WHERE task_id = 'd3'", []),
        0
    );
}

#[test]
fn an_answer_to_a_prompt_reads_the_screen_and_types_nothing() {
    let org = Org::new();
    let gone = org.prompt(GONE_PROMPT);
    let r = org.tool(
        &org.secretary,
        "answer",
        json!({"id": gone, "answer": "yes"}),
    );
    assert_eq!(kind_of(&r), Some(ErrorKind::Stale), "{r:?}");
    let shown = org.prompt(SHOWN_PROMPT);
    let r = org.tool(
        &org.secretary,
        "answer",
        json!({"id": shown, "answer": "yes"}),
    );
    // M6-8: the shown text alone is not the prompt recorded (no blocked episode, no sum match).
    assert_eq!(kind_of(&r), Some(ErrorKind::Stale), "{r:?}");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM pending WHERE kind = 'prompt' AND status = 'open'",
            []
        ),
        2,
        "neither prompt is answered"
    );
    assert_eq!(
        org.count("SELECT COUNT(*) FROM messages", []),
        0,
        "nothing was sent"
    );
}

// ---- the real binary ---------------------------------------------------------------------------

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

const SECRETARY_TOKEN: &str = "5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5";

/// One organization run by `makiba daemon` on the stand-in base, stopped when dropped.
struct Daemon {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    socket: bool,
    child: Child,
}

impl Daemon {
    fn env(&self, c: &mut Command) {
        let root = &self.root;
        c.env("HOME", root.join("home"))
            .env("MAKIBA_STATE_DIR", root.join("state"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("MAKIBA_DOCS_DIR", root.join("docs"))
            .env("MAKIBA_WORKERS_DIR", root.join("workers"))
            .env("MAKIBA_PORT", self.port.to_string())
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
            .env("MAKIBA_MAX_CONCURRENT_WORKERS", "0");
        for v in [
            "MAKIBA_DOOR",
            "MAKIBA_PERSON_KEY",
            "MAKIBA_SHARE_DIR",
            "MAKIBA_LAUNCH_AGENT",
            "MAKIBA_FAKE_AGENT",
        ] {
            c.env_remove(v);
        }
        if !self.socket {
            c.env("MAKIBA_DOOR", "none");
        }
    }

    /// `socket`: whether the person's socket is opened (`MAKIBA_DOOR=none` otherwise; the
    /// agents' port is always open).
    fn start(socket: bool) -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["home", "state", "config", "data", "run", "xdg-state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // not an executable: the run folder, kept to its owner
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        // Queued, not launched: no pass takes it with no capacity, and it has no terminal.
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('secretary', 'secretary', ?1)",
                [sha256_hex(SECRETARY_TOKEN)],
            )
            .unwrap();
        drop(w);
        let mut d = Daemon {
            _tmp: tmp,
            root,
            port: free_port(),
            socket,
            child: Command::new("true").spawn().unwrap(),
        };
        let mut c = makibad::daemon(&d.root);
        c.with(|c| d.env(c));
        d.child = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let door = d.root.join("run/makiba/door.sock");
        let until = Instant::now() + Duration::from_secs(15);
        let mut open = false;
        while Instant::now() < until {
            let sock_ok = !socket || UnixStream::connect(&door).is_ok();
            if sock_ok && TcpStream::connect(("127.0.0.1", d.port)).is_ok() && d.heartbeat() {
                open = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(open, "the door did not open (socket: {socket})");
        d
    }

    fn db_path(&self) -> PathBuf {
        self.root.join("state/state.db")
    }

    fn heartbeat(&self) -> bool {
        Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| {
                c.query_row(
                    "SELECT heartbeat_at IS NOT NULL FROM dispatcher WHERE id = 1",
                    [],
                    |r| r.get::<_, bool>(0),
                )
            })
            .unwrap_or(false)
    }

    /// A tool call on the agents' port, as Claude Code makes it.
    fn tool(&self, token: &str, name: &str, args: Value) -> (bool, Value) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": name, "arguments": args}})
        .to_string();
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut all = String::new();
        s.read_to_string(&mut all).unwrap();
        let body = all.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        let v: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("{all:?}: {e}"));
        let result = &v["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool result: {v}"));
        (
            result["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }

    /// The person's `makiba`, with `extra` environment.
    fn cli(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(MAKIBA);
        self.env(&mut c);
        for (k, v) in extra {
            c.env(k, v);
        }
        c.args(args).stdin(Stdio::null()).output().unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn the_status_tool_goes_through_the_door_and_returns_the_persons_local_status() {
    // No socket: the tool reaches the door on the port; the person's status needs no door.
    let d = Daemon::start(false);
    let mut last = (Value::Null, Value::Null);
    for _ in 0..5 {
        let (is_error, tool) = d.tool(SECRETARY_TOKEN, "status", json!({}));
        assert!(!is_error, "{tool}");
        let out = d.cli(&["status", "--json"], &[]);
        assert!(out.status.success(), "{}", text(&out));
        let local: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("not JSON: {}: {e}", text(&out)));
        if tool == local {
            assert_eq!(tool["makibad"]["running"], json!(true), "{tool}");
            assert!(tool["makibad"]["pid"].is_i64(), "{tool}");
            // The tool's call is recorded; the person's local read is not a request.
            let c =
                Connection::open_with_flags(d.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let (tools, persons): (i64, i64) = c
                .query_row(
                    "SELECT SUM(actor = 'secretary'), SUM(actor = 'person') FROM commands
                     WHERE verb = 'status'",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                            r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        ))
                    },
                )
                .unwrap();
            assert!(tools >= 1);
            assert_eq!(persons, 0, "the person's status went through the door");
            return;
        }
        // A heartbeat was written between the two reads.
        last = (tool, local);
    }
    panic!("the tool and the local status differ: {:?}", last);
}

/// The exit codes of spec section 3, from the table whose first column is `kind`.
fn spec_exit_codes(doc: &str) -> BTreeSet<i32> {
    let s3 = section(doc, 3);
    let rows = table(s3, "`kind`");
    rows[1..]
        .iter()
        .map(|r| r[1].trim().parse().unwrap_or_else(|_| panic!("{r:?}")))
        .collect()
}

/// One run of the person's `makiba`: the exit code wanted, the arguments, extra environment.
type Case<'a> = (i32, Vec<&'a str>, Vec<(&'a str, &'a str)>);

#[test]
fn every_exit_code_of_spec_section_3_is_produced() {
    let d = Daemon::start(true);
    // A prompt for t1, whose worker has no terminal: its screen is gone.
    let prompt: i64 = {
        let c = Connection::open(d.db_path()).unwrap();
        c.execute(
            "INSERT INTO pending (run_id, kind, question, prompt_text, prompt_sum)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'prompt', 'permission', ?1, 'sum')",
            [GONE_PROMPT],
        )
        .unwrap();
        c.last_insert_rowid()
    };
    let file = d.root.join("text.md");
    std::fs::write(&file, "Some text.\n").unwrap();
    let file = file.to_str().unwrap().to_string();
    let bogus = d.root.join("bogus.key");
    std::fs::write(&bogus, "f".repeat(64)).unwrap();
    let bogus = bogus.to_str().unwrap().to_string();
    let prompt = prompt.to_string();
    let cases: Vec<Case> = vec![
        (0, vec!["pending", "list"], vec![]),
        (
            2,
            vec![
                "dispatch",
                "--kind",
                "code",
                "--task-id",
                "c9",
                "--title",
                "T",
                "--brief-file",
                &file,
            ],
            vec![],
        ),
        (3, vec!["peek", "nosuch"], vec![]),
        (
            4,
            vec!["pending", "list"],
            vec![("MAKIBA_PERSON_KEY", bogus.as_str())],
        ),
        (5, vec!["close", "t1"], vec![]),
        (6, vec!["pending", "list"], vec![("MAKIBA_DOOR", "none")]),
        (7, vec!["answer", &prompt, "yes"], vec![]),
        (
            70,
            vec!["config", "set", "max_concurrent_workers", "3"],
            vec![],
        ),
        (0, vec!["renew", "--handover-file", &file], vec![]),
    ];
    let mut seen = BTreeSet::new();
    for (want, args, extra) in &cases {
        let out = d.cli(args, extra);
        assert_eq!(
            out.status.code(),
            Some(*want),
            "makiba {args:?}: {}",
            text(&out)
        );
        seen.insert(*want);
    }
    assert_eq!(seen, spec_exit_codes(&spec_md()));
    // The usage error did not reach the door; the config set did and failed; the renew was
    // accepted (exit 0) and recorded, and makibad may have carried it on since.
    let c = Connection::open_with_flags(d.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(n("SELECT COUNT(*) FROM runs WHERE task_id = 'c9'"), 0);
    assert_eq!(
        n(
            "SELECT COUNT(*) FROM commands WHERE actor = 'person' AND verb = 'config'
             AND status = 'failed' AND error_kind = 'internal'"
        ),
        1
    );
    assert_eq!(
        n(
            "SELECT COUNT(*) FROM commands WHERE actor = 'person' AND verb = 'renew'
             AND status != 'rejected'"
        ),
        1
    );
}

// ---- the clap definition and spec section 12 -----------------------------------------------------

fn section(doc: &str, n: u32) -> &str {
    let head = format!("\n## {n}. ");
    let start = doc.find(&head).unwrap_or_else(|| panic!("no section {n}"));
    let rest = &doc[start + 1..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |e| e + 3);
    &rest[..end]
}

/// The table whose header row's first cell is `first`: its rows, each a list of cells, with `\|`
/// inside a cell kept as `|`.
fn table(section: &str, first: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut inside = false;
    for line in section.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            if inside {
                break;
            }
            continue;
        }
        let cells: Vec<String> = line
            .replace("\\|", "\u{1}")
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().replace('\u{1}', "|"))
            .collect();
        if !inside {
            if cells[0] != first {
                continue;
            }
            inside = true;
        }
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ' '))
        {
            continue;
        }
        rows.push(cells);
    }
    assert!(!rows.is_empty(), "no table headed {first}");
    rows
}

/// The text of each code span of a cell.
fn spans(cell: &str) -> Vec<String> {
    cell.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect()
}

/// One argument of a verb: its key (a positional's place and name or choices, a flag's name and
/// choices) and whether it is required.
type Args = BTreeMap<String, bool>;

fn tokens(form: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut angle = false;
    for c in form.chars() {
        match c {
            '<' => {
                angle = true;
                cur.push(c);
            }
            '>' => {
                angle = false;
                cur.push(c);
            }
            '[' | ']' if !angle => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            }
            c if c.is_whitespace() && !angle => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn choices(t: &str) -> String {
    let mut c: Vec<&str> = t.split('|').collect();
    c.sort();
    c.join(",")
}

fn norm(name: &str) -> String {
    name.trim_matches(['<', '>'])
        .replace([' ', '-'], "_")
        .to_lowercase()
}

/// The arguments one form of the table names, e.g. `<task_id> [--abandon]`.
fn doc_form(form: &str) -> Args {
    let t = tokens(form);
    let mut out = Args::new();
    let (mut optional, mut pos, mut i) = (false, 0, 0);
    while i < t.len() {
        let tok = t[i].as_str();
        match tok {
            "[" => optional = true,
            "]" => optional = false,
            _ if tok.starts_with("--") => {
                let mut ch = String::new();
                if let Some(next) = t.get(i + 1)
                    && !["[", "]"].contains(&next.as_str())
                    && !next.starts_with("--")
                {
                    if next.contains('|') && !next.starts_with('<') {
                        ch = choices(next);
                    }
                    i += 1;
                }
                out.insert(format!("flag {} [{ch}]", &tok[2..]), !optional);
            }
            _ if tok.starts_with('<') => {
                out.insert(format!("pos{pos} {}", norm(tok)), !optional);
                pos += 1;
            }
            _ if tok.contains('|') => {
                out.insert(format!("pos{pos} one of [{}]", choices(tok)), !optional);
                pos += 1;
            }
            other => panic!("cannot read {other:?} in {form:?}"),
        }
        i += 1;
    }
    out
}

/// Every verb of the table the person runs, with its arguments. A cell with two forms (`send`)
/// names an argument as required only when both require it.
fn doc_verbs(doc: &str) -> BTreeMap<String, Args> {
    let mut verbs = BTreeMap::new();
    for row in &table(section(doc, 12), "Verb")[1..] {
        let Some(verb) = spans(&row[0]).into_iter().next() else {
            continue; // `(none)`: the inbox, which has no command.
        };
        let forms: Vec<Args> = spans(&row[1]).iter().map(|f| doc_form(f)).collect();
        let mut args = Args::new();
        for f in &forms {
            for (k, req) in f {
                let all = forms.iter().all(|g| g.get(k) == Some(&true));
                args.insert(k.clone(), *req && all);
            }
        }
        verbs.insert(verb, args);
    }
    verbs
}

/// The same, read from the clap definition through `CommandFactory`.
fn clap_verbs() -> BTreeMap<String, Args> {
    let cmd = makiba::cli::Makiba::command();
    let mut verbs = BTreeMap::new();
    for sub in cmd.get_subcommands() {
        let mut args = Args::new();
        let mut pos = 0;
        for a in sub.get_arguments() {
            let id = a.get_id().as_str();
            if id == "help" || id == "version" {
                continue;
            }
            let ch: Vec<String> = if a.get_action().takes_values() {
                a.get_possible_values()
                    .iter()
                    .map(|v| v.get_name().to_string())
                    .collect()
            } else {
                Vec::new()
            };
            let ch = choices(&ch.join("|"));
            let key = if a.is_positional() {
                let k = if ch.is_empty() {
                    format!("pos{pos} {}", norm(id))
                } else {
                    format!("pos{pos} one of [{ch}]")
                };
                pos += 1;
                k
            } else {
                format!(
                    "flag {} [{ch}]",
                    a.get_long()
                        .unwrap_or_else(|| panic!("{id} has no long name"))
                )
            };
            args.insert(key, a.is_required_set());
        }
        verbs.insert(sub.get_name().to_string(), args);
    }
    verbs
}

/// What is on one side only, or required on one side only.
fn differences(doc: &BTreeMap<String, Args>, code: &BTreeMap<String, Args>) -> Vec<String> {
    let mut out = Vec::new();
    for v in doc.keys().chain(code.keys()).collect::<BTreeSet<_>>() {
        match (doc.get(v), code.get(v)) {
            (Some(_), None) => out.push(format!("verb {v}: in spec section 12 only")),
            (None, Some(_)) => out.push(format!("verb {v}: in the clap definition only")),
            (Some(d), Some(c)) => {
                for k in d.keys().chain(c.keys()).collect::<BTreeSet<_>>() {
                    match (d.get(k), c.get(k)) {
                        (Some(a), Some(b)) if a == b => {}
                        (a, b) => {
                            out.push(format!("{v} {k}: spec required={a:?}, clap required={b:?}"))
                        }
                    }
                }
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

#[test]
fn the_clap_definition_and_the_verb_table_of_spec_section_12_name_the_same_verbs_and_arguments() {
    let doc = doc_verbs(&spec_md());
    // The table is read: the person's verbs, with arguments.
    for v in [
        "up", "down", "status", "dispatch", "send", "pending", "answer", "config",
    ] {
        assert!(doc.contains_key(v), "{v} not read from the table: {doc:?}");
    }
    assert!(!doc.contains_key("inbox"));
    assert!(
        doc["dispatch"].contains_key("flag kind [code,document]"),
        "{doc:?}"
    );
    let diff = differences(&doc, &clap_verbs());
    assert!(diff.is_empty(), "{diff:#?}");
}

#[test]
fn a_verb_or_an_argument_on_one_side_only_fails_the_comparison() {
    let md = spec_md();
    let code = clap_verbs();
    let mutate = |from: &str, to: &str| -> Vec<String> {
        assert!(md.contains(from), "{from}");
        differences(&doc_verbs(&md.replacen(from, to, 1)), &code)
    };
    // A verb the table lacks.
    let peek_row = md
        .lines()
        .find(|l| l.starts_with("| `peek` |"))
        .unwrap()
        .to_string();
    let d = mutate(&format!("{peek_row}\n"), "");
    assert!(
        d.iter()
            .any(|l| l.contains("peek: in the clap definition only")),
        "{d:?}"
    );
    // A verb the definition lacks.
    let d = mutate(
        &format!("{peek_row}\n"),
        &format!("{peek_row}\n| `ghost` | `<x>` | Nothing. | none |\n"),
    );
    assert!(
        d.iter()
            .any(|l| l.contains("ghost: in spec section 12 only")),
        "{d:?}"
    );
    // An argument dropped, a choice changed, a requirement changed.
    let d = mutate("`<task_id> [--abandon]`", "`<task_id>`");
    assert!(d.iter().any(|l| l.contains("close flag abandon")), "{d:?}");
    let d = mutate("`<pending id> yes\\|no", "`<pending id> yes\\|maybe");
    assert!(d.iter().any(|l| l.starts_with("answer pos1")), "{d:?}");
    let d = mutate(
        "`start\\|done\\|blocked --task <task_id> <text>`",
        "`start\\|done\\|blocked [--task <task_id>] <text>`",
    );
    assert!(d.iter().any(|l| l.contains("report flag task")), "{d:?}");
}
