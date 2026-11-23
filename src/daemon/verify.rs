//! Verification (Issue M3-6, spec section 5.3): a launch goes from `dialog_passed` to `verified`
//! only once a `working` status or a turn of its own terminal is recorded, or to `failed` when
//! none is recorded within the timeout.
//!
//! "Its own terminal" is the launch's `spawn_id`: the monitor records every status and turn with
//! the terminal's `makiba.spawn` tag, and that value is minted for the launch, so a record that
//! carries it is of this launch's terminal and was made after the launch. A record of another
//! terminal of the same task or role (an earlier launch, or one without the tag) never counts.
//! `worker_spawn_verified` names the record in its payload (`event_id`); migration v6 refuses the
//! event without one, whoever writes it.
//!
//! A failure, here or in the dialogs (M3-5), is one transaction: `worker_spawn_failed`, stage
//! `failed`, the run `failed`, and a notice to the secretary saying what was seen (Issue 333).
//! A worker's terminal is left for inspection; the curator's and the secretary's are ended by
//! their own passes.
//!
//! The time makibad's loop was held is not charged to a launch (Issue 333). A launch's clock
//! starts at the latest of its `worker_spawned`, the end of the last stall of the loop (a gap
//! between two passes longer than `STALL`, or a pass that itself took that long), a key the dialog
//! step pressed into it (the first `KEYS_RESTART`), and its one grace: a launch at `launched`
//! whose screen shows a dialog of the policy list when the timeout is reached is given one more
//! timeout, for the dialog step to answer it. The time makibad was down still counts.

use super::dialogs::Decision;
use crate::agents;
use crate::base::{Base, State, Status, Terminal};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{AGENT_SECRETARY, ErrorKind, TAG_SPAWN};
use rusqlite::params;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a launch has to be verified, from its `worker_spawned` (the Issue's default).
pub const TIMEOUT: Duration = Duration::from_secs(180);
/// The test seam that sets the timeout in milliseconds (spec section 2).
pub const TIMEOUT_ENV: &str = "MAKIBA_VERIFY_TIMEOUT_MS";
/// How often a pass runs at most.
const EVERY: Duration = Duration::from_secs(1);
/// A gap between two passes (or one pass) longer than this is a stall of makibad's loop: the
/// clocks of the launches under way start again (Issue 333).
pub const STALL: Duration = Duration::from_secs(3);
/// How many keys of the dialog step restart a launch's clock at most: a dialog that keeps coming
/// back cannot keep a launch from failing.
pub const KEYS_RESTART: u32 = 8;

/// The timeout: the seam when it holds a number, otherwise `TIMEOUT`.
pub fn timeout_from_env() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

struct Row {
    id: i64,
    /// `None` for a launch without a run (Issue 166), and `task` with it.
    run_id: Option<i64>,
    task: Option<String>,
    spawn: String,
    stage: String,
    agent: String,
    worktree: String,
    /// Its `worker_spawned` (else its row's `created_at`), as the database writes times.
    launched: String,
}

/// The database's clock, in the form it writes times.
pub(super) fn now_text(w: &StateWriter) -> Result<String> {
    Ok(w.conn()
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |r| {
            r.get(0)
        })?)
}

pub struct Verify {
    timeout: Duration,
    next: Instant,
    /// When the last pass began.
    last: Option<Instant>,
    /// The end of the last stall of the loop seen by this process, as the database writes times.
    restarted: Option<String>,
    /// Per spawn id: when its one grace began (a known dialog on the screen at the timeout).
    graced: BTreeMap<String, String>,
}

impl Verify {
    pub fn new(timeout: Duration) -> Verify {
        Verify {
            timeout,
            next: Instant::now(),
            last: None,
            restarted: None,
            graced: BTreeMap::new(),
        }
    }

    /// One pass over the launches at `launched` and `dialog_passed`, at most once a second.
    /// `keyed`: per spawn id, the last key of the dialog step that restarts its clock. `tick`:
    /// when the loop's tick that calls this began. Every call, a pass or not, looks for a stall:
    /// more than `STALL` since the last call, or since the tick began (a pass before this one in
    /// the same tick was held, the first tick of the process included).
    pub fn pass(
        &mut self,
        w: &StateWriter,
        base: &dyn Base,
        keyed: &BTreeMap<String, String>,
        tick: Instant,
    ) -> Result<()> {
        let began = Instant::now();
        let held = began.saturating_duration_since(tick) > STALL
            || self
                .last
                .is_some_and(|l| began.saturating_duration_since(l) > STALL);
        if held {
            self.restarted = Some(now_text(w)?);
        }
        self.last = Some(began);
        if began < self.next {
            return Ok(());
        }
        self.next = began + EVERY;
        let rows: Vec<Row> = {
            let mut st = w.conn().prepare(
                "SELECT l.id, r.id, r.task_id, l.spawn_id, l.stage, l.agent, l.worktree,
                   COALESCE(
                     (SELECT max(e.occurred_at) FROM events e WHERE e.kind = 'worker_spawned'
                      AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id),
                     l.created_at)
                 FROM run_launch l LEFT JOIN runs r ON r.id = l.run_id
                 WHERE l.stage IN ('launched', 'dialog_passed')
                 ORDER BY l.id",
            )?;
            st.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    task: r.get(2)?,
                    spawn: r.get(3)?,
                    stage: r.get(4)?,
                    agent: r.get(5)?,
                    worktree: r.get(6)?,
                    launched: r.get(7)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        self.graced
            .retain(|spawn, _| rows.iter().any(|r| &r.spawn == spawn));
        let timeout = self.timeout.as_millis() as i64;
        for row in rows {
            let (evidence, waited) = self.clock(w, &row, keyed.get(&row.spawn))?;
            match evidence {
                Some(event) if row.stage == "dialog_passed" => {
                    let applied = verified(w, &row, event)?;
                    if !applied {
                        // Failed or suspended meanwhile (Issue 163): nothing runs for it.
                        let _ = super::resume::end_terminals(base, &row.spawn);
                    }
                }
                // Seen working in time, still at `launched` (a restart came after the timeout):
                // the dialog step moves it on (Issue M3-7).
                Some(_) => {}
                None if waited >= timeout => {
                    let sight = Sight::of(base, &row.spawn, Some(&row.agent));
                    // This pass was held itself (a list or read that blocked): a stall, and
                    // nothing is failed on what was read before it.
                    if began.elapsed() > STALL {
                        self.restarted = Some(now_text(w)?);
                        self.last = Some(Instant::now());
                        return Ok(());
                    }
                    // A dialog of the policy list on the screen: the dialog step, which runs
                    // before this pass in every tick, gets one more timeout to answer it.
                    if row.stage == "launched"
                        && sight.dialog.is_some()
                        && !self.graced.contains_key(&row.spawn)
                    {
                        self.graced.insert(row.spawn.clone(), now_text(w)?);
                        continue;
                    }
                    let why = format!(
                        "no working status or turn of its terminal within {} ms of the launch",
                        self.timeout.as_millis()
                    );
                    let task = row.task.as_deref();
                    if !fail(w, row.id, task, &row.spawn, &row.stage, &why, &sight)? {
                        let _ = super::resume::end_terminals(base, &row.spawn);
                    }
                }
                None => {}
            }
        }
        Ok(())
    }

    /// For `row`: the first working status or turn recorded with its spawn id within the timeout
    /// of its clock's start (one recorded later does not count, even if a pass sees it before the
    /// launch is failed), and the milliseconds since that start.
    fn clock(
        &self,
        w: &StateWriter,
        row: &Row,
        keyed: Option<&String>,
    ) -> Result<(Option<i64>, i64)> {
        Ok(w.conn().query_row(
            "SELECT (SELECT min(e.id) FROM events e
                     WHERE json_extract(e.payload_json, '$.spawn_id') = ?1
                       AND (e.kind = 'terminal_turn' OR (e.kind = 'terminal_status'
                            AND json_extract(e.payload_json, '$.state') = 'working'))
                       AND julianday(e.occurred_at) <= s.start + ?2 / 86400000.0),
                    CAST((julianday('now') - s.start) * 86400000 AS INTEGER)
             FROM (SELECT max(julianday(?3), COALESCE(julianday(?4), 0),
                              COALESCE(julianday(?5), 0), COALESCE(julianday(?6), 0)) AS start) s",
            params![
                row.spawn,
                self.timeout.as_millis() as i64,
                row.launched,
                self.restarted,
                self.graced.get(&row.spawn),
                keyed,
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
}

/// What a pass saw of a launch's terminals when it failed it (Issue 333), for the record and the
/// notice.
#[derive(Debug, Clone, Default)]
pub(super) struct Sight {
    /// The base could not list: nothing is known of the terminals.
    unlisted: bool,
    /// Name and address of every terminal carrying the launch's spawn id.
    terminals: Vec<(String, String)>,
    /// The first terminal's status.
    status: Option<Status>,
    /// The dialog of the policy list the first terminal's screen shows, as the dialog step reads
    /// it (one it would press a key for).
    dialog: Option<&'static str>,
    screen: Vec<String>,
}

impl Sight {
    /// No terminal of the launch (it failed before one was launched, or none carries it).
    pub(super) fn none() -> Sight {
        Sight::default()
    }

    /// The terminals carrying `spawn` as the base lists them, the first one's screen read and
    /// asked against `agent`'s policy list.
    pub(super) fn of(base: &dyn Base, spawn: &str, agent: Option<&str>) -> Sight {
        let Ok(list) = base.list() else {
            return Sight {
                unlisted: true,
                ..Sight::default()
            };
        };
        let ours: Vec<&Terminal> = list
            .iter()
            .filter(|t| t.tags.get(TAG_SPAWN).map(String::as_str) == Some(spawn))
            .collect();
        let screen = ours
            .first()
            .and_then(|t| base.read(&t.id, 0).ok())
            .unwrap_or_default();
        Sight::with_screen(&ours, screen, agent)
    }

    /// `terminals` carrying the launch, the first one showing `screen`.
    pub(super) fn with_screen(
        terminals: &[&Terminal],
        screen: Vec<String>,
        agent: Option<&str>,
    ) -> Sight {
        let policies = agent
            .and_then(agents::by_name)
            .map(|a| a.startup_dialogs().to_vec())
            .unwrap_or_default();
        let dialog = match super::dialogs::decide(&policies, &screen) {
            Decision::Key { dialog, .. } if !terminals.is_empty() => Some(dialog),
            _ => None,
        };
        Sight {
            unlisted: false,
            terminals: terminals
                .iter()
                .map(|t| (t.name.clone(), t.id.0.clone()))
                .collect(),
            status: terminals.first().and_then(|t| t.status.clone()),
            dialog,
            screen,
        }
    }

    /// The kind of what was seen, as the record names it.
    fn kind(&self) -> &'static str {
        match self.terminals.len() {
            _ if self.unlisted => "unlisted",
            0 => "no_terminal",
            1 => match (&self.dialog, &self.status) {
                (Some(_), _) => "dialog",
                (None, None) => "no_status",
                (None, Some(s)) if s.state == State::Working => "working_late",
                (None, Some(_)) => "not_working",
            },
            _ => "several",
        }
    }

    /// What was seen, in words.
    fn said(&self) -> String {
        match self.kind() {
            "unlisted" => "nothing, the base could not list its terminals".into(),
            "no_terminal" => "no terminal of the launch".into(),
            "several" => format!("{} terminals carry the launch", self.terminals.len()),
            "dialog" => format!(
                "the known dialog {} still shown",
                self.dialog.unwrap_or_default()
            ),
            "no_status" => "never any status".into(),
            "working_late" => "a working status recorded after the timeout".into(),
            _ => format!(
                "a status but not working: {}",
                self.status
                    .as_ref()
                    .map(|s| format!("{:?}", s.state).to_lowercase())
                    .unwrap_or_default()
            ),
        }
    }

    /// The terminals, as the notice names them: empty when there is none.
    fn named(&self) -> String {
        let each: Vec<String> = self
            .terminals
            .iter()
            .map(|(name, address)| format!("{name} at {address}"))
            .collect();
        match each.len() {
            0 => String::new(),
            1 => format!(" Its terminal: {}.", each[0]),
            _ => format!(" Its terminals: {}.", each.join(", ")),
        }
    }
}

/// What ends the terminal of a failed launch of `role`, as the notice says it.
fn fate(role: &str) -> &'static str {
    match role {
        "curator" => " It is ended at once, its key revoked.",
        "secretary" => " It is ended when the secretary is launched again.",
        _ => " It is left for inspection until the run is closed.",
    }
}

/// `worker_spawn_verified`, stage `verified` and the run `in_use` (when there is one), together;
/// the conversation id when the agent module has one (a resumed launch keeps the id it resumed). Only over
/// `dialog_passed` (Issue 163): `false`, and nothing written, when the launch was moved meanwhile.
fn verified(w: &StateWriter, row: &Row, event: i64) -> Result<bool> {
    let conversation = agents::by_name(&row.agent)
        .and_then(|a| a.conversation_id(Path::new(&row.worktree), &agents::session_id(&row.spawn)));
    super::crash::hold("verified");
    w.transaction(|w| {
        let n = w.conn().execute(
            "UPDATE run_launch SET stage = 'verified',
               conversation_id = COALESCE(conversation_id, ?2),
               verified_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1 AND stage = 'dialog_passed'",
            params![row.id, conversation],
        )?;
        if n == 0 {
            return Ok(false);
        }
        w.append_event(&NewEvent {
            kind: "worker_spawn_verified",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": row.task, "spawn_id": row.spawn, "event_id": event})),
            run_task_id: row.task.as_deref(),
            ..Default::default()
        })?;
        // ponytail: a run closed while its launch was under way is not brought back.
        if let Some(run) = row.run_id {
            w.conn().execute(
                "UPDATE runs SET status = 'in_use' WHERE id = ?1 AND status IN ('queued', 'suspended')",
                [run],
            )?;
        }
        if let Some(task) = row.task.as_deref() {
            super::door::finish_dispatch(w, task, None)?;
        }
        Ok(true)
    })
}

/// A launch that cannot be verified, at `stage`: `worker_spawn_failed` with what was seen
/// (`sight`: the screen, its kind, the terminals), stage `failed`, the run `failed` (when there is
/// one), and a notice to the secretary if the gate sends it, together. No terminal is ended here.
/// `task` is `None` for a launch without a run (Issue 166).
///
/// A resume of a `suspended` run (Issue 41: a launch with a conversation id before it is
/// verified) does not fail its run: its keys are revoked and a fresh launch is added with the
/// same brief and workplace, and the relaunch ends the terminal left here.
///
/// Only over `stage` (Issue 163): `false`, and nothing written, when the launch was moved meanwhile.
pub(super) fn fail(
    w: &StateWriter,
    id: i64,
    task: Option<&str>,
    spawn: &str,
    stage: &str,
    why: &str,
    sight: &Sight,
) -> Result<bool> {
    super::crash::hold("failed");
    w.transaction(|w| {
        let (resumed, name, role) = w.conn().query_row(
            "SELECT l.conversation_id IS NOT NULL AND l.stage != 'verified'
                    AND COALESCE(r.status = 'suspended', 0),
                    COALESCE(r.task_id, l.role), l.role
             FROM run_launch l LEFT JOIN runs r ON r.id = l.run_id WHERE l.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, bool>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?;
        let n = w.conn().execute(
            "UPDATE run_launch SET stage = 'failed', failed_reason = ?2 WHERE id = ?1 AND stage = ?3",
            params![id, why, stage],
        )?;
        if n == 0 {
            return Ok(false);
        }
        w.append_event(&NewEvent {
            kind: "worker_spawn_failed",
            actor: Some("makibad"),
            payload: Some(&json!({
                "spawn_id": spawn,
                "stage": stage,
                "reason": why,
                "screen": sight.screen.join("\n"),
                "seen": sight.kind(),
                "dialog": sight.dialog,
                "terminals": sight.terminals.iter()
                    .map(|(name, address)| json!({"name": name, "address": address}))
                    .collect::<Vec<_>>(),
            })),
            run_task_id: task,
            ..Default::default()
        })?;
        if resumed {
            super::resume::revoke_keys(w, spawn)?;
            super::launch::add_relaunch(w, id, None)?;
            return Ok(true);
        }
        if let Some(task) = task {
            w.conn().execute(
                "UPDATE runs SET status = 'failed', outcome_note = ?2,
                   completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE task_id = ?1 AND status IN ('queued', 'suspended')",
                params![task, format!("launch failed: {why}")],
            )?;
            let reason = format!("launch failed at stage {stage}: {why}");
            super::door::finish_dispatch(w, task, Some((ErrorKind::Internal, &reason)))?;
            // The belt (Issue 282): the second failed launch of a range in a row pauses it.
            super::belt::launch_failed(w, task)?;
        }
        if !super::notify_gate::decide("worker_spawn_failed").send {
            return Ok(true);
        }
        let fate = if sight.terminals.is_empty() {
            ""
        } else {
            fate(&role)
        };
        let body = format!(
            "Launch of {name} failed at stage {stage}: {why}. Seen: {}.{}{fate}",
            sight.said(),
            sight.named()
        );
        notify_secretary(w, task, &body)?;
        Ok(true)
    })
}

/// The notice (spec 5.4) to the secretary: the event, the message (about `task`'s run, or none),
/// the pending delivery with no attempt.
pub(super) fn notify_secretary(w: &StateWriter, task: Option<&str>, body: &str) -> Result<()> {
    let event = w.append_event(&NewEvent {
        kind: "message_sent",
        actor: Some("makibad"),
        run_task_id: task,
        ..Default::default()
    })?;
    w.conn().execute(
        "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
         VALUES (?1, 'makibad', ?2, 'notice', (SELECT id FROM runs WHERE task_id = ?3), ?4)",
        params![event, AGENT_SECRETARY, task, body],
    )?;
    w.conn().execute(
        "UPDATE events SET payload_json = json_object('message_id', ?1) WHERE id = ?2",
        params![w.conn().last_insert_rowid(), event],
    )?;
    w.conn().execute(
        "INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?1, ?2)",
        params![event, AGENT_SECRETARY],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::stub::Stub;
    use crate::base::{LaunchSpec, TerminalRef};
    use crate::db::writer::RunUpsert;
    use std::collections::BTreeMap;

    /// A queued run `t1` with a launch `s1` at `stage`, launched `ago_ms` before now.
    fn org(stage: &str, ago_ms: i64) -> (tempfile::TempDir, StateWriter) {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                 VALUES (1, 's1', 'fake', 'b', '/w', ?1)",
                [stage],
            )
            .unwrap();
        let at: String = w
            .conn()
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
                [format!("-{} seconds", ago_ms as f64 / 1000.0)],
                |r| r.get(0),
            )
            .unwrap();
        w.append_event(&NewEvent {
            kind: "worker_spawned",
            payload: Some(&json!({"task_id": "t1", "spawn_id": "s1"})),
            occurred_at: Some(&at),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap();
        (dir, w)
    }

    fn record(w: &StateWriter, kind: &str, spawn: &str, extra: serde_json::Value) -> i64 {
        let mut p = json!({"task_id": "t1", "role": "worker", "spawn_id": spawn});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        w.append_event(&NewEvent {
            kind,
            payload: Some(&p),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap()
    }

    fn one<T: rusqlite::types::FromSql>(w: &StateWriter, sql: &str) -> T {
        w.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Two passes, as a second apart.
    fn passes(v: &mut Verify, w: &StateWriter, base: &dyn Base) {
        for _ in 0..2 {
            v.next = Instant::now();
            v.pass(w, base, &BTreeMap::new(), Instant::now()).unwrap();
        }
    }

    #[test]
    fn the_timeout_is_180_s_unless_the_seam_says_otherwise() {
        assert_eq!(TIMEOUT, Duration::from_secs(180));
        // SAFETY: no other test of this binary reads or writes this variable.
        unsafe { std::env::remove_var(TIMEOUT_ENV) };
        assert_eq!(timeout_from_env(), TIMEOUT);
    }

    /// A turn of the launch's own terminal verifies it.
    #[test]
    fn a_turn_verifies() {
        let (_d, w) = org("dialog_passed", 0);
        let turn = record(&w, "terminal_turn", "s1", json!({"turn": 1}));
        let mut v = Verify::new(TIMEOUT);
        v.pass(&w, &Stub::new(), &BTreeMap::new(), Instant::now())
            .unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "verified"
        );
        assert_eq!(one::<String>(&w, "SELECT status FROM runs"), "in_use");
        let p: String = one(
            &w,
            "SELECT payload_json FROM events WHERE kind = 'worker_spawn_verified'",
        );
        let p: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(p["event_id"], turn);
    }

    /// Records of another terminal, an idle status, or a launch still at `launched` verify
    /// nothing; within the timeout nothing fails either.
    #[test]
    fn nothing_but_the_launchs_own_working_or_turn_verifies() {
        let (_d, w) = org("dialog_passed", 0);
        record(&w, "terminal_status", "s1", json!({"state": "idle"}));
        record(&w, "terminal_status", "other", json!({"state": "working"}));
        record(&w, "terminal_turn", "other", json!({"turn": 1}));
        let mut v = Verify::new(TIMEOUT);
        passes(&mut v, &w, &Stub::new());
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "dialog_passed"
        );
        let (_d, w) = org("launched", 0);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        passes(&mut v, &w, &Stub::new());
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
    }

    /// Past the timeout: failed with the screen, the run failed, one notice; the terminal is not
    /// ended. A launch at `launched` times out even with evidence (it never left the dialogs).
    #[test]
    fn a_timeout_fails_the_launch_and_the_run_and_leaves_the_terminal() {
        let (_d, w) = org("launched", 10_000);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        let stub = Stub::new();
        stub.launch(&LaunchSpec {
            argv: vec![],
            cwd: "/".into(),
            name: "w-t1".into(),
            tags: BTreeMap::from([(TAG_SPAWN.to_string(), "s1".to_string())]),
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(&w, &stub, &BTreeMap::new(), Instant::now()).unwrap();
        assert_eq!(one::<String>(&w, "SELECT stage FROM run_launch"), "failed");
        assert_eq!(one::<String>(&w, "SELECT status FROM runs"), "failed");
        let p: String = one(
            &w,
            "SELECT payload_json FROM events WHERE kind = 'worker_spawn_failed'",
        );
        let p: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(p["stage"], "launched");
        assert_eq!(p["spawn_id"], "s1");
        assert_eq!(
            one::<i64>(
                &w,
                "SELECT count(*) FROM messages m JOIN event_deliveries d
                 ON d.source_event_id = m.event_id
                 WHERE m.kind = 'notice' AND m.recipient = 'secretary' AND m.run_id = 1"
            ),
            1
        );
        assert!(stub.get(&TerminalRef("w-t1".into())).unwrap().alive);
    }

    /// Evidence recorded after the timeout does not verify: the launch fails.
    #[test]
    fn evidence_recorded_after_the_timeout_does_not_verify() {
        let (_d, w) = org("dialog_passed", 10_000);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(&w, &Stub::new(), &BTreeMap::new(), Instant::now())
            .unwrap();
        assert_eq!(one::<String>(&w, "SELECT stage FROM run_launch"), "failed");
        // Recorded within it, the same evidence verifies however late the pass.
        let (_d, w) = org("dialog_passed", 10_000);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        let mut v = Verify::new(Duration::from_secs(60));
        v.pass(&w, &Stub::new(), &BTreeMap::new(), Instant::now())
            .unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "verified"
        );
    }

    /// A dialog failure (M3-5) takes the same path: the run failed and the secretary told.
    #[test]
    fn a_dialog_failure_fails_the_run_and_tells_the_secretary() {
        let (_d, w) = org("launched", 0);
        fail(
            &w,
            1,
            Some("t1"),
            "s1",
            "launched",
            "an unknown dialog",
            &Sight::none(),
        )
        .unwrap();
        assert_eq!(one::<String>(&w, "SELECT status FROM runs"), "failed");
        assert_eq!(
            one::<String>(
                &w,
                "SELECT body FROM messages WHERE kind = 'notice' AND recipient = 'secretary'"
            ),
            "Launch of t1 failed at stage launched: an unknown dialog. Seen: no terminal of the launch."
        );
    }

    /// A terminal `name` carrying the spawn id `s1`.
    fn terminal(base: &Stub, name: &str) {
        base.launch(&LaunchSpec {
            argv: vec![],
            cwd: "/".into(),
            name: name.into(),
            tags: BTreeMap::from([(TAG_SPAWN.to_string(), "s1".to_string())]),
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
    }

    /// The payload of the one `worker_spawn_failed` and the body of the one notice.
    fn failure(w: &StateWriter) -> (serde_json::Value, String) {
        let p: String = one(
            w,
            "SELECT payload_json FROM events WHERE kind = 'worker_spawn_failed'",
        );
        let body = one(w, "SELECT body FROM messages WHERE kind = 'notice'");
        (serde_json::from_str(&p).unwrap(), body)
    }

    /// The database's clock `ms` ago, as it writes times.
    fn ago(w: &StateWriter, ms: i64) -> String {
        one(
            w,
            &format!(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{} seconds')",
                ms as f64 / 1000.0
            ),
        )
    }

    /// Issue 333: a gap between passes longer than `STALL` starts the clocks again. A launch 10 s
    /// past a 5 s timeout is not failed, and a working status recorded now verifies it.
    #[test]
    fn a_stall_of_the_loop_restarts_the_clock() {
        let (_d, w) = org("dialog_passed", 10_000);
        record(&w, "terminal_status", "s1", json!({"state": "working"}));
        let mut v = Verify::new(Duration::from_secs(5));
        v.last = Instant::now().checked_sub(STALL + Duration::from_secs(1));
        v.pass(&w, &Stub::new(), &BTreeMap::new(), Instant::now())
            .unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "verified"
        );
        // At `launched`, with no record: not failed either.
        let (_d, w) = org("launched", 10_000);
        let mut v = Verify::new(Duration::from_secs(5));
        v.last = Instant::now().checked_sub(STALL + Duration::from_secs(1));
        v.pass(&w, &Stub::new(), &BTreeMap::new(), Instant::now())
            .unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
        // The first call of the process, in a tick held before it: a stall too.
        let (_d, w) = org("launched", 10_000);
        let mut v = Verify::new(Duration::from_secs(5));
        let tick = Instant::now()
            .checked_sub(STALL + Duration::from_secs(1))
            .unwrap();
        v.pass(&w, &Stub::new(), &BTreeMap::new(), tick).unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
        // A call that only looks (before the next pass is due) still sees a stall.
        let (_d, w) = org("launched", 10_000);
        let mut v = Verify::new(Duration::from_secs(5));
        v.next = Instant::now() + Duration::from_secs(60);
        v.last = Instant::now().checked_sub(STALL + Duration::from_secs(1));
        v.pass(&w, &Stub::new(), &BTreeMap::new(), Instant::now())
            .unwrap();
        assert!(v.restarted.is_some());
    }

    /// The stand-in whose list waits `.1` and whose read with no history is its whole screen.
    struct Held(Stub, Duration);

    impl Base for Held {
        fn name(&self) -> &'static str {
            self.0.name()
        }
        fn hello(&self) -> std::result::Result<crate::base::BaseInfo, crate::base::BaseError> {
            self.0.hello()
        }
        fn launch(
            &self,
            s: &LaunchSpec,
        ) -> std::result::Result<TerminalRef, crate::base::BaseError> {
            self.0.launch(s)
        }
        fn list(&self) -> std::result::Result<Vec<Terminal>, crate::base::BaseError> {
            std::thread::sleep(self.1);
            self.0.list()
        }
        fn get(&self, t: &TerminalRef) -> std::result::Result<Terminal, crate::base::BaseError> {
            self.0.get(t)
        }
        fn read(
            &self,
            t: &TerminalRef,
            lines: usize,
        ) -> std::result::Result<Vec<String>, crate::base::BaseError> {
            self.0.read(t, if lines == 0 { usize::MAX } else { lines })
        }
        fn keys(
            &self,
            t: &TerminalRef,
            k: &[&str],
        ) -> std::result::Result<(), crate::base::BaseError> {
            self.0.keys(t, k)
        }
        fn send(
            &self,
            t: &TerminalRef,
            text: &str,
            idle: bool,
        ) -> std::result::Result<(), crate::base::BaseError> {
            self.0.send(t, text, idle)
        }
        fn end(&self, t: &TerminalRef) -> std::result::Result<(), crate::base::BaseError> {
            self.0.end(t)
        }
        fn events(&self) -> std::result::Result<crate::base::EventStream, crate::base::BaseError> {
            self.0.events()
        }
        fn start(&self) -> std::result::Result<(), crate::base::BaseError> {
            self.0.start()
        }
        fn worker_denies(&self) -> crate::base::BaseDenies {
            self.0.worker_denies()
        }
    }

    /// Issue 333: a pass held in its own read of the screen fails nothing on it, and the clock
    /// starts again after it.
    #[test]
    fn a_pass_held_in_its_own_read_fails_nothing() {
        let (_d, w) = org("launched", 10_000);
        let stub = Stub::new();
        terminal(&stub, "w-t1");
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(
            &w,
            &Held(stub, STALL + Duration::from_millis(200)),
            &BTreeMap::new(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
        assert!(v.restarted.is_some());
    }

    /// The dev-channel dialog as Claude Code draws it, cursor on the line to choose.
    const DEV_CHANNEL: [&str; 8] = [
        "  WARNING: Loading development channels",
        "",
        "  Channels: server:makiba-channel",
        "",
        "  \u{276f} 1. I am using this for local development",
        "    2. Exit",
        "",
        "  Enter to confirm \u{b7} Esc to cancel",
    ];

    /// Issue 333: a launch at `launched` whose screen shows a dialog of the policy list at the
    /// timeout gets one more timeout; past that it fails, the notice naming the dialog.
    #[test]
    fn a_known_dialog_on_the_screen_gets_one_grace() {
        let (_d, w) = org("launched", 10_000);
        let stub = Held(Stub::scripted(&DEV_CHANNEL), Duration::ZERO);
        terminal(&stub.0, "w-t1");
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(&w, &stub, &BTreeMap::new(), Instant::now()).unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
        assert!(v.graced.contains_key("s1"));
        // Within the grace: still not failed.
        v.next = Instant::now();
        v.pass(&w, &stub, &BTreeMap::new(), Instant::now()).unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
        // Past it: failed, the dialog named; no second grace.
        v.graced.insert("s1".into(), ago(&w, 6_000));
        v.next = Instant::now();
        v.pass(&w, &stub, &BTreeMap::new(), Instant::now()).unwrap();
        assert_eq!(one::<String>(&w, "SELECT stage FROM run_launch"), "failed");
        let (p, body) = failure(&w);
        assert_eq!(p["seen"], "dialog");
        assert_eq!(p["dialog"], "dev-channel");
        assert_eq!(p["terminals"], json!([{"name": "w-t1", "address": "w-t1"}]));
        assert_eq!(
            body,
            "Launch of t1 failed at stage launched: no working status or turn of its terminal \
             within 5000 ms of the launch. Seen: the known dialog dev-channel still shown. Its \
             terminal: w-t1 at w-t1. It is left for inspection until the run is closed."
        );
    }

    /// Issue 333: a key the dialog step pressed starts the launch's clock again.
    #[test]
    fn a_dialog_key_restarts_the_clock() {
        let (_d, w) = org("launched", 10_000);
        let mut v = Verify::new(Duration::from_secs(5));
        let keyed = BTreeMap::from([("s1".to_string(), ago(&w, 1_000))]);
        v.pass(&w, &Stub::new(), &keyed, Instant::now()).unwrap();
        assert_eq!(
            one::<String>(&w, "SELECT stage FROM run_launch"),
            "launched"
        );
        // A key longer ago than the timeout does not hold it.
        let keyed = BTreeMap::from([("s1".to_string(), ago(&w, 6_000))]);
        v.next = Instant::now();
        v.pass(&w, &Stub::new(), &keyed, Instant::now()).unwrap();
        assert_eq!(one::<String>(&w, "SELECT stage FROM run_launch"), "failed");
    }

    /// Issue 333: what was seen is told: a status but not working, or several terminals.
    #[test]
    fn the_notice_says_what_was_seen() {
        let (_d, w) = org("dialog_passed", 10_000);
        let stub = Stub::new();
        terminal(&stub, "w-t1");
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(&w, &stub, &BTreeMap::new(), Instant::now()).unwrap();
        let (p, body) = failure(&w);
        assert_eq!(p["seen"], "not_working");
        assert!(
            body.ends_with(
                "Seen: a status but not working: idle. Its terminal: w-t1 at w-t1. It is left for \
                 inspection until the run is closed."
            ),
            "{body}"
        );

        let (_d, w) = org("dialog_passed", 10_000);
        let stub = Stub::new();
        terminal(&stub, "w-t1");
        terminal(&stub, "w-t1b");
        let mut v = Verify::new(Duration::from_secs(5));
        v.pass(&w, &stub, &BTreeMap::new(), Instant::now()).unwrap();
        let (p, body) = failure(&w);
        assert_eq!(p["seen"], "several");
        assert!(
            body.contains(
                "Seen: 2 terminals carry the launch. Its terminals: w-t1 at w-t1, w-t1b at w-t1b."
            ),
            "{body}"
        );
    }
}
