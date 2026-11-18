//! Rebuilding the view after a restart (spec sections 6.3 and 10). `state.db` holds no terminal
//! address, so the tags are the record. The monitor calls `rebuild` for every `Resync`, the first
//! one of a daemon included, inside one transaction:
//!
//! 1. a terminal the monitor knew and the list no longer has is gone (`removed`);
//! 2. the last written status and turn of each of our terminals are read back from `events`, so
//!    what was recorded before the restart is not written again;
//! 3. each terminal goes through the monitor's own write path, so a `done` reported while the
//!    daemon was down is recorded, and a retained dead terminal's exit is recorded once;
//! 4. an `in_use` run with no terminal of ours gets one `terminal_gone` (`lost`), unless the last
//!    thing recorded for its task is already a `terminal_gone`. Failing the run is Issue 18's;
//! 5. an unknown terminal (ours by `makiba.org`, not the secretary or the curator, with no run) is
//!    reported once to the secretary as an anomaly and a `notice`, and left alone;
//! 6. `base_resynced`, last.

use super::monitor::{Memory, append, mine, parse_kind, parse_state};
use crate::base::{Terminal, TerminalRef};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashSet;

/// What `terminal_status`, `terminal_turn` and `terminal_gone` of a terminal are matched by: its
/// task id (or none) and its role (or none), the way the monitor wrote them.
fn key(t: &Terminal) -> (Option<String>, Option<String>) {
    (
        t.tags.get("makiba.task_id").cloned(),
        t.tags.get("makiba.role").cloned(),
    )
}

/// Whether the last `terminal_status` or `terminal_gone` of this task and role is a gone.
/// `role` of `None` matches any role: a lost run's terminal is no longer there to ask.
fn gone_recorded(
    w: &StateWriter,
    task: &Option<String>,
    role: Option<&Option<String>>,
) -> Result<bool> {
    let last: Option<String> = w
        .conn()
        .query_row(
            "SELECT kind FROM events
             WHERE kind IN ('terminal_status', 'terminal_gone')
               AND json_extract(payload_json, '$.task_id') IS ?1
               AND (?2 = 0 OR json_extract(payload_json, '$.role') IS ?3)
             ORDER BY id DESC LIMIT 1",
            params![task, role.is_some(), role.and_then(Clone::clone)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(last.as_deref() == Some("terminal_gone"))
}

fn restore(w: &StateWriter, mem: &mut Memory, t: &Terminal) -> Result<()> {
    let (task, role) = key(t);
    let status: Option<String> = w
        .conn()
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'terminal_status'
               AND json_extract(payload_json, '$.task_id') IS ?1
               AND json_extract(payload_json, '$.role') IS ?2
             ORDER BY id DESC LIMIT 1",
            params![task, role],
            |r| r.get(0),
        )
        .optional()?;
    let status = status
        .and_then(|p| serde_json::from_str::<Value>(&p).ok())
        .and_then(|p| {
            Some((
                parse_state(p["state"].as_str()?)?,
                p["kind"].as_str().and_then(parse_kind),
                p["msg"].as_str().map(String::from),
            ))
        });
    let turn: Option<i64> = w.conn().query_row(
        "SELECT max(json_extract(payload_json, '$.turn')) FROM events WHERE kind = 'terminal_turn'
           AND json_extract(payload_json, '$.task_id') IS ?1
           AND json_extract(payload_json, '$.role') IS ?2",
        params![task, role],
        |r| r.get(0),
    )?;
    mem.restore(&t.id, status, turn.map(|n| n as u64));
    Ok(())
}

pub(super) fn rebuild(
    w: &StateWriter,
    org: &str,
    mem: &mut Memory,
    list: &[Terminal],
    reason: &str,
) -> Result<()> {
    let ours: Vec<&Terminal> = list.iter().filter(|t| mine(org, &t.tags)).collect();
    let present: HashSet<&TerminalRef> = ours.iter().map(|t| &t.id).collect();

    let vanished: Vec<TerminalRef> = mem
        .tracked
        .keys()
        .filter(|id| !present.contains(id))
        .cloned()
        .collect();
    for id in vanished {
        mem.gone(w, &id, None, "removed")?;
        mem.forget(&id);
    }

    for t in &ours {
        restore(w, mem, t)?;
        mem.observe(org, w, t)?;
        let (task, role) = key(t);
        // What was recorded decides, not what this process remembers: a stale `exited` must not
        // hide a new exit, and a loss the live monitor wrote before the restart must not repeat.
        let recorded = !t.alive && gone_recorded(w, &task, Some(&role))?;
        if let Some(tr) = mem.tracked.get_mut(&t.id) {
            tr.exited = recorded;
        }
        if !t.alive {
            mem.gone(w, &t.id, Some(t.exit_code), "exited")?;
        }
    }

    // In-use runs with no terminal of ours.
    let with_terminal: HashSet<String> = ours
        .iter()
        .filter_map(|t| t.tags.get("makiba.task_id").cloned())
        .collect();
    let in_use: Vec<String> = w
        .conn()
        .prepare("SELECT task_id FROM runs WHERE status = 'in_use' ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for task in in_use.into_iter().filter(|t| !with_terminal.contains(t)) {
        let task = Some(task);
        if gone_recorded(w, &task, None)? {
            continue;
        }
        let role: Option<String> = w
            .conn()
            .query_row(
                "SELECT json_extract(payload_json, '$.role') FROM events
                 WHERE kind = 'terminal_status' AND json_extract(payload_json, '$.task_id') IS ?1
                 ORDER BY id DESC LIMIT 1",
                [&task],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let payload = json!({
            "task_id": task, "role": role, "reason": "lost", "exit_code": null,
        });
        append(w, "terminal_gone", task.as_deref(), &payload)?;
    }

    for t in &ours {
        report_unknown(w, t)?;
    }

    let payload = json!({"reason": reason, "terminals": ours.len()});
    w.append_event(&NewEvent {
        kind: "base_resynced",
        actor: Some("makibad"),
        payload: Some(&payload),
        ..Default::default()
    })?;
    Ok(())
}

/// Tell the secretary once about a terminal of this organization that no run explains. The anomaly,
/// the message, its event and its delivery row are written together; "once" is by the terminal's
/// id, looked up in `events`, so it holds across resyncs and restarts.
fn report_unknown(w: &StateWriter, t: &Terminal) -> Result<()> {
    let (task, role) = key(t);
    if matches!(role.as_deref(), Some("secretary" | "curator")) {
        return Ok(());
    }
    if let Some(task) = &task {
        let known: Option<i64> = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .optional()?;
        if known.is_some() {
            return Ok(());
        }
    }
    let reported: Option<i64> = w
        .conn()
        .query_row(
            "SELECT id FROM events WHERE kind = 'anomaly_raised'
               AND json_extract(payload_json, '$.rule') = 'unknown_terminal'
               AND json_extract(payload_json, '$.terminal') = ?1 LIMIT 1",
            [&t.id.0],
            |r| r.get(0),
        )
        .optional()?;
    if reported.is_some() {
        return Ok(());
    }
    let actor = Some("makibad");
    let gate = super::notify_gate::decide("unknown_terminal");
    let mut payload = json!({
        "task_id": task, "rule": "unknown_terminal", "terminal": t.id.0, "name": t.name,
    });
    if !gate.send {
        payload["suppressed"] = gate.reason.into();
    }
    w.append_event(&NewEvent {
        kind: "anomaly_raised",
        actor,
        payload: Some(&payload),
        ..Default::default()
    })?;
    if !gate.send {
        return Ok(());
    }
    let sent = w.append_event(&NewEvent {
        kind: "message_sent",
        actor,
        ..Default::default()
    })?;
    let body = format!(
        "Unknown terminal: {:?} (id {}, role {}, task id {}) carries this organization's tag but \
         matches no run. It is left running; end it or ignore it.",
        t.name,
        t.id.0,
        role.as_deref().unwrap_or("none"),
        task.as_deref().unwrap_or("none"),
    );
    w.conn().execute(
        "INSERT INTO messages (event_id, sender, recipient, kind, body)
         VALUES (?1, 'makibad', 'secretary', 'notice', ?2)",
        params![sent, body],
    )?;
    let message = w.conn().last_insert_rowid();
    w.conn().execute(
        "UPDATE events SET payload_json = json_object('message_id', ?1) WHERE id = ?2",
        params![message, sent],
    )?;
    w.conn().execute(
        "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt)
         VALUES (?1, 'secretary', 'pending', 0)",
        [sent],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::monitor::Monitor;
    use super::*;
    use crate::base::{BaseEvent, State, Status};
    use std::collections::BTreeMap;

    fn term(turn: Option<u64>, alive: bool) -> Terminal {
        Terminal {
            id: TerminalRef("t".into()),
            name: "t".into(),
            tags: BTreeMap::from([
                ("makiba.org".to_string(), "me".to_string()),
                ("makiba.task_id".to_string(), "k".to_string()),
                ("makiba.role".to_string(), "worker".to_string()),
            ]),
            status: Some(Status {
                state: State::Idle,
                kind: None,
                msg: None,
            }),
            turn,
            alive,
            exit_code: (!alive).then_some(1),
        }
    }

    fn kinds(w: &StateWriter) -> Vec<String> {
        let mut q = w
            .conn()
            .prepare("SELECT kind FROM events ORDER BY id")
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// A turn and a dead terminal's exit recorded before a restart are not written again by a new
    /// monitor, and a turn that ended meanwhile is.
    #[test]
    fn a_new_monitor_restores_turns_and_exits_from_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        let resync = |m: &mut Monitor, t| m.handle(&w, BaseEvent::Resync(vec![t])).unwrap();
        resync(&mut Monitor::new("me"), term(Some(1), true));
        resync(&mut Monitor::new("me"), term(Some(1), false));
        let before = kinds(&w);
        resync(&mut Monitor::new("me"), term(Some(1), false));
        let mut again = Monitor::new("me");
        resync(&mut again, term(Some(1), false));
        resync(&mut again, term(Some(1), false));
        assert_eq!(
            kinds(&w)[before.len()..],
            ["base_resynced", "base_resynced", "base_resynced"]
        );
        resync(&mut again, term(Some(2), false));
        assert_eq!(
            kinds(&w).iter().filter(|k| *k == "terminal_turn").count(),
            2
        );
        assert_eq!(
            kinds(&w).iter().filter(|k| *k == "terminal_gone").count(),
            1
        );
    }
}
