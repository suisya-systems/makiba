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
//!   tip T); the paths changed from the tip scanned last (with none, from the common ancestor
//!   of the first parents of every merge not scanned yet) to T; with one
//!   under `tools/`, `.github/` or `tests/common/`, each open run of the project other than P's,
//!   with a current pull request, is read: its pull request fetched as the gate does
//!   (`gate::fetch_with`, killed at makibad's stop) and its test commit found
//!   (`gate::find_manifest`).
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
/// How long a merge recorded but not yet in the fetched tip holds the scan.
const MERGE_WAIT: Duration = Duration::from_secs(60 * 60);
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
    /// The merge commits of every merge not scanned yet (P's among them): with no tip scanned
    /// before, the paths are read from the common ancestor of their first parents.
    unscanned: Vec<String>,
    /// Those recorded within `MERGE_WAIT`: the scan waits until the tip holds each of them, so a
    /// merge recorded before P but landing after it is not passed over. An older one that is not
    /// in the tip (a pull request into another branch) does not hold the scan.
    waiting: Vec<String>,
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
                unscanned: Vec::new(),
                waiting: Vec::new(),
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
        // Every merge not scanned yet, whatever order they were recorded in: the order of the
        // records need not be the order of the merges.
        let unscanned: Vec<(String, bool)> = c
            .prepare(
                "SELECT json_extract(e.payload_json, '$.merge_commit'),
                        julianday('now') - julianday(e.occurred_at) < ?2 / 86400.0
                 FROM events e
                 JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'pr_merged' AND r.project_id = ?1
                   AND json_extract(e.payload_json, '$.merge_commit') IS NOT NULL
                   AND e.id > coalesce((SELECT max(json_extract(s.payload_json, '$.landed_event'))
                                        FROM events s JOIN runs rs ON rs.id = s.run_id
                                        WHERE s.kind = 'landing_scanned'
                                          AND rs.project_id = ?1), 0)
                 ORDER BY e.id",
            )?
            .query_map(params![d.project, MERGE_WAIT.as_secs() as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        d.waiting = unscanned
            .iter()
            .filter(|(_, recent)| *recent)
            .map(|(m, _)| m.clone())
            .collect();
        d.unscanned = unscanned.into_iter().map(|(m, _)| m).collect();
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
    let in_tip = |c: &String| git(&["merge-base", "--is-ancestor", c, &tip]).is_some();
    if !d.waiting.iter().all(in_tip) {
        return None;
    }
    let parent =
        |m: &str| git(&["rev-parse", "--verify", &format!("{m}^1")]).map(|s| s.trim().to_string());
    // The tip scanned last; with none (or one the branch no longer holds), the common ancestor
    // of the first parents of every merge not scanned yet, so that landings before the first
    // scan are read too, in whatever order they were recorded.
    let parents: Vec<String> = d
        .unscanned
        .iter()
        .filter_map(|m| parent(m))
        .filter(in_tip)
        .collect();
    let common = || {
        let mut args = vec!["merge-base", "--octopus"];
        args.extend(parents.iter().map(String::as_str));
        (!parents.is_empty())
            .then(|| git(&args).map(|s| s.trim().to_string()))
            .flatten()
    };
    let from = d
        .prev
        .clone()
        .filter(in_tip)
        .or_else(common)
        .or_else(|| parent(&d.merge))?;
    // Paths git quotes (a tab, a newline, any byte past ASCII, so also one that is no UTF-8) come
    // C-quoted and ASCII: the output is always text, and each path is unquoted to be matched.
    let changed = git(&[
        "-c",
        "core.quotePath=true",
        "diff",
        "--name-only",
        "--no-renames",
        &from,
        &tip,
    ])?;
    let matched: Vec<String> = changed
        .lines()
        .filter(|l| !l.is_empty())
        .map(unquote)
        .filter(|p| WATCHED.iter().any(|w| p.starts_with(w.as_bytes())))
        .map(|p| String::from_utf8_lossy(&p).into_owned())
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
        let read = gate::fetch_with(clone, repo, *pr, &d.branch, cancel)
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

/// A path as `git diff --name-only` prints it: as it is, or C-quoted (`"tools/\303\244.py"`).
fn unquote(line: &str) -> Vec<u8> {
    let Some(inner) = line.strip_prefix('"').and_then(|l| l.strip_suffix('"')) else {
        return line.as_bytes().to_vec();
    };
    let b = inner.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 == b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let c = b[i + 1];
        let octal = b
            .get(i + 1..i + 4)
            .filter(|o| o.iter().all(|d| (b'0'..=b'7').contains(d)));
        if let Some(o) = octal {
            out.push(
                o.iter()
                    .fold(0u8, |n, d| n.wrapping_mul(8).wrapping_add(d - b'0')),
            );
            i += 4;
            continue;
        }
        out.push(match c {
            b'a' => 7,
            b'b' => 8,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 11,
            b'f' => 12,
            b'r' => b'\r',
            other => other,
        });
        i += 2;
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::writer::RunUpsert;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    fn git(at: &Path, args: &[&OsStr]) -> String {
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
    }

    fn g(at: &Path, args: &[&str]) -> String {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        git(at, &args)
    }

    /// A side branch adding `path` (bytes: a name need not be UTF-8), merged into `main` with
    /// `--no-ff` as a pull request lands; the merge commit.
    fn land(at: &Path, branch: &str, path: &[u8]) -> String {
        g(at, &["checkout", "-q", "-b", branch, "main"]);
        let rel = Path::new(OsStr::from_bytes(path));
        let dest = at.join(rel);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, "x\n").unwrap();
        git(at, &[OsStr::new("add"), OsStr::new("--"), rel.as_os_str()]);
        g(at, &["commit", "-q", "-m", branch]);
        g(at, &["checkout", "-q", "main"]);
        g(at, &["merge", "-q", "--no-ff", "-m", "merge", branch]);
        g(at, &["rev-parse", "HEAD"])
    }

    /// A clone of a project whose main has `landings` merged in order; the merge commits.
    fn world(root: &Path, landings: &[&[u8]]) -> (PathBuf, Vec<String>) {
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-q", "-b", "main"]);
        g(&work, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let origin = root.join("origin.git");
        g(root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        g(
            &work,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        let merges = landings
            .iter()
            .enumerate()
            .map(|(n, p)| land(&work, &format!("b{n}"), p))
            .collect();
        g(&work, &["push", "-q", "origin", "main"]);
        let clone = root.join("clone");
        g(
            root,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        );
        (clone, merges)
    }

    fn due_of(merge: &str, unscanned: &[&String]) -> Due {
        Due {
            project: 1,
            slug: "p".into(),
            branch: "main".into(),
            landed_event: 2,
            task: "t".into(),
            repo: Some("o/p".into()),
            pr: Some(2),
            merge: merge.into(),
            prev: None,
            unscanned: unscanned.iter().map(|s| s.to_string()).collect(),
            waiting: Vec::new(),
            runs: Vec::new(),
        }
    }

    /// Before the first scan, every landing not scanned is read, in whatever order the merges
    /// were recorded: a `tools/` change in an older landing is not lost behind a newer one that
    /// changes only `README.md`.
    #[test]
    fn the_first_scan_reads_every_landing_not_scanned_in_any_recorded_order() {
        let dir = tempfile::tempdir().unwrap();
        let (clone, m) = world(dir.path(), &[b"tools/check.py", b"README.md"]);
        let never = AtomicBool::new(false);
        for order in [[&m[0], &m[1]], [&m[1], &m[0]]] {
            let s = scan(&clone, &due_of(&m[1], &order), &never).expect("in the tip");
            assert_eq!(s.tip, m[1]);
            assert_eq!(s.matched, vec!["tools/check.py".to_string()], "{order:?}");
        }
        // Only the newest merge: its own change, not the older one's.
        let only = scan(&clone, &due_of(&m[1], &[&m[1]]), &never).unwrap();
        assert!(only.matched.is_empty(), "{:?}", only.matched);
    }

    /// Merges recorded out of order with the origin behind: a recent merge not yet in the tip
    /// holds the scan until it is there; one recorded long ago that never lands does not.
    #[test]
    fn a_recent_merge_not_yet_in_the_tip_holds_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let (clone, m) = world(dir.path(), &[b"README.md"]);
        let work = dir.path().join("work");
        let later = land(&work, "later", b"tools/new.py");
        let never = AtomicBool::new(false);
        // P is the README merge, recorded last; the tools merge was recorded first, not at the
        // origin yet.
        let mut d = due_of(&m[0], &[&later, &m[0]]);
        d.waiting = vec![later.clone(), m[0].clone()];
        assert!(
            scan(&clone, &d, &never).is_none(),
            "held until the tip has it"
        );
        let mut old = d.clone();
        old.waiting = vec![m[0].clone()];
        let s = scan(&clone, &old, &never).expect("an old merge holds nothing");
        assert!(s.matched.is_empty(), "{:?}", s.matched);
        g(&work, &["push", "-q", "origin", "main"]);
        let s = scan(&clone, &d, &never).expect("the tip has both");
        assert_eq!(s.tip, later);
        assert_eq!(s.matched, vec!["tools/new.py".to_string()]);
    }

    /// A path git quotes (past ASCII, or no UTF-8 at all, inside or outside the watched folders)
    /// does not stop the scan, and a watched one is matched.
    #[test]
    fn paths_git_would_quote_are_read_and_matched() {
        let dir = tempfile::tempdir().unwrap();
        let (clone, m) = world(
            dir.path(),
            &[
                "tools/\u{65e5}\u{672c}.py".as_bytes(),
                b"docs/\xfe.md",
                b"tools/\xff.py",
                b"tools/a\tb.py",
            ],
        );
        let never = AtomicBool::new(false);
        let all: Vec<&String> = m.iter().collect();
        let s = scan(&clone, &due_of(&m[3], &all), &never).expect("the scan is written");
        let mut got = s.matched.clone();
        got.sort();
        assert_eq!(
            got,
            vec![
                "tools/a\tb.py".to_string(),
                "tools/\u{65e5}\u{672c}.py".to_string(),
                "tools/\u{fffd}.py".to_string(),
            ]
        );
    }

    /// Every merge newer than the newest scan is unscanned; `merge` is the newest.
    #[test]
    fn a_scan_due_names_every_merge_not_scanned_yet() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.ensure_project("p", None, Some("https://github.com/o/p"))
            .unwrap();
        for t in ["a", "b", "c", "d"] {
            w.upsert_run(&RunUpsert {
                task_id: t,
                project_slug: "p",
                status: Some("review"),
                ..Default::default()
            })
            .unwrap();
        }
        let merged = |task: &str, merge: &str| {
            w.append_event(&NewEvent {
                kind: "pr_merged",
                actor: Some("makibad"),
                payload: Some(&json!({"repo": "o/p", "pr": 1, "merge_commit": merge})),
                run_task_id: Some(task),
                ..Default::default()
            })
            .unwrap()
        };
        merged("a", "m1");
        let e2 = merged("b", "m2");
        let d = due(&w).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].unscanned, vec!["m1", "m2"]);
        assert_eq!(
            d[0].waiting, d[0].unscanned,
            "recorded just now: both hold the scan"
        );
        assert_eq!((d[0].merge.as_str(), d[0].prev.as_deref()), ("m2", None));
        w.append_event(&NewEvent {
            kind: "landing_scanned",
            actor: Some("makibad"),
            payload: Some(&json!({"landed_event": e2, "base_head": "m2"})),
            run_task_id: Some("b"),
            ..Default::default()
        })
        .unwrap();
        assert!(due(&w).unwrap().is_empty(), "scanned");
        merged("c", "m3");
        merged("d", "m4");
        let d = due(&w).unwrap();
        assert_eq!(d[0].unscanned, vec!["m3", "m4"]);
        assert_eq!(
            (d[0].merge.as_str(), d[0].prev.as_deref()),
            ("m4", Some("m2"))
        );
        assert_eq!(d[0].task, "d");
        assert!(d[0].runs.is_empty(), "no run has a pull request");
    }

    #[test]
    fn a_quoted_path_is_unquoted_to_its_bytes() {
        assert_eq!(unquote("tools/a.py"), b"tools/a.py".to_vec());
        assert_eq!(unquote(r#""tools/\377.py""#), b"tools/\xff.py".to_vec());
        assert_eq!(unquote(r#""a\tb\\c\"d""#), b"a\tb\\c\"d".to_vec());
    }
}
