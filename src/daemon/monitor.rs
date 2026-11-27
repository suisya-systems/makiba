//! What the organization's terminals report, written down as it happens (spec section 10):
//! `terminal_status`, `terminal_turn` and `terminal_gone`, for terminals that carry this
//! organization's `makiba.org` and tied to their run by `makiba.task_id`.
//!
//! The monitor lives as long as the daemon and is fed every `BaseEvent` of the subscription. It
//! keeps, per terminal, the tags and whether it exited (a removal carries only an id), the last
//! status that was written and the last turn that was written. A status identical to the last
//! written one is not written again; the record is updated only after the write succeeded.
//! A `Resync` rebuilds that memory from what is recorded and reconciles it with the base's list
//! (`recover.rs`, M2-5).
//!
//! An event is queued when it arrives, with the time it arrived, and written in arrival order.
//! One the database refused stays at the head of the queue and is tried again on every tick, with
//! its arrival time as `occurred_at`, for the retry window (`RETRY`, Issue 181); after it the event
//! is dropped, said in the log and recorded as `observation_lost` once the database takes writes.
//! A `Resync` is never dropped.
//!
//! Test seam (Issue 571): with `MAKIBA_WRITE_LOST_MARK` naming a file, the monitor appends to it,
//! when it drops an event, one JSON line per record it will write as `observation_lost`, before
//! that record is written. A test that holds the write lock reads it to know the event is dropped,
//! which nothing in the ledger can show while the lock is held.

use crate::base::{BaseEvent, BlockKind, State, Terminal, TerminalEvent, TerminalRef};
use crate::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long an event the database refused is tried again before it is recorded as lost.
pub const RETRY: Duration = Duration::from_secs(60);
/// The test seam that sets the retry window in milliseconds (spec section 10).
pub const RETRY_ENV: &str = "MAKIBA_WRITE_RETRY_MS";

/// The test seam that names the file the monitor marks a dropped event in (spec section 2).
pub const LOST_MARK_ENV: &str = "MAKIBA_WRITE_LOST_MARK";

/// The retry window: the seam when it holds a number, otherwise `RETRY`.
pub fn retry_from_env() -> Duration {
    std::env::var(RETRY_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(RETRY)
}

/// The last status written for a terminal: what is compared, nothing else.
pub(super) type Written = (State, Option<BlockKind>, Option<String>);

#[derive(Clone)]
pub(super) struct Tracked {
    pub(super) tags: BTreeMap<String, String>,
    pub(super) exited: bool,
}

/// What the monitor remembers. Cloned for a resync, so a rolled-back one leaves it as it was.
#[derive(Clone, Default)]
pub(super) struct Memory {
    pub(super) tracked: HashMap<TerminalRef, Tracked>,
    status: HashMap<TerminalRef, Written>,
    turn: HashMap<TerminalRef, u64>,
    /// The arrival time of the event being written: its records' `occurred_at`.
    at: Option<String>,
}

/// An event waiting to be written: its arrival time, and when it was first refused.
struct Queued {
    event: BaseEvent,
    at: Option<String>,
    refused: Option<Instant>,
}

pub struct Monitor {
    org: String,
    mem: Memory,
    /// A `Resync` has been handled: the next is not the start.
    started: bool,
    /// The events not yet written, oldest first.
    queue: VecDeque<Queued>,
    /// `observation_lost` payloads not yet written.
    lost: Vec<Value>,
    window: Duration,
    /// The file a dropped event is marked in (`LOST_MARK_ENV`), if set.
    mark: Option<PathBuf>,
}

/// The identity of a terminal in the records: its `makiba.spawn` tag as it is, `None` without one.
/// Never made up here (spec section 10).
pub(super) fn spawn_of(tags: &BTreeMap<String, String>) -> Option<String> {
    tags.get(crate::spec::TAG_SPAWN).cloned()
}

pub(super) fn mine(org: &str, tags: &BTreeMap<String, String>) -> bool {
    tags.get("makiba.org").map(String::as_str) == Some(org)
}

fn state_name(s: State) -> &'static str {
    match s {
        State::Idle => "idle",
        State::Working => "working",
        State::Blocked => "blocked",
        State::Done => "done",
        State::Error => "error",
    }
}

pub(super) fn parse_state(s: &str) -> Option<State> {
    [
        State::Idle,
        State::Working,
        State::Blocked,
        State::Done,
        State::Error,
    ]
    .into_iter()
    .find(|&x| state_name(x) == s)
}

fn kind_name(k: BlockKind) -> &'static str {
    match k {
        BlockKind::Permission => "permission",
        BlockKind::Question => "question",
        BlockKind::Auth => "auth",
    }
}

pub(super) fn parse_kind(s: &str) -> Option<BlockKind> {
    [BlockKind::Permission, BlockKind::Question, BlockKind::Auth]
        .into_iter()
        .find(|&x| kind_name(x) == s)
}

impl Monitor {
    /// `org` is the value of `makiba.org` of this daemon's own organization; the retry window is
    /// read from the environment (`retry_from_env`).
    pub fn new(org: &str) -> Monitor {
        let window = retry_from_env();
        Monitor {
            org: org.into(),
            mem: Memory::default(),
            started: false,
            queue: VecDeque::new(),
            lost: Vec::new(),
            window,
            mark: std::env::var_os(LOST_MARK_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }

    /// A `Resync` has been handled and none is waiting: the view is built.
    pub fn reconciled(&self) -> bool {
        self.started
            && !self
                .queue
                .iter()
                .any(|q| matches!(q.event, BaseEvent::Resync(_)))
    }

    /// Queue an event with `seen`, the time the subscription received it, formatted by SQLite
    /// (which takes no lock for it).
    pub fn push(&mut self, w: &StateWriter, event: BaseEvent, seen: SystemTime) {
        let secs = seen
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let at = w
            .conn()
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')",
                [secs],
                |r| r.get(0),
            )
            .ok();
        self.queue.push_back(Queued {
            event,
            at,
            refused: None,
        });
    }

    /// Write what is queued, in order, until the database refuses one; called on every tick.
    pub fn drain(&mut self, w: &StateWriter) {
        self.write_lost(w);
        while let Some(q) = self.queue.front() {
            let event = q.event.clone();
            self.mem.at = q.at.clone();
            let result = self.handle(w, event);
            self.mem.at = None;
            let Err(e) = result else {
                self.queue.pop_front();
                continue;
            };
            let q = self.queue.front_mut().expect("the head was just tried");
            let first = q.refused.is_none();
            let since = *q.refused.get_or_insert_with(Instant::now);
            if !matches!(q.event, BaseEvent::Resync(_)) && since.elapsed() >= self.window {
                let q = self.queue.pop_front().expect("the head was just tried");
                eprintln!(
                    "makibad: lost a terminal event after {} ms of retries: {e}",
                    since.elapsed().as_millis()
                );
                let lost: Vec<Value> = self
                    .mem
                    .unwritten(&self.org, &q.event)
                    .into_iter()
                    .map(|(kind, task, role, spawn)| {
                        json!({
                            "kind": kind, "task_id": task, "role": role, "spawn_id": spawn,
                            "observed_at": q.at, "error": e.to_string(),
                        })
                    })
                    .collect();
                self.mark_dropped(&lost);
                self.lost.extend(lost);
                continue;
            }
            if first {
                eprintln!("makibad: cannot record a terminal event (tried again): {e}");
            }
            break;
        }
        self.write_lost(w);
    }

    /// The test seam: one line per record of a dropped event, appended to the mark file if one is
    /// named. A mark that cannot be written is said in the log; the drop goes on.
    fn mark_dropped(&self, lost: &[Value]) {
        let Some(path) = &self.mark else {
            return;
        };
        let text: String = lost.iter().map(|p| format!("{p}\n")).collect();
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = written {
            eprintln!(
                "makibad: cannot mark a dropped event in {}: {e}",
                path.display()
            );
        }
    }

    /// The losses recorded so far, each once; one the database refuses waits for the next tick.
    fn write_lost(&mut self, w: &StateWriter) {
        while let Some(p) = self.lost.first() {
            let task = p["task_id"].as_str().map(str::to_string);
            if append(w, "observation_lost", task.as_deref(), p).is_err() {
                return;
            }
            self.lost.remove(0);
        }
    }

    pub(super) fn handle(&mut self, w: &StateWriter, event: BaseEvent) -> crate::db::Result<()> {
        match event {
            BaseEvent::Resync(list) => self.resync(w, list),
            BaseEvent::Event(TerminalEvent::Created(t) | TerminalEvent::StatusChanged(t)) => {
                self.mem.observe(&self.org, w, &t)
            }
            BaseEvent::Event(TerminalEvent::Exited(t)) => {
                self.mem.observe(&self.org, w, &t)?;
                self.mem.gone(w, &t.id, Some(t.exit_code), "exited")
            }
            BaseEvent::Event(TerminalEvent::Removed(id)) => {
                let result = self.mem.gone(w, &id, None, "removed");
                if result.is_ok() {
                    self.mem.forget(&id);
                }
                result
            }
        }
    }

    /// Rebuild the view from `list` in one transaction (`recover.rs`). The memory changes only
    /// when it commits; on a failure the `Resync` stays queued.
    fn resync(&mut self, w: &StateWriter, list: Vec<Terminal>) -> crate::db::Result<()> {
        let reason = if self.started { "resync" } else { "start" };
        let mut mem = self.mem.clone();
        w.transaction(|w| super::recover::rebuild(w, &self.org, &mut mem, &list, reason))?;
        self.mem = mem;
        self.started = true;
        Ok(())
    }
}

impl Memory {
    pub(super) fn forget(&mut self, id: &TerminalRef) {
        self.tracked.remove(id);
        self.status.remove(id);
        self.turn.remove(id);
    }

    /// A terminal address now carries another identity than the one remembered for it: nothing
    /// remembered under that address belongs to the new terminal.
    pub(super) fn retag(&mut self, t: &Terminal) {
        if let Some(old) = self.tracked.get(&t.id)
            && spawn_of(&old.tags) != spawn_of(&t.tags)
        {
            self.forget(&t.id);
        }
    }

    /// What was recorded for a terminal before this process started.
    pub(super) fn restore(&mut self, id: &TerminalRef, status: Option<Written>, turn: Option<u64>) {
        if let Some(s) = status {
            self.status.insert(id.clone(), s);
        }
        if let Some(t) = turn {
            self.turn.insert(id.clone(), t);
        }
    }

    /// The status and turn of a terminal of ours, each written if it differs from the last written.
    pub(super) fn observe(
        &mut self,
        org: &str,
        w: &StateWriter,
        t: &Terminal,
    ) -> crate::db::Result<()> {
        if !mine(org, &t.tags) {
            return Ok(());
        }
        self.retag(t);
        let tracked = self.tracked.entry(t.id.clone()).or_insert(Tracked {
            tags: t.tags.clone(),
            exited: false,
        });
        tracked.tags = t.tags.clone();
        let task = t.tags.get("makiba.task_id").cloned();
        let role = t.tags.get("makiba.role").cloned();
        let spawn = spawn_of(&t.tags);
        // An absent status does not erase the last one written.
        if let Some(s) = &t.status {
            let now: Written = (s.state, s.kind, s.msg.clone());
            if self.status.get(&t.id) != Some(&now) {
                let payload = json!({
                    "task_id": task, "role": role, "spawn_id": spawn,
                    "state": state_name(s.state), "kind": s.kind.map(kind_name), "msg": s.msg,
                });
                append_at(
                    w,
                    "terminal_status",
                    task.as_deref(),
                    &payload,
                    self.at.as_deref(),
                )?;
                self.status.insert(t.id.clone(), now);
            }
        }
        // The turn is tracked on its own: an identical status must not hide a turn end.
        if let Some(turn) = t.turn
            && turn > self.turn.get(&t.id).copied().unwrap_or(0)
        {
            let payload = json!({
                "task_id": task, "role": role, "spawn_id": spawn, "turn": turn,
            });
            append_at(
                w,
                "terminal_turn",
                task.as_deref(),
                &payload,
                self.at.as_deref(),
            )?;
            self.turn.insert(t.id.clone(), turn);
        }
        Ok(())
    }

    /// One `terminal_gone` per terminal: an exit is followed by a removal that adds nothing.
    /// `exit_code` is `Some` when the base reported an exit.
    pub(super) fn gone(
        &mut self,
        w: &StateWriter,
        id: &TerminalRef,
        exit_code: Option<Option<i32>>,
        reason: &str,
    ) -> crate::db::Result<()> {
        let Some(tracked) = self.tracked.get_mut(id) else {
            return Ok(());
        };
        if tracked.exited {
            return Ok(());
        }
        let task = tracked.tags.get("makiba.task_id").cloned();
        let role = tracked.tags.get("makiba.role").cloned();
        let spawn = spawn_of(&tracked.tags);
        let payload = json!({
            "task_id": task, "role": role, "spawn_id": spawn, "reason": reason,
            "exit_code": exit_code.flatten(),
        });
        append_at(
            w,
            "terminal_gone",
            task.as_deref(),
            &payload,
            self.at.as_deref(),
        )?;
        tracked.exited = true;
        Ok(())
    }
}

/// Kind, task, role and spawn of a record.
type Lost = (&'static str, Option<String>, Option<String>, Option<String>);

impl Memory {
    /// The records an event would still write, as `observe` and `gone` decide them.
    fn unwritten(&self, org: &str, event: &BaseEvent) -> Vec<Lost> {
        let ident = |tags: &BTreeMap<String, String>| {
            (
                tags.get("makiba.task_id").cloned(),
                tags.get("makiba.role").cloned(),
                spawn_of(tags),
            )
        };
        let mut out = Vec::new();
        let gone = |id: &TerminalRef, out: &mut Vec<Lost>| {
            if let Some(tr) = self.tracked.get(id).filter(|tr| !tr.exited) {
                let (task, role, spawn) = ident(&tr.tags);
                out.push(("terminal_gone", task, role, spawn));
            }
        };
        match event {
            BaseEvent::Resync(_) => {}
            BaseEvent::Event(TerminalEvent::Removed(id)) => gone(id, &mut out),
            BaseEvent::Event(
                TerminalEvent::Created(t)
                | TerminalEvent::StatusChanged(t)
                | TerminalEvent::Exited(t),
            ) => {
                if !mine(org, &t.tags) {
                    return out;
                }
                let (task, role, spawn) = ident(&t.tags);
                if let Some(s) = &t.status
                    && self.status.get(&t.id) != Some(&(s.state, s.kind, s.msg.clone()))
                {
                    out.push(("terminal_status", task.clone(), role.clone(), spawn.clone()));
                }
                if let Some(turn) = t.turn
                    && turn > self.turn.get(&t.id).copied().unwrap_or(0)
                {
                    out.push(("terminal_turn", task.clone(), role.clone(), spawn.clone()));
                }
                if matches!(event, BaseEvent::Event(TerminalEvent::Exited(_))) {
                    match self.tracked.get(&t.id) {
                        Some(_) => gone(&t.id, &mut out),
                        None => out.push(("terminal_gone", task, role, spawn)),
                    }
                }
            }
        }
        out
    }
}

pub(super) fn append(
    w: &StateWriter,
    kind: &str,
    task: Option<&str>,
    payload: &Value,
) -> crate::db::Result<()> {
    append_at(w, kind, task, payload, None)
}

/// As `append`, at `at` (now when `None`).
fn append_at(
    w: &StateWriter,
    kind: &str,
    task: Option<&str>,
    payload: &Value,
    at: Option<&str>,
) -> crate::db::Result<()> {
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(payload),
        occurred_at: at,
        run_task_id: task,
        ..Default::default()
    })
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::Status;

    fn term(state: Option<State>, turn: Option<u64>, org: &str) -> Terminal {
        Terminal {
            id: TerminalRef("t".into()),
            name: "t".into(),
            tags: BTreeMap::from([
                ("makiba.org".to_string(), org.to_string()),
                ("makiba.task_id".to_string(), "k".to_string()),
            ]),
            status: state.map(|state| Status {
                state,
                kind: None,
                msg: None,
            }),
            turn,
            alive: true,
            exit_code: None,
        }
    }

    fn run(events: Vec<TerminalEvent>) -> Vec<(String, Value)> {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        let mut m = Monitor::new("me");
        for e in events {
            m.handle(&w, BaseEvent::Event(e)).unwrap();
        }
        let mut q = w
            .conn()
            .prepare("SELECT kind, payload_json FROM events ORDER BY id")
            .unwrap();
        q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn states(got: &[(String, Value)]) -> Vec<String> {
        got.iter()
            .map(|(k, p)| format!("{k}:{}", p["state"].as_str().unwrap_or("")))
            .collect()
    }

    #[test]
    fn only_the_last_record_is_compared_and_an_absent_status_erases_nothing() {
        use State::{Blocked, Working};
        let ev = |s| TerminalEvent::StatusChanged(term(s, None, "me"));
        let got = run(vec![
            ev(Some(Working)),
            ev(Some(Working)),
            ev(None),
            ev(Some(Working)),
            ev(Some(Blocked)),
            ev(Some(Working)),
        ]);
        assert_eq!(
            states(&got),
            [
                "terminal_status:working",
                "terminal_status:blocked",
                "terminal_status:working"
            ]
        );
    }

    #[test]
    fn a_turn_is_written_although_the_status_is_the_same() {
        let ev = |turn| TerminalEvent::StatusChanged(term(Some(State::Idle), Some(turn), "me"));
        let got = run(vec![ev(0), ev(1), ev(1), ev(2)]);
        let kinds: Vec<_> = got.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["terminal_status", "terminal_turn", "terminal_turn"]);
        assert_eq!(got[2].1["turn"], 2);
    }

    #[test]
    fn a_removal_is_a_gone_unless_the_terminal_exited_first_and_foreign_ones_are_ignored() {
        let id = TerminalRef("t".into());
        let removed = run(vec![
            TerminalEvent::Created(term(None, None, "me")),
            TerminalEvent::Removed(id.clone()),
        ]);
        assert_eq!(removed[0].0, "terminal_gone");
        assert_eq!(removed[0].1["reason"], "removed");
        assert_eq!(removed[0].1["exit_code"], Value::Null);

        let mut dead = term(Some(State::Done), None, "me");
        (dead.alive, dead.exit_code) = (false, Some(3));
        let exited = run(vec![
            TerminalEvent::Exited(dead),
            TerminalEvent::Removed(id.clone()),
        ]);
        let gone: Vec<_> = exited
            .iter()
            .filter(|(k, _)| k == "terminal_gone")
            .collect();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].1["exit_code"], 3);

        let foreign = run(vec![
            TerminalEvent::StatusChanged(term(Some(State::Working), Some(1), "other")),
            TerminalEvent::Removed(id),
        ]);
        assert!(foreign.is_empty());
    }

    /// An address that comes back with another `makiba.spawn` inherits nothing of the old one.
    #[test]
    fn a_reused_address_with_another_identity_starts_clean() {
        let with = |spawn: &str, state| {
            let mut t = term(Some(state), Some(1), "me");
            t.tags.insert("makiba.spawn".into(), spawn.into());
            TerminalEvent::StatusChanged(t)
        };
        let got = run(vec![
            with("a", State::Working),
            with("b", State::Working),
            with("b", State::Working),
        ]);
        let spawns: Vec<_> = got
            .iter()
            .map(|(k, p)| format!("{k}:{}", p["spawn_id"].as_str().unwrap()))
            .collect();
        assert_eq!(
            spawns,
            [
                "terminal_status:a",
                "terminal_turn:a",
                "terminal_status:b",
                "terminal_turn:b"
            ]
        );
    }
}
