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
//! secretary and its transport switch (a notice goes through the ledger), the merge watch, the
//! conflict announce and the old-`gh` fallback.

use super::gh::{GhError, SLICE, run_gh};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::AGENT_SECRETARY;
use rusqlite::{OptionalExtension, params};
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
    .is_ok_and(|text| text.lines().any(run_unfinished))
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
}

impl PrWatch {
    /// `$MAKIBA_GH` (default `gh`), `$MAKIBA_PRWATCH_INTERVAL_MS` and `$MAKIBA_PRWATCH_BUDGET_MS`.
    pub fn from_env() -> PrWatch {
        let (tx, rx) = channel();
        PrWatch {
            gh: super::gh::program(),
            interval: millis_env("MAKIBA_PRWATCH_INTERVAL_MS", DEFAULT_INTERVAL),
            budget: millis_env("MAKIBA_PRWATCH_BUDGET_MS", DEFAULT_BUDGET),
            pollers: HashMap::new(),
            tx,
            rx,
            queued: Vec::new(),
        }
    }

    /// Write what the pollers found, then give every `watching` row a poller of its activation.
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
            let cancel = Arc::new(AtomicBool::new(false));
            let handle = {
                let (gh, tx, cancel) = (self.gh.clone(), self.tx.clone(), Arc::clone(&cancel));
                let (interval, budget) = (self.interval, self.budget);
                let done = (id, stamp.clone(), repo.clone(), pr, head.clone());
                std::thread::spawn(move || {
                    let (id, stamp, repo, pr, head) = done;
                    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        poll(&gh, &repo, &head, interval, budget, &cancel)
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
        Ok(())
    }
}

impl Drop for PrWatch {
    /// Every poller is told to stop, its `gh` child is killed and its thread is joined.
    fn drop(&mut self) {
        for p in self.pollers.values() {
            p.cancel.store(true, Ordering::SeqCst);
        }
        for p in self.pollers.values_mut() {
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
                    "The CI watch of {}#{} at {short} ended without a result: {reason}. Run `makiba pr pushed` again for this head to watch it again.",
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
            super::park::unpark(
                w,
                task,
                "ci_failed",
                json!({"repo": d.repo, "pr": d.pr, "head_oid": d.head}),
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
    w.transaction(|w| {
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&json!({"repo": repo, "pr": pr, "head_oid": head})),
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
}
