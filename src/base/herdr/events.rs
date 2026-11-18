//! `events()` for herdr-core (spec section 6.5): one `events.subscribe` on a connection of its own.
//!
//! The base answers with a snapshot of terminals and then sends notifications, one JSON message per
//! line: `event` (a fact change, in order), `heartbeat` (every 15 s) and `missed` (the subscriber
//! fell behind; carries a fresh snapshot). Every snapshot is yielded as a `Resync`: the first one,
//! after a `missed` notice, after a heartbeat gap, and after every reconnect. A reconnect waits 1 s,
//! then 2 s and so on up to 30 s. `seq` is compared only with other numbers of the same connection
//! (events at or below the latest snapshot's are stale); it is forgotten when the connection ends,
//! because a restarted core counts from the start again (claude-org-ja #1235).
//!
//! The field names below are makiba's reading of the base's own descriptions of the method; they
//! are checked on the real base by `scripts/check-events-wire.sh`.

use super::{Herdr, TAG_ORG, terminal_of};
use crate::base::{
    BaseError, BaseEvent, Detail, EventStream, Terminal, TerminalEvent, TerminalRef,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// The base sends a heartbeat every 15 s; three missed ones end the connection.
const HEARTBEAT_GAP: Duration = Duration::from_secs(45);
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct Timing {
    gap: Duration,
    first: Duration,
    max: Duration,
}

const TIMING: Timing = Timing {
    gap: HEARTBEAT_GAP,
    first: BACKOFF_FIRST,
    max: BACKOFF_MAX,
};

impl Timing {
    /// How long to wait before the attempt that follows `failures` failed ones.
    fn backoff(&self, failures: u32) -> Duration {
        self.first
            .saturating_mul(1 << failures.min(16))
            .min(self.max)
    }
}

struct Conn {
    reader: BufReader<UnixStream>,
    /// A line read so far; kept across read timeouts so a slow line is not cut.
    buf: Vec<u8>,
    /// The `seq` of the latest snapshot on this connection.
    watermark: u64,
    /// Without a heartbeat by then the connection is given up.
    deadline: Instant,
}

pub struct Events {
    base: Herdr,
    timing: Timing,
    conn: Option<Conn>,
    queue: VecDeque<BaseEvent>,
    failures: u32,
}

impl Herdr {
    pub(super) fn events_stream(&self) -> Result<EventStream, BaseError> {
        Events::start(self.clone(), TIMING).map(|e| Box::new(e) as EventStream)
    }
}

impl Events {
    /// The first subscription is made here, so that its failure is the caller's to see.
    fn start(base: Herdr, timing: Timing) -> Result<Events, BaseError> {
        let mut events = Events {
            base,
            timing,
            conn: None,
            queue: VecDeque::new(),
            failures: 0,
        };
        events.subscribe()?;
        Ok(events)
    }

    fn subscribe(&mut self) -> Result<(), BaseError> {
        let (conn, terminals) = subscribe(&self.base, self.timing.gap)?;
        self.failures = 0;
        self.conn = Some(conn);
        self.queue.push_back(BaseEvent::Resync(terminals));
        Ok(())
    }
}

impl Iterator for Events {
    type Item = BaseEvent;

    /// Blocks. Never returns `None`: a lost connection is made again, as often as it takes.
    fn next(&mut self) -> Option<BaseEvent> {
        loop {
            if let Some(e) = self.queue.pop_front() {
                return Some(e);
            }
            let Some(conn) = self.conn.as_mut() else {
                std::thread::sleep(self.timing.backoff(self.failures));
                if self.subscribe().is_err() {
                    self.failures = self.failures.saturating_add(1);
                }
                continue;
            };
            match read_message(conn) {
                Some(message) => handle(
                    conn,
                    &message,
                    self.timing.gap,
                    self.base.org.as_deref(),
                    &mut self.queue,
                ),
                None => self.conn = None,
            }
        }
    }
}

fn subscribe(base: &Herdr, gap: Duration) -> Result<(Conn, Vec<Terminal>), BaseError> {
    let stream = UnixStream::connect(&base.socket)
        .map_err(|e| base.unavailable("connect", "cannot connect to", &e))?;
    exchange(base, stream, gap)
}

/// The subscribe request and its reply, on a connection that is open.
fn exchange(
    base: &Herdr,
    stream: UnixStream,
    gap: Duration,
) -> Result<(Conn, Vec<Terminal>), BaseError> {
    for set in [
        stream.set_read_timeout(Some(base.timeout)),
        stream.set_write_timeout(Some(base.timeout)),
    ] {
        set.map_err(|e| base.unavailable("socket", "cannot set a timeout on", &e))?;
    }
    // With an organization the base is asked for its terminals only; the answer is checked too.
    let params = base
        .org
        .as_deref()
        .map_or(json!({}), |org| json!({"labels": {TAG_ORG: org}}));
    let request =
        json!({"jsonrpc": "2.0", "id": 1, "method": "events.subscribe", "params": params});
    let mut writer = &stream;
    writeln!(writer, "{request}").map_err(|e| base.unavailable("write", "cannot write to", &e))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .map_err(|e| base.unavailable("read", "no answer from", &e))?;
    if n == 0 {
        return Err(base.unavailable("closed", "connection closed by", &"no answer"));
    }
    let reply: Value = serde_json::from_str(&line).map_err(|e| other(&e.to_string()))?;
    if let Some(err) = reply.get("error") {
        let text = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
        return Err(BaseError::Other(Detail {
            kind: text(err.pointer("/data/kind")).unwrap_or_else(|| "rpc".into()),
            message: text(err.get("message")).unwrap_or_else(|| err.to_string()),
            hint: text(err.pointer("/data/hint")),
        }));
    }
    let result = reply
        .get("result")
        .ok_or_else(|| other("no result in the reply"))?;
    let conn = Conn {
        reader,
        buf: Vec::new(),
        watermark: result.get("seq").and_then(Value::as_u64).unwrap_or(0),
        deadline: Instant::now() + gap,
    };
    Ok((conn, terminals_of(result, base.org.as_deref())))
}

fn other(message: &str) -> BaseError {
    BaseError::Other(Detail {
        kind: "events".into(),
        message: message.into(),
        hint: None,
    })
}

/// One message, or `None` when the connection is over: closed, bad data, or no heartbeat in time.
fn read_message(conn: &mut Conn) -> Option<Value> {
    loop {
        let left = conn.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        conn.reader.get_ref().set_read_timeout(Some(left)).ok()?;
        // One read at a time, so the deadline is checked between reads, not only between lines.
        match conn.reader.fill_buf() {
            Ok([]) => return None,
            Ok(chunk) => {
                let end = chunk
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(chunk.len(), |i| i + 1);
                conn.buf.extend_from_slice(&chunk[..end]);
                conn.reader.consume(end);
                if conn.buf.ends_with(b"\n") {
                    let parsed = serde_json::from_slice(&conn.buf);
                    let blank = conn.buf.trim_ascii().is_empty();
                    conn.buf.clear();
                    match parsed {
                        Ok(v) => return Some(v),
                        Err(_) if blank => {}
                        Err(_) => return None,
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

fn handle(
    conn: &mut Conn,
    message: &Value,
    gap: Duration,
    org: Option<&str>,
    out: &mut VecDeque<BaseEvent>,
) {
    let params = message.get("params").unwrap_or(&Value::Null);
    let seq = params.get("seq").and_then(Value::as_u64);
    match message.get("method").and_then(Value::as_str) {
        Some("heartbeat") => conn.deadline = Instant::now() + gap,
        Some("missed") => {
            conn.watermark = seq.unwrap_or(conn.watermark);
            out.push_back(BaseEvent::Resync(terminals_of(params, org)));
        }
        Some("event") => {
            if seq.is_some_and(|s| s <= conn.watermark) {
                return; // older than the snapshot taken on this connection
            }
            conn.watermark = seq.unwrap_or(conn.watermark);
            out.extend(event_of(params, org).map(BaseEvent::Event));
        }
        _ => {}
    }
}

/// Only this organization's terminals, when the base has one.
fn mine(t: &Terminal, org: Option<&str>) -> bool {
    org.is_none_or(|o| t.tags.get(TAG_ORG).map(String::as_str) == Some(o))
}

fn terminals_of(v: &Value, org: Option<&str>) -> Vec<Terminal> {
    v.get("terminals")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| terminal_of(t).ok())
                .filter(|t| mine(t, org))
                .collect()
        })
        .unwrap_or_default()
}

/// What a notice means for one terminal; `None` for facts makiba does not follow (size, doc).
fn event_of(params: &Value, org: Option<&str>) -> Option<TerminalEvent> {
    let wire = params.get("terminal").unwrap_or(&Value::Null);
    let wire_terminal = || terminal_of(wire).ok().filter(|t| mine(t, org));
    match params.get("type").and_then(Value::as_str)? {
        "created" => wire_terminal().map(TerminalEvent::Created),
        // A lost terminal is one that is not running any more, as far as the dispatcher can tell.
        "finished" | "lost" => wire_terminal().map(|mut t| {
            t.alive = false;
            TerminalEvent::Exited(t)
        }),
        "removed" => wire
            .get("id")
            .or_else(|| params.get("id"))
            .and_then(Value::as_str)
            .map(|id| TerminalEvent::Removed(TerminalRef(id.into()))),
        "status" | "turn" | "title" | "labels" | "restarted" | "keep" => {
            wire_terminal().map(TerminalEvent::StatusChanged)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::BlockKind;
    use std::collections::BTreeMap;

    #[test]
    fn the_backoff_doubles_from_one_second_to_thirty() {
        let secs: Vec<u64> = (0..8).map(|n| TIMING.backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(TIMING.backoff(u32::MAX), BACKOFF_MAX);
    }

    #[test]
    fn a_terminal_is_read_from_its_wire_form() {
        let t = terminal_of(&json!({"id": "t1", "labels": {"a": "b", "n": 3},
            "status": {"state": "blocked", "kind": "permission", "msg": "m"},
            "turn": 4, "exit": {"code": 2}}))
        .unwrap();
        assert_eq!(t.name, "t1");
        assert_eq!(t.tags, BTreeMap::from([("a".to_string(), "b".to_string())]));
        assert_eq!(t.status.unwrap().kind, Some(BlockKind::Permission));
        assert_eq!((t.turn, t.alive, t.exit_code), (Some(4), false, Some(2)));
        assert!(terminal_of(&json!({"name": "no id"})).is_err());
    }

    #[test]
    fn facts_makiba_does_not_follow_are_skipped() {
        assert!(event_of(&json!({"type": "size", "terminal": {"id": "t1"}}), None).is_none());
        assert!(event_of(&json!({"type": "doc"}), None).is_none());
        let lost = event_of(&json!({"type": "lost", "terminal": {"id": "t1"}}), None);
        assert!(matches!(lost, Some(TerminalEvent::Exited(t)) if !t.alive));
    }

    #[test]
    fn a_line_that_trickles_in_does_not_hold_the_deadline_off() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut conn = Conn {
            reader: BufReader::new(ours),
            buf: Vec::new(),
            watermark: 0,
            deadline: Instant::now() + Duration::from_millis(300),
        };
        let writer = std::thread::spawn(move || {
            let mut w = &theirs;
            for _ in 0..30 {
                if w.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        assert!(read_message(&mut conn).is_none());
        assert!(started.elapsed() < Duration::from_millis(900));
        drop(conn);
        writer.join().unwrap();
    }

    #[test]
    fn no_heartbeat_in_time_ends_the_connection_even_while_events_arrive() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut conn = Conn {
            reader: BufReader::new(ours),
            buf: Vec::new(),
            watermark: 0,
            deadline: Instant::now() + Duration::from_millis(300),
        };
        let writer = std::thread::spawn(move || {
            let mut w = &theirs;
            for _ in 0..20 {
                if writeln!(w, r#"{{"method":"event","params":{{}}}}"#).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        while read_message(&mut conn).is_some() {}
        assert!(started.elapsed() < Duration::from_millis(900));
        drop(conn);
        writer.join().unwrap();
    }

    /// The whole exchange over a connected pair: a snapshot, ordered events, a stale one dropped, a
    /// `missed` notice, and a heartbeat that is not an event.
    #[test]
    fn a_subscription_yields_a_snapshot_then_events_in_order() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(request.contains("events.subscribe"));
            let mut w = &theirs;
            let t = |state: &str| json!({"id": "t1", "status": {"state": state}});
            let lines = [
                json!({"id": 1, "result": {"seq": 5, "terminals": [t("idle")]}}),
                json!({"method": "event", "params": {"seq": 5, "type": "status", "terminal": t("idle")}}),
                json!({"method": "heartbeat"}),
                json!({"method": "event", "params": {"seq": 6, "type": "status", "terminal": t("working")}}),
                json!({"method": "missed", "params": {"seq": 9, "terminals": [t("done")]}}),
                json!({"method": "event", "params": {"seq": 9, "type": "status", "terminal": t("idle")}}),
                json!({"method": "event", "params": {"seq": 10, "type": "removed", "terminal": {"id": "t1"}}}),
            ];
            for l in lines {
                writeln!(w, "{l}").unwrap();
            }
        });
        let base = Herdr::new("unused".into(), Duration::from_secs(2));
        let (mut conn, snapshot) = exchange(&base, ours, Duration::from_secs(5)).unwrap();
        assert_eq!(snapshot.len(), 1);
        let mut out = VecDeque::new();
        while let Some(m) = read_message(&mut conn) {
            handle(&mut conn, &m, Duration::from_secs(5), None, &mut out);
            if out.len() == 3 {
                break;
            }
        }
        peer.join().unwrap();
        let shown: Vec<String> = out.iter().map(|e| format!("{e:?}")).collect();
        assert!(shown[0].contains("StatusChanged") && shown[0].contains("Working"));
        assert!(shown[1].starts_with("Resync") && shown[1].contains("Done"));
        assert!(shown[2].contains("Removed"), "{shown:?}");
    }

    #[test]
    fn with_an_organization_only_its_terminals_are_followed() {
        let mine = json!({"id": "a", "labels": {"makiba.org": "o"}});
        let theirs = json!({"id": "b", "labels": {"makiba.org": "x"}});
        let snapshot = json!({"terminals": [mine, theirs, {"id": "c"}]});
        assert_eq!(terminals_of(&snapshot, Some("o")).len(), 1);
        assert_eq!(terminals_of(&snapshot, None).len(), 3);
        let created = |t: &Value| event_of(&json!({"type": "created", "terminal": t}), Some("o"));
        assert!(created(&mine).is_some() && created(&theirs).is_none());
    }
}
