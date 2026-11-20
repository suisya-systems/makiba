//! Issue 189: the verb `note`, by which the secretary saves a raw note through `makibad`.
//!
//! It runs the real binary, `makiba daemon` on the stand-in base with `MAKIBA_DOOR=none` (no Unix
//! socket is needed), and calls the tool over the MCP port with the agents' tokens. Everything
//! the test writes to `state.db` is written before `makibad` starts; afterwards the test only
//! reads it, so it never races `makibad`'s writes.

use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const SECRETARY_KEY: &str = "5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5";
const WORKER_KEY: &str = "0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a";

const NOTE: &str = "## Facts\nThe port was taken.\n\n## Judgment\nPick a free port.\n\n## Grounds\nThe test failed twice.\n\n## Where it applies\nTests that start makibad.\n";

fn sha256(text: &str) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
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

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_db(db: &Path) -> Connection {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// One tool call on the port with a bearer token: whether it is a tool error, and the JSON of its
/// text.
fn call(addr: &str, token: &str, tool: &str, args: Value) -> (bool, Value) {
    let body = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": tool, "arguments": args}})
    .to_string();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    let body = all.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let r: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON: {all:?}: {e}"));
    let result = &r["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool result: {r}"));
    (
        result["isError"] == json!(true),
        serde_json::from_str(text).unwrap(),
    )
}

/// The notes in `<knowledge>/raw/` as the curator counts them: regular files, no dot names.
fn notes(knowledge: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(knowledge.join("raw")) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| format!("raw/{}", e.file_name().to_string_lossy()))
        .filter(|n| !n["raw/".len()..].starts_with('.'))
        .collect()
}

/// `raw/YYYY-MM-DD-<rest>`
fn dated(file: &str, rest: &str) -> bool {
    let Some(name) = file.strip_prefix("raw/") else {
        return false;
    };
    let b = name.as_bytes();
    name.len() == 11 + rest.len()
        && name.ends_with(rest)
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|i| b[*i].is_ascii_digit())
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'-'
}

#[test]
fn the_secretary_saves_a_raw_note_once_per_key_and_nobody_else_can() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for d in ["home", "state", "config", "data", "run", "xdg-state"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // No `raw/` yet: makibad makes it.
    let knowledge = root.join("knowledge");
    std::fs::create_dir_all(&knowledge).unwrap();
    let db = root.join("state/state.db");
    let w = StateWriter::open(&db).unwrap();
    let run = w
        .upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
    for (agent, role, key, run) in [
        ("secretary", "secretary", SECRETARY_KEY, None),
        ("w-t1", "worker", WORKER_KEY, Some(run)),
    ] {
        w.conn()
            .execute(
                "INSERT INTO agents (agent_id, role, run_id, key_sha256) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![agent, role, run, sha256(key)],
            )
            .unwrap();
    }
    drop(w);

    let port = free_port();
    let _daemon = Daemon(
        Command::new(MAKIBA)
            .env("HOME", root.join("home"))
            .env("MAKIBA_STATE_DIR", root.join("state"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("MAKIBA_KNOWLEDGE_DIR", &knowledge)
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", port.to_string())
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
            .env_remove("MAKIBA_PERSON_KEY")
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let addr = format!("127.0.0.1:{port}");
    let until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < until && TcpStream::connect(&addr).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let saved_event = |file: &str| -> i64 {
        read_db(&db)
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'raw_note_saved' AND actor = 'secretary'
                   AND json_extract(payload_json, '$.task_id') = 't1'
                   AND json_extract(payload_json, '$.file') = ?1",
                [file],
                |r| r.get(0),
            )
            .unwrap()
    };
    let note = |key: &str, topic: &str, text: &str, idem: &str| {
        call(
            &addr,
            key,
            "note",
            json!({"task": "t1", "topic": topic, "text": text, "idem": idem}),
        )
    };

    // 1. One file under raw/ with the task line and the text, and `raw_note_saved`.
    let (is_error, first) = note(SECRETARY_KEY, "free-port", NOTE, "a");
    assert!(!is_error, "{first}");
    let f1 = first["file"].as_str().unwrap_or_default().to_string();
    assert!(dated(&f1, "free-port.md"), "{first}");
    assert_eq!(notes(&knowledge), BTreeSet::from([f1.clone()]));
    let body = std::fs::read_to_string(knowledge.join(&f1)).unwrap();
    assert_eq!(body.lines().next(), Some("Task: t1"), "{body}");
    assert!(body.contains(NOTE), "{body}");
    assert_eq!(saved_event(&f1), 1);

    // 2. Another key, the same topic: a second file. The first key again: the first file.
    let (is_error, second) = note(SECRETARY_KEY, "free-port", NOTE, "b");
    assert!(!is_error, "{second}");
    let f2 = second["file"].as_str().unwrap_or_default().to_string();
    assert!(dated(&f2, "free-port-2.md"), "{second}");
    assert_eq!(notes(&knowledge), BTreeSet::from([f1.clone(), f2.clone()]));
    assert_eq!(saved_event(&f2), 1);
    let (is_error, again) = note(SECRETARY_KEY, "free-port", NOTE, "a");
    assert!(!is_error, "{again}");
    assert_eq!(again["file"], json!(f1));
    assert_eq!(notes(&knowledge), BTreeSet::from([f1.clone(), f2.clone()]));
    assert_eq!(saved_event(&f1), 1);

    // 3. A text without the four headings is a usage error and writes nothing.
    let short = NOTE.replace("## Grounds\n", "");
    let (is_error, refused) = note(SECRETARY_KEY, "no-grounds", &short, "c");
    assert!(is_error, "{refused}");
    assert_eq!(refused["kind"], json!("usage"), "{refused}");
    assert_eq!(notes(&knowledge), BTreeSet::from([f1.clone(), f2.clone()]));

    // 4. A worker's key cannot call it.
    let (is_error, denied) = note(WORKER_KEY, "from-worker", NOTE, "d");
    assert!(is_error, "{denied}");
    assert_eq!(denied["kind"], json!("denied"), "{denied}");
    assert_eq!(notes(&knowledge), BTreeSet::from([f1, f2]));
}
