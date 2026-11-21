//! Issue 191: the verb `candidate`, by which the secretary records the person's decision on a
//! skill candidate in `<knowledge>/skill-candidates.md` through `makibad`.
//!
//! It runs the real binary, `makiba daemon` on the stand-in base with `MAKIBA_DOOR=none` (no Unix
//! socket is needed), and calls the tool over the MCP port with the agents' tokens. Everything
//! the test writes to `state.db` is written before `makibad` starts; afterwards the test only
//! reads it, so it never races `makibad`'s writes.

use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const SECRETARY_KEY: &str = "5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5";
const WORKER_KEY: &str = "0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a";

/// The file as the curator left it: a heading, an open entry, a decided one, and another open one.
const BEFORE: &str = "# Skill candidates\n\
\n\
- pattern: free-port\n\
- date: 2026-11-19\n\
- notes: raw/2026-11-01-a.md, raw/2026-11-02-b.md, raw/2026-11-03-c.md\n\
- when: A test starts makibad and needs a port.\n\
- skill: free-port\n\
- status: pending\n\
- decided:\n\
- reason:\n\
\n\
- pattern: old-one\n\
- date: 2026-11-01\n\
- notes: raw/archive/2026-10-01-x.md\n\
- when: Never again.\n\
- skill: old-one\n\
- status: rejected\n\
- decided: 2026-11-02\n\
- reason: not now\n\
\n\
- pattern: port-retry\n\
- date: 2026-11-19\n\
- notes: raw/2026-11-04-d.md\n\
- when: A port is taken.\n\
- skill: port-retry\n\
- status: pending\n\
- decided:\n\
- reason:\n";

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

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|i| b[*i].is_ascii_digit())
        && b[4] == b'-'
        && b[7] == b'-'
}

#[test]
fn the_secretary_records_a_decision_once_and_nobody_else_can() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for d in ["home", "state", "config", "data", "run", "xdg-state"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let knowledge = root.join("knowledge");
    std::fs::create_dir_all(&knowledge).unwrap();
    let file = knowledge.join("skill-candidates.md");
    std::fs::write(&file, BEFORE).unwrap();
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
    let decided_events = |pattern: &str| -> Vec<(String, String, String, String)> {
        let c = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut q = c
            .prepare(
                "SELECT actor, json_extract(payload_json, '$.status'),
                        json_extract(payload_json, '$.decided'),
                        json_extract(payload_json, '$.reason')
                   FROM events WHERE kind = 'skill_candidate_decided'
                    AND json_extract(payload_json, '$.pattern') = ?1",
            )
            .unwrap();
        q.query_map([pattern], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    let decide = |key: &str, pattern: &str, decision: &str, reason: &str, idem: &str| {
        call(
            &addr,
            key,
            "candidate",
            json!({"pattern": pattern, "decision": decision, "reason": reason, "idem": idem}),
        )
    };
    let read = || std::fs::read_to_string(&file).unwrap();

    // 1. An open entry carries the decision, the date and the reason; nothing else in the file
    //    changes; the event is recorded.
    let (is_error, first) = decide(
        SECRETARY_KEY,
        "free-port",
        "approved",
        "Every test needs it",
        "a",
    );
    assert!(!is_error, "{first}");
    let date = first["decided"].as_str().unwrap_or_default().to_string();
    assert!(is_date(&date), "{first}");
    let after = BEFORE.replacen(
        "- status: pending\n- decided:\n- reason:\n",
        &format!("- status: approved\n- decided: {date}\n- reason: Every test needs it\n"),
        1,
    );
    assert_ne!(after, BEFORE);
    assert_eq!(read(), after);
    assert_eq!(
        decided_events("free-port"),
        vec![(
            "secretary".to_string(),
            "approved".to_string(),
            date.clone(),
            "Every test needs it".to_string()
        )]
    );

    // 2. An unknown pattern is `not_found`, an entry already decided is `conflict`; the file is
    //    unchanged and no event is written.
    let (is_error, unknown) = decide(SECRETARY_KEY, "no-such-pattern", "rejected", "r", "b");
    assert!(is_error, "{unknown}");
    assert_eq!(unknown["kind"], json!("not_found"), "{unknown}");
    assert_eq!(read(), after);
    assert_eq!(decided_events("no-such-pattern"), vec![]);
    for (pattern, idem) in [("old-one", "c"), ("free-port", "d")] {
        let (is_error, decided) = decide(SECRETARY_KEY, pattern, "into-other-skill", "r", idem);
        assert!(is_error, "{decided}");
        assert_eq!(decided["kind"], json!("conflict"), "{decided}");
        assert_eq!(read(), after);
    }
    assert_eq!(decided_events("old-one"), vec![]);
    assert_eq!(decided_events("free-port").len(), 1);

    // 3. A worker's key cannot call it, not even for an open entry.
    let (is_error, denied) = decide(WORKER_KEY, "port-retry", "approved", "r", "e");
    assert!(is_error, "{denied}");
    assert_eq!(denied["kind"], json!("denied"), "{denied}");
    assert_eq!(read(), after);
    assert_eq!(decided_events("port-retry"), vec![]);
}
