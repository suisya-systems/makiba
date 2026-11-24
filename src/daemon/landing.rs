//! The open test commits told of a landing (Issue 461, docs/design/landing-test-commits.md).
//!
//! A declared test cannot change after its test commit. When a landing brings a new check over
//! `tests/` (PR 437 landed `tools/check_test_executables.py` while PR 454's declared test was
//! waiting for its fix), the open run whose declared test fails that check learns it only at its
//! fix head's CI. This pass tells it at the landing, and runs nothing of the project: git only.
//!
//! - Due, per project: its newest `pr_merged` (of any run) is newer than the `landed_event` of
//!   the project's newest `landing_scanned`. Only that newest merge P is scanned: the tip holds
//!   the older ones.
//! - On a thread (one at a time, its result written by the loop's thread in one transaction):
//!   the default branch fetched into the clone and P's merge seen in it (`belt::in_clone`, the
//!   tip T); the paths changed from the tip scanned last (or P's first parent) to T; with one
//!   under `tools/`, `.github/` or `tests/common/`, each open run of the project other than P's,
//!   with a current pull request, is read: its pull request fetched as the gate does
//!   (`gate::fetch`) and its test commit found (`gate::find_manifest`).
//! - `landing_scanned` is written once per P, also when nothing matched; when it names a run (or
//!   could not read one), a notice to the secretary and one to each named run's worker, who runs
//!   the repository's checks over its declared tests in its own sandbox before its fix. A run
//!   closed, or moved to another pull request, while the scan ran is dropped. Nothing is stopped.
//!
//! No state beside the ledger but the running thread: a restart scans again what was not written.

use super::belt::{current_pr_sql, in_clone};
use super::gate;
use super::gh::run_gh;
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{AGENT_SECRETARY, worker_agent_id};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The folders whose change may bring a check over `tests/` (the person's list, Issue 461).
pub const WATCHED: [&str; 3] = ["tools/", ".github/", "tests/common/"];
/// At most this many changed paths are recorded.
const MATCHED_KEPT: usize = 50;
/// At most this many are named in a notice.
const MATCHED_TOLD: usize = 10;
/// The open statuses of a run (those `prwatch::followed` reads).
const OPEN: &str = "('queued', 'in_use', 'review', 'suspended', 'parked')";

/// A candidate run: task, repository, pull request, newest recorded head.
type Candidate = (String, String, i64, String);

/// A scan due: the project and its newest merge.
#[derive(Clone)]
struct Due {
    project: i64,
    slug: String,
    branch: String,
    landed_event: i64,
    task: String,
    repo: Option<String>,
    pr: Option<i64>,
    merge: String,
    /// `base_head` of the project's newest `landing_scanned`.
    prev: Option<String>,
    runs: Vec<Candidate>,
}

/// A run with a test commit: the candidate, its test commit and its declared paths.
struct Named {
    run: Candidate,
    test_commit: String,
    declared: Vec<String>,
}

/// What a scan found.
struct Scan {
    tip: String,
    from: String,
    matched: Vec<String>,
    tasks: Vec<Named>,
    /// The candidate and why it could not be read.
    not_read: Vec<(Candidate, String)>,
}

struct Job {
    due: Due,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<Option<Scan>>,
}

/// The pass of the loop.
pub struct Landing {
    repos: PathBuf,
    retry: Duration,
    job: Option<Job>,
    /// A project whose scan wrote nothing (no fetch, or the merge not in the tip yet) waits.
    not_before: HashMap<i64, Instant>,
}

impl Landing {
    pub fn from_env(repos: PathBuf) -> Landing {
        let retry = std::env::var(super::belt::LANDING_RETRY_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .map_or(Duration::from_secs(60), Duration::from_millis);
        Landing {
            repos,
            retry,
            job: None,
            not_before: HashMap::new(),
        }
    }

    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        if let Some(job) = &mut self.job {
            let got = match job.rx.try_recv() {
                Ok(s) => Some(s),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(None),
            };
            let Some(scan) = got else {
                return Ok(());
            };
            let mut job = self.job.take().expect("a job is running");
            if let Some(h) = job.handle.take() {
                let _ = h.join();
            }
            match scan {
                Some(scan) => write(w, &job.due, &scan)?,
                None => {
                    self.not_before
                        .insert(job.due.project, Instant::now() + self.retry);
                }
            }
        }
        let now = Instant::now();
        let Some(due) = due(w)?
            .into_iter()
            .find(|d| self.not_before.get(&d.project).is_none_or(|t| *t <= now))
        else {
            return Ok(());
        };
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = {
            let (clone, d, cancel) = (self.repos.join(&due.slug), due.clone(), cancel.clone());
            std::thread::spawn(move || {
                let s = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    scan(&clone, &d, &cancel)
                }))
                .unwrap_or(None);
                let _ = tx.send(s);
            })
        };
        self.job = Some(Job {
            due,
            cancel,
            handle: Some(handle),
            rx,
        });
        Ok(())
    }
}

impl Drop for Landing {
    fn drop(&mut self) {
        if let Some(job) = &mut self.job {
            job.cancel.store(true, Ordering::SeqCst);
            if let Some(h) = job.handle.take() {
                let _ = h.join();
            }
        }
    }
}

/// The scans due, one per project, read from the ledger.
fn due(w: &StateWriter) -> Result<Vec<Due>> {
    let c = w.conn();
    let mut q = c.prepare(
        "SELECT p.id, p.slug, coalesce(p.default_branch, 'main'), e.id, r.task_id,
                json_extract(e.payload_json, '$.repo'), json_extract(e.payload_json, '$.pr'),
                json_extract(e.payload_json, '$.merge_commit')
         FROM events e JOIN runs r ON r.id = e.run_id JOIN projects p ON p.id = r.project_id
         WHERE e.kind = 'pr_merged'
           AND json_extract(e.payload_json, '$.merge_commit') IS NOT NULL
           AND e.id = (SELECT max(e2.id) FROM events e2 JOIN runs r2 ON r2.id = e2.run_id
                       WHERE e2.kind = 'pr_merged' AND r2.project_id = p.id
                         AND json_extract(e2.payload_json, '$.merge_commit') IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM events s JOIN runs rs ON rs.id = s.run_id
                           WHERE s.kind = 'landing_scanned' AND rs.project_id = p.id
                             AND json_extract(s.payload_json, '$.landed_event') >= e.id)
         ORDER BY e.id",
    )?;
    let rows: Vec<Due> = q
        .query_map([], |r| {
            Ok(Due {
                project: r.get(0)?,
                slug: r.get(1)?,
                branch: r.get(2)?,
                landed_event: r.get(3)?,
                task: r.get(4)?,
                repo: r.get(5)?,
                pr: r.get(6)?,
                merge: r.get(7)?,
                prev: None,
                runs: Vec::new(),
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    let mut out = Vec::new();
    for mut d in rows {
        d.prev = c
            .query_row(
                "SELECT json_extract(s.payload_json, '$.base_head') FROM events s
                 JOIN runs rs ON rs.id = s.run_id
                 WHERE s.kind = 'landing_scanned' AND rs.project_id = ?1
                 ORDER BY s.id DESC LIMIT 1",
                [d.project],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        d.runs = candidates(w, d.project, &d.task)?;
        out.push(d);
    }
    Ok(out)
}

/// The open runs of `project` other than `task`'s with a current pull request and a recorded
/// head.
fn candidates(w: &StateWriter, project: i64, task: &str) -> Result<Vec<Candidate>> {
    let mut q = w.conn().prepare(&format!(
        "SELECT r.task_id, {repo}, {pr},
                (SELECT json_extract(h.payload_json, '$.head_oid') FROM events h
                 WHERE h.run_id = r.id AND h.kind IN ('pr_opened', 'fix_pushed')
                 ORDER BY h.id DESC LIMIT 1)
         FROM runs r
         WHERE r.project_id = ?1 AND r.task_id != ?2 AND r.status IN {OPEN}
         ORDER BY r.id",
        repo = current_pr_sql("r", "repo"),
        pr = current_pr_sql("r", "pr"),
    ))?;
    let rows = q
        .query_map(params![project, task], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(t, repo, pr, head)| Some((t, repo?, pr?, head?)))
        .collect())
}

/// The scan on its thread; `None`: nothing to write yet (tried again later).
fn scan(clone: &Path, d: &Due, cancel: &AtomicBool) -> Option<Scan> {
    let tip = in_clone(clone, &d.branch, &d.merge, cancel)?;
    let dir = clone.display().to_string();
    let git = |args: &[&str]| {
        let mut all = vec!["-C", dir.as_str(), "-c", super::run_repo::HOOKS_OFF];
        all.extend_from_slice(args);
        run_gh("git", &all, cancel).ok()
    };
    let from = d
        .prev
        .clone()
        .filter(|p| git(&["merge-base", "--is-ancestor", p, &tip]).is_some())
        .or_else(|| {
            git(&["rev-parse", "--verify", &format!("{}^1", d.merge)]).map(|s| s.trim().to_string())
        })?;
    let changed = git(&["diff", "--name-only", "--no-renames", &from, &tip])?;
    let matched: Vec<String> = changed
        .lines()
        .map(str::trim)
        .filter(|p| WATCHED.iter().any(|w| p.starts_with(w)))
        .map(String::from)
        .collect();
    let mut s = Scan {
        tip: tip.clone(),
        from,
        matched,
        tasks: Vec::new(),
        not_read: Vec::new(),
    };
    if s.matched.is_empty() {
        return Some(s);
    }
    for run in &d.runs {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        let (_, repo, pr, head) = run;
        let read = gate::fetch(clone, repo, *pr, &d.branch)
            .and_then(|_| gate::find_manifest(clone, &tip, head));
        match read {
            Ok(Some((test_commit, declared))) => s.tasks.push(Named {
                run: run.clone(),
                test_commit,
                declared,
            }),
            Ok(None) => {}
            Err(e) => s.not_read.push((run.clone(), e.0)),
        }
    }
    Some(s)
}

fn short(s: &str) -> &str {
    &s[..s.len().min(7)]
}

/// Whether the candidate's run is still open on the same pull request.
fn still(w: &StateWriter, run: &Candidate) -> Result<bool> {
    let n: i64 = w.conn().query_row(
        &format!(
            "SELECT count(*) FROM runs r WHERE r.task_id = ?1 AND r.status IN {OPEN}
               AND lower({repo}) = lower(?2) AND {pr} = ?3",
            repo = current_pr_sql("r", "repo"),
            pr = current_pr_sql("r", "pr"),
        ),
        params![run.0, run.1, run.2],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// The scan's record and notices, in one transaction.
fn write(w: &StateWriter, d: &Due, s: &Scan) -> Result<()> {
    w.transaction(|w| {
        let done: i64 = w.conn().query_row(
            "SELECT count(*) FROM events s JOIN runs rs ON rs.id = s.run_id
             WHERE s.kind = 'landing_scanned' AND rs.project_id = ?1
               AND json_extract(s.payload_json, '$.landed_event') >= ?2",
            params![d.project, d.landed_event],
            |r| r.get(0),
        )?;
        if done > 0 {
            return Ok(());
        }
        let mut tasks = Vec::new();
        for t in &s.tasks {
            if still(w, &t.run)? {
                tasks.push(t);
            }
        }
        let mut not_read = Vec::new();
        for t in &s.not_read {
            if still(w, &t.0)? {
                not_read.push(t);
            }
        }
        let payload = json!({
            "repo": d.repo, "pr": d.pr, "merge_commit": d.merge, "landed_event": d.landed_event,
            "base": d.branch, "base_head": s.tip, "from": s.from,
            "matched": s.matched.iter().take(MATCHED_KEPT).collect::<Vec<_>>(),
            "matched_count": s.matched.len(),
            "tasks": tasks.iter().map(|t| json!({"task_id": t.run.0, "repo": t.run.1,
                "pr": t.run.2, "head": t.run.3, "test_commit": t.test_commit,
                "declared": t.declared})).collect::<Vec<Value>>(),
            "not_read": not_read.iter().map(|t| json!({"task_id": t.0.0, "why": t.1}))
                .collect::<Vec<Value>>(),
        });
        w.append_event(&NewEvent {
            kind: "landing_scanned",
            actor: Some("makibad"),
            payload: Some(&payload),
            run_task_id: Some(&d.task),
            ..Default::default()
        })?;
        if tasks.is_empty() && not_read.is_empty() {
            return Ok(());
        }
        let shown: Vec<&str> = s
            .matched
            .iter()
            .take(MATCHED_TOLD)
            .map(String::as_str)
            .collect();
        let more = s.matched.len().saturating_sub(MATCHED_TOLD);
        let paths = if more > 0 {
            format!("{} and {more} more", shown.join(", "))
        } else {
            shown.join(", ")
        };
        let landed = format!(
            "{}#{} landed (merge {})",
            d.repo.as_deref().unwrap_or("?"),
            d.pr.map_or("?".to_string(), |p| p.to_string()),
            short(&d.merge)
        );
        let mut body = format!(
            "{landed}; origin/{} is now {} and changed {paths} since {}.",
            d.branch,
            short(&s.tip),
            short(&s.from)
        );
        if !tasks.is_empty() {
            let named: Vec<String> = tasks
                .iter()
                .map(|t| {
                    format!(
                        "{} (test commit {}: {})",
                        t.run.0,
                        short(&t.test_commit),
                        t.declared.join(", ")
                    )
                })
                .collect();
            body.push_str(&format!(
                " These open runs have a pushed test commit, whose declared tests cannot change; each is told to run the repository's checks over its declared tests at that tip in its own sandbox before its fix: {}.",
                named.join("; ")
            ));
        }
        if !not_read.is_empty() {
            let unread: Vec<String> = not_read
                .iter()
                .map(|t| format!("{} ({})", t.0.0, t.1))
                .collect();
            body.push_str(&format!(" Not read: {}.", unread.join("; ")));
        }
        body.push_str(" Nothing is stopped.");
        super::door::notice(w, AGENT_SECRETARY, &d.task, &body)?;
        for t in &tasks {
            let body = format!(
                "{landed}; origin/{base} is now {tip}, and it changed {paths}. Your test commit {m} declares {declared}, which cannot change after it. Before you write or push your fix: git fetch origin, and in your sandbox run, at origin/{base} with your declared files as your test commit has them, the repository's checks that read tests/ (the commands of the CI jobs, such as python3 tools/check_test_executables.py). If one fails on your declared files, report blocked naming the check and the lines; else report done in one line: \"checked at {tip}: the declared tests pass the checks\".",
                base = d.branch,
                tip = short(&s.tip),
                m = short(&t.test_commit),
                declared = t.declared.join(", "),
            );
            super::door::notice(w, &worker_agent_id(&t.run.0), &t.run.0, &body)?;
        }
        Ok(())
    })
}
