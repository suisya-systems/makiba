//! The triage pass (Issue 40, spec section 10): after a close, and at most once per interval,
//! `makibad` runs the work discovery scan (`crate::work_discovery`) over the active projects and
//! queues a notice to the secretary when what it found differs from what was last sent.
//!
//! Both facts the pass is driven by are rows of `events`, so a restart keeps them: a scan is due
//! when a `run_closed` is newer than the `through_close_id` of the newest scan that did not fail,
//! and the newest scan of any status is at least the interval old. A close inside the interval
//! waits for its end; a close during a scan, or behind a failed or cancelled one, stays due.
//!
//! The reads of GitHub take seconds, so the scan runs on a thread of its own (one at a time); the
//! loop's thread writes the `work_discovery_scanned` event and the notice, in one transaction. No
//! model is called: the order is the scan's (the clause each Issue names, then the carried key).

use super::gh::{GhError, run_gh};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::AGENT_SECRETARY;
use crate::work_discovery::{self as wd, Fetch, Limits, ScanConfig, Truncation};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;

/// The default of `$MAKIBA_TRIAGE_INTERVAL_MS`: 30 minutes.
const DEFAULT_INTERVAL_MS: i64 = 30 * 60 * 1000;

/// What a scan thread hands back.
struct Outcome {
    /// The scan, or why it could not be made.
    scan: std::result::Result<Value, String>,
    /// The repositories scanned, `owner/repo`.
    repos: Vec<String>,
    /// The projects with no GitHub repository, by slug.
    norepo: Vec<String>,
}

struct Job {
    /// The newest `run_closed` when the scan started.
    through: i64,
    free: usize,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<Outcome>,
    /// Received and not written yet: kept until its transaction commits.
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

pub struct Triage {
    gh: String,
    goals: PathBuf,
    repos_dir: PathBuf,
    interval_ms: i64,
    capacity: usize,
    job: Option<Job>,
}

impl Triage {
    /// `$MAKIBA_GH`, `$MAKIBA_GOALS_DIR`, `$MAKIBA_TRIAGE_INTERVAL_MS`; `repos_dir` is where a
    /// project's clone is (its `origin` names the repository when the project has no URL).
    pub fn from_env(repos_dir: PathBuf, capacity: usize) -> Triage {
        Triage {
            gh: super::gh::program(),
            goals: crate::cli::goals_dir(),
            repos_dir,
            interval_ms: std::env::var("MAKIBA_TRIAGE_INTERVAL_MS")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_INTERVAL_MS),
            capacity,
            job: None,
        }
    }

    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        if let Some(job) = self.job.as_mut() {
            if job.done.is_none() {
                match job.rx.try_recv() {
                    Ok(o) => job.done = Some(o),
                    Err(TryRecvError::Empty) => return Ok(()),
                    // Cancelled or crashed: nothing is written, the close stays due.
                    Err(TryRecvError::Disconnected) => {}
                }
            }
            if let Some(o) = &job.done {
                record(w, job.through, job.free, o, &self.goals, &self.repos_dir)?;
            }
            if let Some(mut job) = self.job.take() {
                job.stop();
            }
        }
        let Some(through) = due(w, self.interval_ms)? else {
            return Ok(());
        };
        let free = self.capacity.saturating_sub(super::dispatch::busy(w)?);
        let projects: Vec<(String, Option<String>)> = {
            let mut st = w.conn().prepare(
                "SELECT slug, origin_url FROM projects WHERE status = 'active' ORDER BY slug",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let handle = {
            let (gh, goals, repos_dir) =
                (self.gh.clone(), self.goals.clone(), self.repos_dir.clone());
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                let mut f = GhFetch { gh, cancel };
                if let Some(o) = scan(&mut f, &projects, &repos_dir, &goals, free) {
                    let _ = tx.send(o);
                }
            })
        };
        self.job = Some(Job {
            through,
            free,
            cancel,
            handle: Some(handle),
            rx,
            done: None,
        });
        Ok(())
    }
}

impl Drop for Triage {
    /// A scan under way is cancelled, its `gh` child killed and its thread joined.
    fn drop(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.stop();
        }
    }
}

/// The newest `run_closed` id when a scan is due, else `None`.
fn due(w: &StateWriter, interval_ms: i64) -> Result<Option<i64>> {
    let (close, through, age): (Option<i64>, Option<i64>, Option<f64>) = w.conn().query_row(
        "SELECT (SELECT max(id) FROM events WHERE kind = 'run_closed'),
                (SELECT json_extract(payload_json, '$.through_close_id') FROM events
                  WHERE kind = 'work_discovery_scanned'
                    AND json_extract(payload_json, '$.status') != 'error'
                  ORDER BY id DESC LIMIT 1),
                (SELECT (julianday('now') - julianday(occurred_at)) * 86400000.0 FROM events
                  WHERE kind = 'work_discovery_scanned' ORDER BY id DESC LIMIT 1)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let Some(close) = close.filter(|c| *c > through.unwrap_or(0)) else {
        return Ok(None);
    };
    Ok(age.is_none_or(|a| a >= interval_ms as f64).then_some(close))
}

// ---- the scan thread -----------------------------------------------------------------------------

/// The `Fetch` of the scan: `gh issue list` / `gh pr list` with the carried arguments, the
/// repository pinned to github.com.
struct GhFetch {
    gh: String,
    cancel: Arc<AtomicBool>,
}

impl GhFetch {
    fn list(
        &self,
        what: &str,
        repo: &str,
        state: &str,
        extra: &[&str],
        limit: usize,
        fields: &str,
    ) -> std::result::Result<Vec<Value>, String> {
        let (host_repo, limit) = (format!("github.com/{repo}"), limit.to_string());
        let mut args = vec![what, "list", "--repo", &host_repo, "--state", state];
        args.extend_from_slice(extra);
        args.extend(["--limit", &limit, "--json", fields]);
        let text = run_gh(&self.gh, &args, &self.cancel).map_err(|e| match e {
            GhError::Missing => format!("{} not found", self.gh),
            GhError::Cancelled => "cancelled".into(),
            GhError::Failed(m) => m,
        })?;
        match serde_json::from_str(&text) {
            Ok(Value::Array(rows)) => Ok(rows),
            Ok(_) => Err(format!("gh {what} list returned a non-array JSON payload")),
            Err(_) => Err(format!("gh {what} list wrote no JSON")),
        }
    }
}

const RECENT: &[&str] = &["--search", "sort:updated-desc"];

impl Fetch for GhFetch {
    fn open_issues(&mut self, repo: &str, limit: usize) -> std::result::Result<Vec<Value>, String> {
        self.list(
            "issue",
            repo,
            "open",
            &[],
            limit,
            "number,title,body,labels,updatedAt,createdAt,milestone,comments",
        )
    }
    fn open_prs(&mut self, repo: &str, limit: usize) -> std::result::Result<Vec<Value>, String> {
        self.list("pr", repo, "open", &[], limit, "number")
    }
    fn recent_merges(
        &mut self,
        repo: &str,
        limit: usize,
    ) -> std::result::Result<Vec<Value>, String> {
        self.list(
            "pr",
            repo,
            "merged",
            RECENT,
            limit,
            "number,title,body,mergedAt",
        )
    }
    fn base_merges(
        &mut self,
        repo: &str,
        base: &str,
        limit: usize,
    ) -> std::result::Result<Vec<Value>, String> {
        let extra = ["--base", base, RECENT[0], RECENT[1]];
        self.list(
            "pr",
            repo,
            "merged",
            &extra,
            limit,
            "number,title,body,baseRefName,mergedAt",
        )
    }
    fn effort_history(
        &mut self,
        repo: &str,
        limit: usize,
    ) -> std::result::Result<Vec<Value>, String> {
        self.list(
            "pr",
            repo,
            "merged",
            RECENT,
            limit,
            "number,additions,deletions,changedFiles,reviews,createdAt,mergedAt,closingIssuesReferences",
        )
    }
    fn closed_issues(
        &mut self,
        repo: &str,
        limit: usize,
    ) -> std::result::Result<Vec<Value>, String> {
        self.list("issue", repo, "closed", RECENT, limit, "number,body")
    }
}

/// `owner/repo`, lowercased, of a github.com URL (`https://github.com/o/r(.git)`,
/// `git@github.com:o/r.git`, `ssh://git@github.com/o/r`), as the carried `_owner_repo_from_url`.
/// The host must be `github.com` itself, not a name ending in it.
pub fn owner_repo_from_url(url: &str) -> Option<String> {
    let s = url.trim().to_lowercase();
    let host = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .or_else(|| s.strip_prefix("ssh://"))
        .or_else(|| s.strip_prefix("git://"))
        .unwrap_or(&s);
    let host = host.split_once('@').map_or(host, |(_, h)| h);
    let rest = host.strip_prefix("github.com")?.strip_prefix([':', '/'])?;
    let mut parts = rest.trim_end_matches('/').splitn(3, '/');
    let owner = parts.next().filter(|p| !p.is_empty())?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (ok(owner) && ok(repo) && parts.next().is_none()).then(|| format!("{owner}/{repo}"))
}

/// The project's repository: its URL, else the `origin` of its clone.
pub(crate) fn repo_of(slug: &str, url: Option<&str>, repos_dir: &Path) -> Option<String> {
    if let Some(r) = url.and_then(owner_repo_from_url) {
        return Some(r);
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repos_dir.join(slug))
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| owner_repo_from_url(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// One scan. `None`: cancelled.
fn scan(
    f: &mut GhFetch,
    projects: &[(String, Option<String>)],
    repos_dir: &Path,
    goals: &Path,
    free: usize,
) -> Option<Outcome> {
    let (mut repos, mut norepo) = (Vec::<String>::new(), Vec::new());
    for (slug, url) in projects {
        match repo_of(slug, url.as_deref(), repos_dir) {
            Some(r) if !repos.contains(&r) => repos.push(r),
            Some(_) => {}
            None => norepo.push(slug.clone()),
        }
    }
    let run = |f: &mut GhFetch| -> std::result::Result<Value, String> {
        let (mut bundles, mut t) = (Vec::new(), Truncation::default());
        for r in &repos {
            let (b, tr) = wd::gather(f, r, None, &Limits::default())?;
            bundles.push(b);
            t = t.or(tr);
        }
        // The learned model is one repository's (carried): only for a scan of one. Its reads
        // failing leave the heuristic, and say so.
        let model = (repos.len() == 1).then(|| {
            wd::build_effort_model(f, &repos[0], wd::DEFAULT_EFFORT_HISTORY).unwrap_or_else(|_| {
                wd::effort::empty_effort_model(
                    "effort-history fetch failed -> static heuristic retained",
                )
            })
        });
        let cfg = ScanConfig {
            free_slots: Some(free as i64),
            trigger: "close".into(),
            ..ScanConfig::default()
        };
        Ok(wd::scan_repos(
            &bundles,
            &cfg,
            Some(&t),
            None,
            model.as_ref(),
            Some(goals),
        ))
    };
    let scan = run(f);
    if f.cancel.load(Ordering::SeqCst) {
        return None;
    }
    Some(Outcome {
        scan,
        repos,
        norepo,
    })
}

// ---- the loop's side: the event and the notice ---------------------------------------------------

/// The keys of the scan: its candidates and its diagnostics (the scan's `unset:` / `error:` lines
/// and a `norepo:<slug>` line per project with no repository), sorted, comma-joined.
fn keys(o: &Outcome) -> (String, Vec<String>) {
    let Ok(scan) = &o.scan else {
        return (String::new(), Vec::new());
    };
    let k = wd::forward::scan_keys(scan, "");
    let diag: BTreeSet<String> = k
        .diagnostics
        .split(',')
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .chain(o.norepo.iter().map(|s| format!("norepo:{s}")))
        .collect();
    (k.candidate_refs, diag.into_iter().collect())
}

fn record(
    w: &StateWriter,
    through: i64,
    free: usize,
    o: &Outcome,
    goals: &Path,
    repos_dir: &Path,
) -> Result<()> {
    let (refs, diag) = keys(o);
    let diagnostics = diag.join(",");
    let last: Option<(Option<String>, Option<String>)> = w
        .conn()
        .query_row(
            "SELECT json_extract(payload_json, '$.candidate_refs'),
                    json_extract(payload_json, '$.diagnostics')
             FROM events WHERE kind = 'work_discovery_scanned'
               AND json_extract(payload_json, '$.notified') = 1
             ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (last_refs, last_diag) = last.unwrap_or_default();
    let changed = last_refs.as_deref() != Some(refs.as_str())
        || last_diag.as_deref() != Some(diagnostics.as_str());
    let notify = o.scan.is_ok() && (!refs.is_empty() || !diag.is_empty()) && changed;
    let (status, count, cut, error) = match &o.scan {
        Ok(s) => (
            s["status"].clone(),
            s["candidate_count"].clone(),
            s["truncated_count"].clone(),
            Value::Null,
        ),
        Err(e) => (json!("error"), json!(0), json!(0), json!(e)),
    };
    let payload = json!({
        "trigger": "close", "status": status, "through_close_id": through,
        "candidate_refs": refs, "diagnostics": diagnostics,
        "candidate_count": count, "truncated_count": cut, "free_workers": free,
        "repos": o.repos, "notified": notify, "error": error,
    });
    w.transaction(|w| {
        w.append_event(&NewEvent {
            kind: "work_discovery_scanned",
            actor: Some("makibad"),
            payload: Some(&payload),
            ..Default::default()
        })?;
        if notify && let Ok(scan) = &o.scan {
            let seen: BTreeSet<&str> = last_diag
                .as_deref()
                .unwrap_or("")
                .split(',')
                .filter(|l| !l.is_empty())
                .collect();
            let body = notice(scan, free, &o.norepo, &seen, goals, repos_dir);
            super::door::notice_untasked(w, AGENT_SECRETARY, &body)?;
        }
        Ok(())
    })
}

/// The notice: every candidate in the scan's order with the clause it names, the qualifiers the
/// scan attached, and the diagnostics not in the last notice (`seen`).
fn notice(
    scan: &Value,
    free: usize,
    norepo: &[String],
    seen: &BTreeSet<&str>,
    goals: &Path,
    repos_dir: &Path,
) -> String {
    let arr = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let candidates = arr(&scan["candidates"]);
    let mut out = vec![match candidates.len() {
        0 => format!("Triage after a close: no candidates (free workers: {free})."),
        1 => format!("Triage after a close: 1 candidate (free workers: {free})."),
        n => format!("Triage after a close: {n} candidates (free workers: {free})."),
    }];
    let mut listed: Vec<String> = Vec::new();
    for c in &candidates {
        let repo = s(&c["repo"]);
        let clause = match &c["goal_clause"] {
            Value::Object(k) => format!(
                "{} {}",
                k.get("id").map(s).unwrap_or_default(),
                k.get("heading").map(s).unwrap_or_default()
            ),
            _ => "names no clause".into(),
        };
        out.push(format!(
            "{}. {repo}#{} {} - {clause}",
            c["rank"],
            c["issue"],
            s(&c["title"])
        ));
        let est = |k: &str| {
            if c[k] == json!(true) {
                " (estimated)"
            } else {
                ""
            }
        };
        let par = if c["parallelizable"] == json!(true) {
            "parallelizable"
        } else {
            "not parallelizable"
        };
        out.push(format!(
            "   priority {}, effort {}{}, {par}{}",
            s(&c["priority"]),
            s(&c["effort"]),
            est("effort_estimated"),
            est("parallelizable_estimated")
        ));
        out.push(format!("   {}", s(&c["summary"])));
        for sig in arr(&c["signals"]).iter().map(s) {
            if sig.contains("un-scanned") || sig.contains("treated resolved") {
                out.push(format!("   {sig}"));
            }
        }
        if !listed.contains(&repo) {
            listed.push(repo);
        }
    }
    if let Some(n) = scan["truncated_count"].as_u64().filter(|n| *n > 0) {
        out.push(format!("+{n} more not listed."));
    }
    let gr = &scan["goal_rank"];
    for g in arr(&gr["goals"]) {
        if listed.contains(&s(&g["repo"])) {
            let heads: Vec<String> = arr(&g["clauses"])
                .iter()
                .map(|k| format!("{} {}", s(&k["id"]), s(&k["heading"])))
                .collect();
            out.push(format!(
                "Clauses of {}: {}.",
                s(&g["repo"]),
                heads.join("; ")
            ));
        }
    }
    for sig in arr(&gr["signals"]) {
        out.push(format!("Goal signal: {}", s(&sig)));
    }
    for d in arr(&scan["dependency_warnings"]) {
        out.push(format!(
            "Dependency warning: {}#{}: {}",
            s(&d["repo"]),
            d["issue"],
            s(&d["line"])
        ));
    }
    let t = &scan["input_truncated"];
    let cut: Vec<&str> = [
        ("open_issues", "open Issues"),
        ("open_prs", "open PRs"),
        ("base_merges", "base merges"),
    ]
    .iter()
    .filter(|(k, _)| t[*k] == json!(true))
    .map(|(_, n)| *n)
    .collect();
    if !cut.is_empty() {
        out.push(format!(
            "Input truncated (some rows unseen): {}.",
            cut.join(", ")
        ));
    }
    let new = |key: String| !seen.contains(key.as_str());
    for u in arr(&gr["goal_unset_repos"]) {
        let r = s(&u["repo"]);
        if new(format!("unset:{r}")) {
            out.push(format!(
                "No goal ledger: {r} ({} candidates). Write {}/{r}.md (format in docs/goals.md).",
                u["candidate_count"],
                goals.display()
            ));
        }
    }
    for e in arr(&gr["goal_errors"]) {
        let r = s(&e["repo"]);
        if new(format!("error:{r}")) {
            out.push(format!("Goal ledger error: {r}: {}.", s(&e["error"])));
        }
    }
    for slug in norepo {
        if new(format!("norepo:{slug}")) {
            out.push(format!(
                "No GitHub repository: project {slug} (no origin URL, and no github.com remote origin in {}).",
                repos_dir.join(slug).display()
            ));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::owner_repo_from_url as f;

    #[test]
    fn a_github_url_gives_its_owner_and_repository() {
        for u in [
            "https://github.com/Suisya-Systems/Makiba",
            "https://github.com/suisya-systems/makiba.git",
            "git@github.com:suisya-systems/makiba.git",
            "ssh://git@github.com/suisya-systems/makiba/",
        ] {
            assert_eq!(f(u).as_deref(), Some("suisya-systems/makiba"), "{u}");
        }
        for u in [
            "",
            "/srv/repo",
            "https://gitlab.com/o/r",
            "https://github.com/o",
            "https://notgithub.com/team/project.git",
            "git@notgithub.com:team/project.git",
            "https://github.com.evil.example/o/r",
        ] {
            assert_eq!(f(u), None, "{u}");
        }
    }
}
