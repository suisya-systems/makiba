//! The permission relay (Issue 49, M6-8; spec sections 5.5 and 7), carried from paddock's
//! `org-escalation prompt` / `answer` (inventory row L-N3).
//!
//! A worker's prompt episode is the newest `terminal_status` of its run's newest launch, when that
//! launch is `verified` (a launch before it is the startup dialogs' own, dialogs.rs), the status is
//! `blocked` with kind `permission`, and no `terminal_gone` of that spawn is recorded. For an
//! episode with no entry yet, the pass reads the live terminal's screen once it has stayed the same
//! for `SETTLE` and asks the agent module to recognize it: a recognized prompt becomes a `pending`
//! entry of kind `prompt` (`prompt_text`, `prompt_sum`), any other screen a question holding the
//! screen text (`prompt_text` set, no sum), for which no key is ever pressed; `prompt_seen` either
//! way. An entry made from a prompt that is still open or escalated is withdrawn once its episode
//! is over: the status moved, the terminal is gone, or the launch was replaced. `makiba answer`
//! (door.rs) checks the same episode before its one key.

use super::dialogs::SETTLE;
use super::door::sha256_hex;
use crate::agents;
use crate::base::{Base, BlockKind, State, Terminal};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{TAG_SPAWN, worker_agent_id};
use rusqlite::{OptionalExtension, params};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// A `pending` entry made from a prompt, as SQL on `pending p`: kind `prompt`, or a question that
/// holds an unrecognized prompt's screen (`pending add` never sets `prompt_text`).
const FROM_PROMPT: &str = "(p.kind = 'prompt' OR p.prompt_text IS NOT NULL)";
/// How often a pass runs at most.
const EVERY: Duration = Duration::from_secs(1);

/// A run's newest launch and the newest status of its terminal.
pub(super) struct Scene {
    run: i64,
    task: String,
    pub spawn: String,
    pub agent: String,
    /// When the episode began: the time of the newest status when it is blocked on a permission
    /// prompt of a verified launch whose terminal is not gone.
    pub blocked_since: Option<String>,
}

/// The scenes of the runs `filter` (SQL on `runs r`) selects.
fn scenes(w: &StateWriter, filter: &str, p: impl rusqlite::Params) -> Result<Vec<Scene>> {
    let sql = format!(
        "WITH s AS (
           SELECT r.id AS run, r.task_id AS task, l.spawn_id AS spawn, l.agent AS agent,
                  l.stage AS stage,
                  (SELECT e.id FROM events e WHERE e.run_id = r.id AND e.kind = 'terminal_status'
                     AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id
                   ORDER BY e.id DESC LIMIT 1) AS status,
                  EXISTS (SELECT 1 FROM events g WHERE g.run_id = r.id AND g.kind = 'terminal_gone'
                     AND json_extract(g.payload_json, '$.spawn_id') = l.spawn_id) AS gone
           FROM runs r JOIN run_launch l
             ON l.id = (SELECT MAX(id) FROM run_launch WHERE run_id = r.id)
           WHERE {filter})
         SELECT s.run, s.task, s.spawn, s.agent, s.stage = 'verified' AND NOT s.gone
                  AND json_extract(e.payload_json, '$.state') = 'blocked'
                  AND json_extract(e.payload_json, '$.kind') = 'permission',
                e.occurred_at
         FROM s LEFT JOIN events e ON e.id = s.status ORDER BY s.run"
    );
    let mut st = w.conn().prepare(&sql)?;
    let rows = st
        .query_map(p, |r| {
            let blocked: Option<bool> = r.get(4)?;
            let at: Option<String> = r.get(5)?;
            Ok(Scene {
                run: r.get(0)?,
                task: r.get(1)?,
                spawn: r.get(2)?,
                agent: r.get(3)?,
                blocked_since: at.filter(|_| blocked == Some(true)),
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

/// The scene of `task`'s run, if it has a launch.
pub(super) fn scene(w: &StateWriter, task: &str) -> Result<Option<Scene>> {
    Ok(scenes(w, "r.task_id = ?1", [task])?.pop())
}

/// `t` is the live terminal of `spawn`, blocked on a permission prompt.
pub(super) fn blocked_terminal(t: &Terminal, spawn: &str) -> bool {
    t.alive
        && t.tags.get(TAG_SPAWN).map(String::as_str) == Some(spawn)
        && t.status
            .as_ref()
            .is_some_and(|s| s.state == State::Blocked && s.kind == Some(BlockKind::Permission))
}

/// The screen as one text: trailing blanks of each row and trailing empty rows cut.
fn screen_text(screen: &[String]) -> String {
    let rows: Vec<&str> = screen.iter().map(|l| l.trim_end()).collect();
    let end = rows
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(0, |i| i + 1);
    rows[..end].join("\n")
}

pub struct Prompts {
    /// Per task: the screen last read for its episode, and since when it is the same.
    seen: BTreeMap<String, (Vec<String>, Instant)>,
    next: Instant,
}

impl Default for Prompts {
    fn default() -> Self {
        Self::new()
    }
}

impl Prompts {
    pub fn new() -> Prompts {
        Prompts {
            seen: BTreeMap::new(),
            next: Instant::now(),
        }
    }

    /// One pass, at most once a second.
    pub fn pass(&mut self, w: &StateWriter, base: &dyn Base) -> Result<()> {
        if Instant::now() < self.next {
            return Ok(());
        }
        self.next = Instant::now() + EVERY;
        let all = scenes(
            w,
            &format!(
                "r.status NOT IN ('completed','failed','abandoned','suspended')
                 OR EXISTS (SELECT 1 FROM pending p WHERE p.run_id = r.id
                            AND p.status IN ('open','escalated') AND {FROM_PROMPT})"
            ),
            [],
        )?;
        let mut waiting = Vec::new();
        for s in &all {
            // An entry of an episode that is over (or of an earlier one) is withdrawn. Read first:
            // a write statement takes the write lock even when it changes nothing.
            let over = format!(
                "FROM pending p WHERE p.run_id = ?1 AND p.status IN ('open','escalated')
                   AND {FROM_PROMPT} AND (?2 IS NULL OR p.created_at < ?2)"
            );
            let any: Option<i64> = w
                .conn()
                .query_row(
                    &format!("SELECT p.id {over} LIMIT 1"),
                    params![s.run, s.blocked_since],
                    |r| r.get(0),
                )
                .optional()?;
            if any.is_some() {
                w.conn().execute(
                    &format!(
                        "UPDATE pending SET status = 'withdrawn' WHERE id IN (SELECT p.id {over})"
                    ),
                    params![s.run, s.blocked_since],
                )?;
            }
            let Some(since) = &s.blocked_since else {
                continue;
            };
            // A withdrawn entry does not count: the prompt shown now is recorded again.
            let has: Option<i64> = w
                .conn()
                .query_row(
                    &format!(
                        "SELECT p.id FROM pending p WHERE p.run_id = ?1 AND {FROM_PROMPT}
                           AND p.status IN ('open','escalated','answered') AND p.created_at >= ?2
                         LIMIT 1"
                    ),
                    params![s.run, since],
                    |r| r.get(0),
                )
                .optional()?;
            if has.is_none() {
                waiting.push(s);
            }
        }
        self.seen
            .retain(|task, _| waiting.iter().any(|s| &s.task == task));
        for s in waiting {
            // The base's word now: the status may have moved since it was recorded.
            let Ok(Some(t)) = super::delivery::find_terminal(base, &worker_agent_id(&s.task))
            else {
                continue;
            };
            if !blocked_terminal(&t, &s.spawn) {
                continue;
            }
            let Ok(screen) = base.read(&t.id, 0) else {
                continue;
            };
            if !self.settled(&s.task, &screen) {
                continue;
            }
            self.seen.remove(&s.task);
            record(w, s, &screen)?;
        }
        Ok(())
    }

    /// Whether `screen` has read the same for `SETTLE` (a half-drawn prompt is not recorded).
    fn settled(&mut self, task: &str, screen: &[String]) -> bool {
        match self.seen.get(task) {
            Some((was, since)) if was.as_slice() == screen => since.elapsed() >= SETTLE,
            _ => {
                self.seen
                    .insert(task.to_string(), (screen.to_vec(), Instant::now()));
                false
            }
        }
    }
}

/// The entry for the prompt on `screen` and its `prompt_seen`, in one transaction.
fn record(w: &StateWriter, s: &Scene, screen: &[String]) -> Result<()> {
    let view = agents::by_name(&s.agent).and_then(|a| a.recognize_prompt(screen));
    let worker = worker_agent_id(&s.task);
    let (kind, question, text, sum) = match view {
        Some(v) => (
            "prompt",
            format!("{worker} asks for permission:\n{}", v.text),
            v.text.clone(),
            Some(sha256_hex(&v.text)),
        ),
        None => {
            let text = screen_text(screen);
            (
                "question",
                format!(
                    "{worker} shows a permission prompt makiba does not recognize, so no key is \
                     pressed for it. Its screen:\n{text}"
                ),
                text,
                None,
            )
        }
    };
    w.transaction(|w| {
        w.conn().execute(
            "INSERT INTO pending (run_id, kind, question, prompt_text, prompt_sum)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![s.run, kind, question, text, sum],
        )?;
        let id = w.conn().last_insert_rowid();
        let payload = json!({"pending_id": id, "prompt_sum": sum, "key": null});
        w.append_event(&NewEvent {
            kind: "prompt_seen",
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: Some(&s.task),
            ..Default::default()
        })
        .map(drop)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_text_cuts_trailing_blanks_and_empty_rows() {
        let screen: Vec<String> = ["a  ", "", "b", "  ", ""].map(String::from).into();
        assert_eq!(screen_text(&screen), "a\n\nb");
        assert_eq!(screen_text(&[]), "");
    }
}
