//! Design ahead (Issue 284, docs/design/belt.md section 4.3, the belt's step 4): a range task
//! dispatched for its design only while what it depends on has not landed.
//!
//! - Its dispatch (`belt::dispatch`) records what it waits for in its `belt_dispatched`
//!   (`design_only`, `waits_for`, `waits_tasks`); nothing else of the run changes.
//! - `report`: its worker's `report blocked` is its design (`design_reported`); `report done` is
//!   refused while it waits. From the design report it waits for a landing (`belt::LANDING_WAIT`):
//!   it counts in neither count and may be parked (`park.rs`).
//! - `settle`, from the records each pass: every task it waits for closed `completed`, its pull
//!   request read merged and the merge commit seen in the project's clone (`landed_in_clone`, the
//!   workers' `origin`) is a landing, and the run is woken (or told) with what landed. A task it
//!   waits for that was abandoned, or whose pull request was read closed unmerged, will not land:
//!   the wait is settled and the person is asked whether to go on without it. A task that failed
//!   is a question too, and the person's `no` (`answered`) releases the wait.
//!
//! Each wait is settled once (`ahead_settled`): a task id is dispatched once.

use super::belt::{Stop, named, stop};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

/// What a design-only run waits for: the Issues and their tasks, in the dispatch's order.
pub struct Wait {
    pub issues: Vec<i64>,
    pub tasks: Vec<String>,
}

fn wait_of(payload: &str) -> Wait {
    let v: Value = serde_json::from_str(payload).unwrap_or_default();
    Wait {
        issues: v["waits_for"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default(),
        tasks: v["waits_tasks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The wait of `task` while it is open: `None` for a run not dispatched for its design only, or
/// whose wait was settled.
pub fn open_wait(c: &Connection, task: &str) -> rusqlite::Result<Option<Wait>> {
    let p: Option<String> = c
        .query_row(
            "SELECT e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
             WHERE r.task_id = ?1 AND e.kind = 'belt_dispatched'
               AND json_extract(e.payload_json, '$.design_only') = 1
               AND NOT EXISTS (SELECT 1 FROM events s WHERE s.run_id = r.id
                               AND s.kind = 'ahead_settled')
             ORDER BY e.id LIMIT 1",
            [task],
            |r| r.get(0),
        )
        .optional()?;
    Ok(p.as_deref().map(wait_of))
}

fn event(w: &StateWriter, kind: &str, task: &str, payload: Value) -> Result<i64> {
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(&payload),
        run_task_id: Some(task),
        ..Default::default()
    })
}

/// What the door does with a worker's report on its own task.
pub enum Report {
    /// Not a design-only run waiting: the report as always.
    Plain,
    /// Its design (`blocked`): the line added for the secretary, and what it waits for.
    Design(String, Wait),
    /// `done` while it waits: refused, with why.
    Refused(String),
}

/// A worker's report `state` on `task` (door `report`).
pub fn report(c: &Connection, task: &str, state: &str) -> rusqlite::Result<Report> {
    let Some(wait) = open_wait(c, task)? else {
        return Ok(Report::Plain);
    };
    let what = named(&wait.issues);
    Ok(match state {
        "done" => Report::Refused(format!(
            "task {task} is design-only until {what} lands: report blocked with the design"
        )),
        "blocked" => Report::Design(
            format!(
                "[makiba] design-ahead: {task} waits for {what} to land; nothing to answer. It is parked and woken when {what} lands."
            ),
            wait,
        ),
        _ => Report::Plain,
    })
}

/// The design report of `task` was stored: from now it waits for a landing.
pub fn design_reported(w: &StateWriter, task: &str, wait: &Wait) -> Result<()> {
    event(
        w,
        "design_reported",
        task,
        json!({"task_id": task, "waits_for": wait.issues}),
    )?;
    Ok(())
}

/// `runs[].waiting` of a run waiting for a landing (spec section 12); `None` for any other.
pub fn waiting_field(c: &Connection, task: &str) -> rusqlite::Result<Option<Value>> {
    let waits: bool = c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM runs r WHERE r.task_id = ?1 AND {})",
            super::belt::LANDING_WAIT
        ),
        [task],
        |r| r.get(0),
    )?;
    if !waits {
        return Ok(None);
    }
    let Some(wait) = open_wait(c, task)? else {
        return Ok(None);
    };
    Ok(Some(json!({
        "for": "landing",
        "issues": wait.issues,
        "why": format!("waits for {} to land", named(&wait.issues)),
    })))
}

/// One task a design-only run waits for, as the records hold it now.
struct Dep {
    issue: i64,
    task: String,
    /// Its run's id and status; `None` when the run is gone.
    run: Option<(i64, String)>,
    pr_closed: bool,
    merged: Option<Value>,
    in_clone: Option<Value>,
}

fn dep(c: &Connection, issue: i64, task: &str) -> rusqlite::Result<Dep> {
    let run: Option<(i64, String)> = c
        .query_row(
            "SELECT id, status FROM runs WHERE task_id = ?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let payload = |kind: &str| -> rusqlite::Result<Option<Value>> {
        let p: Option<String> = c
            .query_row(
                "SELECT e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE r.task_id = ?1 AND e.kind = ?2 ORDER BY e.id LIMIT 1",
                [task, kind],
                |r| r.get(0),
            )
            .optional()?;
        Ok(p.and_then(|p| serde_json::from_str(&p).ok()))
    };
    Ok(Dep {
        issue,
        task: task.to_string(),
        run,
        pr_closed: payload("pr_closed")?.is_some(),
        merged: payload("pr_merged")?,
        in_clone: payload("landed_in_clone")?,
    })
}

/// Settle every open wait the records decide, in the caller's transaction (see the module).
pub fn settle(w: &StateWriter) -> Result<()> {
    let runs: Vec<(String, String, i64, String)> = w
        .conn()
        .prepare(
            "SELECT r.task_id, r.status, r.range_id, e.payload_json FROM runs r
             JOIN events e ON e.id = (SELECT min(b.id) FROM events b WHERE b.run_id = r.id
                                      AND b.kind = 'belt_dispatched')
             WHERE r.status IN ('queued', 'in_use', 'suspended', 'parked')
               AND r.range_id IS NOT NULL
               AND json_extract(e.payload_json, '$.design_only') = 1
               AND NOT EXISTS (SELECT 1 FROM events s WHERE s.run_id = r.id
                               AND s.kind = 'ahead_settled')
             ORDER BY r.id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (task, status, range, payload) in runs {
        let wait = wait_of(&payload);
        let deps: Vec<Dep> = wait
            .issues
            .iter()
            .zip(&wait.tasks)
            .map(|(i, t)| dep(w.conn(), *i, t))
            .collect::<rusqlite::Result<_>>()?;
        let dropped: Vec<&Dep> = deps
            .iter()
            .filter(|d| d.pr_closed || d.run.as_ref().is_none_or(|(_, s)| s == "abandoned"))
            .collect();
        if !dropped.is_empty() {
            let what = named(&dropped.iter().map(|d| d.issue).collect::<Vec<_>>());
            event(
                w,
                "ahead_settled",
                &task,
                json!({"task_id": task, "outcome": "dependency_dropped",
                       "issues": wait.issues,
                       "dropped": dropped.iter().map(|d| d.issue).collect::<Vec<_>>()}),
            )?;
            stop(
                w,
                &Stop {
                    range,
                    task: Some(&task),
                    line: "decision",
                    key: format!("ahead:{task}"),
                    question: format!(
                        "{what}, which {task} waited for, will not land (its task abandoned or its pull request closed unmerged). Should {task} go on without it?"
                    ),
                    background: format!(
                        "{task} was dispatched for its design only, because {what} had not landed; it has its design and waits."
                    ),
                    if_yes: format!(
                        "The secretary tells {task} to build without {what}, as its design allows."
                    ),
                    if_no: format!(
                        "{task} is closed as abandoned; its design stays in its report."
                    ),
                    recommend: "None: it depends on what the dropped work was.",
                    why: "Only you decide what the range builds without.",
                    recommend_answer: "none",
                    more: json!({"kind": "order"}),
                },
            )?;
            continue;
        }
        for d in deps
            .iter()
            .filter(|d| d.run.as_ref().is_some_and(|(_, s)| s == "failed"))
        {
            let run = d.run.as_ref().map_or(0, |(id, _)| *id);
            stop(
                w,
                &Stop {
                    range,
                    task: Some(&task),
                    line: "decision",
                    key: format!("ahead-failed:{task}:{run}"),
                    question: format!(
                        "#{}'s task {} failed, and {task} waits for it to land. Keep waiting (yes) or go on without it (no)?",
                        d.issue, d.task
                    ),
                    background: format!(
                        "{task} was dispatched for its design only and waits for #{} to land; the launch of {} failed.",
                        d.issue, d.task
                    ),
                    if_yes: format!(
                        "{task} keeps waiting; when {} is launched again and lands, {task} is woken to build.",
                        d.task
                    ),
                    if_no: format!("{task} is woken and builds without #{}.", d.issue),
                    recommend: "None: it depends on whether the failed task is launched again.",
                    why: "Only you decide what the range builds without.",
                    recommend_answer: "none",
                    more: json!({"kind": "order"}),
                },
            )?;
        }
        let landed = deps.iter().all(|d| {
            d.run.as_ref().is_some_and(|(_, s)| s == "completed")
                && d.merged.is_some()
                && d.in_clone.is_some()
        });
        if deps.is_empty() || !landed {
            continue;
        }
        let prs: Vec<Value> = deps
            .iter()
            .map(|d| {
                let m = d.merged.clone().unwrap_or_default();
                let c = d.in_clone.clone().unwrap_or_default();
                json!({"issue": d.issue, "task_id": d.task, "repo": m["repo"], "pr": m["pr"],
                       "merge_commit": c["merge_commit"], "base": c["base"],
                       "base_head": c["base_head"]})
            })
            .collect();
        let newest = prs.last().cloned().unwrap_or_default();
        let base = newest["base"].as_str().unwrap_or("main").to_string();
        let head = newest["base_head"].as_str().unwrap_or("").to_string();
        let each: Vec<String> = prs
            .iter()
            .map(|p| {
                format!(
                    "#{} landed: {}#{} merged (merge commit {})",
                    p["issue"],
                    p["repo"].as_str().unwrap_or("?"),
                    p["pr"],
                    p["merge_commit"].as_str().unwrap_or("?")
                )
            })
            .collect();
        let body = format!(
            "{}. origin/{base} is now {head}. Your design-only wait is over: run git fetch origin, bring your branch up to origin/{base}, and build: code, tests, the reviews, then report done.",
            each.join("; ")
        );
        let detail = json!({"issues": wait.issues, "prs": prs, "base": base, "base_head": head});
        event(
            w,
            "ahead_settled",
            &task,
            json!({"task_id": task, "outcome": "landed", "issues": wait.issues, "prs": prs,
                   "base": base, "base_head": head}),
        )?;
        if status == "parked" {
            super::park::wake_with_notice(w, &task, "landed", detail, &body)?;
        } else {
            super::door::notice(w, &crate::spec::worker_agent_id(&task), &task, &body)?;
        }
    }
    Ok(())
}

/// The person answered the entry `pending` with `answer`: a `no` to a failed dependency's
/// question (`ahead-failed:`) releases the wait, so the run is woken to build without it.
pub fn answered(w: &StateWriter, pending: i64, answer: &str) -> Result<()> {
    let key: Option<String> = w
        .conn()
        .query_row(
            "SELECT json_extract(context_json, '$.key') FROM pending WHERE id = ?1",
            [pending],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(task) = key
        .as_deref()
        .and_then(|k| k.strip_prefix("ahead-failed:"))
        .and_then(|k| k.rsplit_once(':'))
        .map(|(task, _)| task.to_string())
    else {
        return Ok(());
    };
    if answer != "no" || open_wait(w.conn(), &task)?.is_none() {
        return Ok(());
    }
    event(
        w,
        "ahead_settled",
        &task,
        json!({"task_id": task, "outcome": "released", "pending_id": pending}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::writer::RunUpsert;

    /// A range `1` of project `p` with `design_ahead`, its task `r1-1` (Issue 1) at work with a
    /// pull request, and `r1-2` (Issue 2) dispatched for its design only, waiting for #1, at work
    /// on a verified launch of conversation `c2`.
    fn org() -> (tempfile::TempDir, StateWriter) {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO ranges (id, project_id, at_once, budget_tasks, brief_head, proposed_by,
                                     status, approved_by, approved_at, design_ahead)
                 VALUES (1, (SELECT id FROM projects WHERE slug = 'p'), 2, 9, 'h', 'secretary',
                         'active', 'person', strftime('%Y-%m-%dT%H:%M:%fZ','now'), 1)",
                [],
            )
            .unwrap();
        for task in ["r1-1", "r1-2"] {
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                title: Some("T"),
                status: Some("in_use"),
                ..Default::default()
            })
            .unwrap();
            w.conn()
                .execute("UPDATE runs SET range_id = 1 WHERE task_id = ?1", [task])
                .unwrap();
        }
        ev(
            &w,
            "belt_dispatched",
            "r1-1",
            json!({"range_id": 1, "issue": 1, "task_id": "r1-1", "design_only": false}),
        );
        w.conn()
            .execute_batch(
                "INSERT INTO pr_watches (repo, pr, head_oid, run_id)
                 VALUES ('o/p', 11, 'aaaa', (SELECT id FROM runs WHERE task_id = 'r1-1'));
                 INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage,
                                         conversation_id)
                 VALUES ((SELECT id FROM runs WHERE task_id = 'r1-2'), 's2', 'fake', 'b', '/w',
                         'verified', 'c2');",
            )
            .unwrap();
        ev(
            &w,
            "belt_dispatched",
            "r1-2",
            json!({"range_id": 1, "issue": 2, "task_id": "r1-2", "design_only": true,
                   "waits_for": [1], "waits_tasks": ["r1-1"]}),
        );
        (dir, w)
    }

    fn ev(w: &StateWriter, kind: &str, task: &str, payload: Value) {
        event(w, kind, task, payload).unwrap();
    }

    fn set(w: &StateWriter, task: &str, status: &str) {
        w.conn()
            .execute(
                "UPDATE runs SET status = ?2 WHERE task_id = ?1",
                [task, status],
            )
            .unwrap();
    }

    fn status(w: &StateWriter, task: &str) -> String {
        w.conn()
            .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn n(w: &StateWriter, sql: &str) -> i64 {
        w.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Whether `task` counts against the limits (belt.md 3.2).
    fn counts(w: &StateWriter, task: &str) -> bool {
        w.conn()
            .query_row(
                &format!(
                    "SELECT NOT {} FROM runs r WHERE r.task_id = ?1",
                    super::super::belt::WAITING
                ),
                [task],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn report_design(w: &StateWriter) {
        let Report::Design(_, wait) = report(w.conn(), "r1-2", "blocked").unwrap() else {
            panic!("not a design report");
        };
        design_reported(w, "r1-2", &wait).unwrap();
    }

    /// A design-only candidate may wait only for a task with a pull request, not abandoned or
    /// failed, and not waiting for the person (belt.md 4.2, 4.3).
    #[test]
    fn a_dependency_is_waited_for_only_with_a_pull_request_and_no_open_question() {
        let (_d, w) = org();
        let ahead = |w: &StateWriter| super::super::belt::ahead_of(w.conn(), 1, 1).unwrap();
        assert_eq!(ahead(&w).as_deref(), Some("r1-1"));
        w.conn()
            .execute(
                "INSERT INTO pending (run_id, kind, question, status)
                 VALUES ((SELECT id FROM runs WHERE task_id = 'r1-1'), 'question', 'q?',
                         'escalated')",
                [],
            )
            .unwrap();
        assert_eq!(ahead(&w), None, "its task waits for the person");
        w.conn()
            .execute("UPDATE pending SET status = 'answered'", [])
            .unwrap();
        assert_eq!(ahead(&w).as_deref(), Some("r1-1"));
        for gone in ["abandoned", "failed"] {
            set(&w, "r1-1", gone);
            assert_eq!(ahead(&w), None, "its task {gone}");
        }
        set(&w, "r1-1", "review");
        w.conn().execute("DELETE FROM pr_watches", []).unwrap();
        assert_eq!(ahead(&w), None, "no pull request");
        assert_eq!(super::super::belt::ahead_of(w.conn(), 1, 3).unwrap(), None);
    }

    /// The design report frees the slot; a wake before the landing (a message) counts again
    /// until it is parked again, and a run woken so is parked again by the same rule.
    #[test]
    fn the_design_report_frees_the_slot_and_a_wake_before_the_landing_counts_again() {
        let (_d, w) = org();
        assert!(counts(&w, "r1-2"));
        assert!(matches!(
            report(w.conn(), "r1-2", "done").unwrap(),
            Report::Refused(_)
        ));
        assert!(matches!(
            report(w.conn(), "r1-1", "done").unwrap(),
            Report::Plain
        ));
        report_design(&w);
        assert!(!counts(&w, "r1-2"), "its design reported, it waits");
        assert_eq!(
            waiting_field(w.conn(), "r1-2").unwrap(),
            Some(json!({"for": "landing", "issues": [1], "why": "waits for #1 to land"}))
        );
        ev(&w, "run_parked", "r1-2", json!({"task_id": "r1-2"}));
        assert!(!counts(&w, "r1-2"));
        ev(
            &w,
            "run_unparked",
            "r1-2",
            json!({"task_id": "r1-2", "pending_max": 0}),
        );
        assert!(counts(&w, "r1-2"), "woken by a message, it works again");
        let parkable: i64 = n(
            &w,
            &format!(
                "SELECT count(*) FROM runs r WHERE r.task_id = 'r1-2' AND {}",
                super::super::belt::LANDING_WAIT
            ),
        );
        assert_eq!(parkable, 1, "and may be parked again");
        ev(&w, "run_parked", "r1-2", json!({"task_id": "r1-2"}));
        assert!(!counts(&w, "r1-2"));
        ev(
            &w,
            "run_unparked",
            "r1-2",
            json!({"task_id": "r1-2", "pending_max": 0}),
        );
        report_design(&w);
        assert!(!counts(&w, "r1-2"), "a design reported again waits again");
    }

    /// `park::settle` wakes a parked run with no question left, but not one waiting for a
    /// landing: it has nothing escalated and still waits.
    #[test]
    fn park_settle_leaves_a_run_waiting_for_a_landing_parked() {
        let (_d, w) = org();
        report_design(&w);
        set(&w, "r1-2", "parked");
        w.transaction(super::super::park::settle).unwrap();
        assert_eq!(status(&w, "r1-2"), "parked");
        w.transaction(settle).unwrap();
        assert_eq!(status(&w, "r1-2"), "parked");
        assert_eq!(
            n(
                &w,
                "SELECT count(*) FROM events WHERE kind = 'ahead_settled'"
            ),
            0
        );
    }

    /// A landing is the close, the merge read and the merge in the clone, all three; then the
    /// parked run is woken with the reason `landed` and told what landed.
    #[test]
    fn a_landing_needs_the_close_the_merge_and_the_merge_in_the_clone() {
        let (_d, w) = org();
        report_design(&w);
        set(&w, "r1-2", "parked");
        let settled = |w: &StateWriter| {
            w.transaction(settle).unwrap();
            n(
                w,
                "SELECT count(*) FROM events WHERE kind = 'ahead_settled'",
            )
        };
        set(&w, "r1-1", "completed");
        assert_eq!(settled(&w), 0, "a close alone");
        ev(
            &w,
            "pr_merged",
            "r1-1",
            json!({"repo": "o/p", "pr": 11, "merge_commit": "m1", "task_id": "r1-1"}),
        );
        assert_eq!(settled(&w), 0, "not yet in the clone");
        ev(
            &w,
            "landed_in_clone",
            "r1-1",
            json!({"task_id": "r1-1", "merge_commit": "m1", "base": "main", "base_head": "h9"}),
        );
        assert_eq!(settled(&w), 1);
        assert_eq!(settled(&w), 1, "settled once");
        assert_eq!(status(&w, "r1-2"), "suspended");
        let reason: String = w
            .conn()
            .query_row(
                "SELECT json_extract(payload_json, '$.reason') FROM events
                 WHERE kind = 'run_unparked'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "landed");
        let told: String = w
            .conn()
            .query_row(
                "SELECT body FROM messages WHERE recipient = 'w-r1-2' AND kind = 'notice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            told.starts_with("#1 landed: o/p#11 merged (merge commit m1). origin/main is now h9."),
            "{told}"
        );
        assert!(matches!(
            report(w.conn(), "r1-2", "done").unwrap(),
            Report::Plain
        ));
    }

    /// A dependency abandoned will not land: the wait is settled and the person is asked. A
    /// dependency failed is asked about once, and the person's `no` releases the wait.
    #[test]
    fn a_dropped_dependency_is_a_question_and_a_failed_one_is_released_by_no() {
        let (_d, w) = org();
        report_design(&w);
        set(&w, "r1-1", "abandoned");
        w.transaction(settle).unwrap();
        assert_eq!(
            n(
                &w,
                "SELECT count(*) FROM events WHERE kind = 'ahead_settled'
                   AND json_extract(payload_json, '$.outcome') = 'dependency_dropped'"
            ),
            1
        );
        assert_eq!(
            n(
                &w,
                "SELECT count(*) FROM pending WHERE status = 'escalated'
                   AND json_extract(context_json, '$.stop_line') = 'decision'
                   AND json_extract(context_json, '$.key') = 'ahead:r1-2'"
            ),
            1
        );

        let (_d, w) = org();
        report_design(&w);
        set(&w, "r1-1", "failed");
        w.transaction(settle).unwrap();
        w.transaction(settle).unwrap();
        let id: i64 = n(
            &w,
            "SELECT id FROM pending WHERE json_extract(context_json, '$.key') LIKE 'ahead-failed:r1-2:%'",
        );
        assert!(open_wait(w.conn(), "r1-2").unwrap().is_some());
        answered(&w, id, "yes").unwrap();
        assert!(open_wait(w.conn(), "r1-2").unwrap().is_some(), "yes waits");
        answered(&w, id, "no").unwrap();
        assert!(
            open_wait(w.conn(), "r1-2").unwrap().is_none(),
            "no releases"
        );
        assert_eq!(
            n(
                &w,
                "SELECT count(*) FROM events WHERE kind = 'ahead_settled'
                   AND json_extract(payload_json, '$.outcome') = 'released'"
            ),
            1
        );
    }
}
