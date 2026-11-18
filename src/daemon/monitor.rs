//! What the organization's terminals report, written down as it happens (spec section 10):
//! `terminal_status`, `terminal_turn` and `terminal_gone`, for terminals that carry this
//! organization's `makiba.org` and tied to their run by `makiba.task_id`.
//!
//! The monitor lives as long as the daemon and is fed every `BaseEvent` of the subscription. It
//! keeps, per terminal, the tags and whether it exited (a removal carries only an id), the last
//! status that was written and the last turn that was written. A status identical to the last
//! written one is not written again; the record is updated only after the write succeeded, so a
//! failed write is tried again by the next event. A `Resync` rebuilds that memory from what is
//! recorded and reconciles it with the base's list (`recover.rs`, M2-5).

use crate::base::{BaseEvent, BlockKind, State, Terminal, TerminalEvent, TerminalRef};
use crate::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

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
}

pub struct Monitor {
    org: String,
    mem: Memory,
    /// A `Resync` has been handled: the next is not the start.
    started: bool,
    /// A `Resync` whose handling failed: tried again before the next event.
    retry: Option<Vec<Terminal>>,
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
    /// `org` is the value of `makiba.org` of this daemon's own organization.
    pub fn new(org: &str) -> Monitor {
        Monitor {
            org: org.into(),
            mem: Memory::default(),
            started: false,
            retry: None,
        }
    }

    /// A `Resync` has been handled and none is waiting to be tried again: the view is built.
    pub fn reconciled(&self) -> bool {
        self.started && self.retry.is_none()
    }

    pub fn handle(&mut self, w: &StateWriter, event: BaseEvent) -> crate::db::Result<()> {
        // A newer Resync replaces one that failed; any other event goes after it.
        if let Some(list) = self.retry.take()
            && !matches!(event, BaseEvent::Resync(_))
            && let Err(e) = self.resync(w, list)
        {
            eprintln!("makibad: cannot rebuild the view: {e}");
        }
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
    /// when it commits; on a failure the list is kept and tried again before the next event.
    fn resync(&mut self, w: &StateWriter, list: Vec<Terminal>) -> crate::db::Result<()> {
        let reason = if self.started { "resync" } else { "start" };
        let mut mem = self.mem.clone();
        match w.transaction(|w| super::recover::rebuild(w, &self.org, &mut mem, &list, reason)) {
            Ok(()) => {
                self.mem = mem;
                self.started = true;
                Ok(())
            }
            Err(e) => {
                self.retry = Some(list);
                Err(e)
            }
        }
    }
}

impl Memory {
    pub(super) fn forget(&mut self, id: &TerminalRef) {
        self.tracked.remove(id);
        self.status.remove(id);
        self.turn.remove(id);
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
        let tracked = self.tracked.entry(t.id.clone()).or_insert(Tracked {
            tags: t.tags.clone(),
            exited: false,
        });
        tracked.tags = t.tags.clone();
        let task = t.tags.get("makiba.task_id").cloned();
        let role = t.tags.get("makiba.role").cloned();
        // An absent status does not erase the last one written.
        if let Some(s) = &t.status {
            let now: Written = (s.state, s.kind, s.msg.clone());
            if self.status.get(&t.id) != Some(&now) {
                let payload = json!({
                    "task_id": task, "role": role, "state": state_name(s.state),
                    "kind": s.kind.map(kind_name), "msg": s.msg,
                });
                append(w, "terminal_status", task.as_deref(), &payload)?;
                self.status.insert(t.id.clone(), now);
            }
        }
        // The turn is tracked on its own: an identical status must not hide a turn end.
        if let Some(turn) = t.turn
            && turn > self.turn.get(&t.id).copied().unwrap_or(0)
        {
            let payload = json!({"task_id": task, "role": role, "turn": turn});
            append(w, "terminal_turn", task.as_deref(), &payload)?;
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
        let payload = json!({
            "task_id": task, "role": role, "reason": reason, "exit_code": exit_code.flatten(),
        });
        append(w, "terminal_gone", task.as_deref(), &payload)?;
        tracked.exited = true;
        Ok(())
    }
}

pub(super) fn append(
    w: &StateWriter,
    kind: &str,
    task: Option<&str>,
    payload: &Value,
) -> crate::db::Result<()> {
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(payload),
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
}
