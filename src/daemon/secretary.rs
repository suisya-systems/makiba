//! The secretary: launched at `makiba up` (Issue 43, M6-2), renewed and relaunched (Issue 46,
//! M6-5, carried from paddock's `org-start.sh` and `secretary-handover`).
//!
//! The start decision. `makiba up` starts `makibad` with `MAKIBA_SECRETARY=1`, or, when `makibad`
//! already runs, writes a `commands` row (actor `person`, verb `up`, `accepted`) that this pass
//! takes up; either way `makibad` decides whether to launch one: the base is the truth for
//! "already runs". A live terminal of this organization named `secretary`, whose launch is not
//! `failed`, means none is launched; a launch of the secretary still under way is waited for. A
//! finished one, or one whose launch was failed, has its keys revoked and is ended. Then one
//! launch without a run is added (`launch::add_launch`, Issue 166) in `<state>/secretary/`.
//!
//! A renewal is a `renew` row of `commands` that is `accepted`: the secretary's own request (its
//! handover in `args_json`), or `makibad`'s after the secretary was gone (actor `makibad`). Its
//! progress is in the row's `result_json` (`old`, then `new`), so every step is read back from
//! state and a restart of `makibad` goes on where it stopped (spec section 5.3).

use crate::base::{Base, State, Terminal};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{self, TAG_ORG, TAG_SPAWN};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Set to `1` by `makiba up` on the `makibad` it starts, unless `--no-secretary`.
pub const SECRETARY_ENV: &str = "MAKIBA_SECRETARY";

/// The largest handover `renew` takes, and the largest brief a renewal launches with: it goes
/// into the launch arguments (paddock's limit).
pub const HANDOVER_MAX: usize = 32 * 1024;

/// How long a renewal waits for the secretary to be `idle` or `done` (a first proposal).
const IDLE_WAIT_S: f64 = 600.0;
/// The relaunch window after `secretary_gone`, and how many relaunches it holds (first proposals).
const RELAUNCH_WINDOW_S: f64 = 600.0;
const RELAUNCHES: i64 = 2;

const BRIEF: &str = "You are the secretary of this makiba organization: the one agent the person talks to. Your rules are appended to your system prompt. Wait for the person's first request.";

const RESUME: &str = "You are the secretary of this makiba organization, launched to take over from the secretary before you. Your rules are appended to your system prompt. First call the inbox tool: messages that waited for the secretary are there. Workers keep running; do not dispatch their tasks again. Then resume from what follows and tell the person you are back.";

const CUT: &str =
    "\n[cut: call the pending tool with action list, and the status tool, for the rest]";

pub struct Secretary {
    org: String,
    folder: PathBuf,
    agent: String,
    /// `makiba up` asked for the secretary at this start.
    start: bool,
    decided: bool,
}

/// The renewal under way: its row, who asked, and its progress.
struct Renewal {
    id: i64,
    actor: String,
    handover: String,
    age_s: f64,
    old: Option<String>,
    /// `old` was recorded (it may be null: no secretary ran): the ending has begun.
    ending: bool,
    new: Option<String>,
}

impl Secretary {
    /// For the organization of `state`, with the agent module `dispatch` uses; `start` is whether
    /// `makiba up` asked for the secretary at this start (`MAKIBA_SECRETARY=1`).
    pub fn from_env(state: &Path) -> Secretary {
        let org = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
        let agent = std::env::var(super::door::LAUNCH_AGENT_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "claude_code".into());
        Secretary {
            org: org.to_string_lossy().into_owned(),
            folder: state.join("secretary"),
            agent,
            start: std::env::var(SECRETARY_ENV).as_deref() == Ok("1"),
            decided: false,
        }
    }

    /// A renewal under way first; then the start decision when `makiba up` asked; then the
    /// relaunch of a secretary that is gone.
    pub fn pass(&mut self, w: &StateWriter, base: &dyn Base) -> Result<()> {
        if let Some(r) = renewal(w)? {
            return self.renew(w, base, r);
        }
        let up: Option<i64> = w
            .conn()
            .query_row(
                "SELECT id FROM commands WHERE verb = 'up' AND status = 'accepted'
                 ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if (self.start && !self.decided) || up.is_some() {
            if self.decide(w, base)? {
                self.decided = true;
                if let Some(id) = up {
                    finish(w, id, Ok(json!({"secretary": "decided"})), None)?;
                }
            }
            return Ok(());
        }
        self.relaunch(w)
    }

    /// The start decision of Issue 43. True once it is made (one runs, or a launch was added).
    fn decide(&self, w: &StateWriter, base: &dyn Base) -> Result<bool> {
        if in_flight(w)? {
            return Ok(false);
        }
        let Ok(terminals) = base.list() else {
            return Ok(false);
        };
        for t in terminals.iter().filter(|t| self.is_ours(t)) {
            let spawn = t.tags.get(TAG_SPAWN).cloned().unwrap_or_default();
            let failed = w
                .conn()
                .query_row(
                    "SELECT stage = 'failed' FROM run_launch WHERE spawn_id = ?1",
                    [&spawn],
                    |r| r.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false);
            if t.alive && !failed {
                return Ok(true);
            }
            // Keys first: a failed end is tried again by the next pass.
            revoke_all(w)?;
            if let Err(e) = base.end(&t.id) {
                eprintln!(
                    "makibad: cannot end the old secretary terminal {}: {e}",
                    t.id.0
                );
                return Ok(false);
            }
        }
        // No secretary runs: every secretary key left, its terminal gone or not, dies before a new
        // one is issued. A secretary that ran before resumes from what it left.
        revoke_all(w)?;
        let before: bool = w.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM run_launch WHERE role = 'secretary')",
            [],
            |r| r.get(0),
        )?;
        let brief = if before {
            resume_brief(&last_handover(w)?.map_or_else(|| state_text(w), Ok)?)
        } else {
            BRIEF.to_string()
        };
        if let Err(e) = super::launch::add_launch(
            w,
            spec::Role::Secretary,
            &self.folder,
            &brief,
            None,
            &self.agent,
        ) {
            eprintln!("makibad: cannot add the secretary launch: {e}");
            return Ok(false);
        }
        Ok(true)
    }

    /// One step of the renewal `r` (spec section 5.3).
    fn renew(&self, w: &StateWriter, base: &dyn Base, r: Renewal) -> Result<()> {
        if let Some(new) = &r.new {
            let stage: Option<(String, Option<String>)> = w
                .conn()
                .query_row(
                    "SELECT stage, failed_reason FROM run_launch WHERE spawn_id = ?1",
                    [new],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match stage {
                Some((s, _)) if s == "verified" => w.transaction(|w| {
                    let payload = json!({"old": r.old, "new": new});
                    w.append_event(&NewEvent {
                        kind: "secretary_renewed",
                        actor: Some("makibad"),
                        payload: Some(&payload),
                        ..Default::default()
                    })?;
                    finish(w, r.id, Ok(json!({"old": r.old, "new": new})), None)
                }),
                Some((s, reason)) if s == "failed" => {
                    let why = format!(
                        "the new secretary was not verified: {}",
                        reason.unwrap_or_default()
                    );
                    eprintln!("makibad: renewal of the secretary failed: {why}");
                    // The launch stays named, so its handover is still the last one stored.
                    let launch = json!({"old": r.old, "new": new});
                    finish(w, r.id, Err(why), Some(launch))
                }
                Some(_) => Ok(()),
                None => finish(
                    w,
                    r.id,
                    Err("the new secretary's launch is gone".into()),
                    None,
                ),
            }?;
            return Ok(());
        }
        let Ok(terminals) = base.list() else {
            return Ok(());
        };
        let ours: Vec<&Terminal> = terminals.iter().filter(|t| self.is_ours(t)).collect();
        if !r.ending {
            // The secretary's own request waits for it to be idle or done; makibad's relaunch has
            // nothing to wait for.
            if r.actor != "makibad" {
                if r.age_s > IDLE_WAIT_S {
                    let why =
                        "the secretary was not idle or done within 10 minutes; nothing was ended";
                    eprintln!("makibad: renewal of the secretary failed: {why}");
                    return finish(w, r.id, Err(why.into()), None);
                }
                for t in ours.iter().filter(|t| t.alive) {
                    let state = base.get(&t.id).ok().and_then(|t| t.status).map(|s| s.state);
                    if !matches!(state, Some(State::Idle | State::Done)) {
                        return Ok(());
                    }
                }
            }
            let old = ours
                .iter()
                .find(|t| t.alive)
                .or(ours.first())
                .and_then(|t| t.tags.get(TAG_SPAWN).cloned());
            let old = match old {
                Some(o) => Some(o),
                None => w
                    .conn()
                    .query_row(
                        "SELECT spawn_id FROM run_launch WHERE role = 'secretary'
                         ORDER BY id DESC LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                    .optional()?,
            };
            // Recorded before anything is ended: from here the renewal goes on across restarts.
            return progress(w, r.id, &json!({"old": old}));
        }
        for t in &ours {
            if let Err(e) = base.end(&t.id) {
                eprintln!(
                    "makibad: cannot end the old secretary terminal {}: {e}",
                    t.id.0
                );
                return Ok(());
            }
        }
        let brief = resume_brief(&r.handover);
        let folder = self.folder.clone();
        let agent = self.agent.clone();
        // The launch and the progress that names it are one transaction.
        w.transaction(|w| {
            revoke_all(w)?;
            match super::launch::add_launch(w, spec::Role::Secretary, &folder, &brief, None, &agent)
            {
                Ok(new) => progress(w, r.id, &json!({"old": r.old, "new": new})),
                Err(e) => {
                    eprintln!("makibad: cannot add the secretary launch: {e}");
                    Ok(())
                }
            }
        })
    }

    /// After `secretary_gone`, or a secretary launch that failed: one relaunch, by the path of a
    /// renewal, at most twice in ten minutes; then a stop that only a new launch lifts.
    fn relaunch(&self, w: &StateWriter) -> Result<()> {
        if stopped(w)? || in_flight(w)? || !down(w)? {
            return Ok(());
        }
        let recent: i64 = w.conn().query_row(
            "SELECT count(*) FROM commands WHERE actor = 'makibad' AND verb = 'renew'
               AND status != 'rejected'
               AND (julianday('now') - julianday(received_at)) * 86400.0 < ?1",
            [RELAUNCH_WINDOW_S],
            |r| r.get(0),
        )?;
        if recent >= RELAUNCHES {
            eprintln!(
                "makibad: the secretary is gone and was relaunched twice in 10 minutes; no more until makiba up"
            );
            let why = "relaunch limit reached: twice in 10 minutes; run makiba up";
            return insert(w, json!({"reason": "secretary_gone"}), "rejected", Err(why));
        }
        let handover = last_handover(w)?.map_or_else(|| state_text(w), Ok)?;
        eprintln!("makibad: the secretary is gone; relaunching it");
        insert(
            w,
            json!({"handover": handover, "reason": "secretary_gone"}),
            "accepted",
            Ok(()),
        )
    }

    fn is_ours(&self, t: &Terminal) -> bool {
        t.name == spec::AGENT_SECRETARY && t.tags.get(TAG_ORG) == Some(&self.org)
    }
}

/// The oldest `renew` row that is `accepted`.
fn renewal(w: &StateWriter) -> Result<Option<Renewal>> {
    let row: Option<(i64, String, String, Option<String>, f64)> = w
        .conn()
        .query_row(
            "SELECT id, actor, args_json, result_json,
                    (julianday('now') - julianday(received_at)) * 86400.0
             FROM commands WHERE verb = 'renew' AND status = 'accepted' ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    Ok(row.map(|(id, actor, args, result, age_s)| {
        let args: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
        let result: Value = result
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or(Value::Null);
        let text = |v: &Value| v.as_str().map(String::from);
        Renewal {
            id,
            actor,
            handover: args["handover"].as_str().unwrap_or_default().to_string(),
            age_s,
            old: text(&result["result"]["old"]),
            ending: result["result"].get("old").is_some(),
            new: text(&result["result"]["new"]),
        }
    }))
}

/// Write the renewal's progress into its row, which stays `accepted`; the result keeps the shape
/// of a response line, so a retry of the same `idem` is answered from it.
fn progress(w: &StateWriter, id: i64, result: &Value) -> Result<()> {
    let mut result = result.clone();
    result["renewal"] = "accepted".into();
    w.conn().execute(
        "UPDATE commands SET result_json = ?1 WHERE id = ?2",
        params![json!({"ok": true, "result": result}).to_string(), id],
    )?;
    Ok(())
}

/// Finish a command: `done` with `result`, or `failed` (kind `internal`) with the reason; a failed
/// renewal keeps its `launch` beside the error.
fn finish(
    w: &StateWriter,
    id: i64,
    r: std::result::Result<Value, String>,
    launch: Option<Value>,
) -> Result<()> {
    let (status, line, kind) = match r {
        Ok(v) => ("done", json!({"ok": true, "result": v}), None),
        Err(m) => {
            let mut line =
                json!({"ok": false, "error": {"kind": "internal", "message": m, "hint": ""}});
            if let Some(l) = launch {
                line["result"] = l;
            }
            ("failed", line, Some("internal"))
        }
    };
    w.conn().execute(
        "UPDATE commands SET status = ?1, result_json = ?2, error_kind = ?3,
           finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?4",
        params![status, line.to_string(), kind, id],
    )?;
    Ok(())
}

/// A `renew` row of `makibad`: `accepted` (a relaunch), or `rejected` with `why` (the stop).
fn insert(
    w: &StateWriter,
    args: Value,
    status: &str,
    r: std::result::Result<(), &str>,
) -> Result<()> {
    let (line, kind) = match r {
        Ok(()) => (json!({"ok": true, "result": {"renewal": "accepted"}}), None),
        Err(m) => (
            json!({"ok": false, "error": {"kind": "conflict", "message": m, "hint": ""}}),
            Some("conflict"),
        ),
    };
    // One statement, so a renewal the door accepted meanwhile is seen: one at a time.
    w.conn().execute(
        "INSERT INTO commands (actor, verb, args_json, status, result_json, error_kind, finished_at)
         SELECT 'makibad', 'renew', ?1, ?2, ?3, ?4,
                CASE WHEN ?2 != 'accepted' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') END
         WHERE NOT EXISTS (SELECT 1 FROM commands WHERE verb = 'renew' AND status = 'accepted')",
        params![args.to_string(), status, line.to_string(), kind],
    )?;
    Ok(())
}

/// A secretary launch neither `verified` nor `failed`.
fn in_flight(w: &StateWriter) -> Result<bool> {
    Ok(w.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM run_launch
                        WHERE role = 'secretary' AND stage NOT IN ('verified', 'failed'))",
        [],
        |r| r.get(0),
    )?)
}

/// The relaunch was stopped (a `rejected` renew row of `makibad`) and no secretary launch was
/// added since.
pub(crate) fn stopped(w: &StateWriter) -> Result<bool> {
    Ok(w.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM commands c
           WHERE c.actor = 'makibad' AND c.verb = 'renew' AND c.status = 'rejected'
             AND NOT EXISTS (SELECT 1 FROM run_launch l WHERE l.role = 'secretary'
                               AND julianday(l.created_at) > julianday(c.received_at)))",
        [],
        |r| r.get(0),
    )?)
}

/// The current secretary is down: `secretary_gone` is open for the newest secretary terminal, or
/// the newest secretary launch failed.
pub(crate) fn down(w: &StateWriter) -> Result<bool> {
    let gone = w
        .conn()
        .query_row(
            "SELECT kind = 'anomaly_raised' FROM events
             WHERE kind IN ('anomaly_raised', 'anomaly_cleared')
               AND json_extract(payload_json, '$.rule') = 'secretary_gone'
               AND json_extract(payload_json, '$.generation') =
                   (SELECT max(id) FROM agents WHERE agent_id = 'secretary')
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false);
    let failed = w
        .conn()
        .query_row(
            "SELECT stage = 'failed' FROM run_launch WHERE role = 'secretary'
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false);
    Ok(gone || failed)
}

/// The handover of the newest renewal that launched a secretary at the secretary's or the
/// person's request.
fn last_handover(w: &StateWriter) -> Result<Option<String>> {
    Ok(w.conn()
        .query_row(
            "SELECT json_extract(args_json, '$.handover') FROM commands
             WHERE verb = 'renew' AND actor != 'makibad'
               AND json_extract(result_json, '$.result.new') IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .flatten())
}

/// What `state.db` holds open, for a secretary with no handover: the open pending entries and
/// the running tasks.
fn state_text(w: &StateWriter) -> Result<String> {
    let mut text =
        String::from("No handover was left. What state.db holds open:\n\nOpen pending entries:\n");
    let mut q = w.conn().prepare(
        "SELECT p.id, p.status, r.task_id, p.question FROM pending p
         LEFT JOIN runs r ON r.id = p.run_id
         WHERE p.status IN ('open', 'escalated') ORDER BY p.id",
    )?;
    let rows = q.query_map([], |r| {
        Ok(format!(
            "- #{} ({}, task {}): {}\n",
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "-".into()),
            r.get::<_, String>(3)?
        ))
    })?;
    for row in rows {
        text.push_str(&row?);
    }
    text.push_str("\nRunning tasks:\n");
    let mut q = w.conn().prepare(
        "SELECT task_id, title FROM runs WHERE status IN ('in_use', 'review') ORDER BY id",
    )?;
    let rows = q.query_map([], |r| {
        Ok(format!(
            "- {}: {}\n",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?
        ))
    })?;
    for row in rows {
        text.push_str(&row?);
    }
    Ok(text)
}

/// The resume line and `text`, at most `HANDOVER_MAX` bytes in all.
fn resume_brief(text: &str) -> String {
    let mut brief = format!("{RESUME}\n\n{text}");
    if brief.len() > HANDOVER_MAX {
        let mut end = HANDOVER_MAX - CUT.len();
        while !brief.is_char_boundary(end) {
            end -= 1;
        }
        brief.truncate(end);
        brief.push_str(CUT);
    }
    brief
}

/// Revoke every secretary key not yet revoked.
fn revoke_all(w: &StateWriter) -> Result<()> {
    w.conn().execute(
        "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE role = 'secretary' AND revoked_at IS NULL",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::stub::Stub;

    /// A secretary key whose terminal is gone is revoked before a new launch is added.
    #[test]
    fn a_left_secretary_key_is_revoked_before_a_new_launch() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        super::super::door::issue_key(&w, spec::Role::Secretary, "secretary", None, Some("old"))
            .unwrap();
        let mut s = Secretary::from_env(dir.path());
        s.start = true;
        s.pass(&w, &Stub::new()).unwrap();
        let (revoked, launches): (i64, i64) = w
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM agents WHERE revoked_at IS NOT NULL),
                        (SELECT count(*) FROM run_launch WHERE role = 'secretary')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((revoked, launches), (1, 1));
    }

    /// A renewal with no secretary before it (`old` null) goes on to the launch, and a failed
    /// launch keeps its handover as the last one stored.
    #[test]
    fn a_renewal_with_no_secretary_launches_one_and_a_failure_keeps_its_handover() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.conn()
            .execute(
                "INSERT INTO commands (actor, verb, args_json) VALUES ('person', 'renew', ?1)",
                [json!({"handover": "the state"}).to_string()],
            )
            .unwrap();
        let mut s = Secretary::from_env(dir.path());
        let base = Stub::new();
        for _ in 0..3 {
            s.pass(&w, &base).unwrap();
        }
        let new = renewal(&w)
            .unwrap()
            .unwrap()
            .new
            .expect("no launch was added");
        w.conn()
            .execute(
                "UPDATE run_launch SET stage = 'failed', failed_reason = 'x' WHERE spawn_id = ?1",
                [&new],
            )
            .unwrap();
        s.pass(&w, &base).unwrap();
        assert!(renewal(&w).unwrap().is_none());
        assert_eq!(last_handover(&w).unwrap().as_deref(), Some("the state"));
    }

    /// A brief over the limit is cut on a character boundary and says where the rest is.
    #[test]
    fn a_long_brief_is_cut_within_the_limit() {
        let brief = resume_brief(&"\u{e9}".repeat(HANDOVER_MAX));
        assert!(brief.len() <= HANDOVER_MAX);
        assert!(brief.ends_with(CUT));
        assert_eq!(resume_brief("short"), format!("{RESUME}\n\nshort"));
    }
}
