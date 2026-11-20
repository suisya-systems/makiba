//! M3-3: the door (spec sections 2 to 4 and 12). The tests run the real binary, `makiba daemon` on
//! the stand-in base, in a scratch folder that is also its `HOME` and its XDG folders, so the
//! default places of the person's socket and the person key are the ones used. Requests reach it
//! by the socket (the person's entrance), by the MCP server on the port (the agents' entrance),
//! and by `makiba` itself. Before the start the test writes the runs and the agents' rows, with
//! keys it made and their SHA-256 (from `sha256sum`), as a launch would.

mod common;

use common::local::Local;
use makiba::base::{Base, LaunchSpec};
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const FAKE: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

const SECRETARY_KEY: &str = "5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5";
const WORKER_KEY: &str = "0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a";
const OTHER_WORKER_KEY: &str = "0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b0d2b";
const CURATOR_KEY: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
const UNKNOWN_KEY: &str = "badbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadb";

/// Every verb of spec section 12, the person's and the agents'.
const VERBS: &[&str] = &[
    "up", "down", "status", "dispatch", "close", "peek", "send", "report", "show", "pending",
    "answer", "pr", "renew", "config", "inbox", "note",
];

fn sha256(text: &str) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sha256sum runs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()[..64].to_string()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One organization: its scratch folder and its running `makibad`, stopped when dropped.
struct Org {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    daemon: Child,
}

impl Org {
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    fn state(&self) -> PathBuf {
        self.root.join("state")
    }
    fn db_path(&self) -> PathBuf {
        self.state().join("state.db")
    }
    fn config_home(&self) -> PathBuf {
        self.root.join("config")
    }
    fn data_home(&self) -> PathBuf {
        self.root.join("data")
    }
    fn runtime(&self) -> PathBuf {
        self.root.join("run")
    }
    /// The default of `$MAKIBA_DOOR` (spec section 2).
    fn door(&self) -> PathBuf {
        self.runtime().join("makiba/door.sock")
    }
    /// The default of `$MAKIBA_PERSON_KEY` (spec section 2).
    fn key_file(&self) -> PathBuf {
        self.config_home().join("makiba/person.key")
    }
    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// The environment of every `makiba` this test runs: the scratch folder, nothing inherited.
    fn env(&self, c: &mut Command) {
        c.env("HOME", self.home())
            .env("MAKIBA_STATE_DIR", self.state())
            .env("XDG_STATE_HOME", self.root.join("xdg-state"))
            .env("XDG_CONFIG_HOME", self.config_home())
            .env("XDG_DATA_HOME", self.data_home())
            .env("XDG_RUNTIME_DIR", self.runtime())
            .env("MAKIBA_PORT", self.port.to_string())
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0");
        for v in [
            "MAKIBA_DOOR",
            "MAKIBA_PERSON_KEY",
            "MAKIBA_SHARE_DIR",
            "MAKIBA_WORKERS_DIR",
            "MAKIBA_DOCS_DIR",
        ] {
            c.env_remove(v);
        }
    }

    fn start() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["home", "state", "config", "data", "run", "xdg-state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        // What a dispatch and a launch would have written: two runs and the agents' keys.
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        for task in ["t1", "t2"] {
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                // Dispatched, not launched: an `in_use` run with no terminal is lost (M2-5).
                status: Some("queued"),
                ..Default::default()
            })
            .unwrap();
        }
        for (agent, role, key) in [
            ("secretary", "secretary", SECRETARY_KEY),
            ("w-t1", "worker", WORKER_KEY),
            ("w-t2", "worker", OTHER_WORKER_KEY),
            ("curator", "curator", CURATOR_KEY),
        ] {
            w.conn()
                .execute(
                    "INSERT INTO agents (agent_id, role, key_sha256) VALUES (?1, ?2, ?3)",
                    rusqlite::params![agent, role, sha256(key)],
                )
                .unwrap();
        }
        drop(w);
        let mut org = Org {
            _tmp: tmp,
            root,
            port: free_port(),
            daemon: Command::new("true").spawn().unwrap(),
        };
        let mut c = Command::new(MAKIBA);
        org.env(&mut c);
        org.daemon = c
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(15);
        let mut open = false;
        while Instant::now() < until {
            if UnixStream::connect(org.door()).is_ok() && TcpStream::connect(org.addr()).is_ok() {
                open = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            open,
            "the door did not open: no socket at {} or no server on {}",
            org.door().display(),
            org.addr()
        );
        org
    }

    /// One request on the person's socket: one JSON line in, one out.
    fn sock(&self, req: &Value) -> Value {
        let mut s = UnixStream::connect(self.door()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(s, "{req}").unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON: {line:?}: {e}"))
    }

    /// One JSON-RPC request to the MCP server, with the token as Claude Code sends it.
    fn mcp(&self, token: Option<&str>, method: &str, params: Value) -> Value {
        let body =
            json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}).to_string();
        let auth = token
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let mut s = TcpStream::connect(self.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr(),
            body.len()
        )
        .unwrap();
        let mut all = String::new();
        s.read_to_string(&mut all).unwrap();
        let body = all.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON: {all:?}: {e}"))
    }

    /// A tool call: whether it is a tool error, and the JSON of its text.
    fn call(&self, token: Option<&str>, tool: &str, args: Value) -> (bool, Value) {
        let r = self.mcp(
            token,
            "tools/call",
            json!({"name": tool, "arguments": args}),
        );
        let result = &r["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool result: {r}"));
        (
            result["isError"] == json!(true),
            serde_json::from_str(text).unwrap(),
        )
    }

    fn tools(&self, token: &str) -> BTreeSet<String> {
        let r = self.mcp(Some(token), "tools/list", json!({}));
        r["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("no tool list: {r}"))
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    fn count(&self, sql: &str, p: impl rusqlite::Params) -> i64 {
        self.db().query_row(sql, p, |r| r.get(0)).unwrap()
    }

    /// Rows of `commands` for `verb` recorded as `rejected` with the kind `denied`.
    fn denied_rows(&self, verb: &str) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM commands WHERE verb = ?1 AND status = 'rejected' AND error_kind = 'denied'",
            [verb],
        )
    }

    fn person_key(&self) -> String {
        std::fs::read_to_string(self.key_file())
            .unwrap()
            .trim()
            .to_string()
    }

    /// The person's `makiba`, run from this test process (outside every launched terminal).
    fn cli(&self, args: &[&str]) -> Output {
        let mut c = Command::new(MAKIBA);
        self.env(&mut c);
        c.args(args).stdin(Stdio::null()).output().unwrap()
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.daemon.id().to_string()])
            .status();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.daemon.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// Ends a terminal when dropped.
struct Ended<'a>(&'a Local, makiba::base::TerminalRef);

impl Drop for Ended<'_> {
    fn drop(&mut self) {
        let _ = self.0.end(&self.1);
    }
}

fn kind(answer: &Value) -> &str {
    answer["error"]["kind"].as_str().unwrap_or("")
}

#[test]
fn an_unknown_key_is_denied_and_recorded_on_the_socket_and_on_a_tool_call() {
    let org = Org::start();
    let args = json!({"to": "secretary", "text": "from nobody"});

    let a = org.sock(&json!({"v": 1, "key": UNKNOWN_KEY, "verb": "send", "args": args}));
    assert_eq!(a["ok"], json!(false), "{a}");
    assert_eq!(kind(&a), "denied", "{a}");
    assert_eq!(
        org.denied_rows("send"),
        1,
        "the socket request is recorded as rejected"
    );

    let (is_error, e) = org.call(Some(UNKNOWN_KEY), "send", args);
    assert!(is_error, "{e}");
    assert_eq!(e["kind"], json!("denied"), "{e}");
    assert_eq!(
        org.denied_rows("send"),
        2,
        "the tool call is recorded as rejected"
    );

    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM messages WHERE body = 'from nobody'",
            []
        ),
        0
    );
}

#[test]
fn a_worker_may_report_and_may_not_dispatch() {
    let org = Org::start();
    let tools = org.tools(WORKER_KEY);
    assert!(tools.contains("report"), "{tools:?}");
    assert!(!tools.contains("dispatch"), "{tools:?}");

    let (is_error, r) = org.call(
        Some(WORKER_KEY),
        "report",
        json!({"state": "done", "task": "t1", "text": "t1 is ready for review"}),
    );
    assert!(!is_error, "{r}");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM messages m JOIN runs r ON r.id = m.run_id
             WHERE r.task_id = 't1' AND m.kind = 'report_done' AND m.sender = 'w-t1'
               AND m.recipient = 'secretary' AND m.body = 't1 is ready for review'",
            []
        ),
        1
    );
    let status: String = org
        .db()
        .query_row("SELECT status FROM runs WHERE task_id = 't1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "review", "report done sets review");
    // The report is queued for the secretary: its box returns it.
    let (is_error, inbox) = org.call(Some(SECRETARY_KEY), "inbox", json!({}));
    assert!(!is_error, "{inbox}");
    assert!(
        inbox.to_string().contains("t1 is ready for review"),
        "{inbox}"
    );

    let (is_error, d) = org.call(
        Some(WORKER_KEY),
        "dispatch",
        json!({"kind": "code", "task_id": "t9", "title": "x", "brief": "x", "project": "p"}),
    );
    assert!(is_error, "{d}");
    assert_eq!(d["kind"], json!("denied"), "{d}");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM commands WHERE actor = 'w-t1' AND verb = 'dispatch'
               AND status = 'rejected' AND error_kind = 'denied'",
            []
        ),
        1
    );
    assert_eq!(
        org.count("SELECT COUNT(*) FROM runs WHERE task_id = 't9'", []),
        0
    );
}

#[test]
fn each_role_is_listed_exactly_the_tools_of_its_verbs() {
    let org = Org::start();
    let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
    // Spec section 4, the table "Verbs allowed".
    assert_eq!(
        org.tools(SECRETARY_KEY),
        set(&[
            "dispatch", "close", "answer", "pr", "pending", "peek", "status", "send", "show",
            "renew", "inbox", "note"
        ])
    );
    let limited = set(&["report", "send", "show", "inbox"]);
    assert_eq!(org.tools(WORKER_KEY), limited);
    assert_eq!(org.tools(CURATOR_KEY), limited);
}

#[test]
fn a_worker_and_the_curator_may_send_to_the_secretary_only() {
    let org = Org::start();
    for (key, actor, to) in [
        (WORKER_KEY, "w-t1", "w-t2"),
        (CURATOR_KEY, "curator", "w-t1"),
    ] {
        let text = format!("{actor} to {to}");
        let (is_error, e) = org.call(Some(key), "send", json!({"to": to, "text": text}));
        assert!(is_error, "{e}");
        assert_eq!(e["kind"], json!("denied"), "{e}");
        assert_eq!(
            org.count(
                "SELECT COUNT(*) FROM commands WHERE actor = ?1 AND verb = 'send'
                   AND status = 'rejected' AND error_kind = 'denied'",
                [actor]
            ),
            1
        );
        assert_eq!(
            org.count("SELECT COUNT(*) FROM messages WHERE body = ?1", [&text]),
            0
        );
    }
}

#[test]
fn send_from_a_process_that_cannot_read_the_database_still_stores_the_message() {
    let org = Org::start();
    let db = org.db_path();
    let before = std::fs::metadata(&db).unwrap().permissions();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).unwrap();
    // The file really is unreadable to a process of this user.
    let readable = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|c| c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0)))
        .is_ok();
    let out = org.cli(&["send", "secretary", "sent without the ledger"]);
    std::fs::set_permissions(&db, before).unwrap();
    assert!(
        !readable,
        "state.db is still readable: the test proves nothing"
    );
    assert!(
        out.status.success(),
        "makiba send failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM messages WHERE sender = 'person' AND recipient = 'secretary'
               AND body = 'sent without the ledger'",
            []
        ),
        1
    );
}

#[test]
fn the_sender_is_the_id_bound_to_the_key() {
    let org = Org::start();
    let (is_error, r) = org.call(
        Some(WORKER_KEY),
        "send",
        json!({"to": "secretary", "text": "who am I", "from": "secretary", "sender": "secretary"}),
    );
    assert!(!is_error, "{r}");
    let sender: String = org
        .db()
        .query_row(
            "SELECT sender FROM messages WHERE body = 'who am I'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sender, "w-t1");

    let a = org.sock(&json!({"v": 1, "key": org.person_key(), "verb": "send",
        "args": {"to": "secretary", "text": "the person claims a worker", "from": "w-t1", "sender": "w-t1"}}));
    assert_eq!(a["ok"], json!(true), "{a}");
    let sender: String = org
        .db()
        .query_row(
            "SELECT sender FROM messages WHERE body = 'the person claims a worker'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sender, "person");
}

#[test]
fn the_same_idem_twice_creates_one_message() {
    let org = Org::start();
    let args = json!({"to": "secretary", "text": "once by tool", "idem": "idem-1"});
    let first = org.call(Some(WORKER_KEY), "send", args.clone());
    let second = org.call(Some(WORKER_KEY), "send", args);
    assert!(!first.0, "{}", first.1);
    assert_eq!(first, second, "the first answer is returned again");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM messages WHERE body = 'once by tool'",
            []
        ),
        1
    );

    let req = json!({"v": 1, "key": org.person_key(), "verb": "send",
        "args": {"to": "secretary", "text": "once by socket"}, "idem": "idem-2"});
    let first = org.sock(&req);
    let second = org.sock(&req);
    assert_eq!(first["ok"], json!(true), "{first}");
    assert_eq!(first, second, "the first answer is returned again");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM messages WHERE body = 'once by socket'",
            []
        ),
        1
    );
}

#[test]
fn no_key_from_inside_a_launched_terminal_is_denied_for_every_verb_even_detached() {
    let org = Org::start();
    let record = org.root.join("agent.record");
    let door = org.door().display().to_string();
    let addr = org.addr();
    let mut argv = vec![
        FAKE.to_string(),
        "--record".into(),
        record.display().to_string(),
    ];
    for verb in VERBS {
        argv.push(format!("door:{verb}:{door}"));
        argv.push(format!("port:{verb}:{addr}"));
        argv.push(format!("detach:door:{verb}:{door}"));
        argv.push(format!("detach:port:{verb}:{addr}"));
    }
    // Not `hang`: the terminal ends by itself even if nothing kills it.
    argv.push("sleep:120000".into());
    let org_tag = std::fs::canonicalize(org.state()).unwrap();
    let tags = BTreeMap::from([
        ("makiba.role".to_string(), "worker".to_string()),
        ("makiba.task_id".to_string(), "t1".to_string()),
        ("makiba.spawn".to_string(), "spawn-door-test".to_string()),
        ("makiba.org".to_string(), org_tag.display().to_string()),
    ]);
    // A terminal as makibad launches one: the worker's name and tags, no key anywhere.
    let base = Local::default();
    let terminal = base
        .launch(&LaunchSpec {
            argv,
            cwd: org.root.clone(),
            name: "w-t1".into(),
            tags,
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
    // The terminal is ended when the test ends, passed or failed.
    let _ended = Ended(&base, terminal);

    let want = VERBS.len() * 4;
    let until = Instant::now() + Duration::from_secs(60);
    let mut rows: Vec<String> = Vec::new();
    while Instant::now() < until {
        rows = std::fs::read_to_string(&record)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        if rows.len() >= want {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(rows.len(), want, "the test agent's record: {rows:#?}");

    let mut detached = 0;
    for row in &rows {
        let mut rest = row.as_str();
        if let Some(r) = rest.strip_prefix("detached ") {
            detached += 1;
            let fields: BTreeMap<&str, &str> = r
                .split(' ')
                .take(5)
                .filter_map(|f| f.split_once('='))
                .collect();
            let (ppid, sid, pid, agent, hop) = (
                fields["ppid"],
                fields["sid"],
                fields["pid"],
                fields["agent"],
                fields["hop"],
            );
            assert_ne!(
                ppid, agent,
                "the caller's parent is still the terminal: {row}"
            );
            assert_ne!(ppid, hop, "the caller was not re-parented: {row}");
            assert_eq!(
                sid, pid,
                "the caller did not start a session of its own: {row}"
            );
            rest = r.splitn(6, ' ').nth(5).unwrap();
        }
        let (how, rest) = rest.split_once(' ').unwrap();
        let (verb, answer) = rest.split_once(' ').unwrap();
        let answer: Value = serde_json::from_str(answer).unwrap_or_else(|e| panic!("{row}: {e}"));
        match how {
            "door" => {
                assert_eq!(answer["ok"], json!(false), "{row}");
                assert_eq!(kind(&answer), "denied", "{row}");
            }
            "port" => {
                let result = &answer["result"];
                assert_eq!(result["isError"], json!(true), "{row}");
                let e: Value =
                    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
                assert_eq!(e["kind"], json!("denied"), "{row}");
            }
            other => panic!("unexpected record {other}: {row}"),
        }
        assert!(VERBS.contains(&verb), "{row}");
    }
    assert_eq!(detached, VERBS.len() * 2);
    for verb in VERBS {
        assert_eq!(
            org.denied_rows(verb),
            4,
            "{verb}: socket and port, attached and detached, each recorded as rejected"
        );
    }
}

#[test]
fn the_person_key_from_outside_every_terminal_may_use_config_and_a_worker_key_may_not() {
    let org = Org::start();
    let args = json!({"action": "get", "key": "conversation_language"});
    let a = org.sock(&json!({"v": 1, "key": org.person_key(), "verb": "config", "args": args}));
    assert_eq!(a["ok"], json!(true), "{a}");
    let b = org.sock(&json!({"v": 1, "key": WORKER_KEY, "verb": "config", "args": args}));
    assert_eq!(b["ok"], json!(false), "{b}");
    assert_eq!(kind(&b), "denied", "{b}");
    assert_eq!(
        org.count(
            "SELECT COUNT(*) FROM commands WHERE actor = 'w-t1' AND verb = 'config'
               AND status = 'rejected' AND error_kind = 'denied'",
            []
        ),
        1
    );
}

#[test]
fn the_person_key_file_is_private_and_the_database_holds_only_its_hash() {
    let org = Org::start();
    let file = org.key_file();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600, "{}", file.display());
    assert_eq!(mode(file.parent().unwrap()), 0o700);
    // Not under a worktree or a working folder, nor under the role files (spec section 2).
    for not_under in [
        org.data_home().join("makiba"),
        org.home().join("makiba-workers"),
        org.home().join("makiba-docs"),
    ] {
        assert!(
            !file.starts_with(&not_under),
            "{} is under {}",
            file.display(),
            not_under.display()
        );
    }

    let key = org.person_key();
    assert!(key.len() >= 32, "a key of {} characters", key.len());
    let hashes: Vec<String> = org
        .db()
        .prepare("SELECT key_sha256 FROM agents WHERE role = 'person' AND revoked_at IS NULL")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(hashes, vec![sha256(&key)]);
    for f in ["state.db", "state.db-wal"] {
        let bytes = std::fs::read(org.state().join(f)).unwrap_or_default();
        assert!(
            !bytes.windows(key.len()).any(|w| w == key.as_bytes()),
            "{f} holds the person key itself"
        );
    }
}

/// The module files that the person's command reaches from `src/cli.rs`, through every
/// `crate::<module>` it names, without entering `status_local`.
fn person_command_modules(src: &Path) -> (Vec<PathBuf>, Vec<(PathBuf, String)>) {
    fn file_of(src: &Path, segs: &[&str]) -> Option<PathBuf> {
        for n in (1..=segs.len()).rev() {
            let base = segs[..n].iter().collect::<PathBuf>();
            for f in [
                src.join(&base).with_extension("rs"),
                src.join(&base).join("mod.rs"),
            ] {
                if f.is_file() {
                    return Some(f);
                }
            }
        }
        None
    }
    let ident = |s: &str| {
        s.chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
            .collect::<String>()
    };
    let mut seen = vec![src.join("cli.rs")];
    let mut uses = Vec::new();
    let mut i = 0;
    while i < seen.len() {
        let text = std::fs::read_to_string(&seen[i]).unwrap();
        for part in text.split("crate::").skip(1) {
            let mut paths = Vec::new();
            if let Some(group) = part.strip_prefix('{') {
                let group = &group[..group.find('}').unwrap_or(group.len())];
                paths.extend(group.split(',').map(|p| ident(p.trim())));
            } else {
                paths.push(ident(part));
            }
            for p in paths {
                let segs: Vec<&str> = p.split("::").filter(|s| !s.is_empty()).collect();
                if segs.is_empty() || segs[0] == "status_local" {
                    continue;
                }
                uses.push((seen[i].clone(), p.clone()));
                if let Some(f) = file_of(src, &segs)
                    && !seen.contains(&f)
                {
                    seen.push(f);
                }
            }
        }
        i += 1;
    }
    (seen, uses)
}

#[test]
fn the_persons_command_does_not_use_the_database_and_the_daemon_does() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(src.join("cli.rs").is_file(), "src/cli.rs does not exist");
    let (files, uses) = person_command_modules(&src);
    for (file, path) in &uses {
        assert!(
            path != "db" && !path.starts_with("db::"),
            "{} uses crate::{path}",
            file.display()
        );
    }
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        assert!(!text.contains("rusqlite"), "{} uses rusqlite", f.display());
    }
    let door = std::fs::read_to_string(src.join("daemon/door.rs")).unwrap_or_default();
    assert!(
        door.contains("crate::db"),
        "the daemon's door does not use crate::db"
    );
}
