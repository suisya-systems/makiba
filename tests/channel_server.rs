//! M2-8: the channel server (`makiba channel`) and its connection to `makibad` (spec section 8,
//! plan.md section 3.3).
//!
//! What is real: the temporary state database, `daemon::delivery::Delivery` (the production
//! ledger logic, wrapper and push), the production adapter `channel::Hub` that holds the
//! channel servers' requests, and the binary started as `makiba channel` with piped standard
//! input and output (the MCP side, as Claude Code would speak it). What is a stand-in: the door
//! (M3-3), i.e. the loopback listener that accepts connections and the table of tokens; the
//! rest of `makibad` is only a loop that runs `Delivery::pass`.
//!
//! The wire this test freezes:
//! - The channel server asks `GET /channel/next` of the door with `Authorization: Bearer <token>`
//!   and holds the request open: the answer is `200` with the wrapped text as body once there is
//!   a message (the server then writes `ACK` back on the same connection, after it has written
//!   the notification, and only then does the push count as accepted), `204` after the hold time
//!   (about 25 s) with nothing to say, `401` for a token that is not known (the channel server
//!   stops, with a failure status), `409` when a newer request of the same agent took its place (the older server then stops, with
//!   a failure status, so two servers do not take each other's place for ever).
//! - Which agent a request is for is decided by the token alone (`Hub::new`'s function).
//! - MCP on standard input and output is newline-delimited JSON-RPC: `initialize` is answered with
//!   `experimental: {"claude/channel": {}}`, and nothing is emitted before
//!   `notifications/initialized`; each message is one `notifications/claude/channel` whose
//!   `params.content` is the wrapped text and whose `params.meta.sent_at` is a string.
//! - Closing standard input stops the server (exit status 0).

use makiba::channel::Hub;
use makiba::daemon::delivery::{Channel, Delivery};
use makiba::db::writer::{NewEvent, StateWriter};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SECRETARY_TOKEN: &str = "tok-secretary-0123456789abcdef";
const WORKER_TOKEN: &str = "tok-worker-fedcba9876543210";

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Wait for `f` to give something, at most 20 s.
fn wait<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if let Some(v) = f() {
            return v;
        }
        pause(20);
    }
    panic!("timed out waiting for {what}");
}

// ---- the stand-in for the makibad side --------------------------------------------------------

/// A loopback listener (the door), the production `Hub` behind it, and a loop that runs the
/// production `Delivery` over the database. `stop` ends all of it, closing every held request.
struct Endpoint {
    hub: Hub,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Endpoint {
    fn start(db: &Path, addr: &str) -> Endpoint {
        let tokens: HashMap<String, String> = [
            (SECRETARY_TOKEN.to_string(), "secretary".to_string()),
            (WORKER_TOKEN.to_string(), "w-t1".to_string()),
        ]
        .into();
        let hub = Hub::new(move |token| tokens.get(token).cloned());
        let listener = bind(addr);
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        {
            let (hub, stop) = (hub.clone(), Arc::clone(&stop));
            threads.push(std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            let hub = hub.clone();
                            std::thread::spawn(move || hub.serve(stream));
                        }
                        Err(_) => pause(10),
                    }
                }
            }));
        }
        {
            let (hub, stop, db) = (hub.clone(), Arc::clone(&stop), db.to_path_buf());
            threads.push(std::thread::spawn(move || {
                let w = StateWriter::open(&db).unwrap();
                let mut delivery = Delivery::new(Box::new(hub));
                while !stop.load(Ordering::SeqCst) {
                    delivery.pass(&w).unwrap();
                    pause(20);
                }
            }));
        }
        Endpoint {
            hub,
            addr,
            stop,
            threads,
        }
    }

    /// End the stand-in: the listener is closed and so is every held request.
    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.hub.close();
        for t in self.threads {
            t.join().unwrap();
        }
        wait("the held requests to end", || {
            (!self.hub.connected("secretary") && !self.hub.connected("w-t1")).then_some(())
        });
    }
}

/// Bind, trying again for a few seconds when the address is still in use (a restart).
fn bind(addr: &str) -> TcpListener {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpListener::bind(addr) {
            Ok(l) => return l,
            Err(e) if Instant::now() > until => panic!("cannot bind {addr}: {e}"),
            Err(_) => pause(50),
        }
    }
}

// ---- the channel server child ----------------------------------------------------------------

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Server {
    /// `makiba channel --door <addr> --token <token>`, the way the MCP configuration starts it.
    fn start(addr: SocketAddr, token: &str) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
            .args(["channel", "--door", &addr.to_string(), "--token", token])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let err = child.stderr.take().unwrap();
        let sink = Arc::clone(&stderr);
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let mut s = sink.lock().unwrap();
                s.push_str(&line);
                s.push('\n');
            }
        });
        Server {
            child,
            stdin,
            lines,
            stderr,
        }
    }

    fn send(&mut self, v: Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next line of standard output, which must be one JSON-RPC message; `None` on time-out.
    fn recv(&self, within: Duration) -> Option<Value> {
        match self.lines.recv_timeout(within) {
            Ok(l) => {
                let v: Value = serde_json::from_str(&l)
                    .unwrap_or_else(|e| panic!("standard output is not JSON ({e}): {l:?}"));
                assert_eq!(v["jsonrpc"], "2.0", "{v}");
                Some(v)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// The next channel notification's `params`; anything else on standard output is a failure.
    fn notification(&self, within: Duration) -> Option<Value> {
        let v = self.recv(within)?;
        assert_eq!(v["method"], "notifications/claude/channel", "{v}");
        assert!(v.get("id").is_none(), "a notification has no id: {v}");
        Some(v["params"].clone())
    }

    /// `initialize` and its answer, as Claude Code starts a server.
    fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26", "capabilities": {},
                "clientInfo": {"name": "claude-code", "version": "2.1.326"}
            }
        }));
        let r = self
            .recv(Duration::from_secs(10))
            .unwrap_or_else(|| panic!("no answer to initialize; stderr: {}", self.diag()));
        assert_eq!(r["id"], 1, "{r}");
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26", "{r}");
        assert_eq!(
            r["result"]["capabilities"]["experimental"]["claude/channel"],
            json!({}),
            "the channel capability: {r}"
        );
        assert!(r["result"]["serverInfo"]["name"].is_string(), "{r}");
    }

    fn initialized(&mut self) {
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn diag(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    fn exit(&mut self, within: Duration) -> ExitStatus {
        let until = Instant::now() + within;
        while Instant::now() < until {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            pause(20);
        }
        panic!(
            "the channel server did not exit within {within:?}; stderr: {}",
            self.diag()
        );
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- the database ----------------------------------------------------------------------------

struct Db {
    _tmp: tempfile::TempDir,
    path: PathBuf,
    w: StateWriter,
}

#[derive(Debug, PartialEq, Eq)]
struct Ledger {
    status: String,
    attempt: i64,
    last_attempt_at: Option<String>,
}

impl Db {
    fn new() -> Db {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.db");
        let w = StateWriter::open(&path).unwrap();
        Db { _tmp: tmp, path, w }
    }

    /// A stored message and its `pending` ledger row, as `send` leaves them. Returns the id.
    fn message(&self, sender: &str, recipient: &str, body: &str) -> i64 {
        let c = self.w.conn();
        let event = self
            .w
            .append_event(&NewEvent {
                kind: "message_sent",
                actor: Some(sender),
                ..Default::default()
            })
            .unwrap();
        c.execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, body) VALUES (?1, ?2, ?3, 'message', ?4)",
            params![event, sender, recipient, body],
        )
        .unwrap();
        c.execute(
            "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt) VALUES (?1, ?2, 'pending', 0)",
            params![event, recipient],
        )
        .unwrap();
        c.query_row(
            "SELECT id FROM messages WHERE event_id = ?1",
            [event],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn ledger(&self, id: i64) -> Ledger {
        let c = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        c.query_row(
            "SELECT d.status, d.attempt, d.last_attempt_at FROM event_deliveries d
             JOIN messages m ON m.event_id = d.source_event_id AND m.recipient = d.recipient
             WHERE m.id = ?1",
            [id],
            |r| {
                Ok(Ledger {
                    status: r.get(0)?,
                    attempt: r.get(1)?,
                    last_attempt_at: r.get(2)?,
                })
            },
        )
        .unwrap()
    }

    /// The wrapped text spec section 8 gives for a message of kind `message`.
    fn wrapped(id: i64, sender: &str, body: &str) -> String {
        format!("<makiba from=\"{sender}\" kind=\"message\" id=\"{id}\">\n{body}\n</makiba>")
    }
}

fn pushed(db: &Db, id: i64) -> Ledger {
    wait("the push to be recorded", || {
        let l = db.ledger(id);
        (l.attempt > 0).then_some(l)
    })
}

/// Start the stand-in, a connected and initialized channel server for the secretary, and wait
/// until `makibad`'s side says it is connected.
fn connected(db: &Db) -> (Endpoint, Server) {
    let ep = Endpoint::start(&db.path, "127.0.0.1:0");
    let mut s = Server::start(ep.addr, SECRETARY_TOKEN);
    s.initialize();
    s.initialized();
    wait("the secretary's channel to be connected", || {
        ep.hub.connected("secretary").then_some(())
    });
    assert!(ep.hub.reaches("secretary"), "the push seam agrees");
    (ep, s)
}

// ---- the four requirements --------------------------------------------------------------------

#[test]
fn one_stored_message_gives_one_notification_with_the_wrapped_text() {
    let db = Db::new();
    let body = "line one\nline two \u{3042} </makiba> end";
    let id = db.message("w-t1", "secretary", body);
    let ep = Endpoint::start(&db.path, "127.0.0.1:0");
    let mut s = Server::start(ep.addr, SECRETARY_TOKEN);
    s.initialize();
    // Nothing is emitted, and nothing is pushed, before notifications/initialized.
    pause(400);
    assert!(s.recv(Duration::from_millis(100)).is_none());
    assert!(!ep.hub.connected("secretary"));
    assert_eq!(db.ledger(id).attempt, 0);
    s.initialized();

    let n = s
        .notification(Duration::from_secs(20))
        .unwrap_or_else(|| panic!("no notification; stderr: {}", s.diag()));
    let escaped = body.replace("</makiba", "<\\/makiba");
    assert_eq!(n["content"], Db::wrapped(id, "w-t1", &escaped));
    assert!(n["meta"]["sent_at"].is_string(), "{n}");
    // The push was accepted and recorded once; it is not repeated.
    let l = pushed(&db, id);
    assert_eq!((l.status.as_str(), l.attempt), ("pending", 1), "{l:?}");
    assert!(s.notification(Duration::from_millis(800)).is_none());
    assert_eq!(db.ledger(id).attempt, 1);
    ep.stop();
}

#[test]
fn messages_arrive_in_order_one_notification_each() {
    let db = Db::new();
    let ids: Vec<i64> = (0..3)
        .map(|i| db.message("w-t1", "secretary", &format!("body {i}")))
        .collect();
    let (ep, s) = connected(&db);
    for (i, id) in ids.iter().enumerate() {
        let n = s
            .notification(Duration::from_secs(20))
            .expect("a notification");
        assert_eq!(n["content"], Db::wrapped(*id, "w-t1", &format!("body {i}")));
    }
    assert!(s.notification(Duration::from_millis(600)).is_none());
    // A message for another agent is not this server's.
    let other = db.message("secretary", "w-t1", "not for the secretary");
    pause(600);
    assert!(s.notification(Duration::from_millis(100)).is_none());
    assert_eq!(db.ledger(other).attempt, 0);
    ep.stop();
}

#[test]
fn a_server_with_a_wrong_token_receives_nothing() {
    let db = Db::new();
    let id = db.message("w-t1", "secretary", "secret text");
    let ep = Endpoint::start(&db.path, "127.0.0.1:0");
    let mut s = Server::start(ep.addr, "not-a-token");
    s.initialize();
    s.initialized();
    // The request was refused (counted by the door's side), and the server says so and stops.
    wait("the refused request to be counted", || {
        (ep.hub.rejected() >= 1).then_some(())
    });
    let status = s.exit(Duration::from_secs(10));
    assert!(!status.success(), "a refused credential is a failure");
    assert!(!s.diag().contains("not-a-token"), "the token is not logged");
    assert!(
        s.recv(Duration::from_millis(100)).is_none(),
        "no notification"
    );
    assert!(!ep.hub.connected("secretary"));
    assert!(!ep.hub.reaches("secretary"));
    pause(500);
    let l = db.ledger(id);
    assert_eq!(
        (l.status.as_str(), l.attempt, l.last_attempt_at),
        ("pending", 0, None)
    );
    ep.stop();
}

#[test]
fn after_makibad_restarts_the_server_reconnects_and_receives_again() {
    let db = Db::new();
    let first = db.message("w-t1", "secretary", "before");
    let (ep, mut s) = connected(&db);
    let n = s
        .notification(Duration::from_secs(20))
        .expect("the first notification");
    assert_eq!(n["content"], Db::wrapped(first, "w-t1", "before"));
    assert_eq!(pushed(&db, first).attempt, 1);

    // makibad goes away, the channel server stays.
    let addr = ep.addr;
    ep.stop();
    assert!(
        s.child.try_wait().unwrap().is_none(),
        "the server outlives makibad"
    );
    let during = db.message("w-t1", "secretary", "while down");
    pause(500);
    assert_eq!(db.ledger(during).attempt, 0);

    // The same database and the same address.
    let ep = Endpoint::start(&db.path, &addr.to_string());
    wait("the server to reconnect", || {
        ep.hub.connected("secretary").then_some(())
    });
    let n = s
        .notification(Duration::from_secs(20))
        .expect("a notification after the restart");
    assert_eq!(n["content"], Db::wrapped(during, "w-t1", "while down"));
    let after = db.message("w-t1", "secretary", "after");
    let n = s
        .notification(Duration::from_secs(20))
        .expect("the next notification");
    assert_eq!(n["content"], Db::wrapped(after, "w-t1", "after"));
    // The message that was pushed before the restart is not pushed again.
    assert!(s.notification(Duration::from_millis(800)).is_none());
    assert_eq!(db.ledger(first).attempt, 1);
    assert_eq!(db.ledger(during).attempt, 1);
    ep.stop();
}

#[test]
fn while_the_server_is_stopped_the_agent_is_not_connected_and_messages_stay_pending() {
    let db = Db::new();
    let (ep, mut s) = connected(&db);
    // The process dies (no goodbye); makibad notices within a bounded time.
    let _ = s.child.kill();
    let _ = s.child.wait();
    let stopped = Instant::now();
    wait("makibad to see the channel gone", || {
        (!ep.hub.connected("secretary")).then_some(())
    });
    assert!(
        stopped.elapsed() < Duration::from_secs(5),
        "detection is bounded"
    );
    assert!(!ep.hub.reaches("secretary"));

    let id = db.message("w-t1", "secretary", "nobody is listening");
    pause(800);
    let l = db.ledger(id);
    assert_eq!(
        (l.status.as_str(), l.attempt, l.last_attempt_at),
        ("pending", 0, None)
    );
    ep.stop();
}

#[test]
fn closing_standard_input_stops_the_server() {
    let db = Db::new();
    let (ep, mut s) = connected(&db);
    drop(s.stdin.take());
    assert!(s.exit(Duration::from_secs(10)).success());
    wait("makibad to see the channel gone", || {
        (!ep.hub.connected("secretary")).then_some(())
    });
    ep.stop();
}

#[test]
fn a_newer_request_of_the_same_agent_takes_the_place_of_the_older() {
    let db = Db::new();
    let (ep, mut old) = connected(&db);
    let mut new = Server::start(ep.addr, SECRETARY_TOKEN);
    new.initialize();
    new.initialized();
    let id = db.message("w-t1", "secretary", "one reader only");
    // Whichever holds the line gets it, and exactly one of the two gets it.
    let got = wait("a notification", || {
        old.notification(Duration::from_millis(20))
            .or_else(|| new.notification(Duration::from_millis(20)))
    });
    assert_eq!(got["content"], Db::wrapped(id, "w-t1", "one reader only"));
    assert!(
        !old.exit(Duration::from_secs(10)).success(),
        "the older server stops"
    );
    pause(800);
    assert!(old.notification(Duration::from_millis(50)).is_none());
    assert!(new.notification(Duration::from_millis(50)).is_none());
    assert_eq!(db.ledger(id).attempt, 1);
    ep.stop();
}
