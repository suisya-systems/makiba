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
            let result = self.channel.push(&m.recipient, &text).and_then(|()| {
                w.record_push(m.event_id, &m.recipient)
                    .map_err(|e| e.to_string())
            });
            if let Err(e) = result {
                // A failed push is not recorded; a failed record means the push is repeated.
                self.retry_at
                    .insert(m.recipient.clone(), Instant::now() + RETRY_AFTER);
                eprintln!(
                    "makibad: cannot deliver message {} to {}: {e}",
                    m.id, m.recipient
                );
                continue;
            }
            let pushed = Instant::now(); // the push is accepted: the window runs from here
            if eligible && !self.waiting.contains_key(&m.recipient) {
                self.waiting.insert(
                    m.recipient,
                    InFlight {
                        event_id: m.event_id,
                        cursor,
                        pushed,
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
            let Ok(Some(t)) = find_terminal(base, &recipient) else {
                continue;
            };
            // The status now, not the last record: the wake goes to an idle or done terminal only.
            let idle = match base.get(&t.id).ok().and_then(|t| t.status).map(|s| s.state) {
                Some(State::Idle) => true,
                Some(State::Done) => false,
                _ => continue,
            };
            let screen = base.read(&t.id, SCREEN_LINES).unwrap_or_default();
            if agent.composer(&screen) != Composer::Empty {
                continue;
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

/// The newest event now, and whether the recipient's last record says `idle` or `done`.
fn snapshot(w: &StateWriter, recipient: &str) -> crate::db::Result<(i64, bool)> {
    let cursor: i64 = w
        .conn()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))?;
    let Some((role, task)) = identity(recipient) else {
        return Ok((cursor, false));
    };
    let last: Option<(String, Option<String>)> = w
        .conn()
        .query_row(
            &format!(
                "SELECT kind, json_extract(payload_json, '$.state') FROM events
                 WHERE kind IN ('terminal_status', 'terminal_gone') AND id <= ?3 AND {ABOUT}
                 ORDER BY id DESC LIMIT 1"
            ),
            params![role, task, cursor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    let eligible = matches!(
        last,
        Some((k, Some(s))) if k == "terminal_status" && (s == "idle" || s == "done")
    );
    Ok((cursor, eligible))
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
            window.as_secs_f64()
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
fn find_terminal(base: &dyn Base, recipient: &str) -> Result<Option<Terminal>, BaseError> {
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
    let t = find_terminal(base, recipient)
        .map_err(|e| failed(&e))?
        .ok_or_else(|| TypedError::Conflict(format!("{recipient} has no single live terminal")))?;
    let state = base
        .get(&t.id)
        .map_err(|e| failed(&e))?
        .status
        .map(|s| s.state);
    let only_if_idle = match state {
        Some(State::Idle) => true,
        Some(State::Done) => false,
        other => {
            return Err(TypedError::Conflict(format!(
                "{recipient} is {other:?}, not idle or done"
            )));
        }
    };
    let payload = serde_json::json!({"typed": true});
    let (id, event) = w
        .transaction(|w| {
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
            Ok((id, event))
        })
        .map_err(|e| failed(&e))?;
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
            w.conn()
                .execute("DELETE FROM events WHERE id = ?1", [event])
                .map_err(|e| failed(&e))?;
            Err(TypedError::Conflict(e.to_string()))
        }
        // A timeout or the like: it may have been typed. Kept `pending`, never sent again.
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
        };
        assert!(!turn_started(&w, "w-t1", &f, CONFIRM_WINDOW).unwrap());
        status("worker".into(), "t1", "working");
        assert!(turn_started(&w, "w-t1", &f, CONFIRM_WINDOW).unwrap());
    }
}
