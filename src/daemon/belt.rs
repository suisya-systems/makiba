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
//! - The proof of a landing: the run's registered pull request, read merged (`pr_merged`).
//! - The read surface's `ranges[]` (spec section 12, table "Range field").

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
/// A failed read is tried again after this; so is the landing check.
const RETRY_MS: i64 = 60 * 1000;
/// A read that lists this many open Issues or pull requests is not known to be whole.
const ISSUE_LIMIT: usize = 500;
const PR_LIMIT: usize = 200;
/// `gh` returns at most this many of an Issue's labels or comments, of a PR's closing references.
const NESTED_LIMIT: usize = 100;
/// The kinds of a stop of line 1 (belt.md 5.2).
const STOP_KINDS: [&str; 5] = ["design", "scope", "order", "naming", "other"];

/// A run `r` waits only for the person: it has an escalated pending entry (belt.md 4.1). Such a
/// run counts neither against its range's `at_once` nor against `max_concurrent_workers`.
pub const WAITING: &str =
    "EXISTS (SELECT 1 FROM pending wp WHERE wp.run_id = r.id AND wp.status = 'escalated')";

/// `chain`: the range `?1` and every range it replaced (each `ended` `replaced`). Their runs stay
/// theirs (where a run came from), and count for the range that replaced them.
const CHAIN: &str = "WITH RECURSIVE chain(id) AS (
    SELECT ?1
    UNION SELECT g.replaces FROM ranges g JOIN chain c ON g.id = c.id
          JOIN ranges o ON o.id = g.replaces
          WHERE o.status = 'ended' AND o.ended_reason = 'replaced')";

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
                                 replaces, proposed_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project,
                json!(issues).to_string(),
                takes,
                at_once,
                budget,
                head,
                replaces.as_ref().map(|g| g.id),
                actor
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
/// at work, suspended, or failed and not yet closed (a failed launch holds its slot until the
/// secretary closes it, so a broken launch cannot spend the budget), less those waiting only for
/// the person.
fn range_count(w: &StateWriter, id: i64) -> Result<i64> {
    count(
        w,
        &format!(
            "{CHAIN} SELECT count(*) FROM runs r WHERE r.range_id IN (SELECT id FROM chain)
               AND r.status IN ('queued', 'in_use', 'suspended', 'failed') AND NOT {WAITING}"
        ),
        [id],
    )
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

struct Next {
    issue: i64,
    title: String,
    body: String,
    comments: String,
}

/// The first admitted candidate of the range's newest read that the records still allow: not
/// dispatched or skipped by the range or its chain, no run of this task id, no open run of any
/// range or none naming the Issue.
fn next_candidate(w: &StateWriter, id: i64, slug: &str) -> Result<Option<Next>> {
    let sql = format!(
        "{CHAIN} SELECT c.issue, c.title, c.body, c.comments_json FROM range_candidates c
         WHERE c.range_id = ?1 AND c.admitted = 1
           AND NOT EXISTS (SELECT 1 FROM events e WHERE e.kind IN ('belt_dispatched', 'belt_skipped')
                 AND json_extract(e.payload_json, '$.range_id') IN (SELECT id FROM chain)
                 AND json_extract(e.payload_json, '$.issue') = c.issue)
           AND NOT EXISTS (SELECT 1 FROM runs t WHERE t.task_id = 'r' || ?1 || '-' || c.issue)
           AND NOT EXISTS (SELECT 1 FROM runs o
                 JOIN json_each(CASE WHEN json_valid(o.issue_refs) THEN o.issue_refs ELSE '[]' END) j
                 WHERE o.status NOT IN ('completed', 'abandoned') AND j.value = ?2 || '#' || c.issue)
         ORDER BY c.position LIMIT 1"
    );
    Ok(w.conn()
        .query_row(&sql, params![id, slug], |r| {
            Ok(Next {
                issue: r.get(0)?,
                title: r.get(1)?,
                body: r.get(2)?,
                comments: r.get(3)?,
            })
        })
        .optional()?)
}

/// The brief of a range task: the range's head, then the Issue's title, body and comments.
fn brief(head: &str, n: &Next) -> String {
    let mut b = format!(
        "{}\n\nIssue #{}: {}\n\n{}\n",
        head.trim_end(),
        n.issue,
        n.title,
        n.body.trim_end()
    );
    let comments: Vec<Value> = serde_json::from_str(&n.comments).unwrap_or_default();
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
        json!({"range_id": id, "issue": n.issue, "task_id": task, "design_only": false}),
    )?;
    Ok(task)
}

/// Put the next tasks of every active range in while there is room (belt.md 3.4), and end a range
/// whose budget is spent and whose tasks are all closed, or that has nothing left (`used_up`).
/// Called inside the caller's transaction. Returns the task ids dispatched.
pub fn fill(w: &StateWriter, agent: &str) -> Result<Vec<String>> {
    let capacity = super::dispatch::capacity_from_env();
    let ranges: Vec<(i64, i64, i64, String, String)> = {
        let mut q = w.conn().prepare(
            "SELECT g.id, g.at_once, g.budget_tasks, g.brief_head, p.slug FROM ranges g
             JOIN projects p ON p.id = g.project_id
             WHERE g.status = 'active' ORDER BY g.approved_at, g.id",
        )?;
        q.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    let mut out = Vec::new();
    for (id, at_once, budget, head, slug) in ranges {
        let taken =
            |w: &StateWriter| count(w, "SELECT count(*) FROM runs WHERE range_id = ?1", [id]);
        if fresh_read(w, id)?.is_some() {
            while taken(w)? < budget
                && range_count(w, id)? < at_once
                && global_free(w, capacity)? > 0
            {
                let Some(next) = next_candidate(w, id, &slug)? else {
                    break;
                };
                out.push(dispatch(w, agent, id, &slug, &head, &next)?);
            }
        }
        if open_runs(w, id)? > 0 {
            continue;
        }
        if taken(w)? >= budget {
            end_range(w, id, "budget_tasks", "makibad")?;
        } else if fresh_read(w, id)?.is_some()
            && next_candidate(w, id, &slug)?.is_none()
            && count(
                w,
                "SELECT count(*) FROM range_candidates WHERE range_id = ?1
                   AND left_out_why LIKE 'waits_for%'",
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
    /// (task, repo, pr, the pull request read merged)
    landings: Vec<(String, String, i64, Option<Value>)>,
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

/// The belt's pass of `makibad`: the reads, the landing checks and the fill each tick.
pub struct Belt {
    gh: String,
    repos_dir: PathBuf,
    interval_ms: i64,
    agent: String,
    job: Option<Job>,
    last_landing: Option<Instant>,
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
        // The catch-all: a slot freed where no fill ran (a restart, a launch settled).
        w.transaction(|w| fill(w, &self.agent))?;
        if self.job.is_some() {
            return Ok(());
        }
        let due = due(w, self.interval_ms)?;
        let landings = if self
            .last_landing
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(RETRY_MS as u64))
        {
            landing_candidates(w)?
        } else {
            Vec::new()
        };
        if due.is_none() && landings.is_empty() {
            return Ok(());
        }
        if !landings.is_empty() {
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
                let landings = landings
                    .into_iter()
                    .map(|(task, repo, pr)| {
                        let v = gh_json(
                            &gh,
                            &[
                                "pr",
                                "view",
                                &pr.to_string(),
                                "--repo",
                                &format!("github.com/{repo}"),
                                "--json",
                                "state,mergedAt,title",
                            ],
                            &cancel,
                        )
                        .ok()
                        .filter(|v| v["state"].as_str() == Some("MERGED"));
                        (task, repo, pr, v)
                    })
                    .collect();
                if !cancel.load(Ordering::SeqCst) {
                    let _ = tx.send(Outcome { range, landings });
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
    let ranges: Vec<(i64, String, String, Option<String>, String, String)> = {
        let mut q = w.conn().prepare(
            "SELECT g.id, g.status, p.slug, p.origin_url, g.issues_json, g.takes FROM ranges g
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
            ))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    for (id, status, slug, origin, issues, takes) in ranges {
        let trigger: i64 = count(
            w,
            &format!(
                "{CHAIN} SELECT coalesce(max(e.id), 0) FROM events e WHERE
                   (e.kind IN ('range_proposed', 'range_approved', 'range_resumed')
                    AND json_extract(e.payload_json, '$.range_id') = ?1)
                   OR (e.kind = 'run_closed' AND e.run_id IN
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
            };
            return Ok(Some((spec, trigger, activation)));
        }
    }
    Ok(None)
}

/// The completed runs of a range with a registered pull request and no recorded merge.
fn landing_candidates(w: &StateWriter) -> Result<Vec<(String, String, i64)>> {
    let mut q = w.conn().prepare(
        "SELECT r.task_id, p.repo, p.pr FROM runs r
         JOIN pr_watches p ON p.id = (SELECT max(id) FROM pr_watches WHERE run_id = r.id)
         WHERE r.range_id IS NOT NULL AND r.status = 'completed'
           AND NOT EXISTS (SELECT 1 FROM events e WHERE e.kind = 'pr_merged' AND e.run_id = r.id)
         ORDER BY r.id",
    )?;
    Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?)
}

fn gh_json(gh: &str, args: &[&str], cancel: &AtomicBool) -> std::result::Result<Value, String> {
    let text = run_gh(gh, args, cancel).map_err(|e| match e {
        GhError::Missing => format!("{gh} not found"),
        GhError::Cancelled => "cancelled".into(),
        GhError::Failed(m) => m,
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
        if s.listed.contains(&n) || s.takes.matches(i) {
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
}

/// Apply the range to a read: every Issue the list names or the condition matches, admitted or
/// left out with why (belt.md 2.1), in the read's order (goal clause, then number).
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
    let mut out = Vec::new();
    let mut numbers: BTreeSet<i64> = s.listed.iter().copied().collect();
    numbers.extend(
        open.iter()
            .filter(|(_, i)| s.takes.matches(i))
            .map(|(n, _)| *n),
    );
    // The range's Issues in this read: a follow-up of any of them is asked for.
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
        if why.is_none() && !listed {
            // A follow-up found on the way is asked for until step 4 (belt.md Q3): not in the range.
            let follow = refs_on_lines(&body, "follow-up of");
            let after =
                approved_at.is_some_and(|at| i["createdAt"].as_str().is_some_and(|c| c > at));
            if after
                && follow
                    .iter()
                    .any(|f| members.contains(f) || dispatched_in(w, s.id, *f).unwrap_or(false))
            {
                why = Some("follow_up (ask)".into());
            }
        }
        if why.is_none() {
            let deps: Vec<i64> = refs_on_lines(&body, "depends on");
            let waiting: Vec<String> = deps
                .iter()
                .filter(|d| {
                    open.contains_key(d)
                        || f.views
                            .get(d)
                            .cloned()
                            .flatten()
                            .is_none_or(|v| v["state"].as_str() != Some("CLOSED"))
                })
                .map(|d| format!("#{d}"))
                .collect();
            if !waiting.is_empty() {
                why = Some(format!("waits_for {}", waiting.join(" ")));
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
/// landings found, then the fill, in the caller's transaction.
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
                               body, comments_json, admitted, left_out_why)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                            params![
                                s.id,
                                read,
                                pos as i64,
                                c.issue,
                                c.title,
                                c.body,
                                Value::Array(c.comments.clone()).to_string(),
                                c.why.is_none(),
                                c.why
                            ],
                        )?;
                    }
                }
            }
        }
    }
    for (task, repo, pr, merged) in &o.landings {
        let Some(v) = merged else { continue };
        let known = count(
            w,
            "SELECT count(*) FROM events e JOIN runs r ON r.id = e.run_id
             WHERE e.kind = 'pr_merged' AND r.task_id = ?1",
            [task],
        )?;
        if known == 0 {
            event(
                w,
                "pr_merged",
                "makibad",
                Some(task),
                json!({"repo": repo, "pr": pr, "title": v["title"], "merged_at": v["mergedAt"],
                       "task_id": task}),
            )?;
        }
    }
    fill(w, agent)?;
    Ok(())
}

// ---- the read surface ------------------------------------------------------------------------

/// The read of the candidates of range `id`, for the approval and the read surface.
fn candidates(c: &Connection, id: i64) -> rusqlite::Result<Vec<Value>> {
    let mut q = c.prepare(
        "SELECT issue, title, admitted, left_out_why FROM range_candidates
         WHERE range_id = ?1 ORDER BY position",
    )?;
    q.query_map([id], |r| {
        Ok(
            json!({"issue": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                  "admitted": r.get::<_, i64>(2)? == 1,
                  "left_out_why": r.get::<_, Option<String>>(3)?}),
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
                (SELECT count(*) FROM runs WHERE range_id = g.id)
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
                "budget": {"tasks": r.get::<_, i64>(9)?},
                "approved_by": r.get::<_, Option<String>>(10)?,
                "approved_at": r.get::<_, Option<String>>(11)?,
                "approved_read_at": r.get::<_, Option<String>>(13)?,
                "used": {"tasks": r.get::<_, i64>(14)?},
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

fn item(issue: &Value, title: Value, place: &str) -> Value {
    json!({"issue": issue, "title": title, "place": place, "why": null, "stage": null,
           "since": null, "task_id": null, "pr": null, "pr_title": null})
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
        let run: Option<(i64, String, String, Option<String>, String)> = c
            .query_row(
                "SELECT r.id, r.status, r.title, r.completed_at, r.dispatched_at FROM runs r
                 WHERE r.task_id = ?1",
                [&task],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((run, status, title, completed, dispatched)) = run else {
            continue;
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
        if let Some((why, since)) = stop {
            it["place"] = json!("waiting_person");
            it["why"] = json!(why.lines().next().unwrap_or_default());
            it["since"] = json!(since);
        } else {
            match status.as_str() {
                "review" => {
                    it["place"] = json!("review");
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
                    } else if status == "in_use" && stage.as_deref() == Some("verified") {
                        "at_work"
                    } else {
                        "launching"
                    });
                }
            }
        }
        placed.push((eid, it));
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
    let budget_spent = range["used"]["tasks"].as_i64() >= range["budget"]["tasks"].as_i64();
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
mod tests {
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

    fn org() -> (tempfile::TempDir, StateWriter) {
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

    #[test]
    fn only_the_lines_that_start_with_the_word_are_read() {
        let body =
            "Depends on #3 and #4\nThis mentions depends on #5\n  depends on: #6\nFollow-up of #7";
        assert_eq!(refs_on_lines(body, "depends on"), vec![3, 4, 6]);
        assert_eq!(refs_on_lines(body, "follow-up of"), vec![7]);
    }
}
