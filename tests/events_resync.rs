//! M1-3: `events()` and resync (spec section 6.5). A stand-in core listens on a Unix socket, answers
//! `events.subscribe` with a snapshot, then pushes `event`, `heartbeat` and `missed` notices, one
//! JSON message per line. The test can drop every connection, or restart the core with other
//! terminals and a `seq` that begins again.
//!
//! The wire shape here is what the base's own strings describe (a snapshot, then `event`
//! notifications in order, a `heartbeat` every 15 s, `missed` with a fresh snapshot); the exact
//! field names are makiba's reading of them and are checked on the real base by
//! scripts/check-events-wire.sh. Positions (`seq`) are compared only within one connection.

use makiba::base::{
    Base, BaseError, BaseEvent, State, Status, Terminal, TerminalEvent, TerminalRef, connect,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Long enough for one backoff step (1 s) and a connect, short enough to fail fast.
const WAIT: Duration = Duration::from_secs(15);
const NOTHING: Duration = Duration::from_millis(700);

#[derive(Default)]
struct Shared {
    terminals: Vec<Value>,
    seq: u64,
    conns: Vec<UnixStream>,
    down: bool,
}

struct Core {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    shared: Arc<Mutex<Shared>>,
    subscribes: Arc<AtomicUsize>,
}

fn line(stream: &mut UnixStream, v: &Value) {
    let _ = writeln!(stream, "{v}");
}

impl Core {
    fn start(terminals: Vec<Value>) -> Core {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let shared = Arc::new(Mutex::new(Shared {
            terminals,
            ..Shared::default()
        }));
        let subscribes = Arc::new(AtomicUsize::new(0));
        let (sh, count) = (shared.clone(), subscribes.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (sh, count) = (sh.clone(), count.clone());
                thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut first = String::new();
                    if reader.read_line(&mut first).unwrap_or(0) == 0 {
                        return;
                    }
                    let request: Value = serde_json::from_str(&first).unwrap();
                    assert_eq!(request["jsonrpc"], "2.0");
                    assert_eq!(request["method"], "events.subscribe");
                    {
                        let mut s = sh.lock().unwrap();
                        if s.down {
                            return; // the base is restarting: the connection just closes
                        }
                        count.fetch_add(1, Ordering::SeqCst);
                        let reply = json!({"jsonrpc": "2.0", "id": request["id"],
                            "result": {"seq": s.seq, "terminals": s.terminals}});
                        line(&mut writer, &reply);
                        s.conns.push(writer);
                    }
                    // Hold the connection until the client goes away.
                    let mut rest = String::new();
                    while reader.read_line(&mut rest).unwrap_or(0) > 0 {
                        rest.clear();
                    }
                });
            }
        });
        Core {
            _dir: dir,
            socket,
            shared,
            subscribes,
        }
    }

    fn base(&self) -> Box<dyn Base> {
        connect(&self.socket, Duration::from_secs(2))
    }

    /// Send one notification to every subscriber.
    fn notify(&self, method: &str, params: Value) {
        let mut s = self.shared.lock().unwrap();
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        for c in &mut s.conns {
            line(c, &msg);
        }
    }

    /// An `event` with the next `seq`.
    fn event(&self, kind: &str, terminal: Value) {
        let seq = {
            let mut s = self.shared.lock().unwrap();
            s.seq += 1;
            s.seq
        };
        self.event_at(seq, kind, terminal);
    }

    fn event_at(&self, seq: u64, kind: &str, terminal: Value) {
        self.notify(
            "event",
            json!({"seq": seq, "type": kind, "terminal": terminal}),
        );
    }

    /// The connections break; the core itself stays.
    fn drop_connections(&self) {
        for c in self.shared.lock().unwrap().conns.drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }

    /// The core goes down and a new one comes up: other terminals, `seq` from the start.
    fn restart(&self, terminals: Vec<Value>) {
        self.shared.lock().unwrap().down = true;
        self.drop_connections();
        thread::sleep(Duration::from_millis(300));
        let mut s = self.shared.lock().unwrap();
        s.terminals = terminals;
        s.seq = 0;
        s.down = false;
    }
}

fn wire(id: &str, state: &str, turn: u64, exit: Option<i32>) -> Value {
    let mut t = json!({"id": id, "name": format!("name-{id}"),
        "labels": {"makiba.org": "org-1"},
        "status": {"state": state}, "turn": turn});
    if let Some(code) = exit {
        t["exit"] = json!({"code": code});
    }
    t
}

fn terminal(id: &str, state: State, turn: u64, exit: Option<i32>) -> Terminal {
    Terminal {
        id: TerminalRef(id.into()),
        name: format!("name-{id}"),
        tags: BTreeMap::from([("makiba.org".to_string(), "org-1".to_string())]),
        status: Some(Status {
            state,
            kind: None,
            msg: None,
        }),
        turn: Some(turn),
        alive: exit.is_none(),
        exit_code: exit,
    }
}

/// The stream, read on a thread so that a test can wait with a limit.
fn watch(base: &dyn Base) -> Receiver<BaseEvent> {
    let stream = base.events().expect("events");
    let (tx, rx) = channel();
    thread::spawn(move || {
        for e in stream {
            if tx.send(e).is_err() {
                return;
            }
        }
    });
    rx
}

fn next(rx: &Receiver<BaseEvent>) -> BaseEvent {
    rx.recv_timeout(WAIT).expect("an event within the limit")
}

fn resync(rx: &Receiver<BaseEvent>) -> Vec<Terminal> {
    match next(rx) {
        BaseEvent::Resync(list) => list,
        other => panic!("expected a Resync, got {other:?}"),
    }
}

fn quiet(rx: &Receiver<BaseEvent>) {
    if let Ok(e) = rx.recv_timeout(NOTHING) {
        panic!("expected nothing, got {e:?}");
    }
}

#[test]
fn the_first_thing_yielded_is_a_resync_with_the_terminals_that_exist() {
    let core = Core::start(vec![wire("t1", "idle", 0, None)]);
    let base = core.base();
    let rx = watch(&*base);
    assert_eq!(resync(&rx), vec![terminal("t1", State::Idle, 0, None)]);
    quiet(&rx);
}

#[test]
fn events_for_a_scripted_agent_arrive_in_order() {
    let core = Core::start(vec![]);
    let base = core.base();
    let rx = watch(&*base);
    assert!(resync(&rx).is_empty());

    core.event("created", wire("t7", "idle", 0, None));
    core.event("status", wire("t7", "working", 1, None));
    core.event("unknown-kind", wire("t7", "working", 1, None));
    core.notify("heartbeat", json!({}));
    core.event("status", wire("t7", "done", 1, None));
    core.event("finished", wire("t7", "done", 1, Some(0)));
    core.event("removed", json!({"id": "t7"}));

    let got: Vec<BaseEvent> = (0..5).map(|_| next(&rx)).collect();
    assert_eq!(
        got,
        vec![
            BaseEvent::Event(TerminalEvent::Created(terminal("t7", State::Idle, 0, None))),
            BaseEvent::Event(TerminalEvent::StatusChanged(terminal(
                "t7",
                State::Working,
                1,
                None
            ))),
            BaseEvent::Event(TerminalEvent::StatusChanged(terminal(
                "t7",
                State::Done,
                1,
                None
            ))),
            BaseEvent::Event(TerminalEvent::Exited(terminal(
                "t7",
                State::Done,
                1,
                Some(0)
            ))),
            BaseEvent::Event(TerminalEvent::Removed(TerminalRef("t7".into()))),
        ]
    );
    quiet(&rx);
}

#[test]
fn restarting_the_base_yields_a_resync_without_the_terminal_that_finished_in_the_gap() {
    let core = Core::start(vec![
        wire("keeps", "working", 3, None),
        wire("ends", "working", 2, None),
    ]);
    let base = core.base();
    let rx = watch(&*base);
    assert_eq!(resync(&rx).len(), 2);
    // Some traffic so the old connection's `seq` is well above where the new core begins.
    for turn in 4..8 {
        core.event("turn", wire("keeps", "working", turn, None));
        assert!(matches!(next(&rx), BaseEvent::Event(_)));
    }

    // `ends` finishes and goes while the core is down; the caller sees none of that happen.
    core.restart(vec![wire("keeps", "idle", 7, None)]);

    let list = resync(&rx);
    assert_eq!(list, vec![terminal("keeps", State::Idle, 7, None)]);
    assert!(list.iter().all(|t| t.id.0 != "ends"));

    // The new core's `seq` is 1 again; the old position is not compared with it.
    core.event("status", wire("keeps", "working", 8, None));
    assert_eq!(
        next(&rx),
        BaseEvent::Event(TerminalEvent::StatusChanged(terminal(
            "keeps",
            State::Working,
            8,
            None
        )))
    );
}

#[test]
fn a_dropped_connection_is_made_again_without_the_caller_doing_anything() {
    let core = Core::start(vec![wire("t1", "idle", 0, None)]);
    let base = core.base();
    let rx = watch(&*base);
    resync(&rx);
    assert_eq!(core.subscribes.load(Ordering::SeqCst), 1);

    core.drop_connections();

    assert_eq!(resync(&rx), vec![terminal("t1", State::Idle, 0, None)]);
    assert_eq!(core.subscribes.load(Ordering::SeqCst), 2);
    core.event("status", wire("t1", "working", 1, None));
    assert!(matches!(
        next(&rx),
        BaseEvent::Event(TerminalEvent::StatusChanged(_))
    ));
}

#[test]
fn a_base_that_stays_down_for_a_while_is_waited_for() {
    let core = Core::start(vec![wire("t1", "idle", 0, None)]);
    let base = core.base();
    let rx = watch(&*base);
    resync(&rx);
    core.shared.lock().unwrap().down = true;
    core.drop_connections();
    thread::sleep(Duration::from_millis(2500)); // two refused attempts: 1 s, then 2 s
    core.shared.lock().unwrap().down = false;
    assert_eq!(resync(&rx).len(), 1);
}

#[test]
fn a_missed_notice_yields_a_resync_and_older_events_on_that_connection_are_dropped() {
    let core = Core::start(vec![wire("t1", "idle", 0, None)]);
    let base = core.base();
    let rx = watch(&*base);
    resync(&rx);
    core.event_at(1, "status", wire("t1", "working", 1, None));
    next(&rx);

    // The subscriber fell behind: a fresh snapshot as of seq 10 follows the notice.
    core.notify(
        "missed",
        json!({"seq": 10, "terminals": [wire("t1", "done", 4, None)]}),
    );
    assert_eq!(resync(&rx), vec![terminal("t1", State::Done, 4, None)]);

    // Queued before the snapshot: the snapshot already says more. Then one after it.
    core.event_at(9, "status", wire("t1", "working", 3, None));
    core.event_at(10, "status", wire("t1", "working", 3, None));
    core.event_at(11, "status", wire("t1", "idle", 5, None));
    assert_eq!(
        next(&rx),
        BaseEvent::Event(TerminalEvent::StatusChanged(terminal(
            "t1",
            State::Idle,
            5,
            None
        )))
    );
    quiet(&rx);
    assert_eq!(core.subscribes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_base_that_is_not_running_is_refused_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let base = connect(&dir.path().join("none.sock"), Duration::from_millis(500));
    assert!(matches!(base.events(), Err(BaseError::Unavailable(_))));
}
