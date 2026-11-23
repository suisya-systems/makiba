//! The PR watches (Issue M5-3, spec section 5.6): `makibad` owns them, so a watch is never a
//! process nobody can see (I-6) and its end is always written (I-8).
//!
//! `record_pr` is `makiba pr`: the event, the older watches `superseded` and the new row, in the
//! transaction of the door. `PrWatch::pass` makes the pollers a function of the table: every
//! `watching` row has a poller, the loop starts one for any that has none (every row, on start),
//! and a poller's result is written by the loop's thread only, once, for the row and the
//! activation it was read for.
//!
//! The checks are read from `gh` (`$MAKIBA_GH`, the test seam) for the exact head: the check
//! runs and the commit statuses of that commit, and its workflow runs. The reading and the
//! conclusion are carried from claude-org-ja `tools/pr_watch.py`. Not carried: the push to the
//! secretary and its transport switch (a notice goes through the ledger), the conflict announce
//! and the old-`gh` fallback.
//!
//! The pull request's state (Issue 388, in place of claude-org-ja's merge watch): each pull
//! request a run follows (`followed`, rebuilt from the ledger; a run closed with `--abandon` for
//! a day after its close, Issue 385) has one state poller, which reads
//! `gh pr view` each interval and brings back a merge, or a close or a reopen it did not start
//! with. The loop writes it (`apply_state`) only for the generation it was read for: the merge
//! (`pr_merged`, with who merged it and whether the gate allowed that head), the close
//! (`pr_closed`) or the reopen (`pr_reopened`), the pull request's watches ended, and the
//! secretary told of what happened outside makiba. One `gh` call cannot carry both reads: the
//! state call has no workflow runs and no exact head, which the CI reading relies on.

use super::belt::{AWAITED, current_pr_sql, is_current_pr_sql, pr_closed_sql};
use super::gh::{GhError, SLICE, run_gh};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::AGENT_SECRETARY;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);
/// How long an empty or unreadable observation is retried (the carried empty-race budget).
const DEFAULT_BUDGET: Duration = Duration::from_secs(300);

fn millis_env(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(default, Duration::from_millis)
}

// ---- reading the checks (carried) -------------------------------------------------------------

/// `gh`'s bucket for one check run or commit status, as the carried `_rollup_entry_bucket`: a
/// commit status has a `state`, a check run a `status` and, once completed, a `conclusion`.
/// What cannot be read conclusively is `pending`: a `pass` is never made up.
pub fn bucket(entry: &Value) -> &'static str {
    let up = |k: &str| {
        entry
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase()
    };
    let state = up("state");
    if !state.is_empty() {
        return match state.as_str() {
            "SUCCESS" => "pass",
            "FAILURE" | "ERROR" => "fail",
            _ => "pending",
        };
    }
    let status = up("status");
    if !status.is_empty() && status != "COMPLETED" {
        return "pending";
    }
    match up("conclusion").as_str() {
        "SUCCESS" => "pass",
        "SKIPPED" | "NEUTRAL" => "skipping",
        "FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE" => {
            "fail"
        }
        _ => "pending",
    }
}

/// What the checks add up to, as the carried `_summarize_checks`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Summary {
    /// `passed`, `failed` or `incomplete` (nothing, or something not decided yet).
    pub status: &'static str,
    pub fail_count: usize,
    pub pending_count: usize,
    pub total: usize,
}

pub fn summarize(entries: &[Value]) -> Summary {
    let (mut fail_count, mut pending_count) = (0, 0);
    for e in entries {
        match bucket(e) {
            "fail" => fail_count += 1,
            "pass" | "skipping" => {}
            _ => pending_count += 1,
        }
    }
    let status = if fail_count > 0 {
        "failed"
    } else if entries.is_empty() || pending_count > 0 {
        "incomplete"
    } else {
        "passed"
    };
    Summary {
        status,
        fail_count,
        pending_count,
        total: entries.len(),
    }
}

/// A workflow run in one of these statuses will still produce checks (carried #1044): a workflow
/// queued behind a concurrency group has no check run yet, so the checks alone read "all pass".
pub fn run_unfinished(status: &str) -> bool {
    ["queued", "in_progress", "waiting", "requested", "pending"].contains(&status.trim())
}

// ---- calling gh --------------------------------------------------------------------------------

/// The lines of `gh api --paginate --jq` output: one JSON value each.
fn json_lines(text: &str) -> std::result::Result<Vec<Value>, GhError> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|_| GhError::Failed("gh wrote no JSON".into())))
        .collect()
}

/// The checks of `head` and whether a workflow run of it is still going.
fn observe(
    gh: &str,
    repo: &str,
    head: &str,
    cancel: &AtomicBool,
) -> std::result::Result<Vec<Value>, GhError> {
    let runs = run_gh(
        gh,
        &[
            "api",
            "--paginate",
            &format!("repos/{repo}/commits/{head}/check-runs?per_page=100"),
            "--jq",
            ".check_runs[] | {name, status, conclusion}",
        ],
        cancel,
    )?;
    let statuses = run_gh(
        gh,
        &[
            "api",
            "--paginate",
            &format!("repos/{repo}/commits/{head}/status?per_page=100"),
            "--jq",
            ".statuses[] | {context, state}",
        ],
        cancel,
    )?;
    let mut all = json_lines(&runs)?;
    all.extend(json_lines(&statuses)?);
    Ok(all)
}

/// Whether a workflow run of `head` has not finished. Unreadable counts as finished: a hiccup
/// can only fall back to the verdict of the checks, never hold the watch.
fn unfinished_runs(gh: &str, repo: &str, head: &str, cancel: &AtomicBool) -> bool {
    runs_going(gh, repo, head, cancel).unwrap_or(false)
}

/// Whether a workflow run of `head` has not finished, or why it cannot be read. A rerun's watch
/// (Issue 375) takes an unreadable list as unreadable, never as finished.
fn runs_going(
    gh: &str,
    repo: &str,
    head: &str,
    cancel: &AtomicBool,
) -> std::result::Result<bool, GhError> {
    run_gh(
        gh,
        &[
            "api",
            "--paginate",
            &format!("repos/{repo}/actions/runs?head_sha={head}&per_page=100"),
            "--jq",
            ".workflow_runs[].status",
        ],
        cancel,
    )
    .map(|text| text.lines().any(run_unfinished))
}

// ---- one poller --------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Concluded(Summary, u32),
    Failed(String),
}

struct Done {
    id: i64,
    /// The `started_at` of the activation this result is for.
    stamp: String,
    repo: String,
    pr: i64,
    head: String,
    outcome: Outcome,
}

struct Poller {
    stamp: String,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Poller {
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn sleep_unless(cancel: &AtomicBool, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until && !cancel.load(Ordering::SeqCst) {
        std::thread::sleep(SLICE.min(until - Instant::now()));
    }
}

/// The poll of one activation: `poll_rerun` for a rerun of the head's failed jobs (Issue 375),
/// `poll` for any other.
fn poll_activation(
    gh: &str,
    repo: &str,
    head: &str,
    interval: Duration,
    budget: Duration,
    rerun: bool,
    cancel: &AtomicBool,
) -> Option<Outcome> {
    if rerun {
        poll_rerun(gh, repo, head, interval, budget, cancel)
    } else {
        poll(gh, repo, head, interval, budget, cancel)
    }
}

/// Watch `head` until the checks are decided or the watch cannot go on (carried
/// `_self_poll_watch` and `_resolve_final_status`). `None`: cancelled.
fn poll(
    gh: &str,
    repo: &str,
    head: &str,
    interval: Duration,
    budget: Duration,
    cancel: &AtomicBool,
) -> Option<Outcome> {
    let (mut attempts, mut bad_since) = (0u32, None::<Instant>);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        attempts += 1;
        // Why this observation did not settle anything, if it did not.
        let bad = match observe(gh, repo, head, cancel) {
            Err(GhError::Missing) => return Some(Outcome::Failed("gh_missing".into())),
            Err(GhError::Cancelled) => return None,
            Err(GhError::Failed(m)) => Some(format!("gh_unreadable: {m}")),
            Ok(entries) => {
                let s = summarize(&entries);
                if s.total == 0 {
                    Some("no_checks: the head has no check runs and no statuses".to_string())
                } else {
                    bad_since = None;
                    // A failed check with a sibling still running waits for the sibling.
                    if s.pending_count == 0 {
                        if s.status != "passed" {
                            return Some(Outcome::Concluded(s, attempts));
                        }
                        // The checks are read again once no workflow run is going: one that
                        // started and failed meanwhile is in them, and it is the verdict.
                        if !unfinished_runs(gh, repo, head, cancel) {
                            match observe(gh, repo, head, cancel) {
                                Ok(again) => {
                                    let s = summarize(&again);
                                    if s.pending_count == 0 && s.total > 0 {
                                        return Some(Outcome::Concluded(s, attempts));
                                    }
                                }
                                Err(GhError::Cancelled) => return None,
                                Err(_) => {}
                            }
                        }
                    }
                    None
                }
            }
        };
        if let Some(reason) = bad {
            let since = *bad_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= budget {
                return Some(Outcome::Failed(reason));
            }
        }
        sleep_unless(cancel, interval);
    }
}

/// The watch of a rerun (Issue 375). Right after `gh run rerun --failed` the checks may still
/// read the old failed attempt while the workflow run is queued, so a verdict, failed or passed,
/// is taken only from a reading with no pending check, a readable list of workflow runs none of
/// which is going, and a second reading of the checks that is decided; and only once the rerun
/// was seen going (a pending check or an unfinished run), or once the budget has passed since
/// this poller started with decided readings only (the rerun ended before it was recorded, or
/// never started: what is read is the head's result either way). What cannot be read, the runs
/// included, is retried until the budget has passed since the first such reading, and then ends
/// the watch without a result.
fn poll_rerun(
    gh: &str,
    repo: &str,
    head: &str,
    interval: Duration,
    budget: Duration,
    cancel: &AtomicBool,
) -> Option<Outcome> {
    let started = Instant::now();
    let (mut attempts, mut bad_since, mut seen_going) = (0u32, None::<Instant>, false);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        attempts += 1;
        let unreadable = |e: GhError| match e {
            GhError::Missing => Err(Outcome::Failed("gh_missing".into())),
            GhError::Cancelled => Ok(None),
            GhError::Failed(m) => Ok(Some(format!("gh_unreadable: {m}"))),
        };
        // `Err`: the watch ends with that outcome; `Ok(None)` with `cancel` set: cancelled.
        let step = (|| -> std::result::Result<Option<String>, Outcome> {
            let s = match observe(gh, repo, head, cancel) {
                Ok(entries) => summarize(&entries),
                Err(e) => return unreadable(e),
            };
            if s.total == 0 {
                return Ok(Some(
                    "no_checks: the head has no check runs and no statuses".into(),
                ));
            }
            if s.pending_count > 0 {
                seen_going = true;
                return Ok(None);
            }
            match runs_going(gh, repo, head, cancel) {
                Ok(true) => {
                    seen_going = true;
                    return Ok(None);
                }
                Ok(false) => {}
                Err(e) => return unreadable(e),
            }
            if !seen_going && started.elapsed() < budget {
                return Ok(None);
            }
            match observe(gh, repo, head, cancel) {
                Ok(again) => {
                    let s = summarize(&again);
                    if s.pending_count == 0 && s.total > 0 {
                        return Err(Outcome::Concluded(s, attempts));
                    }
                    Ok(None)
                }
                Err(e) => unreadable(e),
            }
        })();
        let bad = match step {
            Err(outcome) => return Some(outcome),
            Ok(bad) => bad,
        };
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        match bad {
            Some(reason) => {
                let since = *bad_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= budget {
                    return Some(Outcome::Failed(reason));
                }
            }
            None => bad_since = None,
        }
        sleep_unless(cancel, interval);
    }
}

// ---- the loop's side ----------------------------------------------------------------------------

pub struct PrWatch {
    gh: String,
    interval: Duration,
    budget: Duration,
    pollers: HashMap<i64, Poller>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    /// Results read from the pollers and not written yet: kept until their transaction commits.
    queued: Vec<Done>,
    /// The state pollers, by task (Issue 388).
    states: HashMap<String, StatePoller>,
    state_tx: Sender<StateDone>,
    state_rx: Receiver<StateDone>,
    /// State results not written yet: kept until their transaction commits.
    state_queued: Vec<StateDone>,
}

impl PrWatch {
    /// `$MAKIBA_GH` (default `gh`), `$MAKIBA_PRWATCH_INTERVAL_MS` and `$MAKIBA_PRWATCH_BUDGET_MS`.
    pub fn from_env() -> PrWatch {
        let (tx, rx) = channel();
        let (state_tx, state_rx) = channel();
        PrWatch {
            gh: super::gh::program(),
            interval: millis_env("MAKIBA_PRWATCH_INTERVAL_MS", DEFAULT_INTERVAL),
            budget: millis_env("MAKIBA_PRWATCH_BUDGET_MS", DEFAULT_BUDGET),
            pollers: HashMap::new(),
            tx,
            rx,
            queued: Vec::new(),
            states: HashMap::new(),
            state_tx,
            state_rx,
            state_queued: Vec::new(),
        }
    }

    /// Write what the pollers found, end the watches of merged pull requests, then give every
    /// `watching` row a poller of its activation and every followed pull request a state poller.
    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        while let Ok(d) = self.rx.try_recv() {
            self.queued.push(d);
        }
        for d in std::mem::take(&mut self.queued) {
            match apply(w, &d) {
                Ok(()) => {
                    if self.pollers.get(&d.id).is_some_and(|p| p.stamp == d.stamp)
                        && let Some(mut p) = self.pollers.remove(&d.id)
                    {
                        p.stop();
                    }
                }
                Err(e) => {
                    eprintln!("makibad: pr watch {}: cannot record its result: {e}", d.id);
                    self.queued.push(d);
                }
            }
        }
        while let Ok(d) = self.state_rx.try_recv() {
            self.state_queued.push(d);
        }
        for d in std::mem::take(&mut self.state_queued) {
            if let Err(e) = apply_state(w, &d) {
                eprintln!(
                    "makibad: pr watch {}#{}: cannot record its state: {e}",
                    d.f.repo, d.f.pr
                );
                self.state_queued.push(d);
            }
        }
        end_merged(w)?;
        let mut stmt = w.conn().prepare(
            "SELECT id, repo, pr, head_oid, started_at FROM pr_watches WHERE status = 'watching'",
        )?;
        let rows: Vec<(i64, String, i64, String, String)> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        // A poller of a row that is not `watching` any more, or of an earlier activation.
        let ids: Vec<i64> = self.pollers.keys().copied().collect();
        for id in ids {
            let current = rows.iter().find(|r| r.0 == id).map(|r| r.4.as_str());
            if self.pollers.get(&id).map(|p| p.stamp.as_str()) != current
                && let Some(mut p) = self.pollers.remove(&id)
            {
                p.stop();
            }
        }
        for (id, repo, pr, head, stamp) in rows {
            if self.pollers.contains_key(&id) {
                continue;
            }
            let rerun = is_rerun(w, id, &stamp)?;
            let cancel = Arc::new(AtomicBool::new(false));
            let handle = {
                let (gh, tx, cancel) = (self.gh.clone(), self.tx.clone(), Arc::clone(&cancel));
                let (interval, budget) = (self.interval, self.budget);
                let done = (id, stamp.clone(), repo.clone(), pr, head.clone());
                std::thread::spawn(move || {
                    let (id, stamp, repo, pr, head) = done;
                    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        poll_activation(&gh, &repo, &head, interval, budget, rerun, &cancel)
                    }));
                    let outcome = match run {
                        Ok(Some(o)) => o,
                        Ok(None) => return,
                        Err(_) => Outcome::Failed("poller_crashed".into()),
                    };
                    let _ = tx.send(Done {
                        id,
                        stamp,
                        repo,
                        pr,
                        head,
                        outcome,
                    });
                })
            };
            self.pollers.insert(
                id,
                Poller {
                    stamp,
                    cancel,
                    handle: Some(handle),
                },
            );
        }
        self.state_pollers(w)
    }

    /// Every followed pull request has one state poller of its generation: one of a task no longer
    /// followed, of another key, or that has ended (its result sent, or crashed) is stopped, and
    /// the missing ones are started.
    fn state_pollers(&mut self, w: &StateWriter) -> Result<()> {
        let now = followed(w.conn(), None)?;
        let tasks: Vec<String> = self.states.keys().cloned().collect();
        for task in tasks {
            let keep = self.states.get(&task).is_some_and(|p| {
                !p.handle.as_ref().is_none_or(JoinHandle::is_finished) && now.contains(&p.key)
            });
            if !keep && let Some(mut p) = self.states.remove(&task) {
                p.stop();
            }
        }
        for f in now {
            if self.states.contains_key(&f.task) {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let handle = {
                let (gh, tx, cancel) =
                    (self.gh.clone(), self.state_tx.clone(), Arc::clone(&cancel));
                let (interval, f) = (self.interval, f.clone());
                std::thread::spawn(move || {
                    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        poll_state(&gh, &f.repo, f.pr, f.closed, interval, &cancel)
                    }));
                    if let Ok(Some(read)) = read {
                        let _ = tx.send(StateDone { f, read });
                    }
                })
            };
            self.states.insert(
                f.task.clone(),
                StatePoller {
                    key: f,
                    cancel,
                    handle: Some(handle),
                },
            );
        }
        Ok(())
    }
}

impl Drop for PrWatch {
    /// Every poller is told to stop, its `gh` child is killed and its thread is joined.
    fn drop(&mut self) {
        for p in self.pollers.values() {
            p.cancel.store(true, Ordering::SeqCst);
        }
        for p in self.states.values() {
            p.cancel.store(true, Ordering::SeqCst);
        }
        for p in self.pollers.values_mut() {
            p.stop();
        }
        for p in self.states.values_mut() {
            p.stop();
        }
    }
}

/// Write one result: the row, the event and the notice, together. Only the activation the poller
/// was started for may conclude the row, so a late result of a superseded watch writes nothing.
fn apply(w: &StateWriter, d: &Done) -> Result<()> {
    w.transaction(|w| {
        let (status, conclusion) = match &d.outcome {
            Outcome::Concluded(s, _) => ("completed", Some(s.status)),
            Outcome::Failed(_) => ("failed", None),
        };
        let n = w.conn().execute(
            "UPDATE pr_watches SET status = ?1, conclusion = ?2,
               finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?3 AND status = 'watching' AND started_at = ?4",
            params![status, conclusion, d.id, d.stamp],
        )?;
        if n == 0 {
            return Ok(());
        }
        let task: Option<String> = w
            .conn()
            .query_row(
                "SELECT r.task_id FROM pr_watches p JOIN runs r ON r.id = p.run_id WHERE p.id = ?1",
                [d.id],
                |r| r.get(0),
            )
            .optional()?;
        let short = &d.head[..d.head.len().min(7)];
        let (kind, payload, body) = match &d.outcome {
            Outcome::Concluded(s, attempts) => (
                "ci_completed",
                json!({"repo": d.repo, "pr": d.pr, "head_oid": d.head, "status": s.status,
                       "fail_count": s.fail_count, "pending_count": s.pending_count,
                       "total_checks": s.total, "probe_attempts": attempts}),
                format!(
                    "CI {} for {}#{} at {short}: {} of {} checks failed.",
                    s.status, d.repo, d.pr, s.fail_count, s.total
                ),
            ),
            Outcome::Failed(reason) => (
                "ci_watch_failed",
                json!({"repo": d.repo, "pr": d.pr, "head_oid": d.head, "reason": reason}),
                format!(
                    "The CI watch of {}#{} at {short} ended without a result: {reason}. Run `makiba pr pushed` again for this head to watch it again (`makiba pr rerun` when its failed jobs were rerun).",
                    d.repo, d.pr
                ),
            ),
        };
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: task.as_deref(),
            ..Default::default()
        })?;
        // A failed CI brings a parked run back with that reason (Issue 283). A watch that ended
        // without a result does not: it goes to the secretary only.
        if let (Outcome::Concluded(s, _), Some(task)) = (&d.outcome, &task)
            && s.status == "failed"
        {
            super::park::wake_with_notice(
                w,
                task,
                "ci_failed",
                json!({"repo": d.repo, "pr": d.pr, "head_oid": d.head}),
                &body,
            )?;
        }
        // The notice (spec 5.4): the event, the message, the pending delivery with no attempt.
        let event = w.append_event(&NewEvent {
            kind: "message_sent",
            actor: Some("makibad"),
            run_task_id: task.as_deref(),
            ..Default::default()
        })?;
        w.conn().execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
             VALUES (?1, 'makibad', ?2, 'notice', (SELECT run_id FROM pr_watches WHERE id = ?3), ?4)",
            params![event, AGENT_SECRETARY, d.id, body],
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
    })
}

// ---- the pull request's state (Issue 388) --------------------------------------------------------

/// The fields of the state read. Issue 387 adds `mergeable,mergeStateStatus` to this same call.
const STATE_FIELDS: &str = "state,mergedAt,mergedBy,headRefOid,mergeCommit,title";

/// A pull request a run follows, and the generation it is followed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Followed {
    pub task: String,
    /// As the run's current registration spells it.
    pub repo: String,
    pub pr: i64,
    /// The newest `pr_opened` or `fix_pushed` of the run (0: none).
    pub gen_reg: i64,
    /// The newest `pr_closed` or `pr_reopened` of the run (0: none).
    pub gen_close: i64,
    /// Read closed unmerged with nothing since (`pr_closed_sql`).
    pub closed: bool,
}

/// The pull requests the runs follow (of `task` only, when given): each run's current pull
/// request (`current_pr_sql`) with no `pr_merged` of it, while the run is open, a completed run of
/// a range, a run an open design-only run waits for (`AWAITED`, not abandoned), or, unless its
/// pull request was read closed, a run outside a range completed, or any run closed with
/// `--abandon` (Issue 385: its work may still land), within a day of its close.
pub(super) fn followed(c: &Connection, task: Option<&str>) -> Result<Vec<Followed>> {
    let mut q = c.prepare(&format!(
        "SELECT r.task_id, {repo}, {pr},
                coalesce((SELECT max(g.id) FROM events g WHERE g.run_id = r.id
                          AND g.kind IN ('pr_opened', 'fix_pushed')), 0),
                coalesce((SELECT max(g.id) FROM events g WHERE g.run_id = r.id
                          AND g.kind IN ('pr_closed', 'pr_reopened')), 0),
                {closed}
         FROM runs r
         WHERE (?1 IS NULL OR r.task_id = ?1)
           AND {repo} IS NOT NULL AND {pr} IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM events e WHERE e.kind = 'pr_merged' AND e.run_id = r.id
                           AND {merged})
           AND (r.status IN ('queued', 'in_use', 'review', 'suspended', 'parked')
                OR (r.range_id IS NOT NULL AND r.status = 'completed')
                OR (r.status != 'abandoned' AND r.task_id IN ({awaited}))
                OR (NOT {closed} AND r.completed_at IS NOT NULL
                    AND julianday('now') - julianday(r.completed_at) <= 1.0
                    AND ((r.range_id IS NULL AND r.status = 'completed')
                         OR r.status = 'abandoned')))
         ORDER BY r.id",
        repo = current_pr_sql("r", "repo"),
        pr = current_pr_sql("r", "pr"),
        closed = pr_closed_sql("r"),
        awaited = AWAITED,
        merged = is_current_pr_sql(
            "r",
            "json_extract(e.payload_json, '$.repo')",
            "json_extract(e.payload_json, '$.pr')"
        ),
    ))?;
    Ok(q.query_map([task], |r| {
        Ok(Followed {
            task: r.get(0)?,
            repo: r.get(1)?,
            pr: r.get(2)?,
            gen_reg: r.get(3)?,
            gen_close: r.get(4)?,
            closed: r.get(5)?,
        })
    })?
    .collect::<std::result::Result<_, _>>()?)
}

/// A state read worth writing, for the pull request and generation it was read for.
struct StateDone {
    f: Followed,
    read: Value,
}

struct StatePoller {
    key: Followed,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StatePoller {
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Read the state of `repo#pr` each interval until it is merged, or read in the other state than
/// the one it was followed in (`closed`). A failed read is read again, without bound; `None`:
/// cancelled.
fn poll_state(
    gh: &str,
    repo: &str,
    pr: i64,
    closed: bool,
    interval: Duration,
    cancel: &AtomicBool,
) -> Option<Value> {
    let (number, place) = (pr.to_string(), format!("github.com/{repo}"));
    let args = [
        "pr",
        "view",
        &number,
        "--repo",
        &place,
        "--json",
        STATE_FIELDS,
    ];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        match run_gh(gh, &args, cancel) {
            Err(GhError::Cancelled) => return None,
            Err(_) => {}
            Ok(text) => {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    let state = v["state"].as_str().unwrap_or("");
                    if matches!(
                        (state, closed),
                        ("MERGED", _) | ("CLOSED", false) | ("OPEN", true)
                    ) {
                        return Some(v);
                    }
                }
            }
        }
        sleep_unless(cancel, interval);
    }
}

/// End the `watching` row `id` (`failed`) with its `ci_watch_failed` and `reason`, no notice.
fn end_row(w: &StateWriter, id: i64, reason: &str) -> Result<()> {
    let (repo, pr, head, task): (String, i64, String, Option<String>) = w.conn().query_row(
        "SELECT p.repo, p.pr, p.head_oid, r.task_id FROM pr_watches p
         LEFT JOIN runs r ON r.id = p.run_id WHERE p.id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let n = w.conn().execute(
        "UPDATE pr_watches SET status = 'failed', conclusion = NULL,
           finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1 AND status = 'watching'",
        [id],
    )?;
    if n == 0 {
        return Ok(());
    }
    w.append_event(&NewEvent {
        kind: "ci_watch_failed",
        actor: Some("makibad"),
        payload: Some(&json!({"repo": repo, "pr": pr, "head_oid": head, "reason": reason})),
        run_task_id: task.as_deref(),
        ..Default::default()
    })?;
    Ok(())
}

/// End every `watching` row of `repo#pr` (the repository in any case) with `reason`.
fn end_watches(w: &StateWriter, repo: &str, pr: i64, reason: &str) -> Result<()> {
    let ids: Vec<i64> = {
        let mut q = w.conn().prepare(
            "SELECT id FROM pr_watches WHERE status = 'watching'
               AND lower(repo) = lower(?1) AND pr = ?2 ORDER BY id",
        )?;
        q.query_map(params![repo, pr], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    for id in ids {
        end_row(w, id, reason)?;
    }
    Ok(())
}

/// The durable end: a `watching` row of a pull request with a `pr_merged` (also one registered
/// after the merge was recorded) ends with the reason `merged`.
fn end_merged(w: &StateWriter) -> Result<()> {
    let ids: Vec<i64> = {
        let mut q = w.conn().prepare(
            "SELECT p.id FROM pr_watches p WHERE p.status = 'watching'
               AND EXISTS (SELECT 1 FROM events e WHERE e.kind = 'pr_merged'
                           AND lower(json_extract(e.payload_json, '$.repo')) = lower(p.repo)
                           AND json_extract(e.payload_json, '$.pr') = p.pr)
             ORDER BY p.id",
        )?;
        q.query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    if ids.is_empty() {
        return Ok(());
    }
    w.transaction(|w| {
        for id in &ids {
            end_row(w, *id, "merged")?;
        }
        Ok(())
    })
}

/// Write one state read, in one transaction, only while its task still follows the same pull
/// request at the same generation. Whether anything was written.
fn apply_state(w: &StateWriter, d: &StateDone) -> Result<bool> {
    w.transaction(|w| {
        let Some(f) = followed(w.conn(), Some(&d.f.task))?.into_iter().next() else {
            return Ok(false);
        };
        if !(f.repo.eq_ignore_ascii_case(&d.f.repo)
            && f.pr == d.f.pr
            && f.gen_reg == d.f.gen_reg
            && f.gen_close == d.f.gen_close)
        {
            return Ok(false);
        }
        let (task, repo, pr, v) = (f.task.as_str(), d.f.repo.as_str(), f.pr, &d.read);
        let event = |kind: &str, payload: Value| -> Result<i64> {
            w.append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(&payload),
                run_task_id: Some(task),
                ..Default::default()
            })
        };
        match v["state"].as_str() {
            Some("MERGED") => {
                let head = v["headRefOid"].as_str();
                let gate: bool = w.conn().query_row(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'acceptance_gate'
                       AND lower(json_extract(payload_json, '$.repo')) = lower(?1)
                       AND json_extract(payload_json, '$.pr') = ?2
                       AND json_extract(payload_json, '$.head') = ?3
                       AND json_extract(payload_json, '$.allow') = 1)",
                    params![repo, pr, head],
                    |r| r.get(0),
                )?;
                let via = if gate { "gate" } else { "hand" };
                event(
                    "pr_merged",
                    json!({"repo": repo, "pr": pr, "title": v["title"], "merged_at": v["mergedAt"],
                           "merge_commit": v["mergeCommit"]["oid"], "task_id": task,
                           "merged_by": v["mergedBy"]["login"], "head_oid": v["headRefOid"],
                           "merged_via": via}),
                )?;
                end_watches(w, repo, pr, "merged")?;
                let at = v["mergedAt"].as_str().unwrap_or("an unknown time");
                // A parked run whose pull request landed is brought back with that reason, and
                // told (Issue 283).
                super::park::wake_with_notice(
                    w,
                    task,
                    "landed",
                    json!({"repo": repo, "pr": pr}),
                    &format!("Your pull request {repo}#{pr} landed (merged at {at})."),
                )?;
                if !gate {
                    let by = v["mergedBy"]["login"]
                        .as_str()
                        .unwrap_or("an unknown account");
                    let merge = v["mergeCommit"]["oid"]
                        .as_str()
                        .map_or("unknown", |m| &m[..m.len().min(7)]);
                    super::verify::notify_secretary(
                        w,
                        Some(task),
                        &format!(
                            "Pull request {repo}#{pr} of task {task} was merged by hand outside makiba's gate (by {by}, at {at}, merge commit {merge}): run the retro and close the task."
                        ),
                    )?;
                }
                Ok(true)
            }
            // A pull request closed without its merge: it will not land as it is (belt.md 4.3).
            Some("CLOSED") if !f.closed => {
                event(
                    "pr_closed",
                    json!({"repo": repo, "pr": pr, "title": v["title"], "task_id": task}),
                )?;
                end_watches(w, repo, pr, "closed")?;
                super::verify::notify_secretary(
                    w,
                    Some(task),
                    &format!(
                        "Pull request {repo}#{pr} of task {task} was closed without a merge outside makiba; if it is reopened, record its head with makiba pr pushed to watch its CI again."
                    ),
                )?;
                Ok(true)
            }
            Some("OPEN") if f.closed => {
                event(
                    "pr_reopened",
                    json!({"repo": repo, "pr": pr, "task_id": task}),
                )?;
                super::verify::notify_secretary(
                    w,
                    Some(task),
                    &format!(
                        "Pull request {repo}#{pr} of task {task} was reopened outside makiba; record its head with makiba pr pushed to watch its CI again."
                    ),
                )?;
                Ok(true)
            }
            _ => Ok(false),
        }
    })
}

// ---- `makiba pr` -------------------------------------------------------------------------------

/// What `makiba pr` did to the row of the head.
#[derive(Debug, PartialEq, Eq)]
pub struct Registered {
    pub watch_id: i64,
    pub status: String,
}

/// `pr opened|pushed`: the event, every other `watching` row of the PR `superseded`, and the row
/// of `head` `watching` (a new row; a `superseded` or `failed` one of the same head is taken up
/// again as a new activation; a `completed` or already `watching` one is left as it is).
pub fn register(
    w: &StateWriter,
    kind: &str,
    task: &str,
    repo: &str,
    pr: i64,
    head: &str,
) -> Result<Registered> {
    register_with(w, kind, task, repo, pr, head, false)
}

/// As `register`; `base_merge` (Issue 358) marks a `fix_pushed` head whose only new commit is a
/// clean merge of the base, which the fix budget of a range task does not count.
pub fn register_with(
    w: &StateWriter,
    kind: &str,
    task: &str,
    repo: &str,
    pr: i64,
    head: &str,
    base_merge: bool,
) -> Result<Registered> {
    let mut payload = json!({"repo": repo, "pr": pr, "head_oid": head});
    if base_merge {
        payload["base_merge"] = json!(true);
    }
    w.transaction(|w| {
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        w.conn().execute(
            "UPDATE pr_watches SET status = 'superseded',
               finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE repo = ?1 AND pr = ?2 AND head_oid != ?3 AND status = 'watching'",
            params![repo, pr, head],
        )?;
        let run_id: Option<i64> = w
            .conn()
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .optional()?;
        let existing: Option<(i64, String)> = w
            .conn()
            .query_row(
                "SELECT id, status FROM pr_watches WHERE repo = ?1 AND pr = ?2 AND head_oid = ?3",
                params![repo, pr, head],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (watch_id, status) = match existing {
            None => {
                w.conn().execute(
                    "INSERT INTO pr_watches (repo, pr, head_oid, run_id) VALUES (?1, ?2, ?3, ?4)",
                    params![repo, pr, head, run_id],
                )?;
                (w.conn().last_insert_rowid(), "watching".to_string())
            }
            Some((id, s)) if s == "superseded" || s == "failed" => {
                w.conn().execute(
                    "UPDATE pr_watches SET status = 'watching', conclusion = NULL, finished_at = NULL,
                       run_id = ?2, started_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE id = ?1",
                    params![id, run_id],
                )?;
                (id, "watching".to_string())
            }
            Some((id, s)) => return Ok(Registered { watch_id: id, status: s }),
        };
        w.append_event(&NewEvent {
            kind: "ci_watch_started",
            actor: Some("makibad"),
            payload: Some(&json!({"repo": repo, "pr": pr, "head_oid": head})),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        Ok(Registered { watch_id, status })
    })
}

/// Whether the activation `stamp` of watch `id` is a rerun (Issue 375): `pr rerun` began it,
/// which its `ci_rerun` event names. Read from the ledger, so it holds across a restart.
fn is_rerun(w: &StateWriter, id: i64, stamp: &str) -> Result<bool> {
    Ok(w.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'ci_rerun'
           AND json_extract(payload_json, '$.watch_id') = ?1
           AND json_extract(payload_json, '$.started_at') = ?2)",
        params![id, stamp],
        |r| r.get(0),
    )?)
}

/// Why `pr rerun` watched nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum RerunRefused {
    /// No watch of the head was ever recorded for the pull request.
    NotRecorded,
    /// The head recorded last for the pull request is this other one.
    NotCurrent(String),
    /// The newest `ci_completed` of the head is passed.
    Passed,
}

/// `pr rerun` (Issue 375): the head recorded last for the pull request, whose CI was decided
/// (`completed`) or could not be read (`failed`), is watched again as a new activation, which
/// `ci_rerun` (with the watch's id and its new `started_at`) marks as a rerun's; a `watching`
/// row is left as it is. No new head is recorded: no `pr_opened` or `fix_pushed`, and no other
/// watch is touched.
pub fn rerun(
    w: &StateWriter,
    task: &str,
    repo: &str,
    pr: i64,
    head: &str,
) -> Result<std::result::Result<Registered, RerunRefused>> {
    w.transaction(|w| {
        let c = w.conn();
        let recorded: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM pr_watches
               WHERE lower(repo) = lower(?1) AND pr = ?2 AND head_oid = ?3)",
            params![repo, pr, head],
            |r| r.get(0),
        )?;
        if !recorded {
            return Ok(Err(RerunRefused::NotRecorded));
        }
        // The head recorded last, with the repository as that record spelled it: `register`
        // keys the rows by that exact spelling, so another spelling's row is an older watch.
        let current: Option<(Option<String>, Option<String>)> = c
            .query_row(
                "SELECT json_extract(payload_json, '$.repo'), json_extract(payload_json, '$.head_oid')
                 FROM events
                 WHERE kind IN ('pr_opened', 'fix_pushed')
                   AND lower(json_extract(payload_json, '$.repo')) = lower(?1)
                   AND json_extract(payload_json, '$.pr') = ?2
                 ORDER BY id DESC LIMIT 1",
                params![repo, pr],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (current_repo, current_head) = current.unwrap_or_default();
        if current_head.as_deref() != Some(head) {
            return Ok(Err(RerunRefused::NotCurrent(current_head.unwrap_or_default())));
        }
        let existing: Option<(i64, String)> = c
            .query_row(
                "SELECT id, status FROM pr_watches WHERE repo = ?1 AND pr = ?2 AND head_oid = ?3",
                params![current_repo.as_deref().unwrap_or(repo), pr, head],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, status)) = existing else {
            return Ok(Err(RerunRefused::NotRecorded));
        };
        if status == "watching" {
            return Ok(Ok(Registered {
                watch_id: id,
                status,
            }));
        }
        let newest: Option<String> = c
            .query_row(
                "SELECT json_extract(payload_json, '$.status') FROM events
                 WHERE kind = 'ci_completed'
                   AND lower(json_extract(payload_json, '$.repo')) = lower(?1)
                   AND json_extract(payload_json, '$.pr') = ?2
                   AND json_extract(payload_json, '$.head_oid') = ?3
                 ORDER BY id DESC LIMIT 1",
                params![repo, pr, head],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if newest.as_deref() == Some("passed") {
            return Ok(Err(RerunRefused::Passed));
        }
        if status == "superseded" {
            // The head recorded last always has its row taken up; kept for a row edited by hand.
            return Ok(Err(RerunRefused::NotCurrent(head.to_string())));
        }
        let run_id: Option<i64> = c
            .query_row("SELECT id FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .optional()?;
        c.execute(
            "UPDATE pr_watches SET status = 'watching', conclusion = NULL, finished_at = NULL,
               run_id = ?2, started_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1",
            params![id, run_id],
        )?;
        let stamp: String = c.query_row(
            "SELECT started_at FROM pr_watches WHERE id = ?1",
            [id],
            |r| r.get(0),
        )?;
        let watched = json!({"repo": repo, "pr": pr, "head_oid": head});
        let mut payload = watched.clone();
        payload["watch_id"] = json!(id);
        payload["started_at"] = json!(stamp);
        for (kind, payload) in [("ci_rerun", &payload), ("ci_watch_started", &watched)] {
            w.append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(payload),
                run_task_id: Some(task),
                ..Default::default()
            })?;
        }
        Ok(Ok(Registered {
            watch_id: id,
            status: "watching".into(),
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(v: Value) -> &'static str {
        bucket(&v)
    }

    #[test]
    fn a_check_run_and_a_commit_status_map_to_the_same_buckets() {
        assert_eq!(
            b(json!({"status": "completed", "conclusion": "success"})),
            "pass"
        );
        assert_eq!(
            b(json!({"status": "completed", "conclusion": "neutral"})),
            "skipping"
        );
        assert_eq!(
            b(json!({"status": "completed", "conclusion": "skipped"})),
            "skipping"
        );
        for c in [
            "failure",
            "cancelled",
            "timed_out",
            "action_required",
            "startup_failure",
            "stale",
        ] {
            assert_eq!(
                b(json!({"status": "completed", "conclusion": c})),
                "fail",
                "{c}"
            );
        }
        for s in ["queued", "in_progress", "waiting", "requested", "pending"] {
            assert_eq!(
                b(json!({"status": s, "conclusion": null})),
                "pending",
                "{s}"
            );
        }
        assert_eq!(
            b(json!({"status": "completed", "conclusion": null})),
            "pending"
        );
        assert_eq!(b(json!({"state": "success"})), "pass");
        assert_eq!(b(json!({"state": "failure"})), "fail");
        assert_eq!(b(json!({"state": "error"})), "fail");
        assert_eq!(b(json!({"state": "pending"})), "pending");
        assert_eq!(b(json!({})), "pending");
    }

    #[test]
    fn the_summary_counts_and_names_the_verdict() {
        let pass = json!({"status": "completed", "conclusion": "success"});
        let skip = json!({"status": "completed", "conclusion": "skipped"});
        let fail = json!({"status": "completed", "conclusion": "failure"});
        let run = json!({"status": "in_progress"});
        let s = |v: &[&Value]| summarize(&v.iter().map(|x| (*x).clone()).collect::<Vec<_>>());
        assert_eq!(s(&[&pass, &skip]).status, "passed");
        assert_eq!(s(&[&pass, &fail]).status, "failed");
        assert_eq!(s(&[&pass, &run]).status, "incomplete");
        assert_eq!(summarize(&[]).status, "incomplete");
        let mixed = s(&[&fail, &run, &pass]);
        assert_eq!(
            (
                mixed.status,
                mixed.fail_count,
                mixed.pending_count,
                mixed.total
            ),
            ("failed", 1, 1, 3)
        );
        assert_eq!(
            s(&[&json!({"status": "completed", "conclusion": "weird"})]).status,
            "incomplete"
        );
    }

    #[test]
    fn only_a_run_that_will_still_produce_checks_is_unfinished() {
        for s in [
            "queued",
            "in_progress",
            "waiting",
            "requested",
            "pending",
            " queued ",
        ] {
            assert!(run_unfinished(s), "{s}");
        }
        for s in ["completed", "action_required", ""] {
            assert!(!run_unfinished(s), "{s}");
        }
    }

    /// A `gh` stand-in in a folder of its own: `body` is its shell script.
    fn stand_in(body: &str) -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let program = path.display().to_string();
        (dir, program)
    }

    /// A workflow that starts and fails while the run list is read is in the checks read after
    /// it: the verdict is not the green snapshot taken before.
    #[test]
    fn the_checks_are_read_again_once_no_workflow_run_is_going() {
        let (dir, gh) = stand_in(
            r#"d=$(dirname "$0")
case "$3" in
  *check-runs*) if [ -e "$d/late" ]; then echo '{"name":"late","status":"completed","conclusion":"failure"}'; fi
                echo '{"name":"early","status":"completed","conclusion":"success"}' ;;
  *actions/runs*) touch "$d/late"; echo completed ;;
esac"#,
        );
        let _keep = dir;
        let cancel = AtomicBool::new(false);
        let out = poll(
            &gh,
            "o/n",
            "abc",
            Duration::from_millis(10),
            Duration::from_secs(5),
            &cancel,
        );
        match out {
            Some(Outcome::Concluded(s, _)) => {
                assert_eq!((s.status, s.fail_count, s.total), ("failed", 1, 2));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A descendant that keeps the pipe open after `gh` exited does not hold a stop.
    #[test]
    fn a_stop_is_not_held_by_a_descendant_that_keeps_the_output_open() {
        let (dir, gh) = stand_in(r#"sleep 30 & echo $! > "$(dirname "$0")/pid"; exit 0"#);
        let cancel = Arc::new(AtomicBool::new(false));
        let stopper = {
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::SeqCst);
            })
        };
        let started = Instant::now();
        let r = run_gh(&gh, &["api"], &cancel);
        assert!(matches!(r, Err(GhError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(10));
        stopper.join().unwrap();
        // The descendant went with it: no process is left behind.
        let pid: i32 = std::fs::read_to_string(dir.path().join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only asks whether the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the descendant is still there"
        );
    }

    /// A failed CI of a parked run's pull request wakes it with the reason `ci_failed` and leaves
    /// its worker a notice with the result; a watch that ended without a result wakes nothing
    /// (Issue 283).
    #[test]
    fn a_failed_ci_wakes_a_parked_run_and_tells_its_worker() {
        for (outcome, woken) in [
            (
                Outcome::Concluded(
                    Summary {
                        status: "failed",
                        fail_count: 1,
                        pending_count: 0,
                        total: 2,
                    },
                    1,
                ),
                true,
            ),
            (Outcome::Failed("gone".into()), false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
            w.upsert_run(&crate::db::writer::RunUpsert {
                task_id: "t",
                project_slug: "p",
                status: Some("in_use"),
                ..Default::default()
            })
            .unwrap();
            w.conn()
                .execute_batch(
                    "UPDATE runs SET status = 'parked';
                     INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
                     VALUES (1, 's1', 'fake', 'b', '/w', 'verified', 'c1');
                     INSERT INTO pending (run_id, kind, question, status) VALUES (1, 'question', 'q', 'escalated');
                     INSERT INTO pr_watches (repo, pr, head_oid, run_id) VALUES ('o/r', 5, 'abc', 1);",
                )
                .unwrap();
            let stamp: String = w
                .conn()
                .query_row("SELECT started_at FROM pr_watches", [], |r| r.get(0))
                .unwrap();
            apply(
                &w,
                &Done {
                    id: 1,
                    stamp,
                    repo: "o/r".into(),
                    pr: 5,
                    head: "abc".into(),
                    outcome,
                },
            )
            .unwrap();
            let n = |sql: &str| -> i64 { w.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
            let status: String = w
                .conn()
                .query_row("SELECT status FROM runs", [], |r| r.get(0))
                .unwrap();
            assert_eq!(status, if woken { "suspended" } else { "parked" });
            assert_eq!(
                n("SELECT count(*) FROM events WHERE kind = 'run_unparked'
                   AND json_extract(payload_json, '$.reason') = 'ci_failed'"),
                woken as i64
            );
            assert_eq!(
                n(
                    "SELECT count(*) FROM messages WHERE recipient = 'w-t' AND body LIKE 'CI failed%'"
                ),
                woken as i64
            );
        }
    }

    /// A rerun takes up the watch of the head recorded last, as that record spelled the
    /// repository: the rows are keyed by the exact spelling, so the same head recorded earlier
    /// under another case is an older row and is left as it is (Issue 375).
    #[test]
    fn a_rerun_takes_up_the_row_of_the_spelling_recorded_last() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&crate::db::writer::RunUpsert {
            task_id: "t",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        let end = |id: i64| {
            w.conn()
                .execute(
                    "UPDATE pr_watches SET status = 'completed', conclusion = 'failed' WHERE id = ?1",
                    [id],
                )
                .unwrap();
        };
        let old = register(&w, "pr_opened", "t", "O/r", 5, "abc").unwrap();
        end(old.watch_id);
        let new = register(&w, "fix_pushed", "t", "o/r", 5, "abc").unwrap();
        assert_ne!(old.watch_id, new.watch_id);
        end(new.watch_id);
        for asked in ["o/r", "O/R"] {
            w.conn()
                .execute(
                    "UPDATE pr_watches SET status = 'completed' WHERE id = ?1",
                    [new.watch_id],
                )
                .unwrap();
            let r = rerun(&w, "t", asked, 5, "abc").unwrap().unwrap();
            assert_eq!(r.watch_id, new.watch_id);
            assert_eq!(r.status, "watching");
            let status = |id: i64| -> String {
                w.conn()
                    .query_row("SELECT status FROM pr_watches WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })
                    .unwrap()
            };
            assert_eq!(status(old.watch_id), "completed");
            assert_eq!(status(new.watch_id), "watching");
        }
    }

    // ---- the pull request's state (Issue 388; moved from the belt's landing check) ------------

    use super::super::belt::ahead_of;
    use super::super::belt::tests::{active_range, landing, org, run_of, waited_for, watch};

    fn event(w: &StateWriter, kind: &str, actor: &str, task: Option<&str>, payload: Value) {
        w.append_event(&NewEvent {
            kind,
            actor: Some(actor),
            payload: Some(&payload),
            run_task_id: task,
            ..Default::default()
        })
        .unwrap();
    }

    fn count(w: &StateWriter, sql: &str, p: impl rusqlite::Params) -> Result<i64> {
        Ok(w.conn().query_row(sql, p, |r| r.get(0))?)
    }

    /// The followed pull requests as (task, repo, pr).
    fn candidates(w: &StateWriter) -> Vec<(String, String, i64)> {
        followed(w.conn(), None)
            .unwrap()
            .into_iter()
            .map(|f| (f.task, f.repo, f.pr))
            .collect()
    }

    /// `read` of `repo#pr` for `task`, read at the task's current generation, written as the loop
    /// writes a state poller's result.
    fn seen_read(w: &StateWriter, task: &str, repo: &str, pr: i64, read: Value) -> bool {
        let now = followed(w.conn(), Some(task)).unwrap().into_iter().next();
        let f = Followed {
            task: task.into(),
            repo: repo.into(),
            pr,
            gen_reg: now.as_ref().map_or(0, |f| f.gen_reg),
            gen_close: now.as_ref().map_or(0, |f| f.gen_close),
            closed: now.as_ref().is_some_and(|f| f.closed),
        };
        apply_state(w, &StateDone { f, read }).unwrap()
    }

    /// A parked run outside every range is followed, and its pull request read merged writes
    /// `pr_merged`, wakes it with the reason `landed` and tells its worker (Issue 283).
    #[test]
    fn a_landing_wakes_a_parked_run_and_tells_its_worker() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&crate::db::writer::RunUpsert {
            task_id: "t",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute_batch(
                "UPDATE runs SET status = 'parked';
                 INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage, conversation_id)
                 VALUES (1, 's1', 'fake', 'b', '/w', 'verified', 'c1');
                 INSERT INTO pending (run_id, kind, question, status) VALUES (1, 'question', 'q', 'escalated');
                 INSERT INTO pr_watches (repo, pr, head_oid, run_id) VALUES ('o/r', 5, 'abc', 1);",
            )
            .unwrap();
        assert_eq!(candidates(&w), [("t".to_string(), "o/r".to_string(), 5)]);
        seen_read(
            &w,
            "t",
            "o/r",
            5,
            json!({"state": "MERGED", "title": "T", "mergedAt": "2026-11-23T00:00:00Z"}),
        );
        let n = |sql: &str| -> i64 { w.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(n("SELECT count(*) FROM events WHERE kind = 'pr_merged'"), 1);
        assert_eq!(n("SELECT count(*) FROM runs WHERE status = 'suspended'"), 1);
        assert_eq!(
            n("SELECT count(*) FROM events WHERE kind = 'run_unparked'
               AND json_extract(payload_json, '$.reason') = 'landed'"),
            1
        );
        assert_eq!(
            n(
                "SELECT count(*) FROM messages WHERE recipient = 'w-t' AND body LIKE '%o/r#5 landed%'"
            ),
            1
        );
        assert!(candidates(&w).is_empty());
    }

    /// The pull request of a task a design-only run waits for is read whatever its status (here
    /// `failed`, which nothing else follows), so that one closed unmerged is seen; a closed one is
    /// read on, and read open again it is reopened.
    #[test]
    fn the_pull_request_of_an_awaited_task_is_read_whatever_its_status_until_closed() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "r1-1");
        run_of(&w, id, "r1-2");
        event(
            &w,
            "belt_dispatched",
            "makibad",
            Some("r1-1"),
            json!({"range_id": id, "issue": 1, "task_id": "r1-1", "design_only": false}),
        );
        w.conn()
            .execute(
                "UPDATE runs SET status = 'failed' WHERE task_id = 'r1-1'",
                [],
            )
            .unwrap();
        watch(&w, "r1-1", 11, &"a".repeat(40));
        assert!(candidates(&w).is_empty(), "nothing waits for it");
        event(
            &w,
            "belt_dispatched",
            "makibad",
            Some("r1-2"),
            json!({"range_id": id, "issue": 2, "task_id": "r1-2", "design_only": true,
                   "waits_for": [1], "waits_tasks": ["r1-1"]}),
        );
        let read = |w: &StateWriter| -> Vec<i64> {
            candidates(w).into_iter().map(|(_, _, pr)| pr).collect()
        };
        assert_eq!(read(&w), [11]);
        let seen = |w: &StateWriter, state: &str| {
            seen_read(
                w,
                "r1-1",
                "o/p",
                11,
                json!({"state": state, "title": "T", "mergedAt": "2026-11-23T00:00:00Z",
                        "mergeCommit": {"oid": "m1"}}),
            );
        };
        let kinds = |w: &StateWriter, kind: &str| -> i64 {
            count(w, "SELECT count(*) FROM events WHERE kind = ?1", [kind]).unwrap()
        };
        seen(&w, "CLOSED");
        seen(&w, "CLOSED");
        assert_eq!(kinds(&w, "pr_closed"), 1, "closed once");
        assert!(ahead_of(w.conn(), id, 1).unwrap().is_none());
        assert_eq!(read(&w), [11], "awaited: read on while closed");
        // Back in review, so that `ahead_of` may take it once reopened.
        w.conn()
            .execute(
                "UPDATE runs SET status = 'review' WHERE task_id = 'r1-1'",
                [],
            )
            .unwrap();
        seen(&w, "OPEN");
        assert_eq!(kinds(&w, "pr_reopened"), 1);
        assert_eq!(ahead_of(w.conn(), id, 1).unwrap().as_deref(), Some("r1-1"));
        seen(&w, "MERGED");
        assert_eq!(
            kinds(&w, "pr_merged"),
            1,
            "the same pull request reopened, then merged"
        );
        assert_eq!(read(&w), Vec::<i64>::new(), "merged: not read again");
    }

    /// A parked run whose pull request was read closed is still read: reopened and merged on
    /// GitHub with nothing recorded again, its landing is seen and wakes it.
    #[test]
    fn a_parked_runs_pull_request_closed_then_reopened_and_merged_is_seen() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "r1-1");
        watch(&w, "r1-1", 11, &"a".repeat(40));
        w.conn()
            .execute_batch(
                "UPDATE runs SET status = 'parked' WHERE task_id = 'r1-1';
                 INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage,
                                         conversation_id)
                 VALUES ((SELECT id FROM runs WHERE task_id = 'r1-1'), 's1', 'fake', 'b', '/w',
                         'verified', 'c1');",
            )
            .unwrap();
        let seen = |w: &StateWriter, state: &str| {
            seen_read(
                w,
                "r1-1",
                "o/p",
                11,
                json!({"state": state, "title": "T", "mergedAt": "2026-11-23T00:00:00Z",
                        "mergeCommit": {"oid": "m1"}}),
            );
        };
        seen(&w, "CLOSED");
        assert_eq!(
            candidates(&w).len(),
            1,
            "a parked run is read on while its pull request is closed"
        );
        seen(&w, "MERGED");
        assert_eq!(
            count(
                &w,
                "SELECT count(*) FROM events WHERE kind = 'pr_merged'",
                []
            )
            .unwrap(),
            1
        );
        assert_eq!(
            count(
                &w,
                "SELECT count(*) FROM runs WHERE task_id = 'r1-1' AND status = 'suspended'",
                []
            )
            .unwrap(),
            1,
            "woken by its landing"
        );
    }

    /// A completed range task whose pull request was read closed is still read: reopened and
    /// merged on GitHub with nothing recorded again, its landing is recorded (`landed`, not
    /// `closed`), as before closed pull requests were recorded.
    #[test]
    fn a_completed_tasks_pull_request_closed_then_reopened_and_merged_is_seen() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "r1-1");
        watch(&w, "r1-1", 11, &"a".repeat(40));
        w.conn()
            .execute(
                "UPDATE runs SET status = 'completed' WHERE task_id = 'r1-1'",
                [],
            )
            .unwrap();
        let seen = |w: &StateWriter, state: &str| {
            seen_read(
                w,
                "r1-1",
                "o/p",
                11,
                json!({"state": state, "title": "T", "mergedAt": "2026-11-23T00:00:00Z",
                        "mergeCommit": {"oid": "m1"}}),
            );
        };
        seen(&w, "CLOSED");
        assert_eq!(
            candidates(&w).len(),
            1,
            "a completed task is read on while its pull request is closed"
        );
        seen(&w, "MERGED");
        let merged = |w: &StateWriter| {
            count(
                w,
                "SELECT count(*) FROM events WHERE kind = 'pr_merged'",
                [],
            )
            .unwrap()
        };
        assert_eq!(merged(&w), 1);
        assert!(candidates(&w).is_empty(), "merged: read no more");
    }

    /// A state read on the thread for a pull request the task has replaced since is dropped: it
    /// is no longer the task's own.
    #[test]
    fn a_landing_read_of_a_replaced_pull_request_is_dropped() {
        let (_dir, w) = org();
        let id = active_range(&w, None);
        run_of(&w, id, "r1-1");
        watch(&w, "r1-1", 11, &"a".repeat(40));
        watch(&w, "r1-1", 12, &"b".repeat(40));
        seen_read(
            &w,
            "r1-1",
            "o/p",
            11,
            json!({"state": "MERGED", "title": "T", "mergeCommit": {"oid": "m1"}}),
        );
        assert_eq!(
            count(
                &w,
                "SELECT count(*) FROM events WHERE kind = 'pr_merged'",
                []
            )
            .unwrap(),
            0
        );
    }

    /// A, then B, then A again (A's watch row is left older than B's): A is the current pull
    /// request of the state read, of `ahead_of` and of the record of a landing, and a merge of B
    /// is not the landing (Issue 345).
    #[test]
    fn a_pull_request_registered_again_is_current_for_every_reader() {
        let (_dir, w) = org();
        waited_for(&w);
        let reg = |kind: &str, pr: i64, head: &str| {
            register(&w, kind, "dep", "o/p", pr, head).unwrap();
        };
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        reg("pr_opened", 11, &h1);
        reg("pr_opened", 12, &h2);
        reg("fix_pushed", 11, &h1);
        assert_eq!(candidates(&w), [("dep".into(), "o/p".into(), 11)]);
        let range: i64 = w
            .conn()
            .query_row("SELECT range_id FROM runs WHERE task_id = 'dep'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            ahead_of(w.conn(), range, 1).unwrap().as_deref(),
            Some("dep")
        );
        // A read of B merged, in flight while A was registered again: not recorded.
        let read = |w: &StateWriter, pr: i64, merge: &str| {
            seen_read(
                w,
                "dep",
                "o/p",
                pr,
                json!({"state": "MERGED", "title": "T", "mergedAt": "2026-11-23T00:00:00Z",
                        "mergeCommit": {"oid": merge}}),
            );
        };
        read(&w, 12, "m12");
        let merges = |w: &StateWriter| {
            count(
                w,
                "SELECT count(*) FROM events WHERE kind = 'pr_merged'",
                [],
            )
            .unwrap()
        };
        assert_eq!(merges(&w), 0, "B is not current");
        // B's merge recorded before A came back is not A's landing either.
        landing(
            &w,
            "pr_merged",
            json!({"repo": "o/p", "pr": 12, "merge_commit": "m12", "task_id": "dep"}),
        );
        assert_eq!(candidates(&w), [("dep".into(), "o/p".into(), 11)]);
        read(&w, 11, "m11");
        assert_eq!(merges(&w), 2, "A's landing");
        assert!(candidates(&w).is_empty());
    }

    /// A state read whose generation is not the task's current one writes nothing: a merge read
    /// before the task recorded a newer head of the same pull request is dropped, and the same
    /// read at the current generation is written (Issue 388).
    #[test]
    fn a_state_read_of_a_stale_generation_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&crate::db::writer::RunUpsert {
            task_id: "t",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        register(&w, "pr_opened", "t", "o/r", 5, &"1".repeat(40)).unwrap();
        let old = followed(w.conn(), Some("t")).unwrap().remove(0);
        register(&w, "fix_pushed", "t", "o/r", 5, &"2".repeat(40)).unwrap();
        let now = followed(w.conn(), Some("t")).unwrap().remove(0);
        assert!(now.gen_reg > old.gen_reg);
        let read = json!({"state": "MERGED", "title": "T", "mergedAt": "2026-11-23T00:00:00Z",
                          "mergedBy": {"login": "owner"}, "headRefOid": "2".repeat(40),
                          "mergeCommit": {"oid": "m"}});
        let merges = || -> i64 {
            count(
                &w,
                "SELECT count(*) FROM events WHERE kind = 'pr_merged'",
                [],
            )
            .unwrap()
        };
        let stale = StateDone {
            f: old,
            read: read.clone(),
        };
        assert!(!apply_state(&w, &stale).unwrap(), "stale: nothing written");
        assert_eq!(merges(), 0);
        let mut closed_since = now.clone();
        closed_since.gen_close += 1;
        assert!(
            !apply_state(
                &w,
                &StateDone {
                    f: closed_since,
                    read: read.clone()
                }
            )
            .unwrap(),
            "another close generation: nothing written"
        );
        assert!(apply_state(&w, &StateDone { f: now, read }).unwrap());
        assert_eq!(merges(), 1);
        let ended: i64 = count(
            &w,
            "SELECT count(*) FROM pr_watches WHERE status = 'failed'",
            [],
        )
        .unwrap();
        assert_eq!(ended, 1, "the watching row of the current head ended");
    }
}
