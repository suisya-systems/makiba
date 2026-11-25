//! The belt, step 1 (Issue 257, `docs/design/belt.md`): a range of work the person approved once,
//! kept as a record, and `makibad` putting the range's next task in when a slot frees.
//!
//! - The verb `range` (`propose`, `approve`, `pause`, `resume`, `end`, `skip`), in the door's
//!   transaction. Only the person's key approves or resumes; one range of a project is `active` at
//!   a time, checked on every move to `active`.
//! - The read of a range's candidates: on a thread, outside any transaction, through `gh`; written
//!   with its `range_read` event in one transaction. A failed or truncated read is recorded as
//!   incomplete and nothing is dispatched from a range until its newest read is complete and was
//!   started after the range's newest approval or resume.
//! - `fill`: in the transaction of the event that freed a slot (a done report, a close, an
//!   escalation, a read) and once each tick, the next candidate of each active range is dispatched
//!   while the range's runs that count are below `at_once`, `max_concurrent_workers` has room and
//!   the budget allows. The task id `r<range id>-<issue>` is dispatched once whatever restarts.
//! - A run that waits only for the person (an escalated pending entry) counts in neither count.
//! - The proof of a landing: the run's registered pull request read merged (`pr_merged`), which
//!   the PR watch records (`prwatch.rs`, Issue 388); the belt only checks the project's clone.
//! - The read surface's `ranges[]` (spec section 12, table "Range field").
//!
//! Step 2 (Issue 282): the stops `makibad` writes itself, each a pending entry escalated at once
//! with `belt_stopped`, in the transaction of what caused it and before the fill there.
//!
//! - `gate_stop`: a refusal of the merge gate (line `gate`) or tests edited after the test commit
//!   (line `tests`) on a range task's pull request, for the head recorded last.
//! - `head_recorded`: the head that reaches the range's `budget_fixes` (line `budget`); the stops
//!   of an older head go.
//! - `fill`: `budget_hours`, and a budget of the range reached while a candidate is left (line
//!   `budget`, no task).
//! - The pauses: two refused pull requests in a row (`gate_stop`), two failed launches in a row
//!   (`launch_failed`); a failed run holds no slot.
//! - `stop_parts_missing`: what a range task's entry lacks to be escalated.
//!
//! Step 5 (Issue 248): the read surface's `belt[]` (spec section 12, table "Belt field"), one
//! row per open run of a range, placed by the same `run_item` as `ranges[].items[]` and counted
//! by the same `counts_sql` as the fill. Computed on each read; nothing is kept beside the ledger.

use super::door::{Response, internal, refuse, str_arg, usage};
use super::gh::{GhError, run_gh};
use crate::db::Result;
use crate::db::writer::{NewEvent, RunUpsert, StateWriter};
use crate::spec::{DoorError, ErrorKind, Role};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// `$MAKIBA_RANGE_READ_INTERVAL_MS`: how often an active range is read again (default 10 min).
pub const READ_INTERVAL_ENV: &str = "MAKIBA_RANGE_READ_INTERVAL_MS";
const DEFAULT_INTERVAL_MS: i64 = 10 * 60 * 1000;
/// A failed read is tried again after this; so is the clone check.
const RETRY_MS: i64 = 60 * 1000;
/// A read that lists this many open Issues or pull requests is not known to be whole.
const ISSUE_LIMIT: usize = 500;
const PR_LIMIT: usize = 200;
/// `gh` returns at most this many of an Issue's labels or comments, of a PR's closing references.
const NESTED_LIMIT: usize = 100;
/// The kinds of a stop of line 1 (belt.md 5.2).
const STOP_KINDS: [&str; 5] = ["design", "scope", "order", "naming", "other"];
/// `budget_fixes` of a proposal that names none (belt.md 2.1).
const DEFAULT_FIXES: i64 = 3;

/// The SQL of `LANDING_WAIT`, as a literal so that `WAITING` can be one constant.
macro_rules! landing_wait {
    () => {
        "(EXISTS (SELECT 1 FROM events lb WHERE lb.run_id = r.id AND lb.kind = 'belt_dispatched'
                 AND json_extract(lb.payload_json, '$.design_only') = 1)
          AND EXISTS (SELECT 1 FROM events lr WHERE lr.run_id = r.id AND lr.kind = 'design_reported')
          AND NOT EXISTS (SELECT 1 FROM events ls WHERE ls.run_id = r.id AND ls.kind = 'ahead_settled'))"
    };
}

/// A run `r` waits for a landing (belt.md 4.3, Issue 284): it was dispatched for its design
/// only, has reported its design, and what it waits for has not been settled (landed, dropped or
/// released). It may be parked (`park.rs`) as a run waiting for the person is.
pub const LANDING_WAIT: &str = landing_wait!();

/// A run `r` that counts in neither count (belt.md 3.2, 4.1, 4.3): it waits only for the person
/// (an escalated pending entry), or it waits for a landing. A run woken from parking
/// (`run_unparked`, Issue 283) is at work again: it counts from the wake until it is parked again,
/// escalates an entry newer than the wake (`pending_max`) or reports its design again, even while
/// an older entry is still escalated or its landing has not come.
pub const WAITING: &str = concat!(
    "(EXISTS (SELECT 1 FROM pending wp WHERE wp.run_id = r.id
    AND wp.status = 'escalated'
    AND (NOT EXISTS (SELECT 1 FROM events wu WHERE wu.run_id = r.id AND wu.kind = 'run_unparked')
         OR wp.id > (SELECT json_extract(wu.payload_json, '$.pending_max') FROM events wu
                     WHERE wu.run_id = r.id AND wu.kind = 'run_unparked'
                     ORDER BY wu.id DESC LIMIT 1)
         OR coalesce((SELECT max(wk.id) FROM events wk
                      WHERE wk.run_id = r.id AND wk.kind = 'run_parked'), 0)
            > (SELECT max(wu.id) FROM events wu
               WHERE wu.run_id = r.id AND wu.kind = 'run_unparked')))
    OR (",
    landing_wait!(),
    " AND (coalesce((SELECT max(lu.id) FROM events lu
                      WHERE lu.run_id = r.id AND lu.kind = 'run_unparked'), 0)
              < (SELECT max(ld.id) FROM events ld
                 WHERE ld.run_id = r.id AND ld.kind = 'design_reported')
           OR coalesce((SELECT max(lk.id) FROM events lk
                        WHERE lk.run_id = r.id AND lk.kind = 'run_parked'), 0)
              > coalesce((SELECT max(lu.id) FROM events lu
                          WHERE lu.run_id = r.id AND lu.kind = 'run_unparked'), 0))))"
);

/// `chain`: the range `?1` and every range it replaced (each `ended` `replaced`). Their runs stay
/// theirs (where a run came from), and count for the range that replaced them.
const CHAIN: &str = "WITH RECURSIVE chain(id) AS (
    SELECT ?1
    UNION SELECT g.replaces FROM ranges g JOIN chain c ON g.id = c.id
          JOIN ranges o ON o.id = g.replaces
          WHERE o.status = 'ended' AND o.ended_reason = 'replaced')";

/// Whether the range `g` is past its `budget_hours`, counted from its approval (belt.md 2.1).
const HOURS_PAST: &str = "(g.budget_hours IS NOT NULL AND g.approved_at IS NOT NULL
    AND julianday('now') >= julianday(g.approved_at) + g.budget_hours / 24.0)";

/// The reference a run's `issue_refs` holds for an Issue of a project: `<slug>#<n>`.
pub fn issue_ref(slug: &str, issue: i64) -> String {
    format!("{slug}#{issue}")
}

fn event(
    w: &StateWriter,
    kind: &str,
    actor: &str,
    task: Option<&str>,
    payload: Value,
) -> Result<i64> {
    w.append_event(&NewEvent {
        kind,
        actor: Some(actor),
        payload: Some(&payload),
        run_task_id: task,
        ..Default::default()
    })
}

// ---- the verb ------------------------------------------------------------------------------

struct Range {
    id: i64,
    project_id: i64,
    slug: String,
    status: String,
    replaces: Option<i64>,
}

fn load(w: &StateWriter, id: i64) -> std::result::Result<Range, DoorError> {
    w.conn()
        .query_row(
            "SELECT g.id, g.project_id, p.slug, g.status, g.replaces FROM ranges g
             JOIN projects p ON p.id = g.project_id WHERE g.id = ?1",
            [id],
            |r| {
                Ok(Range {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    slug: r.get(2)?,
                    status: r.get(3)?,
                    replaces: r.get(4)?,
                })
            },
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(|| refuse(ErrorKind::NotFound, format!("no range {id}"), ""))
}

fn int_arg(args: &Value, name: &str) -> std::result::Result<i64, DoorError> {
    args.get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| usage(format!("the argument {name} (a number) is missing")))
}

fn conflict(message: impl Into<String>) -> DoorError {
    refuse(ErrorKind::Conflict, message, "")
}

fn db(e: crate::db::Error) -> DoorError {
    internal(e)
}

/// `range` (spec section 12). The door has checked the verb against the role; `approve` and
/// `resume` are the person's alone, whatever else the role may do.
pub(super) fn range(w: &StateWriter, actor: &str, role: Role, args: &Value) -> Response {
    let action = str_arg(args, "action")?;
    if ["approve", "resume"].contains(&action) && role != Role::Person {
        return Err(refuse(
            ErrorKind::Denied,
            format!("only the person may {action} a range, with the person key"),
            "the person runs makiba range in their own shell",
        ));
    }
    match action {
        "propose" => propose(w, actor, args),
        "approve" => approve(w, actor, args),
        "resume" => move_status(w, actor, args, "paused", "active", "range_resumed"),
        "pause" => move_status(w, actor, args, "active", "paused", "range_paused"),
        "end" => {
            let g = load(w, int_arg(args, "id")?)?;
            if g.status == "ended" {
                return Err(conflict(format!("range {} has ended already", g.id)));
            }
            let reason = if role == Role::Person {
                "person"
            } else {
                "secretary"
            };
            end_range(w, g.id, reason, actor).map_err(db)?;
            Ok(json!({"id": g.id, "status": "ended", "ended_reason": reason}))
        }
        "skip" => skip(w, actor, args),
        other => Err(usage(format!(
            "action is propose, approve, pause, resume, end or skip, not {other:?}"
        ))),
    }
}

fn propose(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let slug = str_arg(args, "project")?;
    let project: i64 = w
        .conn()
        .query_row(
            "SELECT id FROM projects WHERE slug = ?1 AND status = 'active'",
            [slug],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(|| {
            refuse(
                ErrorKind::NotFound,
                format!("no active project {slug:?}"),
                "",
            )
        })?;
    let issues: Vec<i64> = match args.get("issues") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_i64().filter(|n| *n > 0))
            .collect::<Option<_>>()
            .ok_or_else(|| usage("issues is a list of Issue numbers"))?,
        Some(_) => return Err(usage("issues is a list of Issue numbers")),
    };
    let takes = args
        .get("takes")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    Takes::parse(takes).map_err(usage)?;
    if issues.is_empty() && takes.is_empty() {
        return Err(usage("a range names Issues, a condition (takes), or both"));
    }
    let capacity = super::dispatch::capacity_from_env() as i64;
    let at_once = int_arg(args, "at_once")?;
    if !(1..=capacity).contains(&at_once) {
        return Err(usage(format!(
            "at_once is 1 to max_concurrent_workers ({capacity}), not {at_once}"
        )));
    }
    let budget = int_arg(args, "budget_tasks")?;
    if budget < 1 {
        return Err(usage("budget_tasks is at least 1"));
    }
    let hours = match args.get("budget_hours") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|h| h.is_finite() && *h > 0.0)
                .ok_or_else(|| usage("budget_hours is a number of hours above 0"))?,
        ),
    };
    let fixes = match args.get("budget_fixes") {
        None | Some(Value::Null) => DEFAULT_FIXES,
        Some(v) => v
            .as_i64()
            .filter(|n| *n >= 1)
            .ok_or_else(|| usage("budget_fixes is at least 1"))?,
    };
    // Follow-ups found on the way are inside a proposal unless it says `ask` (belt.md 2.1, Q3).
    let follow_ups = match args.get("follow_ups") {
        None | Some(Value::Null) => "inside",
        Some(v) => match v.as_str() {
            Some(f @ ("ask" | "inside")) => f,
            _ => return Err(usage("follow_ups is ask or inside")),
        },
    };
    let design_ahead = match args.get("design_ahead") {
        None | Some(Value::Null) => false,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| usage("design_ahead is true or false"))?,
    };
    let head = str_arg(args, "brief_head")?;
    if head.trim().is_empty() {
        return Err(usage("the brief head is not empty"));
    }
    let replaces = match args.get("replaces").and_then(Value::as_i64) {
        None => None,
        Some(id) => {
            let old = load(w, id)?;
            if old.project_id != project || old.status == "ended" {
                return Err(conflict(format!(
                    "range {id} is not a range of {slug} that has not ended"
                )));
            }
            Some(old)
        }
    };
    let store = || -> Result<i64> {
        w.conn().execute(
            "INSERT INTO ranges (project_id, issues_json, takes, at_once, budget_tasks, brief_head,
                                 replaces, proposed_by, budget_hours, budget_fixes, follow_ups,
                                 design_ahead)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                project,
                json!(issues).to_string(),
                takes,
                at_once,
                budget,
                head,
                replaces.as_ref().map(|g| g.id),
                actor,
                hours,
                fixes,
                follow_ups,
                design_ahead
            ],
        )?;
        let id = w.conn().last_insert_rowid();
        event(
            w,
            "range_proposed",
            actor,
            None,
            json!({"range_id": id, "project": slug, "replaces": replaces.as_ref().map(|g| g.id)}),
        )?;
        // A draft replaced by a newer draft ends now; an active or paused one at the approval.
        if let Some(old) = replaces.as_ref().filter(|g| g.status == "proposed") {
            end_range(w, old.id, "replaced", actor)?;
        }
        Ok(id)
    };
    let id = store().map_err(db)?;
    Ok(json!({"id": id, "status": "proposed"}))
}

/// The newest complete read of a range: its event id.
fn newest_complete_read(w: &StateWriter, id: i64) -> Result<Option<i64>> {
    Ok(w.conn()
        .query_row(
            "SELECT id FROM events WHERE kind = 'range_read'
               AND json_extract(payload_json, '$.range_id') = ?1
               AND json_extract(payload_json, '$.complete') = 1
             ORDER BY id DESC LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Another range of the project that is `active`, other than `but`.
fn other_active(w: &StateWriter, g: &Range, but: Option<i64>) -> Result<Option<i64>> {
    Ok(w.conn()
        .query_row(
            "SELECT id FROM ranges WHERE project_id = ?1 AND status = 'active' AND id != ?2
               AND id IS NOT ?3 LIMIT 1",
            params![g.project_id, g.id, but],
            |r| r.get(0),
        )
        .optional()?)
}

fn approve(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let g = load(w, int_arg(args, "id")?)?;
    if g.status != "proposed" {
        return Err(conflict(format!(
            "range {} is {}; only a proposed range is approved",
            g.id, g.status
        )));
    }
    let shown = int_arg(args, "read")?;
    let Some(read) = newest_complete_read(w, g.id).map_err(db)? else {
        return Err(conflict(format!(
            "range {} has no complete read yet; nothing can be shown to approve",
            g.id
        )));
    };
    if read != shown {
        return Err(refuse(
            ErrorKind::Stale,
            format!(
                "range {} was read again (read {read}) since the read shown ({shown})",
                g.id
            ),
            "look at the range again (makiba range approve shows it) and approve that",
        ));
    }
    if let Some(old) = g.replaces {
        let st = load(w, old)?.status;
        // A draft replaced by this draft was ended at the proposal: nothing is left to replace.
        let draft: bool = w
            .conn()
            .query_row(
                "SELECT status = 'ended' AND ended_reason = 'replaced' AND approved_at IS NULL
                 FROM ranges WHERE id = ?1",
                [old],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if !draft && !["active", "paused"].contains(&st.as_str()) {
            return Err(conflict(format!(
                "range {} replaces range {old}, which is {st}; propose again",
                g.id
            )));
        }
    }
    if let Some(other) = other_active(w, &g, g.replaces).map_err(db)? {
        return Err(conflict(format!(
            "range {other} of {} is active; one active range per project",
            g.slug
        )));
    }
    let mode = super::gate::merge_approval_mode();
    let store = || -> Result<()> {
        if let Some(old) = g.replaces {
            end_range(w, old, "replaced", actor)?;
        }
        w.conn().execute(
            "UPDATE ranges SET status = 'active', approved_by = ?2, approval_mode = ?3,
               approved_read_id = ?4, approved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1",
            params![g.id, actor, mode, read],
        )?;
        let shown = candidates(w.conn(), g.id)?;
        event(
            w,
            "range_approved",
            actor,
            None,
            json!({"range_id": g.id, "approved_by": actor, "approval_mode": mode,
                   "read_id": read, "shown": shown}),
        )?;
        Ok(())
    };
    store().map_err(db)?;
    Ok(json!({"id": g.id, "status": "active", "approval_mode": mode}))
}

fn move_status(
    w: &StateWriter,
    actor: &str,
    args: &Value,
    from: &str,
    to: &str,
    kind: &str,
) -> Response {
    let g = load(w, int_arg(args, "id")?)?;
    if g.status != from {
        return Err(conflict(format!(
            "range {} is {}, not {from}",
            g.id, g.status
        )));
    }
    if to == "active"
        && let Some(other) = other_active(w, &g, None).map_err(db)?
    {
        return Err(conflict(format!(
            "range {other} of {} is active; one active range per project",
            g.slug
        )));
    }
    let reason = args.get("reason").and_then(Value::as_str);
    let store = || -> Result<()> {
        w.conn().execute(
            "UPDATE ranges SET status = ?2 WHERE id = ?1",
            params![g.id, to],
        )?;
        event(
            w,
            kind,
            actor,
            None,
            json!({"range_id": g.id, "reason": reason}),
        )?;
        Ok(())
    };
    store().map_err(db)?;
    Ok(json!({"id": g.id, "status": to}))
}

fn skip(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let g = load(w, int_arg(args, "id")?)?;
    let issue = int_arg(args, "issue")?;
    let reason = str_arg(args, "reason")?;
    if reason.trim().is_empty() {
        return Err(usage("the reason is not empty"));
    }
    if g.status == "ended" {
        return Err(conflict(format!("range {} has ended", g.id)));
    }
    let taken: bool = w
        .conn()
        .query_row(
            &format!(
                "{CHAIN} SELECT EXISTS (SELECT 1 FROM events WHERE kind IN ('belt_dispatched','belt_skipped')
                   AND json_extract(payload_json, '$.range_id') IN (SELECT id FROM chain)
                   AND json_extract(payload_json, '$.issue') = ?2)"
            ),
            params![g.id, issue],
            |r| r.get(0),
        )
        .map_err(internal)?;
    if taken {
        return Err(conflict(format!(
            "#{issue} was dispatched or skipped in range {} already",
            g.id
        )));
    }
    event(
        w,
        "belt_skipped",
        actor,
        None,
        json!({"range_id": g.id, "issue": issue, "reason": reason}),
    )
    .map_err(db)?;
    Ok(json!({"id": g.id, "issue": issue, "skipped": true}))
}

fn end_range(w: &StateWriter, id: i64, reason: &str, actor: &str) -> Result<()> {
    let changed = w.conn().execute(
        "UPDATE ranges SET status = 'ended', ended_reason = ?2,
           ended_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1 AND status != 'ended'",
        params![id, reason],
    )?;
    if changed == 1 {
        event(
            w,
            "range_ended",
            actor,
            None,
            json!({"range_id": id, "reason": reason}),
        )?;
    }
    Ok(())
}

// ---- the stop of a range task ----------------------------------------------------------------

/// The belt's fields of `pending add` (belt.md 5.2) as context keys: `stop_kind` as `kind`, and
/// `recommend_answer` (`yes`, `no` or `none`), so that whether the person took the recommendation
/// is computed from `answer`. Empty when neither is given.
pub(super) fn stop_fields(args: &Value) -> std::result::Result<Map<String, Value>, DoorError> {
    let mut m = Map::new();
    if let Some(k) = args.get("stop_kind").and_then(Value::as_str) {
        if !STOP_KINDS.contains(&k) {
            return Err(usage(format!(
                "stop_kind is one of {}, not {k:?}",
                STOP_KINDS.join(", ")
            )));
        }
        m.insert("kind".into(), json!(k));
    }
    if let Some(a) = args.get("recommend_answer").and_then(Value::as_str) {
        if !["yes", "no", "none"].contains(&a) {
            return Err(usage(format!(
                "recommend_answer is yes, no or none, not {a:?}"
            )));
        }
        m.insert("recommend_answer".into(), json!(a));
    }
    Ok(m)
}

/// The escalation of entry `id`, in its transaction: when its run is a range task, the entry
/// records the stop (`stop_line`, `range_id`, `to`, the range's approver) and `belt_stopped` is
/// written. The fill that follows in the same transaction puts the next task in.
pub(super) fn stopped(w: &StateWriter, actor: &str, id: i64) -> Result<()> {
    let row: Option<(i64, String, String, Option<String>)> = w
        .conn()
        .query_row(
            "SELECT r.range_id, r.task_id, p.question, g.approved_by FROM pending p
             JOIN runs r ON r.id = p.run_id JOIN ranges g ON g.id = r.range_id
             WHERE p.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((range, task, question, to)) = row else {
        return Ok(());
    };
    let to = to.unwrap_or_else(|| crate::spec::AGENT_PERSON.to_string());
    w.conn().execute(
        "UPDATE pending SET context_json = json_set(context_json,
           '$.stop_line', coalesce(json_extract(context_json, '$.stop_line'), 'decision'),
           '$.range_id', ?2, '$.to', ?3)
         WHERE id = ?1",
        params![id, range, to],
    )?;
    let line: String = w.conn().query_row(
        "SELECT json_extract(context_json, '$.stop_line') FROM pending WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let reason = question.lines().next().unwrap_or_default();
    event(
        w,
        "belt_stopped",
        actor,
        Some(&task),
        json!({"range_id": range, "task_id": task, "line": line, "reason": reason,
               "pending_id": id}),
    )?;
    Ok(())
}

/// What a range task's entry made by `pending add` still lacks to be escalated (belt.md 5.2): the
/// stop's kind and the recommended answer, by the names `pending add` takes them. Empty for an
/// entry of no range task, and for one made from a screen.
pub(super) fn stop_parts_missing(c: &Connection, id: i64) -> rusqlite::Result<Vec<&'static str>> {
    let row: Option<(Option<String>, Option<String>)> = c
        .query_row(
            "SELECT json_extract(p.context_json, '$.kind'),
                    json_extract(p.context_json, '$.recommend_answer')
             FROM pending p JOIN runs r ON r.id = p.run_id
             WHERE p.id = ?1 AND r.range_id IS NOT NULL AND p.prompt_text IS NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((kind, answer)) = row else {
        return Ok(Vec::new());
    };
    let mut missing = Vec::new();
    if kind.is_none() {
        missing.push("stop_kind");
    }
    if answer.is_none() {
        missing.push("recommend_answer");
    }
    Ok(missing)
}

// ---- the stops makibad writes itself (step 2, Issue 282) -------------------------------------

/// A stop `makibad` writes (belt.md section 5): the question for the person with its context
/// parts, as the secretary would have written them. `task` is `None` for a stop of the whole
/// range. `key` names what is stopped: a second stop with the same key in the range is not written.
pub(super) struct Stop<'a> {
    pub(super) range: i64,
    pub(super) task: Option<&'a str>,
    pub(super) line: &'a str,
    pub(super) key: String,
    pub(super) question: String,
    pub(super) background: String,
    pub(super) if_yes: String,
    pub(super) if_no: String,
    pub(super) recommend: &'a str,
    pub(super) why: &'a str,
    pub(super) recommend_answer: &'a str,
    /// More context keys (the pull request and head of a gate stop).
    pub(super) more: Value,
}

/// Write the stop inside the caller's transaction: a pending entry `escalated` at once (so its
/// run waits only for the person and counts in neither count), `belt_stopped`, and a notice to
/// the secretary, who puts it to the person. Returns the entry's id, and whether it is new.
pub(super) fn stop(w: &StateWriter, s: &Stop) -> Result<(i64, bool)> {
    // The newest entry of this subject stands, whatever its status, except one `makibad` itself
    // withdrew because the pull request had moved to another head (`head_recorded`): a pull
    // request that comes back to that head is stopped again, by a new entry.
    let known: Option<(i64, bool)> = w
        .conn()
        .query_row(
            "SELECT id, status = 'withdrawn'
                        AND coalesce(json_extract(context_json, '$.withdrawn_by') = 'makibad', 0)
             FROM pending WHERE json_extract(context_json, '$.range_id') = ?1
               AND json_extract(context_json, '$.key') = ?2 ORDER BY id DESC LIMIT 1",
            params![s.range, s.key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((id, false)) = known {
        return Ok((id, false));
    }
    let to: Option<String> = w.conn().query_row(
        "SELECT approved_by FROM ranges WHERE id = ?1",
        [s.range],
        |r| r.get(0),
    )?;
    let to = to.unwrap_or_else(|| crate::spec::AGENT_PERSON.to_string());
    let mut context = json!({
        "stop_line": s.line, "range_id": s.range, "to": to, "by": "makibad", "key": s.key,
        "background": s.background, "if_yes": s.if_yes, "if_no": s.if_no,
        "recommend": s.recommend, "why": s.why, "recommend_answer": s.recommend_answer,
    });
    if let (Value::Object(c), Value::Object(m)) = (&mut context, &s.more) {
        c.extend(m.clone());
    }
    w.conn().execute(
        "INSERT INTO pending (run_id, kind, question, context_json, status)
         VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'question', ?2, ?3, 'escalated')",
        params![s.task, s.question, context.to_string()],
    )?;
    let id = w.conn().last_insert_rowid();
    let reason = s.question.lines().next().unwrap_or_default();
    event(
        w,
        "belt_stopped",
        "makibad",
        s.task,
        json!({"range_id": s.range, "task_id": s.task, "line": s.line, "reason": reason,
               "pending_id": id}),
    )?;
    let body = format!(
        "A stop on the belt (range {}, line {}), pending entry {id}: {} Put it to the person with its link; the answer comes back to you.",
        s.range, s.line, s.question
    );
    match s.task {
        Some(t) => super::door::notice(w, crate::spec::AGENT_SECRETARY, t, &body)?,
        None => super::door::notice_untasked(w, crate::spec::AGENT_SECRETARY, &body)?,
    };
    Ok((id, true))
}

/// The range a run's range answers to now: itself while it has not ended, else the range that
/// replaced it (and so on). A run keeps the range it was dispatched under, and counts for the
/// one that replaced it; a pause for what that run did pauses that one.
fn acting(w: &StateWriter, range: i64) -> Result<Option<(i64, String)>> {
    let mut id = range;
    // A chain is as long as the ranges of a project; the bound only guards a loop in the records.
    for _ in 0..1000 {
        let status: Option<String> = w
            .conn()
            .query_row("SELECT status FROM ranges WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        match status {
            None => return Ok(None),
            Some(s) if s != "ended" => return Ok(Some((id, s))),
            Some(_) => {}
        }
        let next: Option<i64> = w
            .conn()
            .query_row(
                "SELECT n.id FROM ranges n JOIN ranges o ON o.id = n.replaces
                 WHERE n.replaces = ?1 AND o.ended_reason = 'replaced' AND n.approved_at IS NOT NULL
                 ORDER BY n.id DESC LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        match next {
            Some(n) => id = n,
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// Pause an active range by `makibad` (belt.md 2.4): nothing new goes in until the person resumes
/// it. The secretary is told.
fn pause(w: &StateWriter, id: i64, reason: &str) -> Result<()> {
    let changed = w.conn().execute(
        "UPDATE ranges SET status = 'paused' WHERE id = ?1 AND status = 'active'",
        [id],
    )?;
    if changed == 1 {
        event(
            w,
            "range_paused",
            "makibad",
            None,
            json!({"range_id": id, "reason": reason}),
        )?;
        super::door::notice_untasked(
            w,
            crate::spec::AGENT_SECRETARY,
            &format!(
                "Range {id} is paused by makibad: {reason}. Nothing new goes in; work already running goes on. Tell the person: only they resume it (makiba range resume {id})."
            ),
        )?;
    }
    Ok(())
}

/// The range of a task, when it is a range task.
fn range_of(w: &StateWriter, task: &str) -> Result<Option<i64>> {
    Ok(w.conn()
        .query_row(
            "SELECT range_id FROM runs WHERE task_id = ?1",
            [task],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// The range of a task and the merge approval mode it was approved under, for the gate: `None`
/// for a task of no range.
pub(super) fn approval_of(w: &StateWriter, task: &str) -> Result<Option<(i64, Option<String>)>> {
    Ok(w.conn()
        .query_row(
            "SELECT g.id, g.approval_mode FROM runs r JOIN ranges g ON g.id = r.range_id
             WHERE r.task_id = ?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// The head recorded last for a pull request: that of its newest `pr opened` or `pr pushed`
/// (every registration writes its event, also one that returns to a head watched before, whose
/// watch row is then left as it was). Without any such event, the newest watch's head.
fn recorded_head(w: &StateWriter, repo: &str, pr: i64) -> Result<Option<String>> {
    let by_event: Option<String> = w
        .conn()
        .query_row(
            "SELECT json_extract(payload_json, '$.head_oid') FROM events
             WHERE kind IN ('pr_opened', 'fix_pushed')
               AND lower(json_extract(payload_json, '$.repo')) = lower(?1)
               AND json_extract(payload_json, '$.pr') = ?2
             ORDER BY id DESC LIMIT 1",
            params![repo, pr],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if by_event.is_some() {
        return Ok(by_event);
    }
    Ok(w.conn()
        .query_row(
            "SELECT head_oid FROM pr_watches WHERE lower(repo) = lower(?1) AND pr = ?2
             ORDER BY id DESC LIMIT 1",
            params![repo, pr],
            |r| r.get(0),
        )
        .optional()?)
}

/// What the gate found about a range task's pull request, to be written as a stop.
pub(super) struct GateStop<'a> {
    pub task: &'a str,
    pub repo: &'a str,
    pub pr: i64,
    pub head: &'a str,
    /// The gate's answer: a refusal counts for the pause of two in a row.
    pub allow: bool,
    /// The gate refused (line `gate`), or it reports only edited tests (line `tests`).
    pub refused: bool,
    /// The gate could not read GitHub after its tries (Issue 512): a stop of line `gate` that
    /// says so, and no refusal for the pause.
    pub unavailable: bool,
    pub reason: &'a str,
    pub edited_tests: &'a [String],
}

/// The gate's answer about a range task's pull request as a stop (belt.md section 5, lines 5 and
/// 6), in the gate's transaction: one per pull request and head, and only for the head recorded
/// last for that pull request (a refusal of an older head is no stop: its subject is gone). Then
/// the pause rule: two refusals in a row of different pull requests pause the range. The caller
/// fills after this, so the refusal that pauses puts nothing in. Returns the entry's id.
pub(super) fn gate_stop(w: &StateWriter, s: &GateStop) -> Result<Option<i64>> {
    let Some(range) = range_of(w, s.task)? else {
        return Ok(None);
    };
    if recorded_head(w, s.repo, s.pr)?.as_deref() != Some(s.head) {
        return Ok(None);
    }
    let subject = format!("{}#{}", s.repo.to_lowercase(), s.pr);
    let short = &s.head[..s.head.len().min(12)];
    let tests = s.edited_tests.join(", ");
    let more = json!({"pr": subject, "head": s.head});
    // One question per head (Issue 512): a stop on the content (a refusal, line `gate`, or edited
    // tests, line `tests`) still escalated stands over a read of GitHub that failed later, and a
    // stop on the content replaces the question a failed read asked, marked as makibad's own
    // withdrawal.
    let read_key = format!("gate-read:{subject}@{}", s.head);
    if s.unavailable {
        let standing: Option<i64> = w
            .conn()
            .query_row(
                "SELECT id FROM pending WHERE status = 'escalated'
                   AND json_extract(context_json, '$.range_id') = ?1
                   AND json_extract(context_json, '$.key') IN (?2, ?3)
                 ORDER BY id DESC LIMIT 1",
                params![
                    range,
                    format!("gate:{subject}@{}", s.head),
                    format!("tests:{subject}@{}", s.head)
                ],
                |r| r.get(0),
            )
            .optional()?;
        if standing.is_some() {
            return Ok(standing);
        }
    } else {
        w.conn().execute(
            "UPDATE pending SET status = 'withdrawn',
               context_json = json_set(context_json, '$.withdrawn_by', 'makibad')
             WHERE status = 'escalated'
               AND json_extract(context_json, '$.range_id') = ?1
               AND json_extract(context_json, '$.key') = ?2",
            params![range, read_key],
        )?;
    }
    let made = if s.unavailable {
        Stop {
            range,
            task: Some(s.task),
            line: "gate",
            key: read_key,
            question: format!(
                "The merge gate could not read GitHub for {}#{} at {short}: {} Should the secretary ask the merge again later?",
                s.repo,
                s.pr,
                sentence(s.reason)
            ),
            background: format!(
                "makibad's merge gate tried to read GitHub for the pull request of task {} and every try failed; this says nothing about the pull request: {}",
                s.task, s.reason
            ),
            if_yes: "The secretary asks merge again; an answer that allows withdraws this stop."
                .into(),
            if_no: "It waits; you merge it yourself in the GitHub web UI or close it.".into(),
            recommend: "Yes: ask again once GitHub answers.",
            why: "A failed read is not a judgement of the pull request.",
            recommend_answer: "yes",
            more: json!({"pr": subject, "head": s.head, "unavailable": true}),
        }
    } else if s.refused {
        Stop {
            range,
            task: Some(s.task),
            line: "gate",
            key: format!("gate:{subject}@{}", s.head),
            question: format!(
                "The merge gate refused {}#{} at {short}: {} Should it land as it is?",
                s.repo,
                s.pr,
                sentence(s.reason)
            ),
            background: format!(
                "makibad's merge gate refused the pull request of task {}: {}",
                s.task, s.reason
            ),
            if_yes: "You merge it yourself in the GitHub web UI; the gate will not let the secretary.".into(),
            if_no: "The secretary sends the worker back to fix what the gate named; the new head is judged again.".into(),
            recommend: "No: fix what the gate named.",
            why: "A refusal stands until its reason is gone.",
            recommend_answer: "no",
            more,
        }
    } else {
        Stop {
            range,
            task: Some(s.task),
            line: "tests",
            key: format!("tests:{subject}@{}", s.head),
            question: format!(
                "{}#{} at {short} edits tests that were there before its test commit and are not declared: {tests}. Should it land?",
                s.repo, s.pr
            ),
            background: format!(
                "The merge gate compared the tests at the head of task {}'s pull request with those before its test commit. {}",
                s.task,
                sentence(s.reason)
            ),
            if_yes: "It lands with those edits (under merge_approval_mode acceptance you merge it yourself in the GitHub web UI).".into(),
            if_no: "The secretary sends the worker back to leave those tests as they were.".into(),
            recommend: "None: read the diff of those tests.",
            why: "Only a reading tells a test repaired from a test weakened to pass.",
            recommend_answer: "none",
            more,
        }
    };
    let (id, _) = stop(w, &made)?;
    // The pause is for refusals: an answer that allows (edited tests under human) pauses nothing,
    // and neither does a read of GitHub that failed (Issue 512).
    if !s.allow
        && !s.unavailable
        && let Some((acting, status)) = acting(w, range)?
        && status == "active"
    {
        // The refusals of the range's pull requests since the last one read merged, and since the
        // person last resumed the range (a resume starts the count again), newest first. A refusal
        // of a head that was not its pull request's recorded head then (as `recorded_head` reads
        // it at that event) made no stop, and does not count either.
        let mut q = w.conn().prepare(&format!(
            "{CHAIN} SELECT e.kind, lower(json_extract(e.payload_json, '$.repo')) || '#'
                            || json_extract(e.payload_json, '$.pr')
             FROM events e WHERE e.run_id IN
                   (SELECT r.id FROM runs r WHERE r.range_id IN (SELECT id FROM chain))
               AND (e.kind = 'pr_merged' OR (e.kind = 'acceptance_gate'
                    AND json_extract(e.payload_json, '$.allow') = 0
                    AND coalesce(json_extract(e.payload_json, '$.result'), '') != 'unavailable'
                    AND json_extract(e.payload_json, '$.head') = coalesce(
                      (SELECT json_extract(p.payload_json, '$.head_oid') FROM events p
                       WHERE p.kind IN ('pr_opened', 'fix_pushed') AND p.id < e.id
                         AND lower(json_extract(p.payload_json, '$.repo'))
                             = lower(json_extract(e.payload_json, '$.repo'))
                         AND json_extract(p.payload_json, '$.pr') = json_extract(e.payload_json, '$.pr')
                       ORDER BY p.id DESC LIMIT 1),
                      (SELECT x.head_oid FROM pr_watches x
                       WHERE lower(x.repo) = lower(json_extract(e.payload_json, '$.repo'))
                         AND x.pr = json_extract(e.payload_json, '$.pr')
                       ORDER BY x.id DESC LIMIT 1))))
               AND e.id > coalesce((SELECT max(a.id) FROM events a WHERE a.kind = 'range_resumed'
                     AND json_extract(a.payload_json, '$.range_id') = ?1), 0)
             ORDER BY e.id DESC LIMIT 100"
        ))?;
        let rows: Vec<(String, Option<String>)> = q
            .query_map([acting], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let refused: BTreeSet<String> = rows
            .into_iter()
            .take_while(|(kind, _)| kind != "pr_merged")
            .filter_map(|(_, pr)| pr)
            .collect();
        if refused.len() >= 2 {
            pause(
                w,
                acting,
                "the merge gate refused two pull requests of the range in a row",
            )?;
        }
    }
    Ok(Some(id))
}

/// The gate allowed a range task's pull request at `head`, in the gate's transaction: the `gate`
/// stops still escalated for that pull request and head are withdrawn (what they were refused
/// for has arrived: a CI result, a review), so the run counts again. When the allowance reports
/// no edited test (`no_edited_tests`: the base has since taken those changes), the `tests` stops
/// of that head go too. Marked as `makibad`'s own withdrawal, so that a later refusal of the
/// same head is a stop again.
pub(super) fn gate_allowed(
    w: &StateWriter,
    repo: &str,
    pr: i64,
    head: &str,
    no_edited_tests: bool,
) -> Result<()> {
    w.conn().execute(
        "UPDATE pending SET status = 'withdrawn',
           context_json = json_set(context_json, '$.withdrawn_by', 'makibad')
         WHERE status = 'escalated'
           AND json_extract(context_json, '$.by') = 'makibad'
           AND (json_extract(context_json, '$.stop_line') = 'gate'
                OR (?3 AND json_extract(context_json, '$.stop_line') = 'tests'))
           AND json_extract(context_json, '$.pr') = ?1
           AND json_extract(context_json, '$.head') = ?2",
        params![
            format!("{}#{pr}", repo.to_lowercase()),
            head,
            no_edited_tests
        ],
    )?;
    // A parked run whose last stop went waits for nobody (Issue 283).
    super::park::settle(w)
}

/// `text` as one sentence: its first line, ended by a full stop.
fn sentence(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.ends_with(['.', '!', '?']) {
        line.to_string()
    } else {
        format!("{line}.")
    }
}

/// A head recorded for a task's pull request (`pr opened` or `pushed`), in the verb's
/// transaction. The open stops `makibad` wrote for an older head of that pull request are
/// withdrawn: their subject is gone, and the run counts again. A `pushed` head that reaches
/// `budget_fixes` fixes (and each `budget_fixes` fixes after it) is a stop of the task, line
/// `budget`; a head recorded as a clean merge of the base is no fix (Issue 358). The push itself
/// is recorded as any other (belt.md 2.1).
pub(super) fn head_recorded(
    w: &StateWriter,
    task: &str,
    repo: &str,
    pr: i64,
    head: &str,
) -> Result<()> {
    let Some(range) = range_of(w, task)? else {
        return Ok(());
    };
    w.conn().execute(
        "UPDATE pending SET status = 'withdrawn',
           context_json = json_set(context_json, '$.withdrawn_by', 'makibad')
         WHERE status = 'escalated'
           AND json_extract(context_json, '$.by') = 'makibad'
           AND json_extract(context_json, '$.stop_line') IN ('gate', 'tests')
           AND json_extract(context_json, '$.pr') = ?1
           AND json_extract(context_json, '$.head') != ?2",
        params![format!("{}#{pr}", repo.to_lowercase()), head],
    )?;
    super::park::settle(w)?;
    let budget: i64 = w.conn().query_row(
        "SELECT budget_fixes FROM ranges WHERE id = ?1",
        [range],
        |r| r.get(0),
    )?;
    // A head ever recorded as a clean merge of the base is no fix (Issue 358), also when it is
    // recorded again (the head before it is then itself, and the new record is not marked).
    let merges = "SELECT json_extract(m.payload_json, '$.head_oid') FROM events m
         JOIN runs mr ON mr.id = m.run_id WHERE m.kind = 'fix_pushed' AND mr.task_id = ?1
           AND coalesce(json_extract(m.payload_json, '$.base_merge'), 0) = 1";
    let merge: bool = w.conn().query_row(
        &format!("SELECT ?2 IN ({merges})"),
        params![task, head],
        |r| r.get(0),
    )?;
    if merge {
        return Ok(());
    }
    let fixes = count(
        w,
        &format!(
            "SELECT count(DISTINCT json_extract(e.payload_json, '$.head_oid')) FROM events e
             JOIN runs r ON r.id = e.run_id WHERE e.kind = 'fix_pushed' AND r.task_id = ?1
               AND json_extract(e.payload_json, '$.head_oid') NOT IN ({merges})"
        ),
        [task],
    )?;
    if fixes == 0 || fixes % budget != 0 {
        return Ok(());
    }
    stop(
        w,
        &Stop {
            range,
            task: Some(task),
            line: "budget",
            key: format!("fixes:{task}:{fixes}"),
            question: format!(
                "Task {task} has {fixes} fix(es) on {repo}#{pr} after its first head (budget_fixes {budget}); merges of the base are not counted. Should it go on?"
            ),
            background: format!(
                "The range allows {budget} fix(es) of a task's pull request before it asks; makibad counted the heads recorded with pr pushed that change the pull request's own work, and not a head whose only new commit is a clean merge of the base."
            ),
            if_yes: format!("The worker goes on; the next stop comes after {budget} more fix(es)."),
            if_no: "The secretary closes the task as abandoned, or you take the pull request over."
                .into(),
            recommend: "None: it depends on what the last fixes were.",
            why: "The count says the task is not converging, not why.",
            recommend_answer: "none",
            more: json!({"pr": format!("{}#{pr}", repo.to_lowercase()), "head": head}),
        },
    )?;
    Ok(())
}

/// A launch of `task` failed and its run is `failed`, in that transaction (belt.md 2.4): two
/// launches of a range failed in a row (a verified one between them, or the person's resume,
/// resets the count) pause the range. The failed run holds no slot, so the pause is what keeps a broken launch from spending
/// the budget.
pub(super) fn launch_failed(w: &StateWriter, task: &str) -> Result<()> {
    let Some(range) = range_of(w, task)? else {
        return Ok(());
    };
    let Some((acting, status)) = acting(w, range)? else {
        return Ok(());
    };
    if status != "active" {
        return Ok(());
    }
    let mut q = w.conn().prepare(&format!(
        "{CHAIN} SELECT e.kind FROM events e
         WHERE e.kind IN ('worker_spawn_failed', 'worker_spawn_verified') AND e.run_id IN
               (SELECT r.id FROM runs r WHERE r.range_id IN (SELECT id FROM chain))
               AND e.id > coalesce((SELECT max(a.id) FROM events a WHERE a.kind = 'range_resumed'
                     AND json_extract(a.payload_json, '$.range_id') = ?1), 0)
         ORDER BY e.id DESC LIMIT 2"
    ))?;
    let last: Vec<String> = q
        .query_map([acting], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if last.len() == 2 && last.iter().all(|k| k == "worker_spawn_failed") {
        pause(w, acting, "two launches of the range failed in a row")?;
    }
    Ok(())
}

// ---- the fill ------------------------------------------------------------------------------

/// The newest read of a range, when it may be dispatched from: complete, and started after the
/// range's newest approval or resume. Its event id.
fn fresh_read(w: &StateWriter, id: i64) -> Result<Option<i64>> {
    Ok(w.conn()
        .query_row(
            "SELECT e.id FROM events e WHERE e.kind = 'range_read'
               AND json_extract(e.payload_json, '$.range_id') = ?1
               AND json_extract(e.payload_json, '$.complete') = 1
               AND json_extract(e.payload_json, '$.activation') IS (
                   SELECT max(a.id) FROM events a WHERE a.kind IN ('range_approved','range_resumed')
                     AND json_extract(a.payload_json, '$.range_id') = ?1)
               AND e.id = (SELECT max(n.id) FROM events n WHERE n.kind = 'range_read'
                             AND json_extract(n.payload_json, '$.range_id') = ?1)",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

fn count(w: &StateWriter, sql: &str, p: impl rusqlite::Params) -> Result<i64> {
    Ok(w.conn().query_row(sql, p, |r| r.get(0))?)
}

/// The room left under `max_concurrent_workers`: its count (`dispatch::busy`), and the queued runs
/// whose launch is not taken yet, which will count once it is.
fn global_free(w: &StateWriter, capacity: usize) -> Result<usize> {
    let waiting: i64 = count(
        w,
        &format!(
            "SELECT count(*) FROM runs r WHERE r.status = 'queued' AND NOT {WAITING}
               AND NOT EXISTS (SELECT 1 FROM run_launch l WHERE l.run_id = r.id
                   AND l.stage NOT IN ('verified', 'failed')
                   AND EXISTS (SELECT 1 FROM events e WHERE e.kind = 'run_taken'
                               AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id))"
        ),
        [],
    )?;
    Ok(capacity.saturating_sub(super::dispatch::busy(w)? + waiting as usize))
}

/// The runs of a range (and of the ranges it replaced) that count against its `at_once`: queued,
/// at work or suspended, less those waiting only for the person. A run whose launch failed holds
/// no slot: two failures in a row pause the range instead (`launch_failed`).
fn range_count(w: &StateWriter, id: i64) -> Result<i64> {
    count(
        w,
        &format!(
            "{CHAIN} SELECT count(*) FROM runs r WHERE r.range_id IN (SELECT id FROM chain)
               AND {}",
            counts_sql()
        ),
        [id],
    )
}

/// A run `r` of a range that counts against its `at_once` (`range_count`, and `belt[].counts` of
/// the read surface): queued, at work or suspended, and not waiting (`WAITING`).
fn counts_sql() -> String {
    format!("(r.status IN ('queued', 'in_use', 'suspended') AND NOT {WAITING})")
}

/// The runs of the range and its chain not closed.
fn open_runs(w: &StateWriter, id: i64) -> Result<i64> {
    count(
        w,
        &format!(
            "{CHAIN} SELECT count(*) FROM runs r WHERE r.range_id IN (SELECT id FROM chain)
               AND r.status NOT IN ('completed', 'abandoned')"
        ),
        [id],
    )
}

/// Whether the run `alias` has its pull request read closed unmerged (`pr_closed`, Issue 284)
/// with nothing since: no pull request recorded again (`pr_opened`, `fix_pushed`) and no read of
/// it open again (`pr_reopened`).
pub(crate) fn pr_closed_sql(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM events pc WHERE pc.run_id = {alias}.id AND pc.kind = 'pr_closed'
           AND pc.id > coalesce((SELECT max(po.id) FROM events po WHERE po.run_id = {alias}.id
                                 AND po.kind IN ('pr_opened', 'fix_pushed', 'pr_reopened')), 0))"
    )
}

/// The current pull request of the run `alias` (Issue 345), its `field` (`repo` or `pr`): that of
/// its newest registration (`pr_opened` or `fix_pushed`, which every `makiba pr` writes, also one
/// that returns to a pull request registered before, whose watch row is then left older than
/// another's). Without any such event (rows older than the events), its newest watch's. The one
/// definition every reader of a run's current pull request uses.
pub(crate) fn current_pr_sql(alias: &str, field: &str) -> String {
    debug_assert!(field == "repo" || field == "pr");
    format!(
        "coalesce((SELECT json_extract(cp.payload_json, '$.{field}') FROM events cp
                   WHERE cp.run_id = {alias}.id AND cp.kind IN ('pr_opened', 'fix_pushed')
                   ORDER BY cp.id DESC LIMIT 1),
                  (SELECT cw.{field} FROM pr_watches cw WHERE cw.run_id = {alias}.id
                   ORDER BY cw.id DESC LIMIT 1))"
    )
}

/// Whether the pull request `repo`, `pr` (SQL expressions) is the current pull request of the run
/// `alias`: the same repository (any case, as GitHub reads it) and number.
pub(super) fn is_current_pr_sql(alias: &str, repo: &str, pr: &str) -> String {
    format!(
        "(lower({repo}) = lower({}) AND {pr} = {})",
        current_pr_sql(alias, "repo"),
        current_pr_sql(alias, "pr")
    )
}

/// The tasks open design-only runs wait for (Issue 284), and every other run that may land what
/// they wait for (`ahead::LANDERS`, Issue 488): their landings and closures are read whatever
/// their status.
pub(super) const AWAITED: &str = super::ahead::AWAITED;

/// The task of the range `id` (or its chain) that a design-only candidate may wait for, for the
/// Issue `issue` (belt.md 4.3): the run dispatched for it, not abandoned or failed, with a pull
/// request recorded, no pull request read closed unmerged, and not waiting for the person (4.2: a
/// candidate does not go in on an answer not given yet). `None` when there is no such task.
pub(super) fn ahead_of(c: &Connection, id: i64, issue: i64) -> rusqlite::Result<Option<String>> {
    c.query_row(
        &format!(
            "{CHAIN} SELECT r.task_id FROM runs r
             WHERE r.range_id IN (SELECT id FROM chain)
               AND r.task_id IN (SELECT json_extract(e.payload_json, '$.task_id') FROM events e
                   WHERE e.kind = 'belt_dispatched'
                     AND json_extract(e.payload_json, '$.range_id') IN (SELECT id FROM chain)
                     AND json_extract(e.payload_json, '$.issue') = ?2)
               AND r.status NOT IN ('abandoned', 'failed')
               AND {current} IS NOT NULL
               AND NOT {closed}
               AND NOT EXISTS (SELECT 1 FROM pending q WHERE q.run_id = r.id
                               AND q.status = 'escalated')
             ORDER BY r.id LIMIT 1",
            closed = pr_closed_sql("r"),
            current = current_pr_sql("r", "pr")
        ),
        params![id, issue],
        |r| r.get(0),
    )
    .optional()
}

struct Next {
    issue: i64,
    title: String,
    body: String,
    comments: String,
    /// A design-only candidate: the Issues it waits for and their tasks (belt.md 4.3).
    waits: Option<(Vec<i64>, Vec<String>)>,
    /// A ready candidate: the landings of its dependencies its base must hold (Issue 490).
    landed: Vec<Landed>,
}

/// A dependency's merge seen in the project's clone, which a ready candidate's worktree starts
/// on (Issue 490, belt.md 11.4).
struct Landed {
    issue: i64,
    task: String,
    merge: String,
}

/// A run of a project whose `issue_refs` names a dependency, with its current pull request
/// recorded, and either not read closed unmerged or read merged since.
struct DepRun {
    task: String,
    repo: String,
    pr: i64,
    /// Its current pull request has a `pr_merged`.
    merged: bool,
    /// That merge's commit (none for a merge recorded before Issue 284).
    merge: Option<String>,
    /// The merge commit of a `landed_in_clone` of the run for that merge: by its commit, or, for
    /// a merge recorded without one, by the pull request the clone check wrote (as `clone_checks`).
    seen: Option<String>,
}

/// The runs of the project `slug` that name the Issue `d` (Issue 490): not abandoned or failed,
/// with a current pull request (`current_pr_sql`) not read closed unmerged, or read merged after
/// a closure (a merge read with no read of it open between).
fn dep_runs(c: &Connection, slug: &str, d: i64) -> Result<Vec<DepRun>> {
    let mut q = c.prepare(&format!(
        "SELECT r.task_id, {repo}, {pr}, m.run_id IS NOT NULL, m.merge,
                CASE WHEN m.run_id IS NOT NULL THEN (
                    SELECT json_extract(x.payload_json, '$.merge_commit') FROM events x
                    WHERE x.run_id = r.id AND x.kind = 'landed_in_clone'
                      AND (json_extract(x.payload_json, '$.merge_commit') = m.merge
                           OR (m.merge IS NULL
                               AND lower(json_extract(x.payload_json, '$.repo')) = lower(m.repo)
                               AND json_extract(x.payload_json, '$.pr') = m.pr))
                    ORDER BY x.id LIMIT 1) END
         FROM runs r
         JOIN projects p ON p.id = r.project_id
         LEFT JOIN (SELECT run_id, json_extract(payload_json, '$.repo') AS repo,
                           json_extract(payload_json, '$.pr') AS pr,
                           json_extract(payload_json, '$.merge_commit') AS merge, min(id)
                    FROM events WHERE kind = 'pr_merged'
                    GROUP BY run_id, lower(json_extract(payload_json, '$.repo')), pr) m
              ON m.run_id = r.id AND {merged}
         WHERE p.slug = ?1 AND r.status NOT IN ('abandoned', 'failed')
           AND EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(r.issue_refs)
                                                    THEN r.issue_refs ELSE '[]' END) j
                       WHERE j.value = ?2)
           AND {repo} IS NOT NULL AND {pr} IS NOT NULL
           AND (NOT {closed} OR m.run_id IS NOT NULL)
         ORDER BY r.id",
        repo = current_pr_sql("r", "repo"),
        pr = current_pr_sql("r", "pr"),
        merged = is_current_pr_sql("r", "m.repo", "m.pr"),
        closed = pr_closed_sql("r"),
    ))?;
    let rows = q
        .query_map(params![slug, issue_ref(slug, d)], |r| {
            Ok(DepRun {
                task: r.get(0)?,
                repo: r.get(1)?,
                pr: r.get(2)?,
                merged: r.get(3)?,
                merge: r.get(4)?,
                seen: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

/// The landings a ready candidate with the body `body` is dispatched on (Issue 490), or `None`
/// while one of its `Depends on` Issues is held: a run naming it has a merge recorded that the
/// project's clone has not been seen to hold (`landed_in_clone`), or has no merge recorded while
/// the PR watch still reads its pull request (`prwatch::followed`). A dependency no such run
/// names, or whose run the watch no longer reads, holds nothing.
fn landings(c: &Connection, slug: &str, body: &str) -> Result<Option<Vec<Landed>>> {
    let mut out = Vec::new();
    for d in refs_on_lines(body, "depends on") {
        for r in dep_runs(c, slug, d)? {
            if r.merged {
                let Some(merge) = r.seen else {
                    return Ok(None);
                };
                out.push(Landed {
                    issue: d,
                    task: r.task,
                    merge,
                });
            } else if !super::prwatch::followed(c, Some(&r.task))?.is_empty() {
                return Ok(None);
            }
        }
    }
    Ok(Some(out))
}

/// The clone checks of the held ready candidates of the active ranges (Issue 490): each run a
/// dependency names whose merge is recorded and not yet seen in the project's clone. A merge
/// recorded without its commit is checked by its pull request, read again (`check_clone`).
fn held_checks(w: &StateWriter) -> Result<Vec<CloneCheck>> {
    let rows: Vec<(String, String, String)> = w
        .conn()
        .prepare(
            "SELECT p.slug, coalesce(p.default_branch, 'main'), c.body
             FROM range_candidates c
             JOIN ranges g ON g.id = c.range_id
             JOIN projects p ON p.id = g.project_id
             WHERE g.status = 'active' AND c.admitted = 1 AND c.waits_json IS NULL
               AND NOT EXISTS (SELECT 1 FROM runs t WHERE t.task_id = 'r' || g.id || '-' || c.issue)
             ORDER BY c.range_id, c.position",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut out: Vec<CloneCheck> = Vec::new();
    for (slug, branch, body) in rows {
        for d in refs_on_lines(&body, "depends on") {
            for r in dep_runs(w.conn(), &slug, d)? {
                if !r.merged || r.seen.is_some() {
                    continue;
                }
                if out.iter().any(|c| c.task == r.task && c.merge == r.merge) {
                    continue;
                }
                out.push(CloneCheck {
                    task: r.task,
                    slug: slug.clone(),
                    branch: branch.clone(),
                    merge: r.merge,
                    pr: Some((r.repo, r.pr)),
                });
            }
        }
    }
    Ok(out)
}

/// The first admitted candidate of the range's newest read that the records still allow: not
/// dispatched or skipped by the range or its chain, no run of this task id, no open run of any
/// range or none naming the Issue. Ready candidates go before those admitted for their design
/// only, and one of these goes in only while each Issue it waits for is still a task it may wait
/// for (`ahead_of`, checked again here since the read). A ready candidate goes in only once the
/// project's clone holds the landings of its dependencies (`landings`, Issue 490).
fn next_candidate(w: &StateWriter, id: i64, slug: &str) -> Result<Option<Next>> {
    Ok(pick(w, id, slug)?.0)
}

/// `next_candidate`, and whether a ready candidate was passed over for a landing its base must
/// hold (Issue 490): such a range is not used up.
fn pick(w: &StateWriter, id: i64, slug: &str) -> Result<(Option<Next>, bool)> {
    let mut held = false;
    let sql = format!(
        "{CHAIN} SELECT c.issue, c.title, c.body, c.comments_json, c.waits_json
         FROM range_candidates c
         WHERE c.range_id = ?1 AND c.admitted = 1
           AND NOT EXISTS (SELECT 1 FROM events e WHERE e.kind IN ('belt_dispatched', 'belt_skipped')
                 AND json_extract(e.payload_json, '$.range_id') IN (SELECT id FROM chain)
                 AND json_extract(e.payload_json, '$.issue') = c.issue)
           AND NOT EXISTS (SELECT 1 FROM runs t WHERE t.task_id = 'r' || ?1 || '-' || c.issue)
           AND NOT EXISTS (SELECT 1 FROM runs o
                 JOIN json_each(CASE WHEN json_valid(o.issue_refs) THEN o.issue_refs ELSE '[]' END) j
                 WHERE o.status NOT IN ('completed', 'abandoned') AND j.value = ?2 || '#' || c.issue)
         ORDER BY c.waits_json IS NOT NULL, c.position"
    );
    let rows: Vec<(i64, String, String, String, Option<String>)> = w
        .conn()
        .prepare(&sql)?
        .query_map(params![id, slug], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for (issue, title, body, comments, waits) in rows {
        let mut landed = Vec::new();
        let waits = match waits {
            None => {
                match landings(w.conn(), slug, &body)? {
                    Some(l) => landed = l,
                    None => {
                        held = true;
                        continue;
                    }
                }
                None
            }
            Some(j) => {
                let issues: Vec<i64> = serde_json::from_str(&j).unwrap_or_default();
                let mut tasks = Vec::new();
                for d in &issues {
                    match ahead_of(w.conn(), id, *d)? {
                        Some(t) => tasks.push(t),
                        None => break,
                    }
                }
                if issues.is_empty() || tasks.len() != issues.len() {
                    continue;
                }
                Some((issues, tasks))
            }
        };
        return Ok((
            Some(Next {
                issue,
                title,
                body,
                comments,
                waits,
                landed,
            }),
            held,
        ));
    }
    Ok((None, held))
}

/// The Issues a design-only task waits for, as the brief and the notices name them: `#1` or
/// `#1, #3`.
pub(super) fn named(issues: &[i64]) -> String {
    issues
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The brief of a range task: the range's head, then the Issue's title, body and comments, then,
/// for a task dispatched for its design only, what it waits for and what to do (belt.md 4.3).
fn brief(head: &str, n: &Next) -> String {
    let comments: Vec<Value> = serde_json::from_str(&n.comments).unwrap_or_default();
    let mut b = format!(
        "{}{}",
        head.trim_end(),
        issue_text(n.issue, &n.title, &n.body, &comments)
    );
    if let Some((issues, _)) = &n.waits {
        let (what, verb) = if issues.len() == 1 {
            (named(issues), "has")
        } else {
            (named(issues), "have")
        };
        b.push_str(&format!(
            "\nDesign only: {what} {verb} not landed. Write the design and have it read as the brief head says, then report blocked with the design, and stop: no code and no commit. When {what} {verb} landed you are woken on this conversation, told what landed, and you bring your branch up to the base and build.\n"
        ));
    }
    b
}

/// An Issue as a brief carries it after its head, a range's or a dispatch's own (Issue 317): its
/// title, body and comments, each comment `{author, at, body}`.
pub(super) fn issue_text(issue: i64, title: &str, body: &str, comments: &[Value]) -> String {
    let mut b = format!("\n\nIssue #{issue}: {title}\n\n{}\n", body.trim_end());
    if !comments.is_empty() {
        b.push_str("\nComments:\n");
        for c in comments {
            b.push_str(&format!(
                "- {} ({}):\n{}\n",
                c["author"].as_str().unwrap_or("-"),
                c["at"].as_str().unwrap_or("-"),
                c["body"].as_str().unwrap_or_default().trim_end()
            ));
        }
    }
    b
}

fn dispatch(
    w: &StateWriter,
    agent: &str,
    id: i64,
    slug: &str,
    head: &str,
    n: &Next,
) -> Result<String> {
    let task = format!("r{id}-{}", n.issue);
    let spawn = super::door::random_key()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?[..32]
        .to_string();
    let worktree = super::prepare::Places::from_env(Path::new(""))
        .workplace("code", slug, &task)
        .display()
        .to_string();
    let r = issue_ref(slug, n.issue);
    let title = if n.title.trim().is_empty() {
        format!("#{}", n.issue)
    } else {
        n.title.clone()
    };
    w.upsert_run(&RunUpsert {
        task_id: &task,
        project_slug: slug,
        title: Some(&title),
        status: Some("queued"),
        issue_refs: Some(&[r.as_str()]),
        ..Default::default()
    })?;
    w.conn().execute(
        "UPDATE runs SET range_id = ?2 WHERE task_id = ?1",
        params![task, id],
    )?;
    w.conn().execute(
        "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
         VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'code', ?3, ?4, ?5)",
        params![task, spawn, agent, brief(head, n), worktree],
    )?;
    event(
        w,
        "delegate_sent",
        "makibad",
        Some(&task),
        json!({"task_id": task, "spawn_id": spawn, "kind": "code"}),
    )?;
    event(
        w,
        "belt_dispatched",
        "makibad",
        Some(&task),
        match &n.waits {
            None if !n.landed.is_empty() => {
                json!({"range_id": id, "issue": n.issue, "task_id": task,
                       "design_only": false,
                       "landed": n.landed.iter()
                           .map(|l| json!({"issue": l.issue, "task_id": l.task,
                                           "merge_commit": l.merge}))
                           .collect::<Vec<_>>()})
            }
            None => json!({"range_id": id, "issue": n.issue, "task_id": task,
                           "design_only": false}),
            Some((issues, tasks)) => json!({"range_id": id, "issue": n.issue, "task_id": task,
                                            "design_only": true, "waits_for": issues,
                                            "waits_tasks": tasks}),
        },
    )?;
    Ok(task)
}

/// Put the next tasks of every active range in while there is room (belt.md 3.4), and end a range
/// whose budget is spent and whose tasks are all closed, or that has nothing left (`used_up`;
/// a candidate waiting for a dependency or that could not be read whole keeps it open).
/// Called inside the caller's transaction. Returns the task ids dispatched.
pub fn fill(w: &StateWriter, agent: &str) -> Result<Vec<String>> {
    let capacity = super::dispatch::capacity_from_env();
    let ranges: Vec<(i64, i64, i64, String, String, bool)> = {
        let mut q = w.conn().prepare(&format!(
            "SELECT g.id, g.at_once, g.budget_tasks, g.brief_head, p.slug, {HOURS_PAST}
             FROM ranges g JOIN projects p ON p.id = g.project_id
             WHERE g.status = 'active' ORDER BY g.approved_at, g.id"
        ))?;
        q.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    let mut out = Vec::new();
    for (id, at_once, budget, head, slug, hours_past) in ranges {
        let taken =
            |w: &StateWriter| count(w, "SELECT count(*) FROM runs WHERE range_id = ?1", [id]);
        if fresh_read(w, id)?.is_some() {
            while !hours_past
                && taken(w)? < budget
                && range_count(w, id)? < at_once
                && global_free(w, capacity)? > 0
            {
                let Some(next) = next_candidate(w, id, &slug)? else {
                    break;
                };
                out.push(dispatch(w, agent, id, &slug, &head, &next)?);
            }
            // A budget reached while it keeps a candidate out is a stop of the range (belt.md
            // section 5, line 4): once per budget. A range that finishes what it planned asks nothing.
            let spent = if taken(w)? >= budget {
                Some("budget_tasks")
            } else if hours_past {
                Some("budget_hours")
            } else {
                None
            };
            if let Some(which) = spent
                && let Some(next) = next_candidate(w, id, &slug)?
            {
                stop(
                    w,
                    &Stop {
                        range: id,
                        task: None,
                        line: "budget",
                        key: format!("{which}:{id}"),
                        question: format!(
                            "Range {id} of {slug} reached its {which}, and #{} (at least) is left inside it. Should a new range be proposed for what is left?",
                            next.issue
                        ),
                        background: format!(
                            "The range's {which} is spent: makibad puts nothing new in, and the range ends when its open tasks close."
                        ),
                        if_yes: "The secretary drafts a new range for what is left (one that replaces this one while it has not ended), for you to approve.".into(),
                        if_no: "The Issues left stay out until you say otherwise.".into(),
                        recommend: "None: the budget was your bound.",
                        why: "Only you widen a range.",
                        recommend_answer: "none",
                        more: json!({}),
                    },
                )?;
            }
        }
        if open_runs(w, id)? > 0 {
            continue;
        }
        if taken(w)? >= budget {
            end_range(w, id, "budget_tasks", "makibad")?;
        } else if hours_past {
            end_range(w, id, "budget_hours", "makibad")?;
        } else if fresh_read(w, id)?.is_some()
            && matches!(pick(w, id, &slug)?, (None, false))
            && count(
                w,
                "SELECT count(*) FROM range_candidates WHERE range_id = ?1
                   AND (left_out_why LIKE 'waits_for%' OR left_out_why LIKE 'unreadable%'
                        OR left_out_why LIKE 'dependency_unreadable%')",
                [id],
            )? == 0
        {
            end_range(w, id, "used_up", "makibad")?;
        }
    }
    Ok(out)
}

// ---- the read --------------------------------------------------------------------------------

/// The condition of a range: words joined by AND (belt.md 2.1).
#[derive(Clone, Debug, Default)]
pub struct Takes(Vec<Cond>);

#[derive(Clone, Debug)]
enum Cond {
    Label(String),
    NoLabel(String),
    Milestone(String),
    ClauseAtMost(u32),
}

impl Takes {
    pub fn parse(text: &str) -> std::result::Result<Takes, String> {
        let mut out = Vec::new();
        for word in text.split_whitespace() {
            let (k, v) = word
                .split_once(':')
                .filter(|(_, v)| !v.is_empty())
                .ok_or_else(|| format!("takes: {word:?} is not <key>:<value>"))?;
            out.push(match k {
                "label" => Cond::Label(v.to_lowercase()),
                "no-label" => Cond::NoLabel(v.to_lowercase()),
                "milestone" => Cond::Milestone(v.to_string()),
                "clause" => Cond::ClauseAtMost(
                    v.strip_prefix("<=G")
                        .and_then(|n| n.parse().ok())
                        .ok_or_else(|| format!("takes: {word:?}: the form is clause:<=G<n>"))?,
                ),
                _ => {
                    return Err(format!(
                        "takes: {k:?} is not label, no-label, milestone or clause"
                    ));
                }
            });
        }
        Ok(Takes(out))
    }

    /// An empty condition matches nothing: such a range is its list alone.
    fn matches(&self, issue: &Value) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let labels = crate::work_discovery::refs::label_names(issue);
        self.0.iter().all(|c| match c {
            Cond::Label(l) => labels.contains(l),
            Cond::NoLabel(l) => !labels.contains(l),
            Cond::Milestone(m) => issue["milestone"]["title"].as_str() == Some(m.as_str()),
            Cond::ClauseAtMost(n) => {
                crate::work_discovery::goals::named_clause(issue["body"].as_str().unwrap_or(""))
                    .is_some_and(|c| c <= *n)
            }
        })
    }
}

/// The `#<n>` numbers of the body's lines that start with `prefix` (any case).
fn refs_on_lines(body: &str, prefix: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.to_lowercase().starts_with(prefix) {
            continue;
        }
        for part in line.split('#').skip(1) {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(n) = digits.parse() {
                out.push(n);
            }
        }
    }
    out
}

/// What one read is for.
struct Spec {
    id: i64,
    slug: String,
    origin: Option<String>,
    listed: Vec<i64>,
    takes: Takes,
    /// `follow_ups` is `inside`: the dependencies of a follow-up are read too.
    follow_inside: bool,
}

/// What a read brings back from GitHub. `views`: an Issue read by itself (a listed one not open,
/// a dependency not open), `None` when it could not be read.
struct Fetched {
    repo: String,
    open: Vec<Value>,
    prs: Vec<Value>,
    views: BTreeMap<i64, Option<Value>>,
}

/// A read, or why it failed and whether the project cannot be read at all.
type ReadResult = std::result::Result<Fetched, (String, bool)>;

struct Outcome {
    range: Option<(Spec, ReadResult)>,
    /// A dependency's merge found in the project's clone after a fetch of its default branch.
    clones: Vec<InClone>,
}

/// A dependency's merge seen in the project's clone.
#[derive(Debug, PartialEq)]
struct InClone {
    task: String,
    merge: String,
    branch: String,
    /// The branch's head there.
    head: String,
    /// The pull request the merge is of, `repo` and number (its current one when checked).
    pr: Option<(String, i64)>,
}

/// `$MAKIBA_LANDING_RETRY_MS`: how often the clone checks run again (default `RETRY_MS`).
pub const LANDING_RETRY_ENV: &str = "MAKIBA_LANDING_RETRY_MS";

/// A dependency of a design-only run waiting for its landing (belt.md 4.3) whose task closed
/// `completed` and that is not yet seen in the project's clone: (task, project, default branch,
/// its merge commit when its pull request was read merged).
struct CloneCheck {
    task: String,
    slug: String,
    branch: String,
    /// From its `pr_merged`; a merge recorded before Issue 284 has none, and is read again.
    merge: Option<String>,
    /// Its newest pull request, `repo` and number.
    pr: Option<(String, i64)>,
}

fn clone_checks(w: &StateWriter) -> Result<Vec<CloneCheck>> {
    // The merge of its current pull request (`current_pr_sql`), if read: a merge of another, even
    // one of another repository with the same number, is not its landing. A merge recorded
    // without its commit is seen in the clone only by a `landed_in_clone` of the same pull request.
    let mut q = w.conn().prepare(&format!(
        "SELECT DISTINCT d.task_id, p.slug, coalesce(p.default_branch, 'main'), m.merge,
                {repo}, {pr}
         FROM ({landers}) l
         JOIN runs d ON d.id = l.run
         JOIN projects p ON p.id = d.project_id
         LEFT JOIN (SELECT run_id, json_extract(payload_json, '$.repo') AS repo,
                           json_extract(payload_json, '$.pr') AS pr,
                           json_extract(payload_json, '$.merge_commit') AS merge, min(id)
                    FROM events WHERE kind = 'pr_merged'
                    GROUP BY run_id, lower(json_extract(payload_json, '$.repo')), pr) m
              ON m.run_id = d.id AND {merged}
         WHERE d.status = 'completed'
           AND NOT EXISTS (SELECT 1 FROM events x WHERE x.run_id = d.id
                           AND x.kind = 'landed_in_clone' AND m.run_id IS NOT NULL
                           AND (json_extract(x.payload_json, '$.merge_commit') = m.merge
                                OR (m.merge IS NULL
                                    AND lower(json_extract(x.payload_json, '$.repo'))
                                        = lower(m.repo)
                                    AND json_extract(x.payload_json, '$.pr') = m.pr)))
         ORDER BY d.id",
        landers = super::ahead::LANDERS,
        repo = current_pr_sql("d", "repo"),
        pr = current_pr_sql("d", "pr"),
        merged = is_current_pr_sql("d", "m.repo", "m.pr"),
    ))?;
    Ok(q.query_map([], |r| {
        let repo: Option<String> = r.get(4)?;
        let pr: Option<i64> = r.get(5)?;
        Ok(CloneCheck {
            task: r.get(0)?,
            slug: r.get(1)?,
            branch: r.get(2)?,
            merge: r.get(3)?,
            pr: repo.zip(pr),
        })
    })?
    .collect::<std::result::Result<_, _>>()?)
}

/// One clone check on the read's thread: the merge commit (recorded, or, for a merge recorded
/// without one, read again with `gh pr view`), then `in_clone`. The task, merge commit, branch
/// and the branch's head when the merge is there.
fn check_clone(gh: &str, repos_dir: &Path, c: &CloneCheck, cancel: &AtomicBool) -> Option<InClone> {
    let read_again = || -> Option<String> {
        let (repo, pr) = c.pr.as_ref()?;
        let v = gh_json(
            gh,
            &[
                "pr",
                "view",
                &pr.to_string(),
                "--repo",
                &format!("github.com/{repo}"),
                "--json",
                "state,mergeCommit",
            ],
            cancel,
        )
        .ok()?;
        (v["state"].as_str() == Some("MERGED"))
            .then(|| v["mergeCommit"]["oid"].as_str().map(String::from))
            .flatten()
    };
    let merge = c.merge.clone().or_else(read_again)?;
    let head = in_clone(&repos_dir.join(&c.slug), &c.branch, &merge, cancel)?;
    Some(InClone {
        task: c.task.clone(),
        merge,
        branch: c.branch.clone(),
        head,
        pr: c.pr.clone(),
    })
}

/// Fetch `branch` into the clone `clone` through its `origin` (what a worker's `git fetch origin`
/// then reads), and its head there when `merge` is in it; `None` when the fetch fails or the
/// merge is not there yet.
pub(super) fn in_clone(
    clone: &Path,
    branch: &str,
    merge: &str,
    cancel: &AtomicBool,
) -> Option<String> {
    let dir = clone.display().to_string();
    let tracking = format!("refs/remotes/origin/{branch}");
    let git = |args: &[&str]| {
        let mut all = vec!["-C", dir.as_str(), "-c", super::run_repo::HOOKS_OFF];
        all.extend_from_slice(args);
        run_gh("git", &all, cancel).ok()
    };
    git(&[
        "fetch",
        "--no-tags",
        "--quiet",
        "origin",
        &format!("+refs/heads/{branch}:{tracking}"),
    ])?;
    git(&["merge-base", "--is-ancestor", merge, &tracking])?;
    Some(
        git(&["rev-parse", "--verify", &tracking])?
            .trim()
            .to_string(),
    )
}

struct Job {
    through: i64,
    activation: Option<i64>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<Outcome>,
    done: Option<Outcome>,
}

impl Job {
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The belt's pass of `makibad`: the reads, the clone checks and the fill each tick.
pub struct Belt {
    gh: String,
    repos_dir: PathBuf,
    interval_ms: i64,
    agent: String,
    job: Option<Job>,
    last_landing: Option<Instant>,
    landing_retry: Duration,
}

impl Belt {
    pub fn from_env(repos_dir: PathBuf) -> Belt {
        Belt {
            gh: super::gh::program(),
            repos_dir,
            interval_ms: std::env::var(READ_INTERVAL_ENV)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_INTERVAL_MS),
            agent: std::env::var(super::door::LAUNCH_AGENT_ENV)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "claude_code".into()),
            job: None,
            last_landing: None,
            landing_retry: Duration::from_millis(
                std::env::var(LANDING_RETRY_ENV)
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(RETRY_MS as u64),
            ),
        }
    }

    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        if let Some(job) = self.job.as_mut() {
            // Finished only once its outcome is received or its thread has dropped the sender
            // (cancelled or crashed: nothing to write, the read stays due). An outcome is kept
            // until its transaction commits.
            let mut finished = job.done.is_some();
            if !finished {
                match job.rx.try_recv() {
                    Ok(o) => {
                        job.done = Some(o);
                        finished = true;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => finished = true,
                }
            }
            if finished {
                if let Some(o) = &job.done {
                    let (through, activation, agent) = (job.through, job.activation, &self.agent);
                    w.transaction(|w| record(w, through, activation, o, agent))?;
                }
                if let Some(mut job) = self.job.take() {
                    job.stop();
                }
            }
        }
        // The catch-all: a design-only wait settled by what changed elsewhere (a dependency
        // abandoned or failed), a slot freed where no fill ran (a restart, a launch settled).
        w.transaction(|w| {
            super::ahead::settle(w)?;
            fill(w, &self.agent)
        })?;
        if self.job.is_some() {
            return Ok(());
        }
        let due = due(w, self.interval_ms)?;
        let checks = if self
            .last_landing
            .is_none_or(|t| t.elapsed() >= self.landing_retry)
        {
            // Only a dependency whose merge is recorded (by the PR watch, Issue 388): the belt
            // reads no pull request's state, and reads one again only for the commit of a merge
            // recorded without it.
            let mut checks = Vec::new();
            for c in clone_checks(w)? {
                if c.merge.is_some() || merge_recorded(w, &c.task)? {
                    checks.push(c);
                }
            }
            // And the merges a held ready candidate's base must hold (Issue 490).
            for c in held_checks(w)? {
                if !checks
                    .iter()
                    .any(|k| k.task == c.task && k.merge == c.merge)
                {
                    checks.push(c);
                }
            }
            checks
        } else {
            Vec::new()
        };
        if due.is_none() && checks.is_empty() {
            return Ok(());
        }
        if !checks.is_empty() {
            self.last_landing = Some(Instant::now());
        }
        let (through, activation) = due.as_ref().map_or((0, None), |(_, t, a)| (*t, *a));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let handle = {
            let (gh, repos_dir, cancel) =
                (self.gh.clone(), self.repos_dir.clone(), Arc::clone(&cancel));
            let spec = due.map(|(s, _, _)| s);
            std::thread::spawn(move || {
                let range = spec.map(|s| {
                    let f = read(&gh, &s, &repos_dir, &cancel);
                    (s, f)
                });
                // A dependency of a design-only run has landed for its worker only once its merge
                // is in the project's clone, the worker's `origin` (belt.md 4.3).
                let clones = checks
                    .iter()
                    .filter_map(|c| check_clone(&gh, &repos_dir, c, &cancel))
                    .collect();
                if !cancel.load(Ordering::SeqCst) {
                    let _ = tx.send(Outcome { range, clones });
                }
            })
        };
        self.job = Some(Job {
            through,
            activation,
            cancel,
            handle: Some(handle),
            rx,
            done: None,
        });
        Ok(())
    }
}

impl Drop for Belt {
    fn drop(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.stop();
        }
    }
}

/// The range whose read is due, oldest first, with the newest trigger event id and the range's
/// activation. Due: no read since its proposal, approval, resume or the close of one of its tasks;
/// a newest read that failed, after `RETRY_MS`; an active range's newest read older than the
/// interval.
fn due(w: &StateWriter, interval_ms: i64) -> Result<Option<(Spec, i64, Option<i64>)>> {
    type Due = (i64, String, String, Option<String>, String, String, String);
    let ranges: Vec<Due> = {
        let mut q = w.conn().prepare(
            "SELECT g.id, g.status, p.slug, p.origin_url, g.issues_json, g.takes, g.follow_ups
             FROM ranges g
             JOIN projects p ON p.id = g.project_id
             WHERE g.status IN ('proposed', 'active') ORDER BY g.id",
        )?;
        q.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    for (id, status, slug, origin, issues, takes, follow_ups) in ranges {
        let trigger: i64 = count(
            w,
            &format!(
                "{CHAIN} SELECT coalesce(max(e.id), 0) FROM events e WHERE
                   (e.kind IN ('range_proposed', 'range_approved', 'range_resumed')
                    AND json_extract(e.payload_json, '$.range_id') = ?1)
                   OR (e.kind IN ('run_closed', 'pr_opened') AND e.run_id IN
                       (SELECT r.id FROM runs r WHERE r.range_id IN (SELECT id FROM chain)))"
            ),
            [id],
        )?;
        let activation: Option<i64> = w.conn().query_row(
            "SELECT max(id) FROM events WHERE kind IN ('range_approved', 'range_resumed')
               AND json_extract(payload_json, '$.range_id') = ?1",
            [id],
            |r| r.get(0),
        )?;
        let last: Option<(i64, bool, f64)> = w
            .conn()
            .query_row(
                "SELECT coalesce(json_extract(payload_json, '$.through'), 0),
                        coalesce(json_extract(payload_json, '$.complete'), 0),
                        (julianday('now') - julianday(occurred_at)) * 86400000.0
                 FROM events WHERE kind = 'range_read'
                   AND json_extract(payload_json, '$.range_id') = ?1
                 ORDER BY id DESC LIMIT 1",
                [id],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1, r.get(2)?)),
            )
            .optional()?;
        let is_due = match last {
            None => true,
            Some((through, complete, age)) => {
                trigger > through
                    || (!complete && age >= RETRY_MS as f64)
                    || (status == "active" && age >= interval_ms as f64)
            }
        };
        if is_due {
            let listed: Vec<i64> = serde_json::from_str(&issues).unwrap_or_default();
            let spec = Spec {
                id,
                slug,
                origin,
                listed,
                takes: Takes::parse(&takes).unwrap_or_default(),
                follow_inside: follow_ups == "inside",
            };
            return Ok(Some((spec, trigger, activation)));
        }
    }
    Ok(None)
}

/// Whether the run of `task` has a `pr_merged` of its current pull request (`current_pr_sql`).
fn merge_recorded(w: &StateWriter, task: &str) -> Result<bool> {
    Ok(w.conn().query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM runs r JOIN events e ON e.run_id = r.id
               WHERE r.task_id = ?1 AND e.kind = 'pr_merged' AND {})",
            is_current_pr_sql(
                "r",
                "json_extract(e.payload_json, '$.repo')",
                "json_extract(e.payload_json, '$.pr')"
            )
        ),
        [task],
        |r| r.get(0),
    )?)
}

fn gh_json(gh: &str, args: &[&str], cancel: &AtomicBool) -> std::result::Result<Value, String> {
    let text = run_gh(gh, args, cancel).map_err(|e| match e {
        GhError::Missing => format!("{gh} not found"),
        GhError::Cancelled => "cancelled".into(),
        GhError::Failed(f) => f.message,
    })?;
    serde_json::from_str(&text).map_err(|_| format!("gh {} {} wrote no JSON", args[0], args[1]))
}

/// One read, on the job's thread. The error's flag: the project has no GitHub repository (the
/// range cannot be read at all, `scan_error`).
fn read(gh: &str, s: &Spec, repos_dir: &Path, cancel: &AtomicBool) -> ReadResult {
    let repo = super::triage::repo_of(&s.slug, s.origin.as_deref(), repos_dir)
        .ok_or_else(|| (format!("project {} has no GitHub repository", s.slug), true))?;
    let host = format!("github.com/{repo}");
    let list = |what: &str,
                limit: usize,
                fields: &str|
     -> std::result::Result<Vec<Value>, (String, bool)> {
        let limit_s = limit.to_string();
        let v = gh_json(
            gh,
            &[
                what, "list", "--repo", &host, "--state", "open", "--limit", &limit_s, "--json",
                fields,
            ],
            cancel,
        )
        .map_err(|e| (e, false))?;
        let rows = v
            .as_array()
            .cloned()
            .ok_or_else(|| (format!("gh {what} list returned no array"), false))?;
        if rows.len() >= limit {
            return Err((
                format!("{limit} or more open {what}s: the read is not known to be whole"),
                false,
            ));
        }
        Ok(rows)
    };
    let open = list(
        "issue",
        ISSUE_LIMIT,
        "number,title,body,labels,milestone,comments,createdAt",
    )?;
    let prs = list("pr", PR_LIMIT, "number,closingIssuesReferences")?;
    let open_numbers: BTreeSet<i64> = open.iter().filter_map(|i| i["number"].as_i64()).collect();
    let mut need: BTreeSet<i64> = s
        .listed
        .iter()
        .copied()
        .filter(|n| !open_numbers.contains(n))
        .collect();
    for i in &open {
        let n = i["number"].as_i64().unwrap_or(0);
        let follow_up = s.follow_inside
            && !refs_on_lines(i["body"].as_str().unwrap_or(""), "follow-up of").is_empty();
        if s.listed.contains(&n) || s.takes.matches(i) || follow_up {
            need.extend(
                refs_on_lines(i["body"].as_str().unwrap_or(""), "depends on")
                    .into_iter()
                    .filter(|d| !open_numbers.contains(d)),
            );
        }
    }
    let mut views = BTreeMap::new();
    for n in need {
        let v = gh_json(
            gh,
            &[
                "issue",
                "view",
                &n.to_string(),
                "--repo",
                &host,
                "--json",
                "number,title,state",
            ],
            cancel,
        )
        .ok();
        if cancel.load(Ordering::SeqCst) {
            return Err(("cancelled".into(), false));
        }
        views.insert(n, v);
    }
    Ok(Fetched {
        repo,
        open,
        prs,
        views,
    })
}

/// One candidate as stored.
struct Candidate {
    issue: i64,
    title: String,
    body: String,
    comments: Vec<Value>,
    why: Option<String>,
    clause: u32,
    /// Admitted for its design only: the Issues it waits for (belt.md 4.3).
    waits: Option<Vec<i64>>,
}

/// The Issues of the repository `repo` (`owner/name`) named on the body's lines that start with
/// `prefix` (any case): each word `#<n>` or `<repo>#<n>` (any case), a trailing `.,;:)` aside.
/// An Issue of another repository (`other/name#<n>`) is not one of them.
fn local_refs_on_lines(body: &str, prefix: &str, repo: &str) -> Vec<i64> {
    let repo = repo.to_ascii_lowercase();
    body.lines()
        .map(str::trim)
        .filter(|l| l.to_lowercase().starts_with(prefix))
        .flat_map(|l| {
            l[prefix.len()..]
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(|word| {
                    let word = word.trim_end_matches(['.', ',', ';', ':', ')']);
                    let (owner, n) = word.split_once('#')?;
                    let local = owner.is_empty() || owner.to_ascii_lowercase() == repo;
                    if !local || n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
                        return None;
                    }
                    n.parse().ok()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A time as GitHub and `makibad` write it (`...:SS[.fff]Z`), with its fractions of a second
/// made nine digits, so that two such times compare as times.
fn as_time(at: &str) -> String {
    let at = at.trim().trim_end_matches('Z');
    let (whole, frac) = at.split_once('.').unwrap_or((at, ""));
    let digits: String = frac.chars().filter(char::is_ascii_digit).take(9).collect();
    format!("{whole}.{digits:0<9}")
}

/// The values of the body's lines that start with `prefix` (any case): what follows it, split
/// at commas and spaces, lowercased.
fn values_on_lines(body: &str, prefix: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|l| l.to_lowercase().starts_with(prefix))
        .flat_map(|l| {
            l[prefix.len()..]
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|v| !v.is_empty())
                .map(|v| v.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether a worker of the project may reach `host`: it is listed, or a listed `*.name` covers
/// it (project.rs, Issue 244).
fn host_listed(listed: &[String], host: &str) -> bool {
    listed.iter().any(|h| {
        h == host
            || h.strip_prefix("*.")
                .is_some_and(|name| host.ends_with(&format!(".{name}")))
    })
}

/// Apply the range to a read: every Issue the list names or the condition matches, and with
/// `follow_ups` `inside` every follow-up of these, admitted or left out with why (belt.md 2.1), in
/// the read's order (goal clause, then number).
fn admit(
    w: &StateWriter,
    s: &Spec,
    f: &Fetched,
    approved_at: Option<&str>,
) -> Result<Vec<Candidate>> {
    let open: BTreeMap<i64, &Value> = f
        .open
        .iter()
        .filter_map(|i| Some((i["number"].as_i64()?, i)))
        .collect();
    let closing: BTreeMap<i64, i64> = f
        .prs
        .iter()
        .flat_map(|p| {
            let pr = p["number"].as_i64().unwrap_or(0);
            p["closingIssuesReferences"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(move |i| Some((i["number"].as_i64()?, pr)))
        })
        .collect();
    let too_many_refs = f.prs.iter().any(|p| {
        p["closingIssuesReferences"]
            .as_array()
            .is_some_and(|a| a.len() >= NESTED_LIMIT)
    });
    // A range with no row (none in practice) is read as a range of `ask` and no design ahead.
    let (inside, design_ahead): (bool, bool) = w
        .conn()
        .query_row(
            "SELECT follow_ups = 'inside', design_ahead = 1 FROM ranges WHERE id = ?1",
            [s.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((false, false));
    let mut out = Vec::new();
    let mut numbers: BTreeSet<i64> = s.listed.iter().copied().collect();
    numbers.extend(
        open.iter()
            .filter(|(_, i)| s.takes.matches(i))
            .map(|(n, _)| *n),
    );
    // A follow-up of `set`: created after the approval, with a line `Follow-up of #<n>` naming an
    // Issue of `set` or one the range dispatched (belt.md 2.1).
    let follow_of = |i: &Value, set: &BTreeSet<i64>| -> bool {
        let after = approved_at.is_some_and(|at| {
            i["createdAt"]
                .as_str()
                .is_some_and(|c| as_time(c) > as_time(at))
        });
        after
            && local_refs_on_lines(i["body"].as_str().unwrap_or(""), "follow-up of", &f.repo)
                .iter()
                .any(|f| set.contains(f) || dispatched_in(w, s.id, *f).unwrap_or(false))
    };
    // With `inside`, a follow-up the range takes only as one stays out (asked for) when it needs a
    // host the project does not list, or names the stop line it would hit.
    let hosts = super::project::hosts_of(w.conn(), &s.slug)?;
    let kept_out = |body: &str| -> Option<String> {
        if let Some(h) = values_on_lines(body, "needs host:")
            .into_iter()
            .find(|h| !host_listed(&hosts, h))
        {
            return Some(format!("follow_up (ask: needs host {h})"));
        }
        values_on_lines(body, "stop line:")
            .first()
            .map(|line| format!("follow_up (ask: stop line {line})"))
    };
    // With `inside`, the follow-ups of the range's Issues are its Issues too, and theirs in turn;
    // one kept out is listed, and its own follow-ups are not taken through it.
    let mut followed = BTreeSet::new();
    let mut seeds = numbers.clone();
    let mut more_to_find = inside;
    while more_to_find {
        let more: Vec<(i64, &Value)> = open
            .iter()
            .filter(|(n, i)| !numbers.contains(n) && follow_of(i, &seeds))
            .map(|(n, i)| (*n, *i))
            .collect();
        more_to_find = !more.is_empty();
        for (n, i) in more {
            numbers.insert(n);
            followed.insert(n);
            if kept_out(i["body"].as_str().unwrap_or("")).is_none() {
                seeds.insert(n);
            }
        }
    }
    // The range's Issues in this read: with `ask`, a follow-up of any of them is asked for.
    let members = numbers.clone();
    for n in numbers {
        let listed = s.listed.contains(&n);
        let Some(i) = open.get(&n) else {
            let view = f.views.get(&n).cloned().flatten();
            let why = match view.as_ref().and_then(|v| v["state"].as_str()) {
                Some("CLOSED") => "closed",
                Some("OPEN") => "unreadable (open, but not in the list of open Issues)",
                _ => "unreadable",
            };
            out.push(Candidate {
                issue: n,
                title: view
                    .as_ref()
                    .and_then(|v| v["title"].as_str())
                    .unwrap_or("")
                    .to_string(),
                body: String::new(),
                comments: Vec::new(),
                why: Some(why.into()),
                clause: u32::MAX,
                waits: None,
            });
            continue;
        };
        let body = i["body"].as_str().unwrap_or("").to_string();
        let labels = i["labels"].as_array().map_or(0, Vec::len);
        let comments: Vec<Value> = i["comments"].as_array().cloned().unwrap_or_default();
        let mut why: Option<String> = None;
        if labels >= NESTED_LIMIT || comments.len() >= NESTED_LIMIT || too_many_refs {
            why = Some("unreadable (too large to read whole)".into());
        }
        if why.is_none() && !listed && !inside && follow_of(i, &members) {
            // With `ask`, a follow-up found on the way is not in the range (belt.md Q3).
            why = Some("follow_up (ask)".into());
        }
        if why.is_none() && followed.contains(&n) {
            why = kept_out(&body);
        }
        if why.is_none()
            && let Some(line) = crate::work_discovery::refs::dependency_line_with_no_number(&body)
        {
            // A dependency in words is not read as none (Issue 299): the line must name `#<n>`.
            // The range's own records come first, so a skipped one does not hold the range open.
            why = Some(match records_why(w, s, n)? {
                Some(r) => r,
                None => format!("dependency_unreadable ({line})"),
            });
        }
        let mut waits = None;
        if why.is_none() {
            let deps: Vec<i64> = refs_on_lines(&body, "depends on");
            let waiting: Vec<i64> = deps
                .into_iter()
                .filter(|d| {
                    open.contains_key(d)
                        || f.views
                            .get(d)
                            .cloned()
                            .flatten()
                            .is_none_or(|v| v["state"].as_str() != Some("CLOSED"))
                })
                .collect();
            // With `design_ahead`, a candidate whose every open dependency is a task of the range
            // it may wait for goes in for its design only (belt.md 4.3).
            let mut ahead = design_ahead && !waiting.is_empty();
            for d in &waiting {
                if ahead && ahead_of(w.conn(), s.id, *d)?.is_none() {
                    ahead = false;
                }
            }
            if ahead {
                waits = Some(waiting);
            } else if !waiting.is_empty() {
                why = Some(format!(
                    "waits_for {}",
                    waiting
                        .iter()
                        .map(|d| format!("#{d}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
        }
        if why.is_none()
            && let Some(pr) = closing.get(&n)
        {
            why = Some(format!("open_pr #{pr}"));
        }
        if why.is_none() {
            why = records_why(w, s, n)?;
        }
        out.push(Candidate {
            issue: n,
            title: i["title"].as_str().unwrap_or("").to_string(),
            body,
            comments: comments
                .iter()
                .map(|c| {
                    json!({"author": c["author"]["login"].as_str().unwrap_or("-"),
                           "at": c["createdAt"], "body": c["body"]})
                })
                .collect(),
            waits: waits.filter(|_| why.is_none()),
            why,
            clause: crate::work_discovery::goals::named_clause(i["body"].as_str().unwrap_or(""))
                .unwrap_or(u32::MAX),
        });
    }
    out.sort_by_key(|c| (c.clause, c.issue));
    Ok(out)
}

fn dispatched_in(w: &StateWriter, id: i64, issue: i64) -> Result<bool> {
    Ok(count(
        w,
        &format!(
            "{CHAIN} SELECT count(*) FROM events WHERE kind = 'belt_dispatched'
               AND json_extract(payload_json, '$.range_id') IN (SELECT id FROM chain)
               AND json_extract(payload_json, '$.issue') = ?2"
        ),
        params![id, issue],
    )? > 0)
}

/// Why the records leave an Issue out: a task of the range, a skip, an open run naming it.
fn records_why(w: &StateWriter, s: &Spec, n: i64) -> Result<Option<String>> {
    let c = w.conn();
    let mine: Option<String> = c
        .query_row(
            &format!(
                "{CHAIN} SELECT json_extract(payload_json, '$.task_id') FROM events
                 WHERE kind = 'belt_dispatched' AND json_extract(payload_json, '$.range_id') IN
                   (SELECT id FROM chain) AND json_extract(payload_json, '$.issue') = ?2
                 ORDER BY id LIMIT 1"
            ),
            params![s.id, n],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(t) = mine {
        return Ok(Some(format!("dispatched {t}")));
    }
    if dispatched_or_skipped(w, s.id, n, "belt_skipped")? {
        return Ok(Some("skipped".into()));
    }
    let other: Option<String> = c
        .query_row(
            "SELECT o.task_id FROM runs o
             JOIN json_each(CASE WHEN json_valid(o.issue_refs) THEN o.issue_refs ELSE '[]' END) j
             WHERE o.status NOT IN ('completed', 'abandoned') AND j.value = ?1 LIMIT 1",
            [issue_ref(&s.slug, n)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(other.map(|t| format!("run {t}")))
}

fn dispatched_or_skipped(w: &StateWriter, id: i64, issue: i64, kind: &str) -> Result<bool> {
    Ok(count(
        w,
        &format!(
            "{CHAIN} SELECT count(*) FROM events WHERE kind = ?3
               AND json_extract(payload_json, '$.range_id') IN (SELECT id FROM chain)
               AND json_extract(payload_json, '$.issue') = ?2"
        ),
        params![id, issue, kind],
    )? > 0)
}

/// The loop's side of a read: the candidates and `range_read` (or a failed `range_read`), the
/// merges found in the clone, then the fill, in the caller's transaction.
fn record(
    w: &StateWriter,
    through: i64,
    activation: Option<i64>,
    o: &Outcome,
    agent: &str,
) -> Result<()> {
    if let Some((s, fetched)) = &o.range {
        let row: Option<(String, Option<String>)> = w
            .conn()
            .query_row(
                "SELECT status, approved_at FROM ranges WHERE id = ?1",
                [s.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // A range that ended meanwhile is not read any more.
        if let Some((status, approved_at)) = row.filter(|(st, _)| st != "ended") {
            match fetched {
                Err((error, fatal)) => {
                    eprintln!("makibad: range {}: the read failed: {error}", s.id);
                    event(
                        w,
                        "range_read",
                        "makibad",
                        None,
                        json!({"range_id": s.id, "complete": false, "error": error,
                               "through": through, "activation": activation,
                               "candidate_refs": ""}),
                    )?;
                    if *fatal && status == "active" {
                        end_range(w, s.id, "scan_error", "makibad")?;
                    }
                }
                Ok(f) => {
                    let cands = admit(w, s, f, approved_at.as_deref())?;
                    let refs: Vec<String> = cands
                        .iter()
                        .filter(|c| c.why.is_none())
                        .map(|c| format!("{}#{}", f.repo, c.issue))
                        .collect();
                    let read = event(
                        w,
                        "range_read",
                        "makibad",
                        None,
                        json!({"range_id": s.id, "complete": true, "through": through,
                               "activation": activation, "repo": f.repo,
                               "candidate_refs": refs.join(",")}),
                    )?;
                    w.conn()
                        .execute("DELETE FROM range_candidates WHERE range_id = ?1", [s.id])?;
                    for (pos, c) in cands.iter().enumerate() {
                        w.conn().execute(
                            "INSERT INTO range_candidates (range_id, read_id, position, issue, title,
                               body, comments_json, admitted, left_out_why, waits_json)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                            params![
                                s.id,
                                read,
                                pos as i64,
                                c.issue,
                                c.title,
                                c.body,
                                Value::Array(c.comments.clone()).to_string(),
                                c.why.is_none(),
                                c.why,
                                c.waits.as_ref().map(|d| json!(d).to_string())
                            ],
                        )?;
                    }
                }
            }
        }
    }
    for InClone {
        task,
        merge,
        branch,
        head,
        pr,
    } in &o.clones
    {
        let (repo, pr) = (pr.as_ref().map(|p| &p.0), pr.as_ref().map(|p| p.1));
        // Once per merge commit and pull request: a record of the same commit written before
        // Issue 345 names no pull request, and does not count for a merge recorded without its
        // commit (`ahead::dep`, `clone_checks`), so this one is written.
        let known = count(
            w,
            "SELECT count(*) FROM events e JOIN runs r ON r.id = e.run_id
             WHERE e.kind = 'landed_in_clone' AND r.task_id = ?1
               AND json_extract(e.payload_json, '$.merge_commit') = ?2
               AND lower(json_extract(e.payload_json, '$.repo')) IS lower(?3)
               AND json_extract(e.payload_json, '$.pr') IS ?4",
            params![task, merge, repo, pr],
        )?;
        if known == 0 {
            event(
                w,
                "landed_in_clone",
                "makibad",
                Some(task),
                json!({"task_id": task, "merge_commit": merge, "base": branch,
                       "base_head": head, "repo": repo, "pr": pr}),
            )?;
        }
    }
    super::ahead::settle(w)?;
    fill(w, agent)?;
    Ok(())
}

// ---- the read surface ------------------------------------------------------------------------

/// The read of the candidates of range `id`, for the approval and the read surface.
fn candidates(c: &Connection, id: i64) -> rusqlite::Result<Vec<Value>> {
    let mut q = c.prepare(
        "SELECT issue, title, admitted, left_out_why, waits_json FROM range_candidates
         WHERE range_id = ?1 ORDER BY position",
    )?;
    q.query_map([id], |r| {
        let waits: Option<String> = r.get(4)?;
        Ok(
            json!({"issue": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                  "admitted": r.get::<_, i64>(2)? == 1,
                  "left_out_why": r.get::<_, Option<String>>(3)?,
                  "design_only_waits": waits.and_then(|w| serde_json::from_str::<Value>(&w).ok())}),
        )
    })?
    .collect()
}

const PLACES: [&str; 7] = [
    "not_in",
    "in_work",
    "waiting_person",
    "review",
    "landed",
    "closed",
    "skipped",
];

/// `ranges[]` of `status --json` (spec section 12, table "Range field"): every range not ended,
/// every ended one with a run not closed, and the newest ended range of each project.
pub fn read_ranges(c: &Connection) -> rusqlite::Result<Vec<Value>> {
    let exists: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ranges')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = {
        let mut q = c.prepare(
            "SELECT g.id FROM ranges g WHERE g.status != 'ended'
               OR EXISTS (SELECT 1 FROM runs r WHERE r.range_id = g.id
                          AND r.status NOT IN ('completed', 'abandoned'))
               OR g.id = (SELECT max(e.id) FROM ranges e
                          WHERE e.project_id = g.project_id AND e.status = 'ended')
             ORDER BY g.id",
        )?;
        q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    ids.into_iter().map(|id| one_range(c, id)).collect()
}

fn one_range(c: &Connection, id: i64) -> rusqlite::Result<Value> {
    let mut v = c.query_row(
        "SELECT g.id, p.slug, g.status, g.ended_reason, g.issues_json, g.takes, g.brief_head,
                g.at_once, g.approval_mode, g.budget_tasks, g.approved_by, g.approved_at,
                g.approved_read_id,
                (SELECT occurred_at FROM events WHERE id = g.approved_read_id),
                (SELECT count(*) FROM runs WHERE range_id = g.id),
                g.budget_hours, g.budget_fixes,
                round((julianday(coalesce(g.ended_at, 'now')) - julianday(g.approved_at)) * 24.0, 3),
                g.follow_ups, g.design_ahead = 1
         FROM ranges g JOIN projects p ON p.id = g.project_id WHERE g.id = ?1",
        [id],
        |r| {
            let issues: String = r.get(4)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "project": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?, "ended_reason": r.get::<_, Option<String>>(3)?,
                "issues": serde_json::from_str::<Value>(&issues).unwrap_or(json!([])),
                "takes": r.get::<_, String>(5)?, "brief_head": r.get::<_, String>(6)?,
                "at_once": r.get::<_, i64>(7)?, "approval_mode": r.get::<_, Option<String>>(8)?,
                "budget": {"tasks": r.get::<_, i64>(9)?, "hours": r.get::<_, Option<f64>>(15)?,
                           "fixes": r.get::<_, i64>(16)?},
                "approved_by": r.get::<_, Option<String>>(10)?,
                "approved_at": r.get::<_, Option<String>>(11)?,
                "approved_read_at": r.get::<_, Option<String>>(13)?,
                "used": {"tasks": r.get::<_, i64>(14)?, "hours": r.get::<_, Option<f64>>(17)?},
                "follow_ups": r.get::<_, String>(18)?,
                "design_ahead": r.get::<_, bool>(19)?,
            }))
        },
    )?;
    let read: Option<(i64, String, bool)> = c
        .query_row(
            "SELECT id, occurred_at, coalesce(json_extract(payload_json, '$.complete'), 0) = 1
             FROM events WHERE kind = 'range_read' AND json_extract(payload_json, '$.range_id') = ?1
             ORDER BY id DESC LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    // The read an approval is made on: the newest complete one.
    let complete: Option<i64> = c
        .query_row(
            "SELECT id FROM events WHERE kind = 'range_read'
               AND json_extract(payload_json, '$.range_id') = ?1
               AND json_extract(payload_json, '$.complete') = 1 ORDER BY id DESC LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    v["read_id"] = json!(complete);
    v["read_at"] = json!(read.as_ref().map(|r| r.1.clone()));
    v["read_complete"] = json!(read.as_ref().map(|r| r.2));
    let skipped = events_of(c, id, "belt_skipped")?;
    v["skipped"] = Value::Array(
        skipped
            .iter()
            .map(|(_, _, p)| json!({"issue": p["issue"], "reason": p["reason"]}))
            .collect(),
    );
    v["candidates"] = Value::Array(candidates(c, id)?);
    let items = items(c, id, &v, &skipped)?;
    let mut places = Map::new();
    for p in PLACES {
        let n = items.iter().filter(|i| i["place"] == json!(p)).count();
        places.insert(p.into(), json!(n));
    }
    v["items"] = Value::Array(items);
    v["places"] = Value::Object(places);
    let (stops, taken) = stops(c, id)?;
    v["stops"] = Value::Array(stops);
    v["stops_taken"] = taken;
    Ok(v)
}

/// The events of `kind` of range `id`: (id, occurred_at, payload).
fn events_of(c: &Connection, id: i64, kind: &str) -> rusqlite::Result<Vec<(i64, String, Value)>> {
    let mut q = c.prepare(
        "SELECT id, occurred_at, payload_json FROM events WHERE kind = ?2
           AND json_extract(payload_json, '$.range_id') = ?1 ORDER BY id",
    )?;
    q.query_map(params![id, kind], |r| {
        let p: String = r.get(2)?;
        Ok((
            r.get(0)?,
            r.get(1)?,
            serde_json::from_str(&p).unwrap_or_default(),
        ))
    })?
    .collect()
}

/// The time of the newest event of `kind` of run `run`.
fn newest_event_at(c: &Connection, run: i64, kind: &str) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT occurred_at FROM events WHERE run_id = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1",
        params![run, kind],
        |r| r.get(0),
    )
    .optional()
}

fn item(issue: &Value, title: Value, place: &str) -> Value {
    json!({"issue": issue, "title": title, "place": place, "why": null, "stage": null,
           "since": null, "task_id": null, "pr": null, "pr_title": null})
}

/// The item of the run of `task`, dispatched for `issue` (belt.md 7.1): its place, stage, why and
/// since as `ranges[].items[]` and `belt[]` show them. None when the task has no run.
fn run_item(c: &Connection, issue: i64, task: &str) -> rusqlite::Result<Option<Value>> {
    let run: Option<(i64, String, String, Option<String>, String)> = c
        .query_row(
            "SELECT r.id, r.status, r.title, r.completed_at, r.dispatched_at FROM runs r
             WHERE r.task_id = ?1",
            [&task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((run, status, title, completed, dispatched)) = run else {
        return Ok(None);
    };
    let mut it = item(&json!(issue), json!(title), "in_work");
    it["task_id"] = json!(task);
    it["since"] = json!(dispatched);
    let stop: Option<(String, String)> = c
        .query_row(
            "SELECT coalesce(json_extract(context_json, '$.stop_line'), 'decision') || ': '
                    || p.question, p.created_at
             FROM pending p WHERE p.run_id = ?1 AND p.status = 'escalated'
             ORDER BY p.id LIMIT 1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    // A design-only run (belt.md 4.3, 7.1): `design_only` at work, `waits_landing #<n>` in
    // review once its design is reported, until what it waits for has landed.
    // Only a run still open waits: an abandoned or closed one is placed by its status.
    let landing: bool = c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM runs r WHERE r.id = ?1
               AND r.status IN ('queued', 'in_use', 'suspended', 'parked') AND {LANDING_WAIT})"
        ),
        [run],
        |r| r.get(0),
    )?;
    let open_wait = super::ahead::open_wait(c, task)?;
    if let Some((why, since)) = stop {
        it["place"] = json!("waiting_person");
        it["why"] = json!(why.lines().next().unwrap_or_default());
        it["since"] = json!(since);
        // Its terminal let go while it waits (Issue 283).
        if status == "parked" {
            it["stage"] = json!("parked");
        }
    } else if landing && let Some(wait) = &open_wait {
        it["place"] = json!("review");
        // In review since its newest design report (spec 5.8: it waits from there).
        if let Some(at) = newest_event_at(c, run, "design_reported")? {
            it["since"] = json!(at);
        }
        it["stage"] = json!(format!(
            "waits_landing {}",
            wait.issues
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    } else {
        match status.as_str() {
            "review" => {
                it["place"] = json!("review");
                // In review since the done report that moved it there (Issue 414); a run
                // moved before `run_in_review` was written keeps its dispatch.
                if let Some(at) = newest_event_at(c, run, "run_in_review")? {
                    it["since"] = json!(at);
                }
                let watch: Option<(String, Option<String>)> = c
                    .query_row(
                        "SELECT status, conclusion FROM pr_watches WHERE run_id = ?1
                         ORDER BY id DESC LIMIT 1",
                        [run],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                it["stage"] = json!(match watch {
                    None => "no_pr",
                    Some((s, Some(c))) if s == "completed" && c == "passed" => "to_land",
                    Some(_) => "ci",
                });
            }
            "completed" => {
                let merged: Option<String> = c
                    .query_row(
                        "SELECT payload_json FROM events WHERE kind = 'pr_merged' AND run_id = ?1
                         ORDER BY id LIMIT 1",
                        [run],
                        |r| r.get(0),
                    )
                    .optional()?;
                it["since"] = json!(completed);
                match merged.and_then(|p| serde_json::from_str::<Value>(&p).ok()) {
                    Some(m) => {
                        it["place"] = json!("landed");
                        it["pr"] = m["pr"].clone();
                        it["pr_title"] = m["title"].clone();
                    }
                    None => {
                        it["place"] = json!("closed");
                        it["why"] = json!("no merge recorded");
                    }
                }
            }
            "abandoned" => {
                it["place"] = json!("skipped");
                it["why"] = json!("abandoned");
                it["since"] = json!(completed);
            }
            _ => {
                let stage: Option<String> = c
                    .query_row(
                        "SELECT stage FROM run_launch WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
                        [run],
                        |r| r.get(0),
                    )
                    .optional()?;
                it["stage"] = json!(if status == "failed" {
                    "launch_failed"
                } else if open_wait.is_some() {
                    "design_only"
                } else if status == "in_use" && stage.as_deref() == Some("verified") {
                    "at_work"
                } else {
                    "launching"
                });
            }
        }
    }
    Ok(Some(it))
}

/// The whole range as a list (belt.md 7.1): items with a run by their `belt_dispatched`, skipped
/// ones by their `belt_skipped`, then the rest of the newest read in its order.
fn items(
    c: &Connection,
    id: i64,
    range: &Value,
    skipped: &[(i64, String, Value)],
) -> rusqlite::Result<Vec<Value>> {
    let title_of = |issue: i64| -> rusqlite::Result<Value> {
        Ok(json!(
            c.query_row(
                "SELECT title FROM range_candidates WHERE range_id = ?1 AND issue = ?2",
                params![id, issue],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        ))
    };
    let mut placed: Vec<(i64, Value)> = Vec::new();
    let mut seen = BTreeSet::new();
    for (eid, _, p) in events_of(c, id, "belt_dispatched")? {
        let issue = p["issue"].as_i64().unwrap_or(0);
        let task = p["task_id"].as_str().unwrap_or("").to_string();
        seen.insert(issue);
        if let Some(it) = run_item(c, issue, &task)? {
            placed.push((eid, it));
        }
    }
    for (eid, at, p) in skipped {
        let issue = p["issue"].as_i64().unwrap_or(0);
        if !seen.insert(issue) {
            continue;
        }
        let mut it = item(&json!(issue), title_of(issue)?, "skipped");
        it["why"] = p["reason"].clone();
        it["since"] = json!(at);
        placed.push((*eid, it));
    }
    placed.sort_by_key(|(e, _)| *e);
    let mut out: Vec<Value> = placed.into_iter().map(|(_, i)| i).collect();
    let budget_spent = range["used"]["tasks"].as_i64() >= range["budget"]["tasks"].as_i64()
        || matches!(
            (range["used"]["hours"].as_f64(), range["budget"]["hours"].as_f64()),
            (Some(used), Some(budget)) if used >= budget
        );
    let mut q = c.prepare(
        "SELECT issue, title, admitted, left_out_why FROM range_candidates
         WHERE range_id = ?1 ORDER BY position",
    )?;
    let rest: Vec<(i64, String, bool, Option<String>)> = q
        .query_map([id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? == 1, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (issue, title, admitted, why) in rest {
        if seen.contains(&issue) {
            continue;
        }
        let mut it = item(&json!(issue), json!(title), "not_in");
        it["why"] = json!(match (admitted, why) {
            (false, Some(w)) => w,
            (false, None) => "left out".into(),
            (true, _) if range["status"] != json!("active") =>
                format!("range {}", range["status"].as_str().unwrap_or("")),
            (true, _) if budget_spent => "budget".into(),
            (true, _) => "no_slot".into(),
        });
        out.push(it);
    }
    Ok(out)
}

/// `belt[]` of `status --json` (belt.md 7, step 5; spec section 12, table "Belt field"): every run
/// of a range not `completed` or `abandoned`, by run id, with the place and stage of its item in
/// `ranges[].items[]`, whether it counts against its range's `at_once`, the stop it waits on and
/// the Issues a design-only run still waits for.
pub fn read_belt(c: &Connection) -> rusqlite::Result<Vec<Value>> {
    let exists: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ranges')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Vec::new());
    }
    let runs: Vec<(i64, i64, String, bool)> = c
        .prepare(&format!(
            "SELECT r.id, r.range_id, r.task_id, {} FROM runs r
             WHERE r.range_id IS NOT NULL AND r.status NOT IN ('completed', 'abandoned')
             ORDER BY r.id",
            counts_sql()
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (run, range, task, counts) in runs {
        let issue: Option<i64> = c
            .query_row(
                "SELECT json_extract(payload_json, '$.issue') FROM events
                 WHERE run_id = ?1 AND kind = 'belt_dispatched' ORDER BY id LIMIT 1",
                [run],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let it = match issue {
            Some(n) => run_item(c, n, &task)?,
            None => None,
        };
        let field = |name: &str| it.as_ref().map_or(Value::Null, |i| i[name].clone());
        // The stop it waits on: the newest `belt_stopped` of the run whose entry is still escalated.
        let stop: Option<(String, String, i64)> = c
            .query_row(
                "SELECT json_extract(e.payload_json, '$.line'),
                        json_extract(e.payload_json, '$.reason'),
                        json_extract(e.payload_json, '$.pending_id')
                 FROM events e JOIN pending p
                      ON p.id = json_extract(e.payload_json, '$.pending_id')
                 WHERE e.run_id = ?1 AND e.kind = 'belt_stopped' AND p.status = 'escalated'
                 ORDER BY e.id DESC LIMIT 1",
                [run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (line, reason, pending) = match stop {
            Some((l, r, p)) => (json!(l), json!(r), json!(p)),
            None => (Value::Null, Value::Null, Value::Null),
        };
        out.push(json!({
            "range_id": range, "task_id": task, "issue": issue,
            "place": field("place"), "stage": field("stage"), "since": field("since"),
            "counts": counts, "stop_line": line, "stop_reason": reason, "pending_id": pending,
            "waits_for": super::ahead::still_waits_for(c, &task)?,
        }));
    }
    Ok(out)
}

/// The stops of line 1 of the range (belt.md 5.2), any status, and per kind how many were
/// answered as recommended.
fn stops(c: &Connection, id: i64) -> rusqlite::Result<(Vec<Value>, Value)> {
    let mut q = c.prepare(
        "SELECT p.id, r.task_id, p.question, json_extract(p.context_json, '$.kind'),
                json_extract(p.context_json, '$.recommend_answer'), p.answer, p.status
         FROM pending p LEFT JOIN runs r ON r.id = p.run_id
         WHERE json_extract(p.context_json, '$.range_id') = ?1
           AND json_extract(p.context_json, '$.stop_line') = 'decision'
         ORDER BY p.id",
    )?;
    let rows: Vec<Value> = q
        .query_map([id], |r| {
            let question: String = r.get(2)?;
            Ok(
                json!({"id": r.get::<_, i64>(0)?, "task_id": r.get::<_, Option<String>>(1)?,
                      "question": question.lines().next().unwrap_or_default(),
                      "kind": r.get::<_, Option<String>>(3)?.unwrap_or_else(|| "other".into()),
                      "recommend_answer": r.get::<_, Option<String>>(4)?,
                      "answer": r.get::<_, Option<String>>(5)?,
                      "status": r.get::<_, String>(6)?}),
            )
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut taken = Map::new();
    for s in &rows {
        let kind = s["kind"].as_str().unwrap_or("other").to_string();
        let e = taken
            .entry(kind)
            .or_insert_with(|| json!({"stops": 0, "as_recommended": 0}));
        e["stops"] = json!(e["stops"].as_i64().unwrap_or(0) + 1);
        if s["answer"].is_string() && s["answer"] == s["recommend_answer"] {
            e["as_recommended"] = json!(e["as_recommended"].as_i64().unwrap_or(0) + 1);
        }
    }
    Ok((rows, Value::Object(taken)))
}

/// One line for a renewed secretary, first in its resume brief (belt.md 3.4): the ranges in force.
pub(super) fn in_force_line(w: &StateWriter) -> Result<Option<String>> {
    let mut q = w.conn().prepare(
        "SELECT printf('#%d %s %s', g.id, p.slug, g.status) FROM ranges g
         JOIN projects p ON p.id = g.project_id WHERE g.status != 'ended' ORDER BY g.id",
    )?;
    let rows: Vec<String> = q
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    Ok((!rows.is_empty()).then(|| {
        format!(
            "Ranges in force: {}. Read them with the status tool before anything else: makibad dispatches their tasks itself.",
            rows.join(", ")
        )
    }))
}

#[cfg(test)]
pub(in crate::daemon) mod tests {
    use super::*;

    #[test]
    fn the_condition_parses_its_four_words_and_refuses_others() {
        assert!(Takes::parse("label:bug no-label:wip milestone:M7 clause:<=G3").is_ok());
        assert!(Takes::parse("").unwrap().0.is_empty());
        for bad in ["size:small", "label:", "clause:G3", "bug"] {
            assert!(Takes::parse(bad).is_err(), "{bad}");
        }
        let t = Takes::parse("label:bug no-label:wip").unwrap();
        let issue = |labels: &[&str]| {
            json!({"labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>(),
                   "body": ""})
        };
        assert!(t.matches(&issue(&["Bug"])));
        assert!(!t.matches(&issue(&["bug", "wip"])));
        assert!(!Takes::default().matches(&issue(&["bug"])));
    }

    pub(in crate::daemon) fn org() -> (tempfile::TempDir, StateWriter) {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        (dir, w)
    }

    /// A draft that replaced a draft (which ended at the proposal) is approved like any draft.
    #[test]
    fn a_draft_that_replaced_a_draft_can_be_approved() {
        let (_dir, w) = org();
        let draft = |replaces: Option<i64>| {
            range(
                &w,
                "secretary",
                Role::Secretary,
                &json!({"action": "propose", "project": "p", "issues": [1], "at_once": 1,
                        "budget_tasks": 1, "brief_head": "h", "replaces": replaces}),
            )
            .unwrap()["id"]
                .as_i64()
                .unwrap()
        };
        let first = draft(None);
        let second = draft(Some(first));
        let read = event(
            &w,
            "range_read",
            "makibad",
            None,
            json!({"range_id": second, "complete": true}),
        )
        .unwrap();
        let r = range(
            &w,
            "person",
            Role::Person,
            &json!({"action": "approve", "id": second, "read": read}),
        );
        assert_eq!(r.unwrap()["status"], json!("active"));
    }

    /// A candidate whose dependency is written in words is left out as `dependency_unreadable`,
    /// but once the range skipped it, it is `skipped`: the range is not held open by it.
    #[test]
    fn a_skipped_dependency_in_words_is_skipped_not_unreadable() {
        let (_dir, w) = org();
        let id = range(
            &w,
            "secretary",
            Role::Secretary,
            &json!({"action": "propose", "project": "p", "issues": [31, 32], "at_once": 1,
                    "budget_tasks": 1, "brief_head": "h"}),
        )
        .unwrap()["id"]
            .as_i64()
            .unwrap();
        let spec = Spec {
            id,
            slug: "p".into(),
            origin: None,
            listed: vec![31, 32],
            takes: Takes::default(),
            follow_inside: false,
        };
        let issue = |n: i64| {
            json!({"number": n, "title": "t", "body": "Depends on the belt's step 2.",
                   "labels": [], "comments": [], "createdAt": "2026-11-01T00:00:00Z"})
        };
        let f = Fetched {
            repo: "o/p".into(),
            open: vec![issue(31), issue(32)],
            prs: Vec::new(),
            views: BTreeMap::new(),
        };
        range(
            &w,
            "secretary",
            Role::Secretary,
            &json!({"action": "skip", "id": id, "issue": 31, "reason": "later"}),
        )
        .unwrap();
        let c = admit(&w, &spec, &f, None).unwrap();
        let why = |n: i64| c.iter().find(|c| c.issue == n).unwrap().why.clone();
        assert_eq!(why(31).as_deref(), Some("skipped"));
        assert_eq!(
            why(32).as_deref(),
            Some("dependency_unreadable (Depends on the belt's step 2.)")
        );
    }

    /// A new Issue the condition matches is left out as a follow-up when its parent is in the
    /// range by the condition too, not only by the list.
    #[test]
    fn a_follow_up_of_an_issue_the_condition_takes_is_asked_for() {
        let (_dir, w) = org();
        let spec = Spec {
            id: 1,
            slug: "p".into(),
            origin: None,
            listed: Vec::new(),
            takes: Takes::parse("label:bug").unwrap(),
            follow_inside: false,
        };
        let issue = |n: i64, body: &str, at: &str| {
            json!({"number": n, "title": "t", "body": body, "labels": [{"name": "bug"}],
                   "comments": [], "createdAt": at})
        };
        let f = Fetched {
            repo: "o/p".into(),
            open: vec![
                issue(5, "The parent.", "2026-11-01T00:00:00Z"),
                issue(6, "Follow-up of #5", "2026-11-23T00:00:00Z"),
            ],
            prs: Vec::new(),
            views: BTreeMap::new(),
        };
        let c = admit(&w, &spec, &f, Some("2026-11-22T00:00:00Z")).unwrap();
        let why = |n: i64| c.iter().find(|c| c.issue == n).unwrap().why.clone();
        assert_eq!(why(5), None);
        assert_eq!(why(6).as_deref(), Some("follow_up (ask)"));
    }

    /// With `inside`, a follow-up kept out (a host not listed, a stop line) is listed as asked
    /// for, and a follow-up of it alone is not taken through it; one of a kept follow-up is.
    #[test]
    fn a_follow_up_kept_out_does_not_take_its_own_follow_ups_in() {
        let (_dir, w) = org();
        w.conn()
            .execute(
                "INSERT INTO ranges (id, project_id, at_once, budget_tasks, brief_head,
                                     proposed_by, follow_ups)
                 VALUES (1, (SELECT id FROM projects WHERE slug = 'p'), 1, 9, 'h', 's', 'inside')",
                [],
            )
            .unwrap();
        let spec = Spec {
            id: 1,
            slug: "p".into(),
            origin: None,
            listed: vec![1],
            takes: Takes::default(),
            follow_inside: true,
        };
        let issue = |n: i64, body: &str| {
            json!({"number": n, "title": "t", "body": body, "labels": [], "comments": [],
                   "createdAt": "2026-11-23T00:00:00Z"})
        };
        let f = Fetched {
            repo: "o/p".into(),
            open: vec![
                issue(1, "The first."),
                issue(2, "Follow-up of #1\nNeeds host: example.org"),
                issue(3, "Follow-up of #2"),
                issue(4, "Follow-up of #1"),
                issue(5, "Follow-up of #4\nStop line: irreversible"),
                issue(6, "Follow-up of #4"),
            ],
            prs: Vec::new(),
            views: BTreeMap::new(),
        };
        let c = admit(&w, &spec, &f, Some("2026-11-22T00:00:00Z")).unwrap();
        let why = |n: i64| {
            c.iter()
                .find(|c| c.issue == n)
                .map(|c| c.why.clone().unwrap_or_else(|| "admitted".into()))
        };
        assert_eq!(
            why(2).as_deref(),
            Some("follow_up (ask: needs host example.org)")
        );
        assert_eq!(why(3), None, "taken only through a follow-up kept out");
        assert_eq!(why(4).as_deref(), Some("admitted"));
        assert_eq!(
            why(5).as_deref(),
            Some("follow_up (ask: stop line irreversible)")
        );
        assert_eq!(why(6).as_deref(), Some("admitted"));
    }

    /// A follow-up names an Issue of this repository: `Follow-up of other/repo#1` is not a
    /// follow-up of #1, and a time with fewer fractions of a second is compared as a time.
    #[test]
    fn a_follow_up_of_another_repository_or_before_the_approval_is_not_taken() {
        let (_dir, w) = org();
        w.conn()
            .execute(
                "INSERT INTO ranges (id, project_id, at_once, budget_tasks, brief_head,
                                     proposed_by, follow_ups)
                 VALUES (1, (SELECT id FROM projects WHERE slug = 'p'), 1, 9, 'h', 's', 'inside')",
                [],
            )
            .unwrap();
        let spec = Spec {
            id: 1,
            slug: "p".into(),
            origin: None,
            listed: vec![1],
            takes: Takes::default(),
            follow_inside: true,
        };
        let issue = |n: i64, body: &str, at: &str| {
            json!({"number": n, "title": "t", "body": body, "labels": [], "comments": [],
                   "createdAt": at})
        };
        let f = Fetched {
            repo: "o/p".into(),
            open: vec![
                issue(1, "The first.", "2026-11-01T00:00:00Z"),
                issue(2, "Follow-up of other/repo#1", "2026-11-23T00:00:00Z"),
                issue(3, "Follow-up of o/p#1, seen in #1", "2026-11-23T00:00:00Z"),
                issue(4, "Follow-up of #1", "2026-11-22T00:00:00Z"),
                issue(5, "Follow-up of #1.", "2026-11-22T00:00:01Z"),
            ],
            prs: Vec::new(),
            views: BTreeMap::new(),
        };
        let c = admit(&w, &spec, &f, Some("2026-11-22T00:00:00.500Z")).unwrap();
        let has = |n: i64| c.iter().any(|c| c.issue == n);
        assert!(!has(2), "another repository's #1");
        assert!(has(3), "this repository named whole");
        assert!(!has(4), "created half a second before the approval");
        assert!(has(5));
    }

    /// A dependency's merge recorded before Issue 284 has no merge commit: the clone check reads
    /// its pull request again for it, then looks for it in the clone.
    #[test]
    fn a_merge_recorded_without_its_commit_is_read_again_for_the_clone_check() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |at: &Path, args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args([
                    "-c",
                    "init.defaultBranch=main",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .current_dir(at)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let origin = root.join("origin.git");
        let clone = root.join("repos").join("p");
        git(root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        std::fs::create_dir_all(&clone).unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(
            &clone,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(
            &clone,
            &["commit", "-q", "--allow-empty", "-m", "the merge"],
        );
        git(&clone, &["push", "-q", "origin", "main"]);
        let merge = git(&clone, &["rev-parse", "HEAD"]);
        let gh = root.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\necho '{}'\n",
                json!({"state": "MERGED", "mergeCommit": {"oid": merge}})
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let check = CloneCheck {
            task: "r1-1".into(),
            slug: "p".into(),
            branch: "main".into(),
            merge: None,
            pr: Some(("o/p".into(), 11)),
        };
        let never = AtomicBool::new(false);
        let found = check_clone(&gh.to_string_lossy(), &root.join("repos"), &check, &never);
        assert_eq!(
            found,
            Some(InClone {
                task: "r1-1".into(),
                merge: merge.clone(),
                branch: "main".into(),
                head: merge.clone(),
                pr: Some(("o/p".into(), 11)),
            })
        );
        let none = CloneCheck { pr: None, ..check };
        assert_eq!(
            check_clone(&gh.to_string_lossy(), &root.join("repos"), &none, &never),
            None,
            "no pull request to read"
        );
    }

    /// A merge is seen in the project's clone only once its default branch, fetched from its
    /// origin, holds it (belt.md 4.3): a merge commit not there yet, or a clone whose fetch fails,
    /// is no landing.
    #[test]
    fn a_merge_is_in_the_clone_only_once_its_fetched_branch_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |at: &Path, args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args([
                    "-c",
                    "init.defaultBranch=main",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .current_dir(at)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let origin = root.join("origin.git");
        let clone = root.join("clone");
        let other = root.join("other");
        git(root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        std::fs::create_dir_all(&clone).unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(
            &clone,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&clone, &["push", "-q", "origin", "main"]);
        git(
            root,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &other.to_string_lossy(),
            ],
        );
        git(
            &other,
            &["commit", "-q", "--allow-empty", "-m", "the merge"],
        );
        let merge = git(&other, &["rev-parse", "HEAD"]);
        let never = AtomicBool::new(false);
        assert_eq!(
            in_clone(&clone, "main", &merge, &never),
            None,
            "not pushed yet"
        );
        git(&other, &["push", "-q", "origin", "main"]);
        assert_eq!(
            in_clone(&clone, "main", &merge, &never),
            Some(merge.clone())
        );
        assert_eq!(
            git(&clone, &["rev-parse", "refs/remotes/origin/main"]),
            merge,
            "what a worker's fetch of the clone reads"
        );
        assert_eq!(
            in_clone(&root.join("missing"), "main", &merge, &never),
            None
        );
    }

    /// A range whose only candidate could not be read whole is not used up: it waits for a read.
    #[test]
    fn an_unreadable_candidate_keeps_the_range_open() {
        let (_dir, w) = org();
        let id = range(
            &w,
            "secretary",
            Role::Secretary,
            &json!({"action": "propose", "project": "p", "issues": [1], "at_once": 1,
                    "budget_tasks": 1, "brief_head": "h"}),
        )
        .unwrap()["id"]
            .as_i64()
            .unwrap();
        w.conn()
            .execute("UPDATE ranges SET status = 'active' WHERE id = ?1", [id])
            .unwrap();
        let activation = event(
            &w,
            "range_approved",
            "person",
            None,
            json!({"range_id": id}),
        )
        .unwrap();
        let read = event(
            &w,
            "range_read",
            "makibad",
            None,
            json!({"range_id": id, "complete": true, "activation": activation}),
        )
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO range_candidates (range_id, read_id, position, issue, title, body,
                   admitted, left_out_why)
                 VALUES (?1, ?2, 0, 1, 't', '', 0, 'unreadable (too large to read whole)')",
                params![id, read],
            )
            .unwrap();
        assert!(fill(&w, "fake").unwrap().is_empty());
        let status: String = w
            .conn()
            .query_row("SELECT status FROM ranges WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "active");
    }

    /// A range of project `p`, made active as an approval would; its id.
    pub(in crate::daemon) fn active_range(w: &StateWriter, replaces: Option<i64>) -> i64 {
        let id = range(
            w,
            "secretary",
            Role::Secretary,
            &json!({"action": "propose", "project": "p", "issues": [1], "at_once": 1,
                    "budget_tasks": 9, "brief_head": "h", "replaces": replaces}),
        )
        .unwrap()["id"]
            .as_i64()
            .unwrap();
        if let Some(old) = replaces {
            end_range(w, old, "replaced", "person").unwrap();
        }
        w.conn()
            .execute(
                "UPDATE ranges SET status = 'active', approved_by = 'person',
                   approved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
                [id],
            )
            .unwrap();
        id
    }

    /// A run of `range`, at work.
    pub(in crate::daemon) fn run_of(w: &StateWriter, range: i64, task: &str) {
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET range_id = ?2 WHERE task_id = ?1",
                params![task, range],
            )
            .unwrap();
    }

    fn status_of(w: &StateWriter, id: i64) -> String {
        w.conn()
            .query_row("SELECT status FROM ranges WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// A launch outcome of `task` as `verify` records it, then the belt's part of a failure.
    fn launch(w: &StateWriter, task: &str, failed: bool) {
        if failed {
            event(w, "worker_spawn_failed", "makibad", Some(task), json!({})).unwrap();
            launch_failed(w, task).unwrap();
            return;
        }
        // A verified launch names the turn of its terminal that proved it (migration v6).
        let turn = event(
            w,
            "terminal_turn",
            "makibad",
            Some(task),
            json!({"spawn_id": task}),
        )
        .unwrap();
        event(
            w,
            "worker_spawn_verified",
            "makibad",
            Some(task),
            json!({"spawn_id": task, "event_id": turn}),
        )
        .unwrap();
    }

    /// Two failed launches in a row pause the range; a verified one between them resets the count.
    #[test]
    fn two_failed_launches_in_a_row_pause_the_range() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        for t in ["a", "b", "c", "d"] {
            run_of(&w, id, t);
        }
        launch(&w, "a", true);
        assert_eq!(status_of(&w, id), "active", "one failure pauses nothing");
        launch(&w, "b", false);
        launch(&w, "c", true);
        assert_eq!(status_of(&w, id), "active", "a verified launch reset it");
        launch(&w, "d", true);
        assert_eq!(status_of(&w, id), "paused");
        let (actor, payload): (String, String) = w
            .conn()
            .query_row(
                "SELECT actor, payload_json FROM events WHERE kind = 'range_paused'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(actor, "makibad");
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["range_id"], json!(id));
        assert!(payload["reason"].as_str().unwrap().contains("launches"));
    }

    /// A run keeps the range it was dispatched under: its failures pause the range that replaced
    /// that one, which is the one dispatching now.
    #[test]
    fn failures_of_a_replaced_ranges_runs_pause_the_range_that_replaced_it() {
        let (_dir, w) = org();
        let old = active_range(&w, None);
        run_of(&w, old, "a");
        run_of(&w, old, "b");
        let new = active_range(&w, Some(old));
        assert_eq!(acting(&w, old).unwrap(), Some((new, "active".into())));
        launch(&w, "a", true);
        launch(&w, "b", true);
        assert_eq!(status_of(&w, old), "ended");
        assert_eq!(status_of(&w, new), "paused");
    }

    pub(in crate::daemon) fn watch(w: &StateWriter, task: &str, pr: i64, head: &str) {
        w.conn()
            .execute(
                "INSERT INTO pr_watches (repo, pr, head_oid, run_id)
                 VALUES ('o/p', ?2, ?3, (SELECT id FROM runs WHERE task_id = ?1))",
                params![task, pr, head],
            )
            .unwrap();
    }

    fn refused(w: &StateWriter, task: &str, pr: i64, head: &str) -> Option<i64> {
        event(
            w,
            "acceptance_gate",
            "merge-gate",
            Some(task),
            json!({"repo": "o/p", "pr": pr, "head": head, "allow": false}),
        )
        .unwrap();
        gate_stop(
            w,
            &GateStop {
                task,
                repo: "o/p",
                pr,
                head,
                allow: false,
                refused: true,
                unavailable: false,
                reason: "no passed CI",
                edited_tests: &[],
            },
        )
        .unwrap()
    }

    fn escalated(w: &StateWriter) -> i64 {
        count(
            w,
            "SELECT count(*) FROM pending WHERE status = 'escalated'",
            [],
        )
        .unwrap()
    }

    /// A refusal of a head that is no longer the pull request's recorded head is no stop; the stop
    /// of a head goes when a newer head is recorded, and the run counts again.
    #[test]
    fn a_gate_stop_is_for_the_head_recorded_last_and_goes_with_a_newer_head() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "a");
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        watch(&w, "a", 7, &h1);
        watch(&w, "a", 7, &h2);
        assert_eq!(refused(&w, "a", 7, &h1), None, "an older head: no stop");
        assert_eq!(escalated(&w), 0);
        let stop = refused(&w, "a", 7, &h2).expect("the newest head: a stop");
        assert_eq!(refused(&w, "a", 7, &h2), Some(stop), "one per head");
        assert_eq!(escalated(&w), 1);
        assert_eq!(range_count(&w, id).unwrap(), 0, "it waits for the person");
        let h3 = "3".repeat(40);
        watch(&w, "a", 7, &h3);
        head_recorded(&w, "a", "o/p", 7, &h3).unwrap();
        assert_eq!(escalated(&w), 0, "the stop of the older head is withdrawn");
        assert_eq!(range_count(&w, id).unwrap(), 1, "the run counts again");
        assert_eq!(status_of(&w, id), "active", "one pull request: no pause");
    }

    /// A pull request that returns to a head recorded before (its watch row is left as it was):
    /// that head is the one recorded last again, and its refusal is a stop.
    #[test]
    fn a_head_recorded_again_is_the_head_recorded_last() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "a");
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        for (kind, head) in [("pr_opened", &h1), ("fix_pushed", &h2)] {
            super::super::prwatch::register(&w, kind, "a", "o/p", 7, head).unwrap();
        }
        w.conn()
            .execute("UPDATE pr_watches SET status = 'completed'", [])
            .unwrap();
        super::super::prwatch::register(&w, "fix_pushed", "a", "O/p", 7, &h1).unwrap();
        assert_eq!(recorded_head(&w, "o/p", 7).unwrap(), Some(h1.clone()));
        assert_eq!(refused(&w, "a", 7, &h2), None, "the head it left: no stop");
        assert!(
            refused(&w, "a", 7, &h1).is_some(),
            "the head it returned to"
        );
    }

    /// H1 refused (a stop), H2 recorded (the stop goes), H1 recorded again and refused again: a
    /// new stop, escalated, so the refusal can be answered and the run waits for the person.
    #[test]
    fn a_stop_withdrawn_for_a_newer_head_is_written_again_when_the_head_comes_back() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "a");
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        let record = |kind: &str, head: &str| {
            super::super::prwatch::register(&w, kind, "a", "o/p", 7, head).unwrap();
            head_recorded(&w, "a", "o/p", 7, head).unwrap();
        };
        // Fixes are not what this test is about.
        w.conn()
            .execute("UPDATE ranges SET budget_fixes = 99 WHERE id = ?1", [id])
            .unwrap();
        record("pr_opened", &h1);
        let first = refused(&w, "a", 7, &h1).unwrap();
        record("fix_pushed", &h2);
        assert_eq!(escalated(&w), 0);
        record("fix_pushed", &h1);
        let again = refused(&w, "a", 7, &h1).unwrap();
        assert_ne!(again, first, "a new entry, not the withdrawn one");
        assert_eq!(escalated(&w), 1);
        assert_eq!(range_count(&w, id).unwrap(), 0);
        assert_eq!(refused(&w, "a", 7, &h1), Some(again), "one per head still");
        // A stop the secretary withdrew is not written again.
        w.conn()
            .execute(
                "UPDATE pending SET status = 'withdrawn' WHERE id = ?1",
                [again],
            )
            .unwrap();
        assert_eq!(refused(&w, "a", 7, &h1), Some(again));
        assert_eq!(escalated(&w), 0);
    }

    /// Two refusals in a row of different pull requests pause the range that replaced the runs'
    /// range; a merged pull request between them resets the count.
    #[test]
    fn refusals_count_across_a_replacement_and_a_merge_resets_them() {
        let (_dir, w) = org();
        let old = active_range(&w, None);
        for t in ["a", "b", "c"] {
            run_of(&w, old, t);
        }
        let new = active_range(&w, Some(old));
        let h = "1".repeat(40);
        watch(&w, "a", 7, &h);
        watch(&w, "b", 8, &h);
        watch(&w, "c", 9, &h);
        refused(&w, "a", 7, &h).unwrap();
        event(
            &w,
            "pr_merged",
            "makibad",
            Some("a"),
            json!({"repo": "o/p", "pr": 7}),
        )
        .unwrap();
        refused(&w, "b", 8, &h).unwrap();
        assert_eq!(status_of(&w, new), "active", "the merge reset the count");
        refused(&w, "c", 9, &h).unwrap();
        assert_eq!(status_of(&w, new), "paused");
    }

    /// A refusal of a head its pull request has left (a stale verdict) makes no stop and does
    /// not count: with one current refusal of another pull request after it, the range goes on.
    #[test]
    fn a_stale_refusal_does_not_count_toward_the_pause() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "a");
        run_of(&w, id, "b");
        w.conn()
            .execute("UPDATE ranges SET budget_fixes = 99 WHERE id = ?1", [id])
            .unwrap();
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        let record = |task: &str, pr: i64, kind: &str, head: &str| {
            super::super::prwatch::register(&w, kind, task, "o/p", pr, head).unwrap();
            head_recorded(&w, task, "o/p", pr, head).unwrap();
        };
        record("a", 7, "pr_opened", &h1);
        record("a", 7, "fix_pushed", &h2);
        record("b", 8, "pr_opened", &h1);
        assert_eq!(refused(&w, "a", 7, &h1), None, "a stale head makes no stop");
        refused(&w, "b", 8, &h1).expect("a stop of the current head");
        assert_eq!(
            status_of(&w, id),
            "active",
            "the stale refusal does not count"
        );
        refused(&w, "a", 7, &h2).expect("a stop of the current head");
        assert_eq!(status_of(&w, id), "paused", "two current refusals do");
    }

    /// The person's resume starts the counts again: after it, an answer that allows with edited
    /// tests pauses nothing, one more refusal pauses nothing, and one more failed launch neither.
    #[test]
    fn a_resume_starts_the_counts_again_and_an_allowance_pauses_nothing() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        for t in ["a", "b", "c", "d"] {
            run_of(&w, id, t);
        }
        let h = "1".repeat(40);
        for (t, pr) in [("a", 7), ("b", 8), ("c", 9), ("d", 10)] {
            watch(&w, t, pr, &h);
        }
        launch(&w, "a", true);
        launch(&w, "b", true);
        assert_eq!(status_of(&w, id), "paused");
        let resume = |w: &StateWriter| {
            range(
                w,
                "person",
                Role::Person,
                &json!({"action": "resume", "id": id}),
            )
            .unwrap();
        };
        resume(&w);
        launch(&w, "c", true);
        assert_eq!(status_of(&w, id), "active", "one failure since the resume");
        refused(&w, "a", 7, &h).unwrap();
        refused(&w, "b", 8, &h).unwrap();
        assert_eq!(status_of(&w, id), "paused");
        resume(&w);
        // Allowed, with edited tests (human): a stop of line tests, and no pause.
        let tests = ["tests/x.rs".to_string()];
        event(
            &w,
            "acceptance_gate",
            "merge-gate",
            Some("c"),
            json!({"repo": "o/p", "pr": 9, "head": h, "allow": true}),
        )
        .unwrap();
        gate_stop(
            &w,
            &GateStop {
                task: "c",
                repo: "o/p",
                pr: 9,
                head: &h,
                allow: true,
                refused: false,
                unavailable: false,
                reason: "passed",
                edited_tests: &tests,
            },
        )
        .unwrap()
        .expect("a stop of line tests");
        assert_eq!(status_of(&w, id), "active", "an allowance pauses nothing");
        refused(&w, "d", 10, &h).unwrap();
        assert_eq!(status_of(&w, id), "active", "one refusal since the resume");
        refused(&w, "c", 9, &h).unwrap();
        assert_eq!(status_of(&w, id), "paused", "two since the resume");
    }

    /// A refusal, then an allowance of the same head (the missing CI result or review arrived):
    /// the stop goes and the run counts again; a later refusal of that head is a stop again.
    #[test]
    fn an_allowance_of_the_same_head_withdraws_its_gate_stop() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "a");
        run_of(&w, id, "b");
        let h = "1".repeat(40);
        watch(&w, "a", 7, &h);
        watch(&w, "b", 8, &h);
        let first = refused(&w, "a", 7, &h).unwrap();
        refused(&w, "b", 8, &h).unwrap();
        w.conn()
            .execute("UPDATE ranges SET status = 'active' WHERE id = ?1", [id])
            .unwrap();
        assert_eq!(range_count(&w, id).unwrap(), 0);
        gate_allowed(&w, "O/p", 7, &h, true).unwrap();
        assert_eq!(escalated(&w), 1, "only the stop of that pull request goes");
        assert_eq!(range_count(&w, id).unwrap(), 1, "its run counts again");
        let again = refused(&w, "a", 7, &h).unwrap();
        assert_ne!(again, first);
        assert_eq!(escalated(&w), 2);
    }

    /// A head stopped for edited tests, then allowed with none (the base took those changes):
    /// its `tests` stop goes; an allowance that still reports edited tests leaves it.
    #[test]
    fn an_allowance_without_edited_tests_withdraws_the_tests_stop_of_that_head() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "a");
        let h = "1".repeat(40);
        watch(&w, "a", 7, &h);
        let tests = ["tests/x.rs".to_string()];
        event(
            &w,
            "acceptance_gate",
            "merge-gate",
            Some("a"),
            json!({"repo": "o/p", "pr": 7, "head": h, "allow": false}),
        )
        .unwrap();
        let stopped = gate_stop(
            &w,
            &GateStop {
                task: "a",
                repo: "o/p",
                pr: 7,
                head: &h,
                allow: false,
                refused: false,
                unavailable: false,
                reason: "tests edited",
                edited_tests: &tests,
            },
        )
        .unwrap()
        .expect("a stop of line tests");
        gate_allowed(&w, "O/p", 7, &h, false).unwrap();
        assert_eq!(
            escalated(&w),
            1,
            "edited tests still reported: the stop stays"
        );
        gate_allowed(&w, "O/p", 7, &h, true).unwrap();
        assert_eq!(escalated(&w), 0, "no edited test: the stop goes");
        let status: String = w
            .conn()
            .query_row("SELECT status FROM pending WHERE id = ?1", [stopped], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "withdrawn");
    }

    /// A range budget is a stop only while it keeps a candidate out, and once.
    #[test]
    fn a_spent_budget_is_a_stop_only_when_a_candidate_is_left() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        w.conn()
            .execute("UPDATE ranges SET budget_tasks = 1 WHERE id = ?1", [id])
            .unwrap();
        let activation = event(
            &w,
            "range_approved",
            "person",
            None,
            json!({"range_id": id}),
        )
        .unwrap();
        let read = event(
            &w,
            "range_read",
            "makibad",
            None,
            json!({"range_id": id, "complete": true, "activation": activation}),
        )
        .unwrap();
        run_of(&w, id, "r-taken");
        fill(&w, "fake").unwrap();
        assert_eq!(escalated(&w), 0, "nothing is left: no question");
        w.conn()
            .execute(
                "INSERT INTO range_candidates (range_id, read_id, position, issue, title, body,
                   admitted) VALUES (?1, ?2, 0, 5, 't', '', 1)",
                params![id, read],
            )
            .unwrap();
        fill(&w, "fake").unwrap();
        fill(&w, "fake").unwrap();
        assert_eq!(escalated(&w), 1);
        let (run, line): (Option<i64>, String) = w
            .conn()
            .query_row(
                "SELECT run_id, json_extract(context_json, '$.stop_line') FROM pending",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((run, line.as_str()), (None, "budget"));
    }

    #[test]
    fn only_the_lines_that_start_with_the_word_are_read() {
        let body =
            "Depends on #3 and #4\nThis mentions depends on #5\n  depends on: #6\nFollow-up of #7";
        assert_eq!(refs_on_lines(body, "depends on"), vec![3, 4, 6]);
        assert_eq!(refs_on_lines(body, "follow-up of"), vec![7]);
    }

    /// A completed run `dep` and a design-only run `wait` waiting for it (Issue 345 tests).
    pub(in crate::daemon) fn waited_for(w: &StateWriter) {
        let id = active_range(w, None);
        run_of(w, id, "dep");
        run_of(w, id, "wait");
        event(
            w,
            "belt_dispatched",
            "makibad",
            Some("dep"),
            json!({"range_id": id, "issue": 1, "task_id": "dep", "design_only": false}),
        )
        .unwrap();
        event(
            w,
            "belt_dispatched",
            "makibad",
            Some("wait"),
            json!({"range_id": id, "issue": 2, "task_id": "wait", "design_only": true,
                   "waits_for": [1], "waits_tasks": ["dep"]}),
        )
        .unwrap();
        w.conn()
            .execute(
                "UPDATE runs SET status = 'completed' WHERE task_id = 'dep'",
                [],
            )
            .unwrap();
    }

    pub(in crate::daemon) fn landing(w: &StateWriter, kind: &str, payload: Value) {
        event(w, kind, "makibad", Some("dep"), payload).unwrap();
    }

    /// The clone check is for the merge of the current pull request: not B's after A, B, A; a
    /// merge of A recorded without its commit is not seen in the clone by B's record; a pull
    /// request of another repository with the same number is not landed by the old one's merge
    /// (Issue 345).
    #[test]
    fn the_clone_check_is_for_the_merge_of_the_current_pull_request() {
        let (_dir, w) = org();
        waited_for(&w);
        let reg = |kind: &str, repo: &str, pr: i64, head: &str| {
            super::super::prwatch::register(&w, kind, "dep", repo, pr, head).unwrap();
        };
        let (h1, h2, h3) = ("1".repeat(40), "2".repeat(40), "3".repeat(40));
        reg("pr_opened", "o/p", 11, &h1);
        reg("pr_opened", "o/p", 12, &h2);
        reg("fix_pushed", "o/p", 11, &h1);
        type Check = (String, Option<String>, Option<(String, i64)>);
        let checks = |w: &StateWriter| -> Vec<Check> {
            clone_checks(w)
                .unwrap()
                .into_iter()
                .map(|c| (c.task, c.merge, c.pr))
                .collect()
        };
        let a: Check = ("dep".into(), None, Some(("o/p".into(), 11)));
        assert_eq!(checks(&w), std::slice::from_ref(&a));
        landing(
            &w,
            "pr_merged",
            json!({"repo": "o/p", "pr": 12, "merge_commit": "m12", "task_id": "dep"}),
        );
        landing(
            &w,
            "landed_in_clone",
            json!({"task_id": "dep", "merge_commit": "m12", "base": "main", "base_head": "m12",
                   "repo": "o/p", "pr": 12}),
        );
        assert_eq!(checks(&w), std::slice::from_ref(&a), "B's merge is not A's");
        // A merge of A recorded without its commit (before v11): B's clone record is not its.
        landing(
            &w,
            "pr_merged",
            json!({"repo": "o/p", "pr": 11, "task_id": "dep"}),
        );
        assert_eq!(
            checks(&w),
            std::slice::from_ref(&a),
            "read again for its commit"
        );
        landing(
            &w,
            "landed_in_clone",
            json!({"task_id": "dep", "merge_commit": "m11", "base": "main", "base_head": "m11",
                   "repo": "O/p", "pr": 11}),
        );
        assert!(checks(&w).is_empty(), "A is in the clone");
        // Replaced by new/p#11: the merge of o/p#11 is not its landing.
        reg("pr_opened", "new/p", 11, &h3);
        assert_eq!(
            checks(&w),
            [("dep".into(), None, Some(("new/p".into(), 11)))]
        );
        landing(
            &w,
            "pr_merged",
            json!({"repo": "new/p", "pr": 11, "merge_commit": "mn", "task_id": "dep"}),
        );
        assert_eq!(
            checks(&w),
            [("dep".into(), Some("mn".into()), Some(("new/p".into(), 11)))]
        );
    }

    /// A merge recorded without its commit and a clone record written before Issue 345 (no `repo`
    /// or `pr`): the clone check reads it again and its new record, of this pull request, is
    /// written though one of the same commit exists, so the wait settles (diff review round 1).
    #[test]
    fn a_merge_without_its_commit_and_an_older_clone_record_settle_after_the_check() {
        let (_dir, w) = org();
        waited_for(&w);
        super::super::prwatch::register(&w, "pr_opened", "dep", "o/p", 11, &"1".repeat(40))
            .unwrap();
        landing(
            &w,
            "pr_merged",
            json!({"repo": "o/p", "pr": 11, "task_id": "dep"}),
        );
        landing(
            &w,
            "landed_in_clone",
            json!({"task_id": "dep", "merge_commit": "m11", "base": "main", "base_head": "m11"}),
        );
        assert_eq!(
            clone_checks(&w).unwrap().len(),
            1,
            "the older record is not of it"
        );
        let o = Outcome {
            range: None,
            clones: vec![InClone {
                task: "dep".into(),
                merge: "m11".into(),
                branch: "main".into(),
                head: "m11".into(),
                pr: Some(("o/p".into(), 11)),
            }],
        };
        w.transaction(|w| record(w, 0, None, &o, "fake")).unwrap();
        assert!(clone_checks(&w).unwrap().is_empty(), "its own record");
        w.transaction(|w| record(w, 0, None, &o, "fake")).unwrap();
        assert_eq!(
            count(
                &w,
                "SELECT count(*) FROM events WHERE kind = 'landed_in_clone'",
                []
            )
            .unwrap(),
            2,
            "written once"
        );
        let settled = count(
            &w,
            "SELECT count(*) FROM events WHERE kind = 'ahead_settled'",
            [],
        )
        .unwrap();
        assert_eq!(settled, 1, "the wait settles");
    }

    /// A run `task` of the project naming Issue 1, at work, with its pull request 7 registered.
    fn dependency(w: &StateWriter, task: &str) {
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            title: Some("T"),
            status: Some("in_use"),
            issue_refs: Some(&["p#1"]),
            ..Default::default()
        })
        .unwrap();
        event(
            w,
            "pr_opened",
            "secretary",
            Some(task),
            json!({"repo": "o/p", "pr": 7, "task_id": task}),
        )
        .unwrap();
    }

    fn landed_of(w: &StateWriter) -> Option<Vec<(i64, String, String)>> {
        landings(w.conn(), "p", "On top.\n\nDepends on #1\n")
            .unwrap()
            .map(|l| l.into_iter().map(|l| (l.issue, l.task, l.merge)).collect())
    }

    /// Issue 490, review round 1: a merge recorded without its commit (before Issue 284) holds
    /// the candidate, is checked in the clone by its pull request, and lands on the commit the
    /// clone check found.
    #[test]
    fn a_merge_recorded_without_its_commit_holds_until_the_clone_check_finds_it() {
        let (_dir, w) = org();
        dependency(&w, "d");
        event(
            &w,
            "pr_merged",
            "makibad",
            Some("d"),
            json!({"repo": "o/p", "pr": 7, "task_id": "d"}),
        )
        .unwrap();
        assert_eq!(
            landed_of(&w),
            None,
            "held while the clone check has not seen it"
        );
        let id = active_range(&w, None);
        let read = event(
            &w,
            "range_read",
            "makibad",
            None,
            json!({"range_id": id, "complete": true}),
        )
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO range_candidates (range_id, read_id, position, issue, title, body,
                   admitted) VALUES (?1, ?2, 0, 2, 'On top', 'On top.\n\nDepends on #1\n', 1)",
                params![id, read],
            )
            .unwrap();
        let checks = held_checks(&w).unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].task, "d");
        assert_eq!(checks[0].merge, None);
        assert_eq!(checks[0].pr, Some(("o/p".to_string(), 7)));
        event(
            &w,
            "landed_in_clone",
            "makibad",
            Some("d"),
            json!({"task_id": "d", "merge_commit": "m1", "repo": "o/p", "pr": 7}),
        )
        .unwrap();
        assert_eq!(
            landed_of(&w),
            Some(vec![(1, "d".to_string(), "m1".to_string())])
        );
        assert!(held_checks(&w).unwrap().is_empty());
    }

    /// Issue 490, review round 1: a pull request read closed, then merged with no read of it
    /// open between, holds the candidate until its merge is in the clone.
    #[test]
    fn a_merge_read_after_a_closure_still_holds_until_it_is_in_the_clone() {
        let (_dir, w) = org();
        dependency(&w, "d");
        event(
            &w,
            "pr_closed",
            "makibad",
            Some("d"),
            json!({"repo": "o/p", "pr": 7, "task_id": "d"}),
        )
        .unwrap();
        assert_eq!(
            landed_of(&w),
            Some(vec![]),
            "a closed pull request holds nothing"
        );
        event(
            &w,
            "pr_merged",
            "makibad",
            Some("d"),
            json!({"repo": "o/p", "pr": 7, "task_id": "d", "merge_commit": "m2"}),
        )
        .unwrap();
        assert_eq!(landed_of(&w), None, "held while the clone does not hold m2");
        event(
            &w,
            "landed_in_clone",
            "makibad",
            Some("d"),
            json!({"task_id": "d", "merge_commit": "m2", "repo": "o/p", "pr": 7}),
        )
        .unwrap();
        assert_eq!(
            landed_of(&w),
            Some(vec![(1, "d".to_string(), "m2".to_string())])
        );
    }
}
