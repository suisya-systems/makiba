//! Issue 563: a stopped or stalled `makibad` is noticed from outside it
//! (docs/design/makibad-watch.md).
//!
//! The watch is the secretary's channel server (`makiba channel`): the door adds the heartbeat's
//! age to every answer to the secretary, and the server sends one `watch` notice when the door
//! stops answering or the age passes 30 s. `makiba status` puts the stale line first, and
//! `makiba up` names a `makibad` that stopped without recording it (and records that in the
//! ledger as `dispatcher_stop_unrecorded`, keeping its last heartbeat).
//!
//! What is real: the binary as `makiba daemon` (on the stand-in base, `MAKIBA_BASE=stub`), as
//! `makiba channel` speaking MCP on piped standard input and output, as `makiba up` and as
//! `makiba status`. The door listens on a loopback port the test picks. The bounds are wide so
//! that a slow runner does not fail them (Issue 571); the lower bounds of a stalled tick are read
//! from the database clock that wrote the heartbeat.

mod common;

use common::makibad;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const SECRETARY_TOKEN: &str = "tok-secretary-563-0123456789abcdef";
const WORKER_TOKEN: &str = "tok-worker-563-fedcba9876543210";
/// The door's hold in these tests (the seam `MAKIBA_CHANNEL_HOLD_MS`), so an idle channel server
/// gets an answer every half second instead of every 25 s.
const HOLD_MS: &str = "500";
/// `makiba status`'s first line when the heartbeat is stale.
const STALE_LINE: &str = "makibad is not running: the heartbeat is older than 30 s";

// ---- an organization in a temporary folder ------------------------------------------------------

/// A temporary root: `state/makiba` is the state folder (what `up` finds through
/// `XDG_STATE_HOME`), `bin` holds the stand-in `claude` and `hc` that `up` runs.
struct Org {
    root: tempfile::TempDir,
    port: u16,
    /// Processes this test started, killed when it ends.
    children: Vec<Child>,
}

impl Org {
    fn new() -> Org {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(root.path().join("state/makiba")).unwrap();
        let (a, b, c) = makiba::agents::version::LOWEST;
        script(
            &bin.join("claude"),
            &format!("echo '{a}.{b}.{c} (Claude Code)'"),
        );
        let rec = root.path().join("hc.rec");
        script(
            &bin.join("hc"),
            &format!("echo \"CALL $*\" >> {}", rec.display()),
        );
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Org {
            root,
            port,
            children: Vec::new(),
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    fn state(&self) -> PathBuf {
        self.path("state/makiba")
    }

    fn db(&self) -> PathBuf {
        self.state().join("state.db")
    }

    fn door(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// The environment every `makiba` of this test runs with; nothing of the person's.
    fn env(&self, c: &mut Command, port: &str) {
        c.env_clear()
            .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(self.root.path()))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("MAKIBA_STATE_DIR", self.state())
            .env("MAKIBA_PERSON_KEY", self.path("config/makiba/person.key"))
            .env("HC_BIN", self.path("bin/hc"))
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", port)
            .env("MAKIBA_CHANNEL_HOLD_MS", HOLD_MS)
            .stdin(Stdio::null());
    }

    /// An agent row whose key is `token`, as a launch leaves it (what the door's `caller` reads).
    fn agent(&self, agent_id: &str, role: &str, token: &str) {
        let w = makiba::db::writer::StateWriter::open(&self.db()).unwrap();
        w.conn()
            .execute(
                "INSERT INTO agents (agent_id, role, key_sha256) VALUES (?1, ?2, ?3)",
                params![agent_id, role, makiba::daemon::door::sha256_hex(token)],
            )
            .unwrap();
    }

    /// A message to `recipient` with its pending ledger row, as `send` leaves them.
    fn message(&self, recipient: &str, body: &str) {
        let w = makiba::db::writer::StateWriter::open(&self.db()).unwrap();
        let event = w
            .append_event(&makiba::db::writer::NewEvent {
                kind: "message_sent",
                actor: Some("secretary"),
                ..Default::default()
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO messages (event_id, sender, recipient, kind, body)
                 VALUES (?1, 'secretary', ?2, 'message', ?3)",
                params![event, recipient, body],
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt)
                 VALUES (?1, ?2, 'pending', 0)",
                params![event, recipient],
            )
            .unwrap();
    }

    /// `makiba daemon` on this state folder, its door on the test's port. Returns its index.
    fn daemon(&mut self) -> usize {
        let mut c = makibad::daemon(self.root.path());
        let port = self.port.to_string();
        c.with(|c| {
            self.env(c, &port);
        });
        let child = c
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.children.push(child);
        self.children.len() - 1
    }

    /// `makiba up --no-secretary`; the `makibad` it starts listens on a port of its own.
    fn up(&self) -> Output {
        let mut c = makibad::up(self.root.path());
        c.with(|c| self.env(c, "0"));
        c.arg("--no-secretary");
        c.output().unwrap()
    }

    fn status(&self) -> Output {
        let mut c = Command::new(MAKIBA);
        self.env(&mut c, "0");
        c.arg("status").output().unwrap()
    }

    /// The pid in `makibad.lock`, if any.
    fn lock_pid(&self) -> Option<i32> {
        fs::read_to_string(self.state().join("makibad.lock"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn read(&self) -> Option<Connection> {
        Connection::open_with_flags(self.db(), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
    }

    /// The dispatcher row: pid, started_at, heartbeat_at, subscribed.
    fn row(&self) -> Option<(i64, String, String, i64)> {
        self.read()?
            .query_row(
                "SELECT pid, started_at, heartbeat_at, subscribed FROM dispatcher WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok()
    }

    /// The age of the dispatcher row's heartbeat in seconds, by the database's clock.
    fn heartbeat_age(&self) -> f64 {
        self.read()
            .unwrap()
            .query_row(
                "SELECT (julianday('now') - julianday(heartbeat_at)) * 86400.0
                 FROM dispatcher WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The payloads of the events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let Some(conn) = self.read() else {
            return Vec::new();
        };
        let mut q = conn
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        // A `makibad` that `up` started is ended by this test.
        if let Some(pid) = self.lock_pid() {
            kill(pid, libc::SIGKILL);
        }
    }
}

fn script(path: &Path, body: &str) {
    common::exe::write(path, format!("#!/bin/sh\n{body}\n"));
}

fn kill(pid: i32, sig: i32) {
    // SAFETY: kill(2) of a pid this test started (or read from its own lock file).
    unsafe { libc::kill(pid, sig) };
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn wait<T>(what: &str, within: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

/// Wait for the process `pid` (not a child of this test) to end.
fn wait_gone(pid: i32) {
    wait(
        &format!("makibad {pid} to end"),
        Duration::from_secs(20),
        || (!alive(pid)).then_some(()),
    );
}

// ---- the channel server -----------------------------------------------------------------------

/// `makiba channel` with its MCP side on pipes; every channel notification's content arrives on
/// `notes`.
struct Server {
    child: Child,
    _stdin: ChildStdin,
    notes: Receiver<String>,
}

impl Server {
    fn start(door: &str, token: &str) -> Server {
        let mut child = Command::new(MAKIBA)
            .args(["channel", "--door", door, "--token", token])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, notes) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v["method"] == "notifications/claude/channel" {
                    let content = v["params"]["content"].as_str().unwrap_or("").to_string();
                    if tx.send(content).is_err() {
                        return;
                    }
                }
            }
        });
        for msg in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                              "clientInfo": {"name": "test", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        ] {
            writeln!(stdin, "{msg}").unwrap();
        }
        stdin.flush().unwrap();
        Server {
            child,
            _stdin: stdin,
            notes,
        }
    }

    /// The next notification within `within`, if any.
    fn next(&self, within: Duration) -> Option<String> {
        match self.notes.recv_timeout(within) {
            Ok(n) => Some(n),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the channel server's output ended")
            }
        }
    }

    /// The next `watch` notice within `within`; any other notification is a failure.
    fn next_watch(&self, within: Duration) -> Option<String> {
        let n = self.next(within)?;
        assert!(n.contains("kind=\"watch\""), "not a watch notice: {n}");
        Some(n)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- the cases ----------------------------------------------------------------------------------

#[test]
fn a_makibad_whose_tick_stops_is_reported_stale_to_the_secretary_within_the_bound() {
    let mut org = Org::new();
    org.agent("secretary", "secretary", SECRETARY_TOKEN);
    // The tick stops at its first pass while the process, its door and its lock live on.
    fs::write(org.state().join("makibad.hold-at"), "tick").unwrap();
    let d = org.daemon();
    let pid = org.children[d].id() as i32;
    wait("the tick to be held", Duration::from_secs(20), || {
        org.state().join("makibad.held").exists().then_some(())
    });
    wait("the dispatcher row", Duration::from_secs(20), || org.row());
    let server = Server::start(&org.door(), SECRETARY_TOKEN);

    let notice = server
        .next_watch(Duration::from_secs(60))
        .expect("no watch notice within 60 s of a stopped tick");
    let age = org.heartbeat_age();
    assert!(
        age >= 29.0,
        "the notice came while the heartbeat was {age:.1} s old: {notice}"
    );
    assert!(
        age <= 50.0,
        "the notice came only when the heartbeat was {age:.1} s old: {notice}"
    );
    for words in [
        "its heartbeat is",
        "its tick has stopped",
        "makiba status",
        "makiba down",
        "kill -KILL",
        "makiba up",
    ] {
        assert!(
            notice.contains(words),
            "the notice lacks {words:?}: {notice}"
        );
    }
    assert!(
        alive(pid),
        "makibad's process ended: only its tick was to stop"
    );
    assert_eq!(
        server.next(Duration::from_secs(3)),
        None,
        "a second notice for the same stall"
    );

    let out = org.status();
    assert!(!out.status.success(), "status of a stale makibad exited 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().next(),
        Some(STALE_LINE),
        "status's first line: {stdout}"
    );

    let out = org.up();
    let said = text(&out);
    assert!(
        said.contains("holds its lock but its heartbeat is"),
        "up does not name the stalled makibad: {said}"
    );
    assert!(said.contains("its tick has stopped"), "{said}");
}

#[test]
fn a_killed_makibad_is_reported_stale_to_the_secretary_within_the_bound() {
    let mut org = Org::new();
    org.agent("secretary", "secretary", SECRETARY_TOKEN);
    let d = org.daemon();
    wait("makibad to be subscribed", Duration::from_secs(20), || {
        org.row().filter(|r| r.3 == 1)
    });
    org.message("secretary", "sync-563");
    let server = Server::start(&org.door(), SECRETARY_TOKEN);
    // A completed answer: the server has heard from the door before it goes.
    let first = server
        .next(Duration::from_secs(30))
        .expect("the queued message never reached the channel server");
    assert!(
        first.contains("sync-563") && !first.contains("kind=\"watch\""),
        "the first notification is not the queued message: {first}"
    );

    let killed = Instant::now();
    org.children[d].kill().unwrap();
    org.children[d].wait().unwrap();
    let notice = server
        .next_watch(Duration::from_secs(60))
        .expect("no watch notice within 60 s of a killed makibad");
    let after = killed.elapsed().as_secs_f64();
    assert!(
        after >= 29.0,
        "the notice came {after:.1} s after the kill: {notice}"
    );
    assert!(
        after <= 50.0,
        "the notice came only {after:.1} s after the kill: {notice}"
    );
    for words in ["the door does not answer", "makiba up"] {
        assert!(
            notice.contains(words),
            "the notice lacks {words:?}: {notice}"
        );
    }
}

#[test]
fn a_worker_channel_server_does_not_watch() {
    let mut org = Org::new();
    org.agent("w-t1", "worker", WORKER_TOKEN);
    let d = org.daemon();
    wait("makibad to be subscribed", Duration::from_secs(20), || {
        org.row().filter(|r| r.3 == 1)
    });
    org.message("w-t1", "sync-563");
    let server = Server::start(&org.door(), WORKER_TOKEN);
    let first = server
        .next(Duration::from_secs(30))
        .expect("the queued message never reached the channel server");
    assert!(
        first.contains("sync-563"),
        "the first notification is not the queued message: {first}"
    );

    org.children[d].kill().unwrap();
    org.children[d].wait().unwrap();
    assert_eq!(
        server.next(Duration::from_secs(35)),
        None,
        "a worker's channel server sent a notification after makibad was killed"
    );
}

#[test]
fn up_names_the_previous_makibad_that_stopped_without_recording_it() {
    let org = Org::new();
    let out = org.up();
    assert!(out.status.success(), "the first up: {}", text(&out));
    let first = org.lock_pid().expect("up started no makibad");
    wait("makibad to be subscribed", Duration::from_secs(20), || {
        org.row().filter(|r| r.0 == first as i64 && r.3 == 1)
    });
    kill(first, libc::SIGKILL);
    wait_gone(first);
    // Read after the end: the heartbeat cannot move any more.
    let (pid, started_at, heartbeat_at, _) = org.row().unwrap();
    assert_eq!(pid, first as i64);

    let out = org.up();
    let said = text(&out);
    assert!(out.status.success(), "the second up: {said}");
    let line = format!(
        "makibad: the previous makibad (pid {first}) stopped without recording it; its last heartbeat was {heartbeat_at}"
    );
    assert!(said.contains(&line), "up does not say {line:?}: {said}");
    let second = org.lock_pid().expect("the second up started no makibad");
    wait(
        "the second makibad to be subscribed",
        Duration::from_secs(20),
        || org.row().filter(|r| r.0 == second as i64 && r.3 == 1),
    );
    let recorded = org.events("dispatcher_stop_unrecorded");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["pid"], json!(first), "{recorded:?}");
    assert_eq!(recorded[0]["started_at"], json!(started_at), "{recorded:?}");
    assert_eq!(
        recorded[0]["heartbeat_at"],
        json!(heartbeat_at),
        "{recorded:?}"
    );

    // A recorded stop is not named.
    kill(second, libc::SIGTERM);
    wait_gone(second);
    wait(
        "the second makibad's dispatcher_stopped",
        Duration::from_secs(10),
        || {
            org.events("dispatcher_stopped")
                .iter()
                .any(|p| p["pid"] == json!(second))
                .then_some(())
        },
    );
    let out = org.up();
    let said = text(&out);
    assert!(out.status.success(), "the third up: {said}");
    assert!(
        !said.contains("stopped without recording it"),
        "a clean stop is named as unrecorded: {said}"
    );
    let third = org.lock_pid().expect("the third up started no makibad");
    wait(
        "the third makibad to be subscribed",
        Duration::from_secs(20),
        || org.row().filter(|r| r.0 == third as i64 && r.3 == 1),
    );
    assert_eq!(
        org.events("dispatcher_stop_unrecorded").len(),
        1,
        "a second dispatcher_stop_unrecorded"
    );
}

#[test]
fn a_start_that_fails_before_its_row_keeps_the_earlier_heartbeat() {
    let mut org = Org::new();
    let out = org.up();
    assert!(out.status.success(), "the first up: {}", text(&out));
    let first = org.lock_pid().expect("up started no makibad");
    wait("makibad to be subscribed", Duration::from_secs(20), || {
        org.row().filter(|r| r.0 == first as i64 && r.3 == 1)
    });
    kill(first, libc::SIGKILL);
    wait_gone(first);
    let (_, started_at, heartbeat_at, _) = org.row().unwrap();

    // A start that dies right before the transaction that writes its dispatcher row.
    let die_at = org.state().join("makibad.die-at");
    fs::write(&die_at, "dispatcher_row").unwrap();
    let d = org.daemon();
    let until = Instant::now() + Duration::from_secs(20);
    let ended = loop {
        if let Some(status) = org.children[d].try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= until {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        ended.is_some(),
        "the start armed to die before its dispatcher row did not end"
    );
    fs::remove_file(&die_at).unwrap();

    let out = org.up();
    let said = text(&out);
    assert!(
        out.status.success(),
        "the up after the failed start: {said}"
    );
    let line = format!(
        "makibad: the previous makibad (pid {first}) stopped without recording it; its last heartbeat was {heartbeat_at}"
    );
    assert!(said.contains(&line), "up does not say {line:?}: {said}");
    let next = org.lock_pid().expect("up started no makibad");
    wait(
        "the new makibad to be subscribed",
        Duration::from_secs(20),
        || org.row().filter(|r| r.0 == next as i64 && r.3 == 1),
    );
    let recorded = org.events("dispatcher_stop_unrecorded");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["pid"], json!(first), "{recorded:?}");
    assert_eq!(recorded[0]["started_at"], json!(started_at), "{recorded:?}");
    assert_eq!(
        recorded[0]["heartbeat_at"],
        json!(heartbeat_at),
        "{recorded:?}"
    );
}
