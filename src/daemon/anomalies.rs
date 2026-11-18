//! The anomaly rules of spec section 11, and the pass that applies them.
//!
//! Two halves. The rules are pure: `run_rules` and `secretary_rules` take a typed scene and `now`
//! (milliseconds since the epoch) and say which rules hold. The pass reads the scenes from
//! `state.db`, compares the rules that hold with the anomalies that are open, and writes the
//! difference: `anomaly_raised` / `anomaly_cleared`, and for a raise told to the secretary the
//! notice (spec 5.4), and for `gone` the run's failure, all in one transaction. What is open is
//! read from the latest raised / cleared event of each subject; the memory kept in `Anomalies` is a
//! cache of that, changed only after a commit and dropped when a write fails.
//!
//! What the scenes are made of: the run's `terminal_status`, `terminal_turn` and `terminal_gone`
//! events since its latest launch (`run_launch.created_at`; none: no `no_status`), and its reports
//! (`messages` of kind `report_done` / `report_blocked`). The secretary has no run: its records are
//! the events whose `role` is `secretary`, one terminal generation per `agents` row of
//! `secretary`, and a renewal under way is a `renew` row of `commands` that is `accepted`, or
//! `done` with the gone inside the time it took. Elapsed time is measured from persisted times and
//! the `now` given; "after the last turn" is by event id. The time of a status is when it was
//! recorded, which is when it changed (the monitor writes only changes).
//!
//! Not here: the `pending` entry of `blocked_permission` (spec 7, the permission relay), delivery
//! of the notices (M2-4) and the relaunch of the secretary (M6-5).

use crate::db::{Result, StateWriter};
use crate::spec::AGENT_SECRETARY;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::{Duration, Instant};

const BLOCKED_QUESTION_MS: i64 = 60_000;
const DONE_NO_REPORT_MS: i64 = 120_000;
const NO_STATUS_MS: i64 = 180_000;
const SECRETARY_NO_STATUS_MS: i64 = 180_000;
const SECRETARY_STILL_MS: i64 = 600_000;
/// How often a pass runs at most.
const EVERY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    BlockedQuestion,
    BlockedPermission,
    BlockedAuth,
    Error,
    Gone,
    DoneNoReport,
    NoStatus,
    SecretaryGone,
    SecretaryStill,
}

impl Rule {
    pub const ALL: [Rule; 9] = [
        Rule::BlockedQuestion,
        Rule::BlockedPermission,
        Rule::BlockedAuth,
        Rule::Error,
        Rule::Gone,
        Rule::DoneNoReport,
        Rule::NoStatus,
        Rule::SecretaryGone,
        Rule::SecretaryStill,
    ];

    /// The name written in events (spec section 11).
    pub const fn name(self) -> &'static str {
        match self {
            Rule::BlockedQuestion => "blocked_question",
            Rule::BlockedPermission => "blocked_permission",
            Rule::BlockedAuth => "blocked_auth",
            Rule::Error => "error",
            Rule::Gone => "gone",
            Rule::DoneNoReport => "done_no_report",
            Rule::NoStatus => "no_status",
            Rule::SecretaryGone => "secretary_gone",
            Rule::SecretaryStill => "secretary_still",
        }
    }

    fn from_name(n: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.name() == n)
    }

    /// The two rules told to the person (the log and `makiba status`) rather than the secretary.
    const fn about_secretary(self) -> bool {
        matches!(self, Rule::SecretaryGone | Rule::SecretaryStill)
    }

    fn text(self) -> &'static str {
        match self {
            Rule::BlockedQuestion => "the worker has been blocked on a question for over 60 s.",
            Rule::BlockedPermission => "the worker is blocked on a permission prompt.",
            Rule::BlockedAuth => "the worker is blocked on authentication and needs the person.",
            Rule::Error => "the worker's terminal reports an error.",
            Rule::Gone => {
                "the worker's terminal is gone and no report_done was made; the run is now failed."
            }
            Rule::DoneNoReport => {
                "the worker has been done for over 120 s with no report_done or report_blocked since its last turn."
            }
            Rule::NoStatus => "no status was recorded within 180 s of the launch.",
            Rule::SecretaryGone | Rule::SecretaryStill => "",
        }
    }
}

/// A terminal's latest status record: when it was written, and what it said.
pub struct Status {
    pub at: i64,
    pub state: String,
    pub kind: Option<String>,
}

/// What the worker rules need to know about one run.
pub struct RunScene {
    /// The latest launch, if one is recorded.
    pub launched: Option<i64>,
    /// The latest status record since the launch.
    pub status: Option<Status>,
    /// A `terminal_gone` since the launch.
    pub gone: bool,
    /// A `report_done` for the run, ever.
    pub report_done: bool,
    /// A `report_done` or `report_blocked` after the last `terminal_turn` (any since the launch
    /// when there is no turn).
    pub reported_since_turn: bool,
}

/// What the secretary rules need to know about the current secretary terminal.
pub struct SecretaryScene {
    pub launched: i64,
    /// A `terminal_gone` of the secretary in this generation that no renewal accounts for.
    pub gone: bool,
    pub status_at: Option<i64>,
}

/// The rules that hold for a run at `now`, whatever its state: the caller decides which count.
pub fn run_rules(s: &RunScene, now: i64) -> Vec<Rule> {
    let mut out = Vec::new();
    if let Some(st) = &s.status {
        let age = now - st.at;
        match (st.state.as_str(), st.kind.as_deref()) {
            ("blocked", Some("permission")) => out.push(Rule::BlockedPermission),
            ("blocked", Some("auth")) => out.push(Rule::BlockedAuth),
            ("blocked", _) if age >= BLOCKED_QUESTION_MS => out.push(Rule::BlockedQuestion),
            ("error", _) => out.push(Rule::Error),
            ("done", _) if age >= DONE_NO_REPORT_MS && !s.reported_since_turn => {
                out.push(Rule::DoneNoReport)
            }
            _ => {}
        }
    } else if let Some(l) = s.launched
        && now - l >= NO_STATUS_MS
    {
        out.push(Rule::NoStatus);
    }
    if s.gone && !s.report_done {
        out.push(Rule::Gone);
    }
    out
}

/// The rules that hold for the secretary at `now`; `worker_anomaly` is whether any worker anomaly
/// is open.
pub fn secretary_rules(s: &SecretaryScene, worker_anomaly: bool, now: i64) -> Vec<Rule> {
    let mut out = Vec::new();
    if s.gone || (s.status_at.is_none() && now - s.launched >= SECRETARY_NO_STATUS_MS) {
        out.push(Rule::SecretaryGone);
    }
    if worker_anomaly && now - s.status_at.unwrap_or(s.launched) >= SECRETARY_STILL_MS {
        out.push(Rule::SecretaryStill);
    }
    out
}

/// What an anomaly is about: a run (by task id), or a secretary terminal generation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Subject {
    task: Option<String>,
    rule: Rule,
    generation: Option<i64>,
}

impl Subject {
    fn payload(&self) -> Value {
        let mut p = json!({"task_id": self.task, "rule": self.rule.name()});
        if let Some(g) = self.generation {
            p["generation"] = g.into();
        }
        p
    }
}

pub struct Anomalies {
    /// The open anomalies, once read from the events.
    open: Option<HashSet<Subject>>,
    next: Instant,
}

impl Default for Anomalies {
    fn default() -> Self {
        Self::new()
    }
}

/// `col` (a UTC text time) as milliseconds since the epoch.
fn ms(col: &str) -> String {
    format!("CAST(ROUND((julianday({col}) - 2440587.5) * 86400000.0) AS INTEGER)")
}

impl Anomalies {
    pub fn new() -> Anomalies {
        Anomalies {
            open: None,
            next: Instant::now(),
        }
    }

    /// One pass, at most once a second, at the database's clock sampled once.
    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        if Instant::now() < self.next {
            return Ok(());
        }
        self.next = Instant::now() + EVERY;
        let now: String =
            w.conn()
                .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                    r.get(0)
                })?;
        self.pass_at(w, &now)
    }

    /// One pass at `now` (a UTC text time as the database writes it). A write that fails leaves
    /// nothing of its anomaly behind and is tried again by the next pass.
    pub fn pass_at(&mut self, w: &StateWriter, now: &str) -> Result<()> {
        let open = match self.open.take() {
            Some(o) => o,
            None => load_open(w)?,
        };
        // Whatever happens below, what is open is read again unless it is put back at the end.
        let now_ms: i64 = w
            .conn()
            .query_row(&format!("SELECT {}", ms("?1")), [now], |r| r.get(0))?;
        let desired = evaluate(w, &open, now_ms)?;
        let mut open = open;
        let mut first_err = None;
        let wanted: HashSet<&Subject> = desired.iter().collect();
        let to_clear: Vec<Subject> = open
            .iter()
            .filter(|s| !wanted.contains(s))
            .cloned()
            .collect();
        for s in to_clear {
            match w.transaction(|w| {
                w.append_event(&crate::db::writer::NewEvent {
                    kind: "anomaly_cleared",
                    actor: Some("makibad"),
                    payload: Some(&s.payload()),
                    occurred_at: Some(now),
                    run_task_id: s.task.as_deref(),
                    ..Default::default()
                })
                .map(|_| ())
            }) {
                Ok(()) => {
                    open.remove(&s);
                }
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
        let to_raise: Vec<&Subject> = desired.iter().filter(|s| !open.contains(*s)).collect();
        for s in to_raise {
            match w.transaction(|w| raise(w, s, now)) {
                Ok(()) => {
                    if s.rule.about_secretary() {
                        eprintln!("makibad: anomaly {}", s.rule.name());
                    }
                    open.insert(s.clone());
                }
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
        match first_err {
            None => {
                self.open = Some(open);
                Ok(())
            }
            Some(e) => Err(e),
        }
    }
}

/// The open anomalies: the subjects whose latest raised / cleared event is a raise.
fn load_open(w: &StateWriter) -> Result<HashSet<Subject>> {
    let mut q = w.conn().prepare(
        "SELECT kind, payload_json FROM events
         WHERE kind IN ('anomaly_raised', 'anomaly_cleared') ORDER BY id",
    )?;
    let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut open = HashSet::new();
    for row in rows {
        let (kind, payload) = row?;
        let p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let Some(rule) = p["rule"].as_str().and_then(Rule::from_name) else {
            continue;
        };
        let s = Subject {
            task: p["task_id"].as_str().map(String::from),
            rule,
            generation: p["generation"].as_i64(),
        };
        if kind == "anomaly_raised" {
            open.insert(s);
        } else {
            open.remove(&s);
        }
    }
    Ok(open)
}

/// The subjects whose rule holds at `now`, workers first (`secretary_still` needs them).
fn evaluate(w: &StateWriter, open: &HashSet<Subject>, now: i64) -> Result<Vec<Subject>> {
    let mut out = Vec::new();
    let runs: Vec<(i64, String, bool)> = {
        let mut q = w
            .conn()
            .prepare("SELECT id, task_id, status FROM runs ORDER BY id")?;
        let rows = q.query_map([], |r| {
            let status: String = r.get(2)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                status == "in_use" || status == "review",
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    for (id, task, active) in runs {
        let held_gone = Subject {
            task: Some(task.clone()),
            rule: Rule::Gone,
            generation: None,
        };
        // A run that ended keeps only the anomaly it already had: `gone` makes it `failed`.
        if !active && !open.contains(&held_gone) {
            continue;
        }
        for rule in run_rules(&run_scene(w, id)?, now) {
            if active || rule == Rule::Gone {
                out.push(Subject {
                    task: Some(task.clone()),
                    rule,
                    generation: None,
                });
            }
        }
    }
    let worker_anomaly = !out.is_empty();
    let latest: Option<(i64, i64)> = w
        .conn()
        .query_row(
            &format!(
                "SELECT id, {} FROM agents WHERE agent_id = ?1 ORDER BY id DESC LIMIT 1",
                ms("created_at")
            ),
            [AGENT_SECRETARY],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((generation, launched)) = latest {
        for rule in secretary_rules(&secretary_scene(w, launched)?, worker_anomaly, now) {
            out.push(Subject {
                task: None,
                rule,
                generation: Some(generation),
            });
        }
    }
    Ok(out)
}

fn run_scene(w: &StateWriter, run: i64) -> Result<RunScene> {
    let c = w.conn();
    let launched: Option<i64> = c
        .query_row(
            &format!(
                "SELECT {} FROM run_launch WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
                ms("created_at")
            ),
            [run],
            |r| r.get(0),
        )
        .optional()?;
    let since = launched.unwrap_or(i64::MIN);
    let at = ms("occurred_at");
    let status = c
        .query_row(
            &format!(
                "SELECT {at}, payload_json FROM events
                 WHERE run_id = ?1 AND kind = 'terminal_status' AND {at} >= ?2
                 ORDER BY occurred_at DESC, id DESC LIMIT 1"
            ),
            params![run, since],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(at, payload)| {
            let p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            Status {
                at,
                state: p["state"].as_str().unwrap_or("").into(),
                kind: p["kind"].as_str().map(String::from),
            }
        });
    let gone: bool = c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM events
             WHERE run_id = ?1 AND kind = 'terminal_gone' AND {at} >= ?2)"
        ),
        params![run, since],
        |r| r.get(0),
    )?;
    let last_turn: i64 = c.query_row(
        &format!(
            "SELECT COALESCE(MAX(id), 0) FROM events
             WHERE run_id = ?1 AND kind = 'terminal_turn' AND {at} >= ?2"
        ),
        params![run, since],
        |r| r.get(0),
    )?;
    let report_done: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages WHERE run_id = ?1 AND kind = 'report_done')",
        [run],
        |r| r.get(0),
    )?;
    let reported_since_turn: bool = c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM messages m JOIN events e ON e.id = m.event_id
             WHERE m.run_id = ?1 AND m.kind IN ('report_done', 'report_blocked')
               AND e.id > ?2 AND {} >= ?3)",
            ms("e.occurred_at")
        ),
        params![run, last_turn, since],
        |r| r.get(0),
    )?;
    Ok(RunScene {
        launched,
        status,
        gone,
        report_done,
        reported_since_turn,
    })
}

fn secretary_scene(w: &StateWriter, launched: i64) -> Result<SecretaryScene> {
    let c = w.conn();
    let at = ms("e.occurred_at");
    let status_at: Option<i64> = c.query_row(
        &format!(
            "SELECT MAX({at}) FROM events e WHERE kind = 'terminal_status'
             AND json_extract(payload_json, '$.role') = 'secretary' AND {at} >= ?1"
        ),
        [launched],
        |r| r.get(0),
    )?;
    // A gone inside a renewal (under way, or done and the gone came in while it took) is no gone.
    let gone: bool = c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM events e WHERE kind = 'terminal_gone'
               AND json_extract(payload_json, '$.role') = 'secretary' AND {at} >= ?1
               AND NOT EXISTS (SELECT 1 FROM commands c
                 WHERE c.verb = 'renew' AND c.status IN ('accepted', 'done')
                   AND {} <= {at}
                   AND (c.finished_at IS NULL OR {at} <= {})))",
            ms("c.received_at"),
            ms("c.finished_at")
        ),
        [launched],
        |r| r.get(0),
    )?;
    Ok(SecretaryScene {
        launched,
        gone,
        status_at,
    })
}

/// Record a raise: the event, the notice if the gate sends it, the run's failure for
/// `gone`. The caller wraps it in a transaction.
fn raise(w: &StateWriter, s: &Subject, now: &str) -> Result<()> {
    use crate::db::writer::NewEvent;
    // The gate decides whether the secretary is told; a suppressed raise is still an event.
    let gate = super::notify_gate::decide(s.rule.name());
    let mut payload = s.payload();
    if !gate.send {
        payload["suppressed"] = gate.reason.into();
    }
    w.append_event(&NewEvent {
        kind: "anomaly_raised",
        actor: Some("makibad"),
        payload: Some(&payload),
        occurred_at: Some(now),
        run_task_id: s.task.as_deref(),
        ..Default::default()
    })?;
    let Some(task) = s.task.as_deref().filter(|_| gate.send) else {
        return Ok(());
    };
    if s.rule == Rule::Gone {
        w.update_run_status(
            task,
            "failed",
            Some(now),
            Some("terminal gone, no report_done"),
        )?;
    }
    // The notice (spec 5.4): the event, the message, the pending delivery with no attempt.
    let event = w.append_event(&NewEvent {
        kind: "message_sent",
        actor: Some("makibad"),
        occurred_at: Some(now),
        run_task_id: Some(task),
        ..Default::default()
    })?;
    let body = format!("Anomaly {} on {task}: {}", s.rule.name(), s.rule.text());
    w.conn().execute(
        "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
         VALUES (?1, 'makibad', ?2, 'notice', (SELECT id FROM runs WHERE task_id = ?3), ?4)",
        params![event, AGENT_SECRETARY, task, body],
    )?;
    let message = w.conn().last_insert_rowid();
    w.conn().execute(
        "UPDATE events SET payload_json = ?1 WHERE id = ?2",
        params![json!({"message_id": message}).to_string(), event],
    )?;
    w.conn().execute(
        "INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?1, ?2)",
        params![event, AGENT_SECRETARY],
    )?;
    Ok(())
}
