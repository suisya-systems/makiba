//! Delivering stored messages (spec section 8): each pass pushes every `pending` message not yet
//! pushed (`attempt` 0) through its recipient's channel, then confirms the pushes whose recipient
//! showed a turn start in time.
//!
//! Order of events, which is the whole protocol: push first, then one transaction that counts the
//! attempt. A crash in between leaves the row at `attempt` 0, so the same message (same id) is
//! pushed again after a restart: at least once. After the attempt is recorded the message is not
//! pushed again, whether or not it was confirmed; what is left is for the box (M2-7).
//!
//! Confirmation reads only what the monitor committed (`terminal_status` records), never the
//! base: a push to a recipient that was `idle` or `done` is confirmed by a later `terminal_status`
//! of the same recipient that says `working`, committed within the window after the push. That is
//! a heuristic: a turn that starts for another cause also confirms. A push made while the
//! recipient was `working`, `blocked` or in error, or while an earlier push to it is still
//! waiting for its confirmation, is never confirmed here. The waiting is kept in memory only, so
//! after a restart a pushed message stays `pending` for the box.
//!
//! A push remembers the launch identity of the connection it went to (`agents.spawn_id` of the
//! row of that connection's token) and only a `working` of a terminal with that `spawn_id` confirms
//! it, so a replacement terminal cannot confirm a push made to its predecessor. Messages stay
//! addressed to the logical recipient: mail waiting for the secretary survives its replacement.
//!
//! Who a terminal is comes from the record's `role` (`secretary`, `curator`, `worker`) and, for a
//! worker, its `task_id`; a record with no role and a task id is taken as that worker's.

use crate::agents::{Agent, Composer};
use crate::base::{Base, BaseError, State, Terminal};
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{BODY_LIMIT, CUT_AT, WAKE_LINE};
use rusqlite::params;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long after a push a turn start confirms it (a first proposal, spec section 8, probes P8).
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(10);
/// How long a recipient whose push failed is left alone.
const RETRY_AFTER: Duration = Duration::from_secs(1);
/// How long one pass may spend pushing before it hands control back to the daemon loop, so the
/// monitor, the heartbeat and a stop are not held up by a backlog. What is left waits for the next pass.
const PASS_BUDGET: Duration = Duration::from_millis(200);
/// A push to a recipient this recent is still waiting for its turn start: no wake line yet.
const WAKE_AFTER_PUSH: f64 = 10.0;
/// At most one wake line per recipient in this time. Kept in memory only: after a restart a
/// recipient may be woken once more than this allows (owner, 2026-11-18).
const WAKE_EVERY: f64 = 60.0;
/// How many lines of the screen are read to find the prompt line.
const SCREEN_LINES: usize = 12;
/// How long past the window a push is still looked at, for a monitor that is a little behind.
const SETTLE_GRACE: Duration = Duration::from_secs(1);

/// The seam to the channel servers (M2-8 supplies the real one).
pub trait Channel {
    /// True when the recipient's agent program has a channel (`Agent::has_channel`) and its
    /// channel server is connected. Without both, nothing is pushed.
    fn reaches(&self, recipient: &str) -> bool;
    /// Hand `text` to the recipient's channel server. `Ok` means the server accepted it. Must
    /// give up after a bounded time, so one recipient cannot stall the daemon.
    fn push(&self, recipient: &str, text: &str) -> Result<(), String>;
    /// `push`, and the hash of the token (`agents.key_sha256`) of the connection the text went to,
    /// taken from the same connection that took it; `None` when the channel does not know.
    /// Delivery then takes the recipient's newest key that is not revoked.
    fn push_keyed(&self, recipient: &str, text: &str) -> Result<Option<String>, String> {
        self.push(recipient, text).map(|()| None)
    }
}

/// No channel server is connected to anything.
pub struct NoChannel;

impl Channel for NoChannel {
    fn reaches(&self, _recipient: &str) -> bool {
        false
    }
    fn push(&self, _recipient: &str, _text: &str) -> Result<(), String> {
        Err("no channel".into())
    }
}

/// A push waiting for its turn start.
struct InFlight {
    event_id: i64,
    /// The newest event at the time of the push: only later records count.
    cursor: i64,
    pushed: Instant,
    /// The launch identity (`agents.spawn_id`) of the connection the push went to: only a turn
    /// start of a terminal with this `spawn_id` (null: with none) confirms it.
    spawn: Option<String>,
}

pub struct Delivery {
    channel: Box<dyn Channel>,
    window: Duration,
    waiting: HashMap<String, InFlight>,
    retry_at: HashMap<String, Instant>,
    clock: Box<dyn Fn() -> SystemTime + Send>,
    agent: Option<Box<dyn Agent>>,
    /// When each recipient was last sent the wake line, by `clock`, in seconds.
    woken: HashMap<String, f64>,
    /// The recipient the base was last asked about, for rotating through a backlog.
    last_checked: Option<String>,
}

struct Due {
    id: i64,
    event_id: i64,
    sender: String,
    recipient: String,
    kind: String,
}

impl Delivery {
    pub fn new(channel: Box<dyn Channel>) -> Delivery {
        Delivery {
            channel,
            window: CONFIRM_WINDOW,
            waiting: HashMap::new(),
            retry_at: HashMap::new(),
            clock: Box::new(SystemTime::now),
            agent: None,
            woken: HashMap::new(),
            last_checked: None,
        }
    }

    /// The clock the wake rules (10 s, 60 s) read; a test sets its own.
    pub fn with_clock(mut self, clock: Box<dyn Fn() -> SystemTime + Send>) -> Delivery {
        self.clock = clock;
        self
    }

    /// The agent program that reads the prompt line. With none (before M4 supplies
    /// `claude_code`) no wake line is typed: a screen that is not understood gets nothing.
    pub fn with_agent(mut self, agent: Box<dyn Agent>) -> Delivery {
        self.agent = Some(agent);
        self
    }

    pub fn with_window(mut self, window: Duration) -> Delivery {
        self.window = window;
        self
    }

    /// One pass: confirm what can be confirmed, then push what is due, oldest first.
    pub fn pass(&mut self, w: &StateWriter) -> crate::db::Result<()> {
        self.settle(w)?;
        // Only addressed messages with a ledger row that was never attempted.
        let mut q = w.conn().prepare(
            "SELECT m.id, m.event_id, m.sender, m.recipient, m.kind
             FROM messages m JOIN event_deliveries d
               ON d.source_event_id = m.event_id AND d.recipient = m.recipient
             JOIN events e ON e.id = m.event_id
             WHERE d.status = 'pending' AND d.attempt = 0
               AND COALESCE(json_extract(e.payload_json, '$.typed'), 0) = 0
             ORDER BY m.id",
        )?;
        let due = q
            .query_map([], |r| {
                Ok(Due {
                    id: r.get(0)?,
                    event_id: r.get(1)?,
                    sender: r.get(2)?,
                    recipient: r.get(3)?,
                    kind: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let now = Instant::now();
        for m in due {
            if now.elapsed() > PASS_BUDGET {
                break;
            }
            if !self.channel.reaches(&m.recipient)
                || self.retry_at.get(&m.recipient).is_some_and(|t| now < *t)
            {
                continue;
            }
            let (cursor, eligible) = snapshot(w, &m.recipient)?;
            // The body is read only for a message that is going out, not for a backlog.
            let body: String =
                w.conn()
                    .query_row("SELECT body FROM messages WHERE id = ?1", [m.id], |r| {
                        r.get(0)
                    })?;
            let text = wrap(m.id, &m.sender, &m.kind, &body);
            let result = self
                .channel
                .push_keyed(&m.recipient, &text)
                .and_then(|key| {
                    w.record_push(m.event_id, &m.recipient)
                        .map(|()| key)
                        .map_err(|e| e.to_string())
                });
            let key = match result {
                Ok(key) => key,
                Err(e) => {
                    // A failed push is not recorded; a failed record means the push is repeated.
                    self.retry_at
                        .insert(m.recipient.clone(), Instant::now() + RETRY_AFTER);
                    eprintln!(
                        "makibad: cannot deliver message {} to {}: {e}",
                        m.id, m.recipient
                    );
                    continue;
                }
            };
            let pushed = Instant::now(); // the push is accepted: the window runs from here
            if eligible && !self.waiting.contains_key(&m.recipient) {
                let spawn = launch_identity(w, key, &m.recipient)?;
                self.waiting.insert(
                    m.recipient,
                    InFlight {
                        event_id: m.event_id,
                        cursor,
                        pushed,
                        spawn,
                    },
                );
            }
        }
        Ok(())
    }

    /// The wake line (spec section 7): for each recipient with an unconfirmed message that is
    /// `idle` or `done` now, no push to it in the last 10 s and no wake line to it in the last
    /// 60 s, type `WAKE_LINE` if the prompt line is empty. At most one terminal is written to per
    /// pass, so a backlog cannot hold up the daemon; the rest wait for the next pass.
    pub fn wake_pass(&mut self, w: &StateWriter, base: &dyn Base) -> crate::db::Result<()> {
        let Some(agent) = self.agent.as_ref() else {
            return Ok(());
        };
        let now = unix(&(self.clock)());
        // Pending, not typed: pushed (`attempt` > 0), or never pushed because there is no channel.
        let mut q = w.conn().prepare(
            "SELECT DISTINCT d.recipient, d.attempt FROM event_deliveries d
             JOIN messages m ON m.event_id = d.source_event_id AND m.recipient = d.recipient
             JOIN events e ON e.id = m.event_id
             WHERE d.status = 'pending' AND COALESCE(json_extract(e.payload_json, '$.typed'), 0) = 0
             ORDER BY d.recipient",
        )?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let mut candidates: Vec<String> = Vec::new();
        for (recipient, attempt) in rows {
            if (attempt > 0 || !self.channel.reaches(&recipient))
                && !candidates.contains(&recipient)
            {
                candidates.push(recipient);
            }
        }
        // Rotate, so that a recipient that is never ready cannot starve the others.
        if let Some(i) = candidates
            .iter()
            .position(|c| Some(c) == self.last_checked.as_ref())
        {
            let n = candidates.len();
            candidates.rotate_left((i + 1) % n);
        }
        for recipient in candidates {
            if self
                .woken
                .get(&recipient)
                .is_some_and(|t| now - t < WAKE_EVERY)
                || self
                    .retry_at
                    .get(&recipient)
                    .is_some_and(|t| Instant::now() < *t)
            {
                continue;
            }
            // The latest push of any message to it, delivered ones too.
            let last_push: Option<f64> = w.conn().query_row(
                "SELECT MAX((julianday(last_attempt_at) - 2440587.5) * 86400.0)
                 FROM event_deliveries WHERE recipient = ?1 AND attempt > 0",
                [&recipient],
                |r| r.get(0),
            )?;
            if last_push.is_some_and(|t| now - t < WAKE_AFTER_PUSH) {
                continue;
            }
            // What the monitor last recorded rules out most recipients without asking the base.
            let known = last_record(w, &recipient, i64::MAX)?;
            if known.is_some_and(|(k, st)| {
                k != "terminal_status" || !matches!(st.as_deref(), Some("idle" | "done"))
            }) {
                continue;
            }
            // From here the base is asked: for one recipient only per pass, whatever it answers,
            // so a slow base costs one bounded call sequence and not one per recipient.
            self.last_checked = Some(recipient.clone());
            let Ok(Some(t)) = find_terminal(base, &recipient) else {
                break;
            };
            // The status now, not the last record: the wake goes to an idle or done terminal only.
            let idle = match base.get(&t.id).ok().and_then(|t| t.status).map(|s| s.state) {
                Some(State::Idle) => true,
                Some(State::Done) => false,
                _ => break,
            };
            let screen = base.read(&t.id, SCREEN_LINES).unwrap_or_default();
            if agent.composer(&screen) != Composer::Empty {
                break;
            }
            match base.send(&t.id, WAKE_LINE, idle) {
                Ok(()) => {
                    self.woken.insert(recipient, now);
                }
                // Not counted against the 60 s: tried again after a short pause.
                Err(e) => {
                    self.retry_at
                        .insert(recipient.clone(), Instant::now() + RETRY_AFTER);
                    eprintln!("makibad: cannot type the wake line to {recipient}: {e}");
                }
            }
            break;
        }
        Ok(())
    }

    fn settle(&mut self, w: &StateWriter) -> crate::db::Result<()> {
        let mut done = Vec::new();
        for (recipient, f) in &self.waiting {
            if turn_started(w, recipient, f, self.window)? {
                w.confirm_push(f.event_id, recipient)?;
                done.push(recipient.clone());
            } else if f.pushed.elapsed() > self.window + SETTLE_GRACE {
                done.push(recipient.clone());
            }
        }
        for r in done {
            self.waiting.remove(&r);
        }
        Ok(())
    }
}

/// The SQL that says whether an event's payload is about `recipient` (`?1` role, `?2` task id).
const ABOUT: &str = "((?2 IS NULL AND json_extract(payload_json, '$.role') = ?1) OR (?2 IS NOT NULL \
    AND json_extract(payload_json, '$.task_id') = ?2 \
    AND (json_extract(payload_json, '$.role') IS NULL OR json_extract(payload_json, '$.role') = 'worker')))";

/// The role and task id of a recipient's terminal; `None` for an agent with no terminal of its own.
fn identity(recipient: &str) -> Option<(&str, Option<&str>)> {
    match recipient {
        "secretary" => Some(("secretary", None)),
        "curator" => Some(("curator", None)),
        _ => recipient
            .strip_prefix(crate::spec::WORKER_AGENT_PREFIX)
            .map(|t| ("worker", Some(t))),
    }
}

/// The recipient's last record up to event `upto`: its kind and `state`; `None` when it has none.
fn last_record(
    w: &StateWriter,
    recipient: &str,
    upto: i64,
) -> crate::db::Result<Option<(String, Option<String>)>> {
    let Some((role, task)) = identity(recipient) else {
        return Ok(None);
    };
    w.conn()
        .query_row(
            &format!(
                "SELECT kind, json_extract(payload_json, '$.state') FROM events
                 WHERE kind IN ('terminal_status', 'terminal_gone') AND id <= ?3 AND {ABOUT}
                 ORDER BY id DESC LIMIT 1"
            ),
            params![role, task, upto],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e.into()),
        })
}

/// The newest event now, and whether the recipient's last record says `idle` or `done`.
fn snapshot(w: &StateWriter, recipient: &str) -> crate::db::Result<(i64, bool)> {
    let cursor: i64 = w
        .conn()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))?;
    let eligible = matches!(
        last_record(w, recipient, cursor)?,
        Some((k, Some(s))) if k == "terminal_status" && (s == "idle" || s == "done")
    );
    Ok((cursor, eligible))
}

/// The `spawn_id` of the agents row a push to `recipient` is bound to: the row of the connection
/// the push went to, else the recipient's newest key that is not revoked. `None` when there is no
/// row or its `spawn_id` is null (a terminal launched before tags).
fn launch_identity(
    w: &StateWriter,
    key: Option<String>,
    recipient: &str,
) -> crate::db::Result<Option<String>> {
    let row = match key {
        Some(key) => w.conn().query_row(
            "SELECT spawn_id FROM agents WHERE key_sha256 = ?1",
            [key],
            |r| r.get(0),
        ),
        None => w.conn().query_row(
            "SELECT spawn_id FROM agents WHERE agent_id = ?1 AND revoked_at IS NULL
             ORDER BY id DESC LIMIT 1",
            [recipient],
            |r| r.get(0),
        ),
    };
    match row {
        Ok(spawn) => Ok(spawn),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Did the recipient report `working`, after the push and within the window of it?
fn turn_started(
    w: &StateWriter,
    recipient: &str,
    f: &InFlight,
    window: Duration,
) -> crate::db::Result<bool> {
    let Some((role, task)) = identity(recipient) else {
        return Ok(false);
    };
    let n: i64 = w.conn().query_row(
        &format!(
            "SELECT COUNT(*) FROM events e, event_deliveries d
             WHERE d.source_event_id = ?3 AND d.recipient = ?5
               AND e.id > ?4 AND e.kind = 'terminal_status'
               AND json_extract(e.payload_json, '$.state') = 'working'
               AND json_extract(e.payload_json, '$.spawn_id') IS ?7
               AND (julianday(e.occurred_at) - julianday(d.last_attempt_at)) * 86400.0 <= ?6
               AND {}",
            ABOUT.replace("payload_json", "e.payload_json")
        ),
        params![
            role,
            task,
            f.event_id,
            f.cursor,
            recipient,
            window.as_secs_f64(),
            f.spawn
        ],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn unix(t: &SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The one live terminal of this organization that is `recipient`'s (by its tags). `None` when
/// there is none or more than one.
pub(super) fn find_terminal(
    base: &dyn Base,
    recipient: &str,
) -> Result<Option<Terminal>, BaseError> {
    let Some((role, task)) = identity(recipient) else {
        return Ok(None);
    };
    let mut found = base.list()?.into_iter().filter(|t| {
        t.alive
            && t.tags.get("makiba.role").map(String::as_str) == Some(role)
            && task.is_none_or(|k| t.tags.get("makiba.task_id").map(String::as_str) == Some(k))
    });
    let one = found.next();
    Ok(if found.next().is_some() { None } else { one })
}

/// Why a typed request did not go out.
#[derive(Debug)]
pub enum TypedError {
    /// The request itself is wrong (the secretary, or an agent that is not a worker or the curator).
    Usage(String),
    /// The recipient cannot take typed text now. Nothing is stored and nothing is typed.
    Conflict(String),
    /// The record or the send failed with an outcome that is not known. A message stored before
    /// the send stays `pending` and is never sent again by `makibad`.
    Failed(String),
    /// The recipient's run is parked: it has no terminal. Nothing is stored and nothing is typed.
    Parked,
}

/// `send` or `answer` with `typed` (spec sections 7 and 8): record the message as a typed one,
/// type its wrapped text once, and mark it `delivered` when `Base::send` returns. Returns the
/// message id. The record comes first, so a crash in between leaves a row that is `pending`, is
/// not eligible for a push, the wake line or the box, and is never sent again.
pub fn send_typed(
    w: &StateWriter,
    base: &dyn Base,
    sender: &str,
    recipient: &str,
    kind: &str,
    body: &str,
) -> Result<i64, TypedError> {
    if recipient != "curator" && !recipient.starts_with(crate::spec::WORKER_AGENT_PREFIX) {
        return Err(TypedError::Usage(format!(
            "typed goes to a worker or the curator, not to {recipient}"
        )));
    }
    let failed = |e: &dyn std::fmt::Display| TypedError::Failed(e.to_string());
    // A terminal the park pass ended is not found: its run was parked before it was ended, so a
    // lookup that fails reads the run again and stores the message when it is parked (Issue 332).
    let parked_or = |e: TypedError| match super::park::parked_worker(w, recipient) {
        Ok(Some(_)) => TypedError::Parked,
        Ok(None) => e,
        Err(read) => failed(&read),
    };
    let t = find_terminal(base, recipient)
        .map_err(|e| parked_or(failed(&e)))?
        .ok_or_else(|| {
            parked_or(TypedError::Conflict(format!(
                "{recipient} has no single live terminal"
            )))
        })?;
    let t = base.get(&t.id).map_err(|e| parked_or(failed(&e)))?;
    let state = t.status.as_ref().map(|s| s.state);
    let only_if_idle = match state {
        Some(State::Idle) => true,
        Some(State::Done) => false,
        other => {
            return Err(TypedError::Conflict(format!(
                "{recipient} is {other:?}, not idle or done"
            )));
        }
    };
    // The launch and the turn counter of the terminal typed into: the park pass keeps the run
    // until a later turn of that launch is recorded, the proof that this text was taken.
    let payload = serde_json::json!({
        "typed": true,
        "spawn_id": t.tags.get(crate::spec::TAG_SPAWN),
        "turn": t.turn,
    });
    // Decided in the transaction that records the message (Issue 332): a run parked before it
    // is not typed to, and a pass that parks after it finds this message and gives up.
    let recorded = w
        .transaction(|w| {
            if super::park::parked_worker(w, recipient)?.is_some() {
                return Ok(None);
            }
            let event = w.append_event(&NewEvent {
                kind: "message_sent",
                actor: Some(sender),
                payload: Some(&payload),
                ..Default::default()
            })?;
            w.conn().execute(
                "INSERT INTO messages (event_id, sender, recipient, kind, body) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![event, sender, recipient, kind, body],
            )?;
            let id = w.conn().last_insert_rowid();
            w.conn().execute(
                "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt, first_attempt_at, last_attempt_at)
                 VALUES (?1, ?2, 'pending', 1, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                params![event, recipient],
            )?;
            // A run in review is sent back to work with the record (Issue 654), so a crash after
            // the text is typed cannot leave its rework uncounted; a refusal below undoes it.
            let moved = super::door::sent_back(w, recipient)?;
            Ok(Some((id, event, moved)))
        })
        .map_err(|e| failed(&e))?;
    let Some((id, event, moved)) = recorded else {
        return Err(TypedError::Parked);
    };
    match base.send(&t.id, &wrap(id, sender, kind, body), only_if_idle) {
        Ok(()) => {
            w.confirm_push(event, recipient).map_err(|e| failed(&e))?;
            Ok(id)
        }
        // The base refused: nothing was typed, so nothing stays stored.
        Err(
            e @ (BaseError::StatusMismatch(_)
            | BaseError::Blocked(_)
            | BaseError::Finished(_)
            | BaseError::NotFound(_)),
        ) => {
            w.transaction(|w| {
                w.conn()
                    .execute("DELETE FROM events WHERE id = ?1", [event])?;
                if moved {
                    super::door::not_sent_back(w, recipient)?;
                }
                Ok(())
            })
            .map_err(|e| failed(&e))?;
            Err(TypedError::Conflict(e.to_string()))
        }
        // A timeout or the like: it may have been typed. Kept `pending`, never sent again. A run
        // in review stays sent back to work (Issue 654): a worker that took the text is at work,
        // and one that did not is counted until its next report, never too few.
        Err(e) => Err(TypedError::Failed(format!(
            "message {id}: the outcome of the send is not known: {e}"
        ))),
    }
}

/// A header value that cannot leave its quotes or its line.
fn attr(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
}

/// The wrapped text of spec section 8. `id` is the message id.
pub fn wrap(id: i64, sender: &str, kind: &str, body: &str) -> String {
    let total = body.chars().count();
    let escape = |s: &str| s.replace("</makiba", "<\\/makiba");
    let body = if total > BODY_LIMIT {
        let kept: String = body.chars().take(CUT_AT).collect();
        format!(
            "{}\n[cut: {} more characters. Call the show tool with id {id}]",
            escape(&kept),
            total - CUT_AT
        )
    } else {
        escape(body)
    };
    format!(
        "<makiba from=\"{}\" kind=\"{}\" id=\"{id}\">\n{body}\n</makiba>",
        attr(sender),
        attr(kind)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_cannot_be_closed_or_left() {
        let t = wrap(7, "a\"b\nc", "message", "</makiba></makiba");
        assert_eq!(
            t,
            "<makiba from=\"a&quot;b&#10;c\" kind=\"message\" id=\"7\">\n<\\/makiba><\\/makiba\n</makiba>"
        );
    }

    #[test]
    fn the_limit_counts_characters_and_the_cut_counts_the_rest() {
        let at = "\u{3042}".repeat(BODY_LIMIT);
        assert!(!wrap(1, "s", "message", &at).contains("[cut"));
        let t = wrap(1, "s", "message", &format!("{at}x"));
        assert!(t.contains(&format!(
            "[cut: {} more characters.",
            BODY_LIMIT + 1 - CUT_AT
        )));
        assert!(t.contains("Call the show tool with id 1]"));
    }

    #[test]
    fn a_workers_records_are_told_apart_by_task_id() {
        use crate::db::writer::NewEvent;
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        let status = |role: serde_json::Value, task: &str, state: &str| {
            let p = serde_json::json!({"task_id": task, "role": role, "state": state});
            w.append_event(&NewEvent {
                kind: "terminal_status",
                payload: Some(&p),
                ..Default::default()
            })
            .unwrap();
        };
        status("worker".into(), "t1", "idle");
        status("worker".into(), "t2", "working");
        status(serde_json::Value::Null, "t3", "done");
        assert!(snapshot(&w, "w-t1").unwrap().1);
        assert!(!snapshot(&w, "w-t2").unwrap().1);
        assert!(
            snapshot(&w, "w-t3").unwrap().1,
            "no role: the task id decides"
        );
        assert!(!snapshot(&w, "secretary").unwrap().1);
        // t2 working is not a turn start of t1.
        w.conn()
            .execute(
                "INSERT INTO event_deliveries (source_event_id, recipient, last_attempt_at) VALUES (1, 'w-t1', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [],
            )
            .unwrap();
        let f = InFlight {
            event_id: 1,
            cursor: 0,
            pushed: Instant::now(),
            spawn: None,
        };
        assert!(!turn_started(&w, "w-t1", &f, CONFIRM_WINDOW).unwrap());
        status("worker".into(), "t1", "working");
        assert!(turn_started(&w, "w-t1", &f, CONFIRM_WINDOW).unwrap());
    }

    #[test]
    fn a_recipient_that_cannot_be_woken_does_not_hold_back_the_next() {
        use crate::agents::fake::Fake;
        use crate::base::stub::Stub;
        use crate::base::{Base, LaunchSpec};
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        for to in ["w-a", "w-b"] {
            let e = w
                .append_event(&crate::db::writer::NewEvent {
                    kind: "message_sent",
                    ..Default::default()
                })
                .unwrap();
            w.conn()
                .execute(
                    "INSERT INTO messages (event_id, sender, recipient, kind, body) VALUES (?1, 's', ?2, 'message', 'x')",
                    params![e, to],
                )
                .unwrap();
            w.conn()
                .execute(
                    "INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?1, ?2)",
                    params![e, to],
                )
                .unwrap();
        }
        // `w-a` has no terminal; `w-b` has one with an empty prompt.
        let base = Stub::scripted(&["> "]);
        base.launch(&LaunchSpec {
            argv: Vec::new(),
            cwd: ".".into(),
            name: "w-b".into(),
            tags: [("makiba.role", "worker"), ("makiba.task_id", "b")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into(),
            env: Default::default(),
            unset: Vec::new(),
        })
        .unwrap();
        let mut d = Delivery::new(Box::new(NoChannel)).with_agent(Box::new(Fake::new(
            std::path::Path::new("a"),
            std::path::Path::new("r"),
            &[],
        )));
        d.wake_pass(&w, &base).unwrap();
        d.wake_pass(&w, &base).unwrap();
        assert!(d.woken.contains_key("w-b"));
        assert!(!d.woken.contains_key("w-a"));
    }

    /// Issue 654 (diff review): a typed message to the worker of a run in review sends the run
    /// back to work when the base takes it and when its outcome is not known (it may have been
    /// typed); a definitive refusal deletes the message and moves nothing.
    #[test]
    fn a_typed_send_moves_a_run_in_review_unless_the_base_refused_it() {
        use crate::base::stub::Stub;
        use crate::base::{Base, LaunchSpec};
        use crate::db::writer::RunUpsert;
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        let status = |task: &str| -> String {
            w.conn()
                .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        let base = Stub::scripted(&["> "]);
        let mut terminals = Vec::new();
        for task in ["a", "b", "c"] {
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                title: Some("T"),
                status: Some("review"),
                ..Default::default()
            })
            .unwrap();
            let t = base
                .launch(&LaunchSpec {
                    argv: Vec::new(),
                    cwd: ".".into(),
                    name: format!("w-{task}"),
                    tags: [("makiba.role", "worker"), ("makiba.task_id", task)]
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .into(),
                    env: Default::default(),
                    unset: Vec::new(),
                })
                .unwrap();
            base.set_state(&t, State::Idle);
            terminals.push(t);
        }
        // Taken.
        send_typed(&w, &base, "secretary", "w-a", "message", "Write the fix.").unwrap();
        assert_eq!(status("a"), "in_use");
        // Refused after the record (the run moved with it): the terminal blocked on a prompt
        // before the text went in, so the base types nothing. The move is undone with the message.
        base.refuse_sends(true);
        let r = send_typed(&w, &base, "secretary", "w-b", "message", "x");
        assert!(
            matches!(&r, Err(TypedError::Conflict(m)) if m.contains("blocked on a prompt")),
            "{r:?}"
        );
        base.refuse_sends(false);
        assert_eq!(status("b"), "review");
        let kept: i64 = w
            .conn()
            .query_row(
                "SELECT count(*) FROM messages WHERE recipient = 'w-b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 0, "nothing stays stored");
        // Not known: typed, then the answer timed out.
        base.fail_sends_after_typing();
        let r = send_typed(&w, &base, "secretary", "w-c", "message", "Write the fix.");
        assert!(matches!(r, Err(TypedError::Failed(_))), "{r:?}");
        assert_eq!(status("c"), "in_use");
    }
}
