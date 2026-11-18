//! M2-7: the box (`inbox`), the wake line and typed delivery (spec sections 7 and 8).
//!
//! Everything runs in this process: a `Delivery` is driven pass by pass on a state database in a
//! temporary folder, the base is the stand-in wrapped so that what is typed into a terminal is
//! noted and the screen can be set, and the clock is a manual one, so the 10 s and 60 s rules are
//! tested at their edges without waiting. Nothing here starts a daemon or a terminal program.
//!
//! The public seams this file fixes:
//! - `daemon::inbox::inbox(&StateWriter, caller) -> Inbox`: the function behind the door verb.
//! - `Delivery::with_clock`, `Delivery::with_agent`, `Delivery::wake_pass(&StateWriter, &dyn Base)`.
//! - `daemon::delivery::send_typed(...)` and `TypedError`: the function behind `typed`.
//! - The test agent (`Fake`) reads the prompt line as the last screen line that begins with `>`:
//!   nothing after the `>` (a no-break space counts as nothing) is an empty prompt, anything else
//!   is typed input, and a screen with no such line is not understood. Nothing is typed unless the
//!   prompt is empty.

use makiba::agents::fake::Fake;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, Detail, EventStream, LaunchSpec, State, Terminal,
    TerminalRef,
};
use makiba::daemon::delivery::{Channel, Delivery, TypedError, send_typed};
use makiba::daemon::inbox::{INBOX_LIMIT, inbox};
use makiba::db::writer::{NewEvent, StateWriter};
use makiba::spec::WAKE_LINE;
use rusqlite::params;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

// ---- the stand-ins ----------------------------------------------------------------------------

/// What was typed: `(terminal, text, only_if_idle)`.
type Sent = (String, String, bool);

/// The stand-in base, with a screen the test sets, a note of every text typed, and a failure the
/// next `send` can be told to return.
struct TestBase {
    inner: Stub,
    screen: Mutex<Vec<String>>,
    sent: Mutex<Vec<Sent>>,
    fail_send: Mutex<Option<BaseError>>,
}

impl TestBase {
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    fn wakes(&self) -> Vec<Sent> {
        self.sent()
            .into_iter()
            .filter(|s| s.1 == WAKE_LINE)
            .collect()
    }
}

impl Base for TestBase {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.inner.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.inner.launch(spec)
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        self.inner.list()
    }
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.inner.get(t)
    }
    fn read(&self, t: &TerminalRef, _lines: usize) -> Result<Vec<String>, BaseError> {
        self.inner.get(t)?;
        Ok(self.screen.lock().unwrap().clone())
    }
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.inner.keys(t, keys)
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        if let Some(e) = self.fail_send.lock().unwrap().take() {
            return Err(e);
        }
        self.inner.send(t, text, only_if_idle)?;
        self.sent
            .lock()
            .unwrap()
            .push((t.0.clone(), text.to_string(), only_if_idle));
        Ok(())
    }
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        self.inner.end(t)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.inner.events()
    }
    fn start(&self) -> Result<(), BaseError> {
        self.inner.start()
    }
    fn worker_denies(&self) -> BaseDenies {
        self.inner.worker_denies()
    }
}

/// A clock that moves only when the test says so.
#[derive(Clone)]
struct TestClock(Arc<Mutex<SystemTime>>);

impl TestClock {
    fn advance(&self, secs: u64) {
        *self.0.lock().unwrap() += Duration::from_secs(secs);
    }
    fn reader(&self) -> Box<dyn Fn() -> SystemTime + Send> {
        let c = Arc::clone(&self.0);
        Box::new(move || *c.lock().unwrap())
    }
}

/// A channel with a note of what it was asked to push: `(recipient, text)`.
struct RecChannel {
    reaches: Vec<String>,
    pushes: Arc<Mutex<Vec<(String, String)>>>,
}

impl Channel for RecChannel {
    fn reaches(&self, recipient: &str) -> bool {
        self.reaches.iter().any(|r| r == recipient)
    }
    fn push(&self, recipient: &str, text: &str) -> Result<(), String> {
        self.pushes
            .lock()
            .unwrap()
            .push((recipient.to_string(), text.to_string()));
        Ok(())
    }
}

// ---- the organization under test --------------------------------------------------------------

struct Org {
    _tmp: tempfile::TempDir,
    w: StateWriter,
    base: Arc<TestBase>,
    clock: TestClock,
    pushes: Arc<Mutex<Vec<(String, String)>>>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
struct Ledger {
    status: String,
    attempt: i64,
    first_attempt_at: Option<String>,
    last_attempt_at: Option<String>,
    delivered_at: Option<String>,
}

fn wrapped(from: &str, kind: &str, id: i64, body: &str) -> String {
    format!("<makiba from=\"{from}\" kind=\"{kind}\" id=\"{id}\">\n{body}\n</makiba>")
}

fn screen(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

impl Org {
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
        let base = Arc::new(TestBase {
            inner: Stub::new(),
            screen: Mutex::new(screen(&["> "])),
            sent: Mutex::new(Vec::new()),
            fail_send: Mutex::new(None),
        });
        Org {
            _tmp: tmp,
            w,
            base,
            clock: TestClock(Arc::new(Mutex::new(SystemTime::now()))),
            pushes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A delivery whose channel reaches `reach`, on the manual clock, for the test agent.
    fn delivery(&self, reach: &[&str]) -> Delivery {
        let channel = RecChannel {
            reaches: reach.iter().map(|r| r.to_string()).collect(),
            pushes: Arc::clone(&self.pushes),
        };
        Delivery::new(Box::new(channel))
            .with_window(Duration::from_millis(50))
            .with_clock(self.clock.reader())
            .with_agent(Box::new(Fake::new(
                Path::new("agent"),
                Path::new("rec"),
                &[],
            )))
    }

    /// A terminal of the organization, as the base lists it, with the tags `makibad` gives it.
    fn terminal(&self, role: &str, task: Option<&str>, state: State) {
        let name = match task {
            Some(t) => format!("w-{t}"),
            None => role.to_string(),
        };
        let mut tags = BTreeMap::new();
        tags.insert("makiba.role".to_string(), role.to_string());
        if let Some(t) = task {
            tags.insert("makiba.task_id".to_string(), t.to_string());
        }
        let id = self
            .base
            .launch(&LaunchSpec {
                argv: Vec::new(),
                cwd: ".".into(),
                name,
                tags,
                env: BTreeMap::new(),
                unset: Vec::new(),
            })
            .unwrap();
        self.base.inner.set_state(&id, state);
    }

    fn set_state(&self, name: &str, state: State) {
        self.base.inner.set_state(&TerminalRef(name.into()), state);
    }

    fn set_screen(&self, lines: &[&str]) {
        *self.base.screen.lock().unwrap() = screen(lines);
    }

    /// The record the monitor writes for a terminal that reports `state`.
    fn status(&self, role: &str, task: Option<&str>, state: &str) {
        self.w
            .append_event(&NewEvent {
                kind: "terminal_status",
                actor: Some("makibad"),
                payload: Some(&json!({
                    "task_id": task, "role": role, "state": state, "kind": null, "msg": null,
                })),
                ..Default::default()
            })
            .unwrap();
    }

    /// A stored message and its `pending` ledger row, as `send` leaves them (spec section 5.4).
    fn message(&self, sender: &str, recipient: &str, kind: &str, body: &str) -> i64 {
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
            "INSERT INTO messages (event_id, sender, recipient, kind, body) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event, sender, recipient, kind, body],
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

    fn ledger(&self, message: i64) -> Ledger {
        self.w
            .conn()
            .query_row(
                "SELECT d.status, d.attempt, d.first_attempt_at, d.last_attempt_at, d.delivered_at
                 FROM messages m JOIN event_deliveries d
                   ON d.source_event_id = m.event_id AND d.recipient = m.recipient
                 WHERE m.id = ?1",
                [message],
                |r| {
                    Ok(Ledger {
                        status: r.get(0)?,
                        attempt: r.get(1)?,
                        first_attempt_at: r.get(2)?,
                        last_attempt_at: r.get(3)?,
                        delivered_at: r.get(4)?,
                    })
                },
            )
            .unwrap()
    }

    fn count(&self, table: &str) -> i64 {
        self.w
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn message_events(&self) -> i64 {
        self.w
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'message_sent'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn pushed(&self) -> Vec<(String, String)> {
        self.pushes.lock().unwrap().clone()
    }

    /// Nothing was stored for a refused typed request, and nothing was typed.
    fn assert_nothing_stored_or_typed(&self) {
        assert_eq!(self.count("messages"), 0, "no message stored");
        assert_eq!(self.count("event_deliveries"), 0, "no ledger row stored");
        assert_eq!(self.message_events(), 0, "no message event stored");
        assert!(self.base.sent().is_empty(), "nothing typed");
    }
}

// ---- the wake line ----------------------------------------------------------------------------

#[test]
fn a_push_after_which_no_turn_starts_is_woken_once_and_the_box_delivers_it() {
    let s = Org::new();
    s.terminal("worker", Some("t1"), State::Idle);
    s.status("worker", Some("t1"), "idle");
    let id = s.message("secretary", "w-t1", "message", "next step");
    let mut d = s.delivery(&["w-t1"]);

    d.pass(&s.w).unwrap();
    assert_eq!(s.pushed().len(), 1, "the channel took the message");
    let pushed = s.ledger(id);
    assert_eq!((pushed.status.as_str(), pushed.attempt), ("pending", 1));

    // Inside 10 s of the push nothing is typed; no turn has started.
    d.wake_pass(&s.w, &*s.base).unwrap();
    s.clock.advance(9);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert!(
        s.base.wakes().is_empty(),
        "a push 9 s ago is not yet unconfirmed"
    );

    // Past 10 s: the wake line, once, into the idle recipient (the base checks it is idle).
    s.clock.advance(2);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(
        s.base.sent(),
        vec![("w-t1".to_string(), WAKE_LINE.to_string(), true)]
    );
    assert_eq!(s.ledger(id), pushed, "the wake line is not a delivery");

    // Not again within 60 s of it, nor from a second pass.
    d.wake_pass(&s.w, &*s.base).unwrap();
    s.clock.advance(20);
    d.wake_pass(&s.w, &*s.base).unwrap();
    s.clock.advance(39);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(
        s.base.wakes().len(),
        1,
        "59 s after the line: not twice in 60 s"
    );
    s.clock.advance(2);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(
        s.base.wakes().len(),
        2,
        "61 s on, the message is still unread"
    );

    // The recipient reads its box: the same message, the same id, and now it is delivered.
    let got = inbox(&s.w, "w-t1").unwrap();
    assert!(!got.more);
    assert_eq!(got.messages.len(), 1);
    assert_eq!(got.messages[0].id, id);
    assert_eq!(
        got.messages[0].text,
        wrapped("secretary", "message", id, "next step")
    );
    assert_eq!(
        s.pushed()[0].1,
        got.messages[0].text,
        "same text as the push"
    );
    let after = s.ledger(id);
    assert_eq!(after.status, "delivered");
    assert!(after.delivered_at.is_some());
    assert_eq!(after.attempt, 1, "reading the box is not a push");
    assert_eq!(after.last_attempt_at, pushed.last_attempt_at);

    // Nothing is unconfirmed any more: no more lines, nothing more in the box.
    s.clock.advance(200);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(s.base.wakes().len(), 2);
    assert!(inbox(&s.w, "w-t1").unwrap().messages.is_empty());
}

#[test]
fn a_message_pushed_while_working_is_woken_when_the_turn_ends() {
    let s = Org::new();
    s.terminal("worker", Some("t2"), State::Working);
    s.status("worker", Some("t2"), "working");
    let id = s.message("secretary", "w-t2", "message", "while you work");
    let mut d = s.delivery(&["w-t2"]);

    d.pass(&s.w).unwrap();
    assert_eq!(
        s.pushed().len(),
        1,
        "a channel reaches a working recipient at once"
    );
    let pushed = s.ledger(id);
    assert_eq!((pushed.status.as_str(), pushed.attempt), ("pending", 1));

    // Still working a long time later: nothing is typed into a working terminal.
    s.clock.advance(120);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert!(s.base.sent().is_empty());

    // The turn ends (`done`: the status is read just before the send, no `only_if_idle`).
    s.set_state("w-t2", State::Done);
    s.status("worker", Some("t2"), "done");
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(
        s.base.sent(),
        vec![("w-t2".to_string(), WAKE_LINE.to_string(), false)]
    );

    let got = inbox(&s.w, "w-t2").unwrap();
    assert_eq!(got.messages.len(), 1);
    assert_eq!(got.messages[0].id, id, "the id it was pushed with");
    assert_eq!(
        got.messages[0].text,
        wrapped("secretary", "message", id, "while you work")
    );
    assert_eq!(s.ledger(id).status, "delivered");
}

#[test]
fn a_blocked_or_failed_recipient_is_not_woken() {
    let s = Org::new();
    s.terminal("worker", Some("t3"), State::Blocked);
    s.terminal("worker", Some("t4"), State::Error);
    s.message("secretary", "w-t3", "message", "a");
    s.message("secretary", "w-t4", "message", "b");
    let mut d = s.delivery(&[]);
    d.pass(&s.w).unwrap();
    s.clock.advance(300);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert!(s.base.sent().is_empty());
}

#[test]
fn a_recipient_with_no_channel_gets_the_wake_line_and_the_box_only() {
    let s = Org::new();
    s.terminal("worker", Some("t5"), State::Idle);
    s.status("worker", Some("t5"), "idle");
    let id = s.message("secretary", "w-t5", "message", "by the box");
    // The recipient's agent program has no channel, or its server is not connected.
    let mut d = s.delivery(&[]);

    d.pass(&s.w).unwrap();
    assert!(s.pushed().is_empty(), "nothing is pushed");
    let l = s.ledger(id);
    assert_eq!((l.status.as_str(), l.attempt), ("pending", 0));

    // Unconfirmed from the start: no push to wait out, so the line is typed at once.
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(
        s.base.sent(),
        vec![("w-t5".to_string(), WAKE_LINE.to_string(), true)]
    );
    assert!(s.pushed().is_empty(), "the wake line is not a push");

    let got = inbox(&s.w, "w-t5").unwrap();
    assert_eq!(got.messages.len(), 1);
    assert_eq!(got.messages[0].id, id);
    assert_eq!(
        got.messages[0].text,
        wrapped("secretary", "message", id, "by the box")
    );
    let l = s.ledger(id);
    assert_eq!(l.status, "delivered");
    assert!(l.delivered_at.is_some());
    assert_eq!(l.attempt, 0, "it was never pushed");
}

#[test]
fn the_wake_line_is_not_typed_over_input_or_into_a_screen_that_is_not_understood() {
    let s = Org::new();
    s.terminal("worker", Some("t6"), State::Idle);
    s.message("secretary", "w-t6", "message", "wake me");
    let mut d = s.delivery(&[]);

    for typed_input in [
        vec!["output", "> half a sentence"],
        vec!["output", "> first line of a draft", "  second line of it"],
        vec!["output", "> x", "  ? for shortcuts"],
        // Not understood: no prompt line, an empty screen.
        vec!["output only", "no prompt here"],
        vec![],
    ] {
        s.set_screen(&typed_input);
        d.wake_pass(&s.w, &*s.base).unwrap();
        assert!(
            s.base.sent().is_empty(),
            "nothing is typed when the screen is {typed_input:?}"
        );
    }

    // An empty prompt, with the footer under it, and a no-break space after the `>`.
    s.set_screen(&["output", "> \u{a0}", "  ? for shortcuts"]);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(s.base.wakes().len(), 1);

    // Not twice in 60 s, with an empty prompt both times; again after.
    s.set_screen(&["> "]);
    s.clock.advance(59);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(s.base.wakes().len(), 1, "not twice in 60 s");
    s.clock.advance(2);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert_eq!(s.base.wakes().len(), 2);
}

#[test]
fn a_failed_wake_line_is_not_counted_and_nothing_is_marked_delivered() {
    let s = Org::new();
    s.terminal("worker", Some("t7"), State::Idle);
    let id = s.message("secretary", "w-t7", "message", "x");
    let mut d = s.delivery(&[]);
    *s.base.fail_send.lock().unwrap() = Some(BaseError::Unavailable(Detail {
        kind: "timeout".into(),
        message: "no answer".into(),
        hint: None,
    }));
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert!(s.base.sent().is_empty());
    assert_eq!(s.ledger(id).status, "pending");
    // The next try, after a short back-off in real time or at once, may type it: no 60 s hold
    // is taken by a send that failed.
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while s.base.wakes().is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
        d.wake_pass(&s.w, &*s.base).unwrap();
    }
    assert_eq!(s.base.wakes().len(), 1);
}

// ---- typed delivery ---------------------------------------------------------------------------

#[test]
fn typed_to_an_idle_worker_types_the_wrapped_text_once_and_is_delivered() {
    let s = Org::new();
    s.terminal("worker", Some("t8"), State::Idle);
    let id = send_typed(&s.w, &*s.base, "secretary", "w-t8", "message", "do it").unwrap();
    assert_eq!(
        s.base.sent(),
        vec![(
            "w-t8".to_string(),
            wrapped("secretary", "message", id, "do it"),
            true
        )],
        "the wrapped text, once, with the base checking that the terminal is idle"
    );
    let l = s.ledger(id);
    assert_eq!((l.status.as_str(), l.attempt), ("delivered", 1));
    assert!(l.delivered_at.is_some());

    // Never pushed, never woken, never in the box, even with a channel that reaches it.
    let mut d = s.delivery(&["w-t8"]);
    d.pass(&s.w).unwrap();
    s.clock.advance(300);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert!(s.pushed().is_empty());
    assert_eq!(s.base.sent().len(), 1, "typed once, and no wake line");
    assert!(inbox(&s.w, "w-t8").unwrap().messages.is_empty());
}

#[test]
fn typed_to_a_done_worker_and_to_the_curator() {
    let s = Org::new();
    s.terminal("worker", Some("t9"), State::Done);
    s.terminal("curator", None, State::Idle);
    let a = send_typed(&s.w, &*s.base, "secretary", "w-t9", "message", "one").unwrap();
    let b = send_typed(&s.w, &*s.base, "secretary", "curator", "message", "two").unwrap();
    assert_eq!(
        s.base.sent(),
        vec![
            (
                "w-t9".to_string(),
                wrapped("secretary", "message", a, "one"),
                false
            ),
            (
                "curator".to_string(),
                wrapped("secretary", "message", b, "two"),
                true
            ),
        ],
        "`done`: the status is read just before and the base is not asked to check"
    );
    assert_eq!(s.ledger(a).status, "delivered");
    assert_eq!(s.ledger(b).status, "delivered");
}

#[test]
fn typed_to_a_terminal_that_is_not_idle_or_done_is_a_conflict_and_stores_nothing() {
    let s = Org::new();
    s.terminal("worker", Some("w1"), State::Working);
    s.terminal("worker", Some("w2"), State::Blocked);
    s.terminal("worker", Some("w3"), State::Error);
    for to in ["w-w1", "w-w2", "w-w3", "w-gone"] {
        let r = send_typed(&s.w, &*s.base, "secretary", to, "message", "x");
        assert!(matches!(r, Err(TypedError::Conflict(_))), "{to}: {r:?}");
        s.assert_nothing_stored_or_typed();
    }
}

#[test]
fn typed_to_the_secretary_is_a_usage_error_and_stores_nothing() {
    let s = Org::new();
    s.terminal("secretary", None, State::Idle);
    for to in ["secretary", "nobody"] {
        let r = send_typed(&s.w, &*s.base, "w-t1", to, "message", "x");
        assert!(matches!(r, Err(TypedError::Usage(_))), "{to}: {r:?}");
        s.assert_nothing_stored_or_typed();
    }
}

#[test]
fn typed_when_the_base_refuses_because_the_status_changed_stores_nothing() {
    let s = Org::new();
    s.terminal("worker", Some("t10"), State::Idle);
    // The terminal was idle when read and is not when the base checks (the accepted race).
    *s.base.fail_send.lock().unwrap() = Some(BaseError::StatusMismatch(Detail {
        kind: "status_mismatch".into(),
        message: "working".into(),
        hint: None,
    }));
    let r = send_typed(&s.w, &*s.base, "secretary", "w-t10", "message", "x");
    assert!(matches!(r, Err(TypedError::Conflict(_))), "{r:?}");
    s.assert_nothing_stored_or_typed();
}

#[test]
fn typed_when_the_outcome_is_unknown_is_kept_unconfirmed_and_never_sent_again() {
    let s = Org::new();
    s.terminal("worker", Some("t11"), State::Idle);
    *s.base.fail_send.lock().unwrap() = Some(BaseError::Unavailable(Detail {
        kind: "timeout".into(),
        message: "no answer from the base".into(),
        hint: None,
    }));
    let r = send_typed(
        &s.w,
        &*s.base,
        "secretary",
        "w-t11",
        "message",
        "maybe typed",
    );
    assert!(matches!(r, Err(TypedError::Failed(_))), "{r:?}");

    // The record was made before the send and stays: the sender can see it.
    assert_eq!(s.count("messages"), 1);
    let id: i64 =
        s.w.conn()
            .query_row("SELECT id FROM messages", [], |r| r.get(0))
            .unwrap();
    let l = s.ledger(id);
    assert_eq!(l.status, "pending");
    assert!(l.delivered_at.is_none());

    // And `makibad` never sends it: no push, no wake line, not in the box.
    let mut d = s.delivery(&["w-t11"]);
    d.pass(&s.w).unwrap();
    s.clock.advance(300);
    d.wake_pass(&s.w, &*s.base).unwrap();
    assert!(s.pushed().is_empty());
    assert!(s.base.sent().is_empty());
    assert!(inbox(&s.w, "w-t11").unwrap().messages.is_empty());
    assert_eq!(s.ledger(id).status, "pending");
}

// ---- the box ----------------------------------------------------------------------------------

#[test]
fn inbox_returns_only_the_callers_messages_oldest_first_within_the_limit() {
    assert_eq!(INBOX_LIMIT, 20);
    let s = Org::new();
    let mut a = Vec::new();
    let mut b = Vec::new();
    for i in 0..25 {
        a.push(s.message("secretary", "w-a", "message", &format!("a{i}")));
        if i % 8 == 0 {
            b.push(s.message("w-b", "secretary", "report_done", &format!("b{i}")));
        }
    }
    let other = s.message("secretary", "w-b", "message", "for b");

    // Another agent's box shows only its own, and leaves everything else as it was.
    let got = inbox(&s.w, "w-b").unwrap();
    assert_eq!(
        got.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![other]
    );
    assert!(!got.more);
    assert_eq!(s.ledger(a[0]).status, "pending");
    assert_eq!(s.ledger(b[0]).status, "pending");

    // 25 are waiting: 20 oldest first, and it says there are more.
    let first = inbox(&s.w, "w-a").unwrap();
    assert!(first.more);
    assert_eq!(
        first.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        a[..20].to_vec()
    );
    assert_eq!(
        first.messages[3].text,
        wrapped("secretary", "message", a[3], "a3")
    );
    for id in &a[..20] {
        assert_eq!(s.ledger(*id).status, "delivered");
    }
    for id in &a[20..] {
        assert_eq!(s.ledger(*id).status, "pending", "only what was returned");
    }

    // The next call: the rest, and nothing more waiting; then an empty box.
    let second = inbox(&s.w, "w-a").unwrap();
    assert!(!second.more);
    assert_eq!(
        second.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        a[20..].to_vec()
    );
    let third = inbox(&s.w, "w-a").unwrap();
    assert!(third.messages.is_empty() && !third.more);

    // The secretary's box has its own messages only.
    let sec = inbox(&s.w, "secretary").unwrap();
    assert_eq!(sec.messages.iter().map(|m| m.id).collect::<Vec<_>>(), b);
    assert!(inbox(&s.w, "nobody").unwrap().messages.is_empty());
}
