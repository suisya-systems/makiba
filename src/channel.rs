//! The channel server (`makiba channel`, spec sections 4 and 8) and the side of `makibad` that
//! it talks to.
//!
//! The wire between them is one held request. The server asks `GET /channel/next` with
//! `Authorization: Bearer <token>` and the door's `Hub` keeps the request open, at most `HOLD`
//! (25 s). Answers: `200` with the wrapped text as the body, `204` when the hold ran out (the
//! server asks again at once), `401` for a token the door does not know (the server stops with a
//! failure), `409` when a newer request of the same agent took the place (the older server
//! stops, so two never take each other's place for ever).
//!
//! A push counts as accepted only when the server has taken the text: it writes the MCP
//! notification to its standard output, and only then `ACK` back on the same connection. Until
//! that `ACK` the ledger is not touched (delivery records the attempt after `push` returns `Ok`).
//! Whether an agent's channel is connected is read from its held request being there; the hub
//! sees a dead server by the connection closing, within `PEEK_EVERY`.
//!
//! The ledger stays with `daemon::delivery`: this module carries already-wrapped text and never
//! reads or marks a message. Which agent a request is for comes from the token only (`Hub::new`
//! takes the function the door supplies); the token travels in the launch arguments.

use crate::daemon::delivery::Channel;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The path of the held request.
pub const PATH: &str = "/channel/next";
/// How long a held request stays open with nothing to say.
pub const HOLD: Duration = Duration::from_secs(25);
/// How often a held connection is looked at for a peer that has gone.
const PEEK_EVERY: Duration = Duration::from_millis(100);
/// How long the hub waits for `ACK` after it wrote the answer.
const ACK_WAIT: Duration = Duration::from_millis(1500);
const IO_LIMIT: Duration = Duration::from_secs(5);
const HEAD_MAX: usize = 8192;
/// A wrapped text is at most about `BODY_LIMIT` characters plus its header.
const BODY_MAX: usize = 64 * 1024;

/// Read from `r` until the end of an HTTP head; returns the bytes read (the head and maybe the
/// start of the body) and the head's length. Bounded by `HEAD_MAX`.
fn read_head(r: &mut impl Read) -> std::io::Result<(Vec<u8>, usize)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok((buf, i + 4));
        }
        if buf.len() > HEAD_MAX {
            return Err(std::io::Error::other("head too long"));
        }
        let n = r.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn respond(s: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let _ = s.set_write_timeout(Some(IO_LIMIT));
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    s.flush()
}

// ---- the side of makibad ----------------------------------------------------------------------

struct Offer {
    text: String,
    reply: Sender<Result<(), String>>,
}

struct Slot {
    serial: u64,
    tx: Sender<Offer>,
}

type Auth = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

struct Inner {
    auth: Auth,
    slots: Mutex<HashMap<String, Slot>>,
    rejected: AtomicU64,
    next_gen: AtomicU64,
    closed: AtomicBool,
}

/// The channel servers' held requests, and the `Channel` that delivery pushes through.
#[derive(Clone)]
pub struct Hub(Arc<Inner>);

/// Removes a held request's slot when its connection ends, whichever way.
struct Registered<'a> {
    hub: &'a Inner,
    agent: String,
    serial: u64,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        let mut slots = self.hub.slots.lock().unwrap();
        if slots
            .get(&self.agent)
            .is_some_and(|s| s.serial == self.serial)
        {
            slots.remove(&self.agent);
        }
    }
}

impl Hub {
    /// `auth` maps a token to the agent id it is bound to; `None` for an unknown token.
    pub fn new(auth: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Hub {
        Hub(Arc::new(Inner {
            auth: Box::new(auth),
            slots: Mutex::new(HashMap::new()),
            rejected: AtomicU64::new(0),
            next_gen: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        }))
    }

    /// Is the agent's channel server holding a request now?
    pub fn connected(&self, agent: &str) -> bool {
        self.0.slots.lock().unwrap().contains_key(agent)
    }

    /// How many requests were refused for a token the door does not know.
    pub fn rejected(&self) -> u64 {
        self.0.rejected.load(Ordering::SeqCst)
    }

    /// Drop every held request (a stop of `makibad`).
    pub fn close(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.slots.lock().unwrap().clear();
    }

    /// Serve one connection of the door that asks for the channel path; returns when it ends.
    /// The door calls it on a thread of its own.
    pub fn serve(&self, mut s: TcpStream) {
        let _ = s.set_read_timeout(Some(IO_LIMIT));
        let Ok((buf, n)) = read_head(&mut s) else {
            return;
        };
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut req = httparse::Request::new(&mut headers);
        if !matches!(req.parse(&buf[..n]), Ok(httparse::Status::Complete(_))) {
            let _ = respond(&mut s, "400 Bad Request", "");
            return;
        }
        if req.method != Some("GET") || req.path != Some(PATH) {
            let _ = respond(&mut s, "404 Not Found", "");
            return;
        }
        let token = req
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("authorization"))
            .and_then(|h| std::str::from_utf8(h.value).ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let Some(agent) = token.and_then(|t| (self.0.auth)(t)) else {
            self.0.rejected.fetch_add(1, Ordering::SeqCst);
            let _ = respond(&mut s, "401 Unauthorized", "");
            return;
        };
        let (tx, rx) = channel::<Offer>();
        let serial = self.0.next_gen.fetch_add(1, Ordering::SeqCst);
        if self.0.closed.load(Ordering::SeqCst) {
            return;
        }
        // A request already held for this agent is dropped from the map here: its receiver
        // disconnects and it answers 409.
        self.0
            .slots
            .lock()
            .unwrap()
            .insert(agent.clone(), Slot { serial, tx });
        let _reg = Registered {
            hub: &self.0,
            agent,
            serial,
        };
        let until = Instant::now() + HOLD;
        while Instant::now() < until {
            if self.0.closed.load(Ordering::SeqCst) {
                return;
            }
            match rx.recv_timeout(PEEK_EVERY) {
                Ok(offer) => {
                    let r = hand_over(&mut s, &offer.text);
                    let _ = offer.reply.send(r);
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if peer_gone(&s) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if !self.0.closed.load(Ordering::SeqCst) {
                        let _ = respond(&mut s, "409 Conflict", "");
                    }
                    return;
                }
            }
        }
        let _ = respond(&mut s, "204 No Content", "");
    }
}

/// Has the client closed its end? (The client sends nothing after its request.)
fn peer_gone(s: &TcpStream) -> bool {
    if s.set_nonblocking(true).is_err() {
        return true;
    }
    let mut b = [0u8; 1];
    let gone = match s.peek(&mut b) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
    };
    gone || s.set_nonblocking(false).is_err()
}

/// Write the answer and wait for the server's `ACK`: only then is the text taken.
fn hand_over(s: &mut TcpStream, text: &str) -> Result<(), String> {
    respond(s, "200 OK", text).map_err(|e| format!("cannot write the answer: {e}"))?;
    let _ = s.set_read_timeout(Some(ACK_WAIT));
    let mut ack = [0u8; 3];
    s.read_exact(&mut ack)
        .map_err(|e| format!("the channel server did not acknowledge: {e}"))?;
    if &ack == b"ACK" {
        let _ = s.shutdown(Shutdown::Both);
        Ok(())
    } else {
        Err("the channel server answered something else than ACK".into())
    }
}

impl Channel for Hub {
    fn reaches(&self, recipient: &str) -> bool {
        self.connected(recipient)
    }

    fn push(&self, recipient: &str, text: &str) -> Result<(), String> {
        let tx = self
            .0
            .slots
            .lock()
            .unwrap()
            .get(recipient)
            .map(|s| s.tx.clone())
            .ok_or("no channel server is connected")?;
        let (reply, got) = channel();
        tx.send(Offer {
            text: text.to_string(),
            reply,
        })
        .map_err(|_| "the channel server went away")?;
        match got.recv_timeout(ACK_WAIT + IO_LIMIT + Duration::from_secs(1)) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => {
                Err("the channel server did not answer in time".into())
            }
            Err(RecvTimeoutError::Disconnected) => Err("the channel server went away".into()),
        }
    }
}

// ---- the channel server (`makiba channel`) ----------------------------------------------------

/// MCP protocol versions the server answers with as asked; any other gets the first.
const PROTOCOLS: [&str; 3] = ["2025-03-26", "2025-06-18", "2024-11-05"];

/// What one pass of asking the door came to.
enum Ask {
    Text,
    Nothing,
    Refused,
    Superseded,
    Failed(String),
}

fn ask(door: &str, token: &str, out: &Out) -> Ask {
    let run = || -> Result<Ask, String> {
        let addr = door
            .to_socket_addrs()
            .map_err(|e| format!("bad door address: {e}"))?
            .next()
            .ok_or("bad door address")?;
        let mut s = TcpStream::connect_timeout(&addr, IO_LIMIT).map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(IO_LIMIT))
            .map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(HOLD + IO_LIMIT * 2))
            .map_err(|e| e.to_string())?;
        write!(
            s,
            "GET {PATH} HTTP/1.1\r\nHost: {door}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        )
        .map_err(|e| e.to_string())?;
        let (mut buf, n) = read_head(&mut s).map_err(|e| e.to_string())?;
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut resp = httparse::Response::new(&mut headers);
        resp.parse(&buf[..n]).map_err(|e| e.to_string())?;
        let len = resp
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-length"))
            .and_then(|h| {
                std::str::from_utf8(h.value)
                    .ok()?
                    .trim()
                    .parse::<usize>()
                    .ok()
            })
            .unwrap_or(0);
        match resp.code {
            Some(200) if len > 0 && len <= BODY_MAX => {
                let mut body = buf.split_off(n);
                while body.len() < len {
                    let mut chunk = [0u8; 4096];
                    let k = s.read(&mut chunk).map_err(|e| e.to_string())?;
                    if k == 0 {
                        return Err("the answer ended early".into());
                    }
                    body.extend_from_slice(&chunk[..k]);
                }
                body.truncate(len);
                let text = String::from_utf8(body).map_err(|e| e.to_string())?;
                // Taken only when it is written out: then, and only then, `ACK`.
                out.notify(&text);
                s.write_all(b"ACK").map_err(|e| e.to_string())?;
                Ok(Ask::Text)
            }
            Some(204) => Ok(Ask::Nothing),
            Some(401) => Ok(Ask::Refused),
            Some(409) => Ok(Ask::Superseded),
            c => Err(format!("unexpected answer {c:?}")),
        }
    };
    run().unwrap_or_else(Ask::Failed)
}

/// Standard output, written by one writer at a time and flushed; a failed write ends the process.
struct Out(Mutex<std::io::Stdout>);

impl Out {
    fn emit(&self, v: &Value) {
        let mut o = self.0.lock().unwrap();
        if writeln!(o, "{v}").and_then(|()| o.flush()).is_err() {
            std::process::exit(1);
        }
    }

    fn notify(&self, text: &str) {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| format!("{}.{:03}", d.as_secs(), d.subsec_millis()))
            .unwrap_or_default();
        self.emit(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel",
            "params": {"content": text, "meta": {"sent_at": at}}
        }));
    }
}

fn poll(door: String, token: String, out: Arc<Out>) {
    let mut wait = Duration::from_millis(200);
    let mut said = false;
    loop {
        match ask(&door, &token, &out) {
            Ask::Text | Ask::Nothing => {
                wait = Duration::from_millis(200);
                said = false;
            }
            Ask::Refused => {
                eprintln!("makiba channel: the door refused the token");
                std::process::exit(3);
            }
            Ask::Superseded => {
                eprintln!("makiba channel: a newer channel server of this agent took over");
                std::process::exit(4);
            }
            Ask::Failed(e) => {
                if !said {
                    eprintln!("makiba channel: cannot reach the door: {e}");
                    said = true;
                }
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(2));
            }
        }
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// `makiba channel --door <host:port> --token <token>`: MCP on standard input and output.
pub fn main(args: &[String]) -> ExitCode {
    let (Some(door), Some(token)) = (arg(args, "--door"), arg(args, "--token")) else {
        eprintln!("usage: makiba channel --door <host:port> --token <token>");
        return ExitCode::from(2);
    };
    let out = Arc::new(Out(Mutex::new(std::io::stdout())));
    let started = AtomicBool::new(false);
    let mut line = String::new();
    let stdin = std::io::stdin();
    // The messages come only after `notifications/initialized`, so the poller starts there.
    while stdin.read_line(&mut line).is_ok_and(|n| n > 0) {
        let msg: Option<Value> = serde_json::from_str(line.trim()).ok();
        line.clear();
        let Some(msg) = msg else { continue };
        let (method, id) = (msg["method"].as_str().unwrap_or(""), msg.get("id"));
        let reply = |result: Value| {
            if let Some(id) = id {
                out.emit(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
        };
        match method {
            "initialize" => {
                let want = msg["params"]["protocolVersion"].as_str().unwrap_or("");
                let version = PROTOCOLS
                    .iter()
                    .find(|p| **p == want)
                    .unwrap_or(&PROTOCOLS[0]);
                reply(json!({
                    "protocolVersion": version,
                    "capabilities": {"experimental": {"claude/channel": {}}},
                    "serverInfo": {"name": "makiba", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Messages from other agents arrive as channel notifications. There is no tool to call."
                }));
            }
            "notifications/initialized" => {
                if !started.swap(true, Ordering::SeqCst) {
                    let (door, token, out) = (door.clone(), token.clone(), Arc::clone(&out));
                    std::thread::spawn(move || poll(door, token, out));
                }
            }
            "ping" => reply(json!({})),
            "tools/list" => reply(json!({"tools": []})),
            "resources/list" => reply(json!({"resources": []})),
            "prompts/list" => reply(json!({"prompts": []})),
            _ => {
                if let Some(id) = id {
                    out.emit(&json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "method not found"}}));
                }
            }
        }
    }
    ExitCode::SUCCESS
}
