//! Rebuilding the view after a restart (spec sections 6.3 and 10). `state.db` holds no terminal
//! address, so the tags are the record. The monitor calls `rebuild` for every `Resync`, the first
//! one of a daemon included, inside one transaction:
//!
//! 1. a terminal the monitor knew and the list no longer has is gone (`removed`);
//! 2. the last written status and turn of each of our terminals are read back from `events` by
//!    task, role and `spawn_id` (a terminal never reads another identity's history), so
//!    what was recorded before the restart is not written again;
//! 3. each terminal goes through the monitor's own write path, so a `done` reported while the
//!    daemon was down is recorded, and a retained dead terminal's exit is recorded once;
//! 4. an `in_use` run with no terminal of ours gets one `terminal_gone` (`lost`), carrying the
//!    `spawn_id` of its latest `run_launch` row, unless the last thing recorded for that task and
//!    identity is already a `terminal_gone`. Failing the run is Issue 18's;
//! 5. an unknown terminal (ours by `makiba.org`, not the secretary or the curator, with no run) is
//!    reported once to the secretary as an anomaly and a `notice`, and left alone;
//! 6. `base_resynced`, last, with `restart` saying whether step 0 judged a core restart.
//!
//! Step 0, first (Issue 579): a core stop and start replaces every kept terminal's program with
//! the machine's shell (#560 reads each as not alive, with no exit code). The list is judged a
//! core restart when no terminal of ours in it is alive, at least two are replaced (not alive, no
//! exit code; one that exited with a code finished on its own and is not counted), and at least
//! one of those has no `terminal_gone` recorded (it is newly lost). One lost program while
//! another of ours runs is no restart, and its run fails as before; a lone terminal cannot be
//! judged. On a restart, the runs at work (`down::AT_WORK`) and the suspended runs with a launch
//! that had a terminal (the relaunch of an earlier restart) are suspended (`down::suspend_runs`,
//! those runs only) and resumed as `makiba up` does (`up::resume_as`), in this transaction: the
//! anomaly pass then fails none, and each relaunch ends the shell its run left. A suspended run
//! whose launch has no terminal yet is left to go on. Nothing is kept in memory for it.

use super::monitor::{Memory, append, mine, parse_kind, parse_state, spawn_of};
use crate::base::{Terminal, TerminalRef};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashSet;

/// What `terminal_status`, `terminal_turn` and `terminal_gone` of a terminal are matched by: its
/// task id, role and `makiba.spawn` (each or none), the way the monitor wrote them. A record
/// without `spawn_id` is an earlier generation: only an untagged terminal matches it (`IS`).
type Key = (Option<String>, Option<String>, Option<String>);

fn key(t: &Terminal) -> Key {
    (
        t.tags.get("makiba.task_id").cloned(),
        t.tags.get("makiba.role").cloned(),
        spawn_of(&t.tags),
    )
}

/// Whether the last `terminal_status` or `terminal_gone` of this task, role and identity is a
/// gone. `role` of `None` matches any role: a lost run's terminal is no longer there to ask.
fn gone_recorded(
    w: &StateWriter,
    task: &Option<String>,
    role: Option<&Option<String>>,
    spawn: &Option<String>,
) -> Result<bool> {
    let last: Option<String> = w
        .conn()
        .query_row(
            "SELECT kind FROM events
             WHERE kind IN ('terminal_status', 'terminal_gone')
               AND json_extract(payload_json, '$.task_id') IS ?1
               AND (?2 = 0 OR json_extract(payload_json, '$.role') IS ?3)
               AND json_extract(payload_json, '$.spawn_id') IS ?4
             ORDER BY id DESC LIMIT 1",
            params![task, role.is_some(), role.and_then(Clone::clone), spawn],
            |r| r.get(0),
        )
        .optional()?;
    Ok(last.as_deref() == Some("terminal_gone"))
}

/// Whether the list shows a core restart (step 0): none of our terminals alive, two or more
/// replaced (not alive with no exit code, as #560 reads a shell in place of the program), and one
/// of those newly lost. A terminal whose program exited with a code finished on its own: it
/// neither counts nor stops the judgment.
fn core_restarted(w: &StateWriter, ours: &[&Terminal]) -> Result<bool> {
    if ours.iter().any(|t| t.alive) {
        return Ok(false);
    }
    let replaced: Vec<&&Terminal> = ours.iter().filter(|t| t.exit_code.is_none()).collect();
    if replaced.len() < 2 {
        return Ok(false);
    }
    for t in replaced {
        let (task, role, spawn) = key(t);
        if !gone_recorded(w, &task, Some(&role), &spawn)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The runs a restart suspends and resumes: those at work (`down::AT_WORK`), and every
/// `suspended` run with a launch that had a terminal (the relaunch of an earlier restart). A
/// suspended run whose launch has no terminal yet (`makiba up`'s own relaunch) goes on as it is.
fn restart_runs(w: &StateWriter) -> Result<Vec<i64>> {
    let ids = w
        .conn()
        .prepare(&format!(
            "SELECT id FROM runs r WHERE {}
               OR (r.status = 'suspended' AND EXISTS (SELECT 1 FROM run_launch l
                   WHERE l.run_id = r.id AND l.stage IN ('launched', 'dialog_passed')))
             ORDER BY id",
            crate::down::AT_WORK
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(ids)
}

fn restore(w: &StateWriter, mem: &mut Memory, t: &Terminal) -> Result<()> {
    let (task, role, spawn) = key(t);
    let status: Option<String> = w
        .conn()
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'terminal_status'
               AND json_extract(payload_json, '$.task_id') IS ?1
               AND json_extract(payload_json, '$.role') IS ?2
               AND json_extract(payload_json, '$.spawn_id') IS ?3
             ORDER BY id DESC LIMIT 1",
            params![task, role, spawn],
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
           AND json_extract(payload_json, '$.role') IS ?2
           AND json_extract(payload_json, '$.spawn_id') IS ?3",
        params![task, role, spawn],
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

    // Step 0: a core restart suspends and resumes the runs at work instead of failing them.
    let restart = core_restarted(w, &ours)?;
    if restart {
        let runs = restart_runs(w)?;
        if !runs.is_empty() {
            crate::down::suspend_runs(w, &runs, "the core restarted")?;
            crate::up::resume_as(w, "makibad", Some(&runs))?;
        }
    }

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
        mem.retag(t);
        restore(w, mem, t)?;
        mem.observe(org, w, t)?;
        let (task, role, spawn) = key(t);
        // What was recorded decides, not what this process remembers: a stale `exited` must not
        // hide a new exit, and a loss the live monitor wrote before the restart must not repeat.
        let recorded = !t.alive && gone_recorded(w, &task, Some(&role), &spawn)?;
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
        // The identity of the loss is the run's latest launch (spec section 6.3), none without one.
        let spawn: Option<String> = w
            .conn()
            .query_row(
                "SELECT l.spawn_id FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE r.task_id = ?1 ORDER BY l.id DESC LIMIT 1",
                [&task],
                |r| r.get(0),
            )
            .optional()?;
        if gone_recorded(w, &task, None, &spawn)? {
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
            "task_id": task, "role": role, "spawn_id": spawn, "reason": "lost", "exit_code": null,
        });
        append(w, "terminal_gone", task.as_deref(), &payload)?;
    }

    // The secretary's newest verified launch with no terminal of ours, its key still live, and no
    // gone recorded: it vanished while no monitor watched (Issue 46). A key revoked by `makiba
    // down --secretary` or a renewal means it was ended on purpose.
    let lost: Option<String> = w
        .conn()
        .query_row(
            "SELECT l.spawn_id FROM run_launch l
             WHERE l.id = (SELECT max(id) FROM run_launch WHERE role = 'secretary')
               AND l.stage = 'verified'
               AND EXISTS (SELECT 1 FROM agents a WHERE a.spawn_id = l.spawn_id
                             AND a.revoked_at IS NULL)",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(spawn) = lost.map(Some)
        && !ours
            .iter()
            .any(|t| t.tags.get("makiba.spawn") == spawn.as_ref())
        && !gone_recorded(w, &None, Some(&Some("secretary".into())), &spawn)?
    {
        let payload = json!({
            "task_id": null, "role": "secretary", "spawn_id": spawn, "reason": "lost",
            "exit_code": null,
        });
        append(w, "terminal_gone", None, &payload)?;
    }

    for t in &ours {
        report_unknown(w, t)?;
    }

    let payload = json!({"reason": reason, "terminals": ours.len(), "restart": restart});
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
    let (task, role, _) = key(t);
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

    /// A loss carries the identity of the run's latest launch, and an earlier launch's loss does
    /// not hide a later one's.
    #[test]
    fn a_loss_is_recorded_per_launch_of_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        let run = w
            .upsert_run(&crate::db::writer::RunUpsert {
                task_id: "k",
                project_slug: "p",
                status: Some("in_use"),
                ..Default::default()
            })
            .unwrap();
        let launch = |spawn: &str| {
            w.conn()
                .execute(
                    "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree)
                     VALUES (?1, ?2, 'fake', 'b', 'w')",
                    params![run, spawn],
                )
                .unwrap();
        };
        let resync = || {
            Monitor::new("me")
                .handle(&w, BaseEvent::Resync(vec![]))
                .unwrap()
        };
        let lost = || -> Vec<Value> {
            let mut q = w
                .conn()
                .prepare("SELECT payload_json FROM events WHERE kind = 'terminal_gone' ORDER BY id")
                .unwrap();
            q.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
                .collect()
        };
        resync(); // no launch row: null
        resync();
        launch("s1");
        resync();
        resync();
        launch("s2");
        resync();
        let ids: Vec<Value> = lost().iter().map(|p| p["spawn_id"].clone()).collect();
        assert_eq!(ids, [Value::Null, "s1".into(), "s2".into()]);
    }

    /// A terminal of `me` at `id` for `role`, `task` and `spawn`, as #560 reads a replaced one
    /// (not alive, no exit code) or a running one.
    fn of(id: &str, role: &str, task: Option<&str>, spawn: &str, alive: bool) -> Terminal {
        let mut tags = BTreeMap::from([
            ("makiba.org".to_string(), "me".to_string()),
            ("makiba.role".to_string(), role.to_string()),
            ("makiba.spawn".to_string(), spawn.to_string()),
        ]);
        if let Some(task) = task {
            tags.insert("makiba.task_id".into(), task.into());
        }
        Terminal {
            id: TerminalRef(id.into()),
            name: id.into(),
            tags,
            status: None,
            turn: None,
            alive,
            exit_code: None,
        }
    }

    /// Runs `t1` and `t2`, `status`, each with a launch `<task>-a` at `stage`.
    fn two_runs(status: &str, stage: &str) -> (tempfile::TempDir, StateWriter) {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        for task in ["t1", "t2"] {
            let run = w
                .upsert_run(&crate::db::writer::RunUpsert {
                    task_id: task,
                    project_slug: "p",
                    status: Some(status),
                    ..Default::default()
                })
                .unwrap();
            w.conn()
                .execute(
                    "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                     VALUES (?1, ?2, 'fake', 'b', 'w', ?3)",
                    params![run, format!("{task}-a"), stage],
                )
                .unwrap();
        }
        (dir, w)
    }

    fn workers(alive: [bool; 2]) -> Vec<Terminal> {
        vec![
            of("a1", "worker", Some("t1"), "t1-a", alive[0]),
            of("a2", "worker", Some("t2"), "t2-a", alive[1]),
        ]
    }

    fn status(w: &StateWriter, task: &str) -> String {
        w.conn()
            .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// The run's launches, oldest first: (spawn id, stage).
    fn launches(w: &StateWriter, task: &str) -> Vec<(String, String)> {
        let mut q = w
            .conn()
            .prepare(
                "SELECT l.spawn_id, l.stage FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE r.task_id = ?1 ORDER BY l.id",
            )
            .unwrap();
        q.query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn last_restart(w: &StateWriter) -> bool {
        w.conn()
            .query_row(
                "SELECT json_extract(payload_json, '$.restart') FROM events
                 WHERE kind = 'base_resynced' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// One anomaly pass, a minute after now.
    fn anomalies(w: &StateWriter) {
        let now: String = w
            .conn()
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now','+60 seconds')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        super::super::anomalies::Anomalies::new()
            .pass_at(w, &now)
            .unwrap();
    }

    /// Issue 579: every program replaced at once is a core restart: both runs are suspended and
    /// get a relaunch taken by makibad, and the anomaly pass fails neither.
    #[test]
    fn a_restart_suspends_and_relaunches_the_runs_at_work() {
        let (_dir, w) = two_runs("in_use", "verified");
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(workers([false, false])))
            .unwrap();
        assert!(last_restart(&w));
        anomalies(&w);
        for task in ["t1", "t2"] {
            assert_eq!(status(&w, task), "suspended");
            let l = launches(&w, task);
            assert_eq!(l.len(), 2, "{task}: {l:?}");
            assert_eq!(l[1].1, "worktree_ready");
        }
        let takers: i64 = w
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'run_taken' AND actor = 'makibad'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(takers, 2);
    }

    /// Issue 579: one program lost while another runs is no restart; its run fails by the gone
    /// anomaly as #560 makes it, and the other is left as it is.
    #[test]
    fn one_loss_beside_a_running_program_is_no_restart() {
        let (_dir, w) = two_runs("in_use", "verified");
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(workers([false, true])))
            .unwrap();
        assert!(!last_restart(&w));
        anomalies(&w);
        assert_eq!(status(&w, "t1"), "failed");
        assert_eq!(status(&w, "t2"), "in_use");
        assert_eq!(launches(&w, "t2").len(), 1);
    }

    /// Issue 579, the known limit: a lone terminal cannot be told from a core restart; its run
    /// fails.
    #[test]
    fn a_lone_lost_worker_is_no_restart() {
        let (_dir, w) = two_runs("in_use", "verified");
        Monitor::new("me")
            .handle(
                &w,
                BaseEvent::Resync(vec![of("a1", "worker", Some("t1"), "t1-a", false)]),
            )
            .unwrap();
        assert!(!last_restart(&w));
        anomalies(&w);
        assert_eq!(status(&w, "t1"), "failed");
    }

    /// Issue 579: a restart while the secretary's loss is already recorded (its renewal pending)
    /// is still a restart: the worker's run is resumed, not failed.
    #[test]
    fn a_restart_with_a_recorded_secretary_loss_resumes_the_worker() {
        let (_dir, w) = two_runs("in_use", "verified");
        let secretary = of("s", "secretary", None, "sec-1", false);
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(vec![secretary.clone()]))
            .unwrap();
        let mut list = vec![secretary];
        list.extend(workers([false, true]).into_iter().take(1));
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(list))
            .unwrap();
        assert!(last_restart(&w));
        anomalies(&w);
        assert_eq!(status(&w, "t1"), "suspended");
        assert_eq!(launches(&w, "t1").len(), 2);
    }

    /// Issue 579: a second restart while the relaunches are launched but not verified fails them
    /// and relaunches again.
    #[test]
    fn a_second_restart_replaces_launched_relaunches() {
        let (_dir, w) = two_runs("suspended", "launched");
        let list = vec![
            of("a1", "worker", Some("t1"), "t1-a", false),
            of("a2", "worker", Some("t2"), "t2-a", false),
        ];
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(list))
            .unwrap();
        assert!(last_restart(&w));
        for task in ["t1", "t2"] {
            let l = launches(&w, task);
            assert_eq!(
                l.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(),
                ["failed", "worktree_ready"],
                "{task}"
            );
            assert_eq!(status(&w, task), "suspended");
        }
    }

    /// Issue 579: a restart's suspend and resume leave the session row as it is.
    #[test]
    fn a_restart_leaves_the_session_row() {
        let (_dir, w) = two_runs("in_use", "verified");
        w.conn()
            .execute(
                "UPDATE org_sessions SET status = 'SUSPENDED', resumed_at = NULL WHERE id = 1",
                [],
            )
            .unwrap();
        let row = |w: &StateWriter| -> (String, Option<String>, Option<String>) {
            w.conn()
                .query_row(
                    "SELECT status, resumed_at, suspended_at FROM org_sessions WHERE id = 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
        };
        let before = row(&w);
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(workers([false, false])))
            .unwrap();
        assert!(last_restart(&w));
        assert_eq!(launches(&w, "t1").len(), 2);
        assert_eq!(row(&w), before);
    }

    /// Issue 579: two programs that finished on their own (an exit code) while unsubscribed are
    /// no restart: both runs fail by the gone anomaly.
    #[test]
    fn programs_that_exited_with_a_code_are_no_restart() {
        let (_dir, w) = two_runs("in_use", "verified");
        let list: Vec<Terminal> = workers([false, false])
            .into_iter()
            .map(|mut t| {
                t.exit_code = Some(1);
                t
            })
            .collect();
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(list))
            .unwrap();
        assert!(!last_restart(&w));
        anomalies(&w);
        for task in ["t1", "t2"] {
            assert_eq!(status(&w, task), "failed", "{task}");
            assert_eq!(launches(&w, task).len(), 1, "{task}");
        }
    }

    /// Issue 579: a restart that suspends a run at work leaves another suspended run's pending
    /// relaunch (no terminal yet) and its keys as they are.
    #[test]
    fn a_restart_leaves_a_pending_relaunch_of_another_run() {
        let (_dir, w) = two_runs("in_use", "verified");
        w.conn()
            .execute_batch(
                "UPDATE runs SET status = 'suspended' WHERE task_id = 't2';
                 INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                   VALUES ((SELECT id FROM runs WHERE task_id = 't2'), 't2-b', 'fake', 'b', 'w',
                           'worktree_ready');
                 INSERT INTO agents (agent_id, role, key_sha256, run_id, spawn_id)
                   VALUES ('w-t2', 'worker', 'h2', (SELECT id FROM runs WHERE task_id = 't2'),
                           't2-b');",
            )
            .unwrap();
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(workers([false, false])))
            .unwrap();
        assert!(last_restart(&w));
        assert_eq!(status(&w, "t1"), "suspended");
        assert_eq!(launches(&w, "t1").len(), 2);
        assert_eq!(
            launches(&w, "t2"),
            [
                ("t2-a".to_string(), "verified".to_string()),
                ("t2-b".to_string(), "worktree_ready".to_string())
            ]
        );
        let revoked: i64 = w
            .conn()
            .query_row(
                "SELECT count(*) FROM agents WHERE spawn_id = 't2-b' AND revoked_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(revoked, 0, "the pending relaunch's key was revoked");
    }

    /// Issue 579: `makiba up`'s own relaunches (before a terminal) are not suspended again by
    /// makibad's first resync after it, though the shells of the stopped core are listed.
    #[test]
    fn ups_relaunches_are_left_to_go_on() {
        let (_dir, w) = two_runs("suspended", "worktree_ready");
        Monitor::new("me")
            .handle(&w, BaseEvent::Resync(workers([false, false])))
            .unwrap();
        assert!(last_restart(&w));
        for task in ["t1", "t2"] {
            assert_eq!(
                launches(&w, task),
                [(format!("{task}-a"), "worktree_ready".to_string())]
            );
        }
    }
}
