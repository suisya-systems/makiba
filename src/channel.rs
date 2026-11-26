//! The channel server (`makiba channel`, spec sections 4 and 8) and the side of `makibad` that
//! it talks to.
//!
//! The wire between them is one held request. The server asks `GET /channel/next` with
//! `Authorization: Bearer <token>` and the door's `Hub` keeps the request open, at most `HOLD`
//! (25 s). Answers: `200` with the wrapped text as the body, `204` when the hold ran out (the
//! server asks again at once), `401` for a token the door does not know (the server stops with a
//! failure), `409` when a newer request of the same agent took the place (the older server
//! stops, so two never take each other's place for ever). A server names its session in
//! `X-Makiba-Session: <start in nanoseconds>-<pid>`; the hub remembers the newest start it saw
//! for an agent and answers 409 to any request of an older one, even between two of its requests.
//!
//! A push counts as accepted only when the server has taken the text: it writes the MCP
//! notification to its standard output, and only then `ACK` back on the same connection. Until
//! that `ACK` the ledger is not touched (delivery records the attempt after `push` returns `Ok`).
//! Whether an agent's channel is connected is read from its held request being there; the hub
//! sees a dead server by the connection closing, within `PEEK_EVERY`.
//!
//! The watch of `makibad` (Issue 563, docs/design/makibad-watch.md): `makibad` cannot watch
//! itself, and the secretary's channel server is the one process outside it that always holds a
//! request to its door. Every answer to the secretary carries `BEAT_HEADER`, the heartbeat's age
//! read from `state.db` (`Hub::with_beat`); the door's threads are not the tick, so a stopped tick
//! still answers with a growing age. A server that has seen the header is the watcher: it gives
//! the secretary one `watch` notice when an answer's age reaches `status_local::STALE` (the tick
//! has stopped) or when the door has not answered for that long (`makibad` has stopped), and
//! another only after an answer with a fresh heartbeat. What it knows is in memory only.
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
pub(crate) fn read_head(r: &mut impl Read) -> std::io::Result<(Vec<u8>, usize)> {
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
    respond_with(s, status, "", body)
}

/// As `respond`, with `extra`, whole header lines each ending in CRLF, before the blank line.
fn respond_with(s: &mut TcpStream, status: &str, extra: &str, body: &str) -> std::io::Result<()> {
    let _ = s.set_write_timeout(Some(IO_LIMIT));
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    s.flush()
}

/// The header that carries the heartbeat's age to the secretary's channel server (Issue 563).
pub const BEAT_HEADER: &str = "X-Makiba-Heartbeat-Age";

/// How long a held request stays open: `HOLD`, or `MAKIBA_CHANNEL_HOLD_MS` (a test's seam).
fn hold() -> Duration {
    std::env::var("MAKIBA_CHANNEL_HOLD_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map_or(HOLD, Duration::from_millis)
}

// ---- the side of makibad ----------------------------------------------------------------------

struct Offer {
    text: String,
    reply: Sender<Result<(), String>>,
}

struct Slot {
    serial: u64,
    /// The hash of the token this held request came with (`agents.key_sha256`).
    key: String,
    tx: Sender<Offer>,
}

type Auth = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// The heartbeat's age in seconds, `None` when it cannot be read.
type Beat = Box<dyn Fn() -> Option<f64> + Send + Sync>;

struct Inner {
    auth: Auth,
    /// Set by `with_beat`: the answers to the secretary carry its value (Issue 563).
    beat: std::sync::OnceLock<Beat>,
    slots: Mutex<HashMap<String, Slot>>,
    /// The newest session start seen for each agent.
    newest: Mutex<HashMap<String, u128>>,
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
            beat: std::sync::OnceLock::new(),
            slots: Mutex::new(HashMap::new()),
            newest: Mutex::new(HashMap::new()),
            rejected: AtomicU64::new(0),
            next_gen: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        }))
    }

    /// With `beat`, the heartbeat's age: every answer to the secretary's requests carries it in
    /// `BEAT_HEADER`, so its channel server watches `makibad` (Issue 563). Set once, before the
    /// hub is cloned.
    pub fn with_beat(self, beat: impl Fn() -> Option<f64> + Send + Sync + 'static) -> Hub {
        let _ = self.0.beat.set(Box::new(beat));
        self
    }

    /// The header lines of an answer to `agent`: the heartbeat's age for the secretary.
    fn extra(&self, agent: &str) -> String {
        if agent != crate::spec::AGENT_SECRETARY {
            return String::new();
        }
        match self.0.beat.get().and_then(|f| f()) {
            Some(age) => format!("{BEAT_HEADER}: {age:.1}\r\n"),
            None => String::new(),
        }
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
        let Some((agent, token)) = token.and_then(|t| (self.0.auth)(t).map(|a| (a, t))) else {
            self.0.rejected.fetch_add(1, Ordering::SeqCst);
            let _ = respond(&mut s, "401 Unauthorized", "");
            return;
        };
        let start = req
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("x-makiba-session"))
            .and_then(|h| std::str::from_utf8(h.value).ok())
            .and_then(|v| v.split('-').next()?.parse::<u128>().ok())
            .unwrap_or(0);
        let (tx, rx) = channel::<Offer>();
        let serial = self.0.next_gen.fetch_add(1, Ordering::SeqCst);
        if self.0.closed.load(Ordering::SeqCst) {
            return;
        }
        {
            // One critical section: the check of the session and the replacement of the slot,
            // so an older session cannot overwrite a newer registration. The slot that is
            // replaced is dropped: its receiver disconnects and that request answers 409.
            let mut slots = self.0.slots.lock().unwrap();
            let mut newest = self.0.newest.lock().unwrap();
            let before = newest.get(&agent).copied();
            let seen = newest.entry(agent.clone()).or_insert(start);
            if start < *seen {
                drop((slots, newest));
                let _ = respond(&mut s, "409 Conflict", "");
                return;
            }
            *seen = start;
            // Issue 563: the first request of a new session of a watching server is answered at
            // once, so it learns it watches without waiting out a hold.
            let extra = self.extra(&agent);
            if !extra.is_empty() && before.is_none_or(|b| start > b) {
                drop((slots, newest));
                let _ = respond_with(&mut s, "204 No Content", &extra, "");
                return;
            }
            slots.insert(
                agent.clone(),
                Slot {
                    serial,
                    key: crate::daemon::door::sha256_hex(token),
                    tx,
                },
            );
        }
        let _reg = Registered {
            hub: &self.0,
            agent: agent.clone(),
            serial,
        };
        let until = Instant::now() + hold();
        while Instant::now() < until {
            if self.0.closed.load(Ordering::SeqCst) {
                return;
            }
            match rx.recv_timeout(PEEK_EVERY) {
                Ok(offer) => {
                    let r = hand_over(&mut s, &self.extra(&agent), &offer.text);
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
        let _ = respond_with(&mut s, "204 No Content", &self.extra(&agent), "");
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
fn hand_over(s: &mut TcpStream, extra: &str, text: &str) -> Result<(), String> {
    respond_with(s, "200 OK", extra, text).map_err(|e| format!("cannot write the answer: {e}"))?;
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
        self.push_keyed(recipient, text).map(|_| ())
    }

    fn push_keyed(&self, recipient: &str, text: &str) -> Result<Option<String>, String> {
        // The sender and the key are read from the one slot, under one lock.
        let (tx, key) = self
            .0
            .slots
            .lock()
            .unwrap()
            .get(recipient)
            .map(|s| (s.tx.clone(), s.key.clone()))
            .ok_or("no channel server is connected")?;
        let (reply, got) = channel();
        tx.send(Offer {
            text: text.to_string(),
            reply,
        })
        .map_err(|_| "the channel server went away")?;
        match got.recv_timeout(ACK_WAIT + IO_LIMIT + Duration::from_secs(1)) {
            Ok(r) => r.map(|()| Some(key)),
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
    /// A message, and the heartbeat's age if the answer carried it.
    Text(Option<f64>),
    /// The hold ran out, and the heartbeat's age if the answer carried it.
    Nothing(Option<f64>),
    Refused,
    Superseded,
    Failed(String),
}

fn ask(door: &str, token: &str, session: &str, out: &Out) -> Ask {
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
            "GET {PATH} HTTP/1.1\r\nHost: {door}\r\nAuthorization: Bearer {token}\r\nX-Makiba-Session: {session}\r\nConnection: close\r\n\r\n"
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
        let age = resp
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(BEAT_HEADER))
            .and_then(|h| {
                std::str::from_utf8(h.value)
                    .ok()?
                    .trim()
                    .parse::<f64>()
                    .ok()
            });
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
                Ok(Ask::Text(age))
            }
            Some(204) => Ok(Ask::Nothing(age)),
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

/// The watch of `makibad` (Issue 563, docs/design/makibad-watch.md), kept in memory by the
/// channel server only; a new server starts with nothing.
#[derive(Default)]
struct Watch {
    /// An answer carried the heartbeat's age: this is the secretary's server.
    watcher: bool,
    /// The first failed ask since the last answer.
    unreachable_since: Option<Instant>,
    /// A notice was given for this stale spell.
    noticed: bool,
}

impl Watch {
    /// An answer of the door, with the age it carried. Returns the notice to give, if any.
    fn answered(&mut self, age: Option<f64>) -> Option<String> {
        self.unreachable_since = None;
        let age = age?;
        self.watcher = true;
        if age < crate::status_local::STALE.as_secs_f64() {
            self.noticed = false;
            return None;
        }
        self.notice(format!(
            "makibad answers but its heartbeat is {age:.0} s old: its tick has stopped and it \
             cannot say so itself. Tell the person once: run makiba status, then makiba down; if \
             down says makibad is still running after 20 s, kill -KILL the pid status shows; then \
             makiba up."
        ))
    }

    /// A failed ask. Returns the notice to give, if any.
    fn failed(&mut self) -> Option<String> {
        let since = *self.unreachable_since.get_or_insert_with(Instant::now);
        let gone = since.elapsed();
        if gone < crate::status_local::STALE {
            return None;
        }
        self.notice(format!(
            "makibad has not answered for {} s: the door does not answer, so makibad has stopped \
             and cannot say so itself. Tell the person once: run makiba up; it names a stop that \
             was not recorded. If the person stopped it with makiba down, nothing is wrong.",
            gone.as_secs()
        ))
    }

    fn notice(&mut self, text: String) -> Option<String> {
        if !self.watcher || self.noticed {
            return None;
        }
        self.noticed = true;
        Some(format!(
            "<makiba from=\"makiba channel\" kind=\"watch\">\n{text}\n</makiba>"
        ))
    }
}

fn poll(door: String, token: String, session: String, out: Arc<Out>) {
    let mut wait = Duration::from_millis(200);
    let mut said = false;
    let mut watch = Watch::default();
    loop {
        match ask(&door, &token, &session, &out) {
            Ask::Text(age) | Ask::Nothing(age) => {
                wait = Duration::from_millis(200);
                said = false;
                if let Some(text) = watch.answered(age) {
                    out.notify(&text);
                }
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
                if let Some(text) = watch.failed() {
                    out.notify(&text);
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
    let session = format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
        std::process::id()
    );
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
                    let session = session.clone();
                    std::thread::spawn(move || poll(door, token, session, out));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn status_of(addr: std::net::SocketAddr, session: &str) -> TcpStream {
        let mut s = TcpStream::connect(addr).unwrap();
        write!(
            s,
            "GET {PATH} HTTP/1.1\r\nAuthorization: Bearer t\r\nX-Makiba-Session: {session}\r\n\r\n"
        )
        .unwrap();
        s
    }

    fn code(s: &mut TcpStream) -> String {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut b = [0u8; 12];
        s.read_exact(&mut b).unwrap();
        String::from_utf8_lossy(&b[9..12]).into_owned()
    }

    #[test]
    fn an_older_session_cannot_take_the_place_back() {
        let hub = Hub::new(|t| (t == "t").then(|| "a".to_string()));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = hub.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let h = h.clone();
                std::thread::spawn(move || h.serve(s));
            }
        });
        let mut old = status_of(addr, "100-1");
        while !hub.connected("a") {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _new = status_of(addr, "200-2");
        assert_eq!(code(&mut old), "409", "the held request of the old session");
        // Asking again, as the old server would after a 200, is refused at once.
        assert_eq!(code(&mut status_of(addr, "100-1")), "409");
        assert!(hub.connected("a"));
    }

    fn head_of(s: &mut TcpStream) -> String {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (buf, n) = read_head(s).unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    /// Issue 563: a new session of the secretary's server is answered at once with the
    /// heartbeat's age, so a makibad that dies before any hold ends is still watched; a worker's
    /// request carries no age and is held.
    #[test]
    fn a_new_secretary_session_learns_the_heartbeat_at_once() {
        let hub = Hub::new(|t| match t {
            "t" => Some(crate::spec::AGENT_SECRETARY.to_string()),
            "w" => Some("w-t1".to_string()),
            _ => None,
        })
        .with_beat(|| Some(1.5));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = hub.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let h = h.clone();
                std::thread::spawn(move || h.serve(s));
            }
        });
        let asked = Instant::now();
        let head = head_of(&mut status_of(addr, "100-1"));
        assert!(asked.elapsed() < HOLD / 2, "the first request was held");
        assert!(head.starts_with("HTTP/1.1 204"), "{head}");
        assert!(head.contains(&format!("{BEAT_HEADER}: 1.5\r\n")), "{head}");
        // The same session asks again: now it is held, as before.
        let _held = status_of(addr, "100-1");
        let until = Instant::now() + Duration::from_secs(5);
        while !hub.connected(crate::spec::AGENT_SECRETARY) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(hub.connected(crate::spec::AGENT_SECRETARY));
        // A worker's first request is held and carries no age.
        let mut s = TcpStream::connect(addr).unwrap();
        write!(
            s,
            "GET {PATH} HTTP/1.1\r\nAuthorization: Bearer w\r\nX-Makiba-Session: 100-2\r\n\r\n"
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !hub.connected("w-t1") && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(hub.connected("w-t1"), "the worker's request was not held");
    }
}
