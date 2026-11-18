//! Issue 115: `MAKIBA_DOOR=none`, the test seam that starts `makibad` without the person's Unix
//! socket (spec section 2). The test needs no Unix socket itself, so it runs in a sandbox that
//! cannot create one. It runs the real binary, `makiba daemon` on the stand-in base, in a scratch
//! folder that is also its `HOME` and its XDG folders.

use makiba::db::writer::StateWriter;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const SECRETARY_KEY: &str = "5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5";
const WORKER_KEY: &str = "0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a";

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

fn env(c: &mut Command, root: &Path, port: u16) {
    c.env("HOME", root.join("home"))
        .env("MAKIBA_STATE_DIR", root.join("state"))
        .env("XDG_STATE_HOME", root.join("xdg-state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", port.to_string())
        .env("MAKIBA_BASE", "stub")
        .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
        .env_remove("MAKIBA_PERSON_KEY");
}

fn event_count(db: &Path, kind: &str) -> i64 {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|c| {
            c.query_row("SELECT COUNT(*) FROM events WHERE kind = ?1", [kind], |r| {
                r.get(0)
            })
        })
        .unwrap_or(0)
}

/// Every socket file under `dir`.
fn sockets(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let t = e.file_type().unwrap();
        if t.is_socket() {
            found.push(e.path());
        } else if t.is_dir() {
            sockets(&e.path(), found);
        }
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

#[test]
fn makibad_starts_without_the_socket_and_the_person_command_says_why_it_cannot_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for d in ["home", "state", "config", "data", "run", "xdg-state"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let db = root.join("state/state.db");
    let w = StateWriter::open(&db).unwrap();
    for (agent, role, key) in [
        ("secretary", "secretary", SECRETARY_KEY),
        ("w-t1", "worker", WORKER_KEY),
    ] {
        w.conn()
            .execute(
                "INSERT INTO agents (agent_id, role, key_sha256) VALUES (?1, ?2, ?3)",
                rusqlite::params![agent, role, sha256(key)],
            )
            .unwrap();
    }
    drop(w);

    let port = free_port();
    let mut c = Command::new(MAKIBA);
    env(&mut c, root, port);
    let _daemon = Daemon(
        c.arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // It starts: it writes `dispatcher_started` and serves the port.
    let addr = format!("127.0.0.1:{port}");
    let until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < until
        && !(event_count(&db, "dispatcher_started") == 1 && TcpStream::connect(&addr).is_ok())
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        event_count(&db, "dispatcher_started"),
        1,
        "makibad did not start with MAKIBA_DOOR=none"
    );

    // A tool call on the port with a valid token is answered.
    let (is_error, answer) = call(
        &addr,
        WORKER_KEY,
        "send",
        json!({"to": "secretary", "text": "hello"}),
    );
    assert!(!is_error, "{answer}");

    // No socket file under the state or the runtime folder.
    let mut found = Vec::new();
    sockets(&root.join("state"), &mut found);
    sockets(&root.join("run"), &mut found);
    assert!(found.is_empty(), "socket files: {found:?}");

    // The person's `makiba` fails at once and names the reason.
    let mut c = Command::new(MAKIBA);
    env(&mut c, root, port);
    let started = Instant::now();
    let out = c
        .args(["send", "secretary", "hello"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "makiba send succeeded: {out:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "makiba hung");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MAKIBA_DOOR=none") && stderr.contains("socket is off"),
        "no reason named: {stderr}"
    );
}
