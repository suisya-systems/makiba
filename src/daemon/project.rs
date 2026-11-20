//! The verb `project` (Issue 184): register a project and have `makibad` fetch its repository.
//!
//! The door checks the request in its own transaction and records it `accepted`; a pass of
//! `makibad` then runs git on a thread of its own (a clone can take minutes) and finishes the
//! command `done` or `failed`, with the project row and a notice to the secretary, in one
//! transaction. An `accepted` row with no thread (a restart) is simply taken again: a clone is
//! made in `<repos>/.<slug>.partial-<command id>` and renamed into place, so a half-made one is
//! never at `<repos>/<slug>` and is made anew.
//!
//! git runs as the person, with no terminal prompt: the person's own git configuration and ssh
//! key, plus the GitHub CLI's stored login for github.com. Hooks are off, submodules are not
//! fetched, LFS content is not downloaded, and only https and ssh are allowed. The request names
//! `OWNER/NAME` only; `makibad` builds the URL.

use super::door::Response;
use crate::db::Result;
use crate::db::writer::StateWriter;
use crate::spec::{AGENT_SECRETARY, DoorError, ErrorKind, Role};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The organization's setting that lists the owners the secretary may fetch from.
pub const OWNERS_KEY: &str = "allowed_repo_owners";
/// Test seam (spec section 2): where `OWNER/NAME` is looked up, default `https://github.com`.
pub const GIT_URL_ENV: &str = "MAKIBA_GIT_URL";
/// A git call that takes longer is killed. ponytail: one limit for every repository; a size
/// check would need the GitHub API.
const GIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn refuse(kind: ErrorKind, message: impl Into<String>, hint: impl Into<String>) -> DoorError {
    DoorError {
        kind,
        message: message.into(),
        hint: hint.into(),
    }
}

/// A GitHub owner: letters, digits and `-`, not starting with `-`, at most 39.
fn owner_ok(s: &str) -> bool {
    (1..=39).contains(&s.len())
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// `OWNER/NAME`, nothing else: no URL, no path, no `.git`.
fn parse_repo(s: &str) -> Option<(&str, &str)> {
    let (owner, name) = s.split_once('/')?;
    let name_ok = (1..=100).contains(&name.len())
        && !name.starts_with('-')
        && name != "."
        && name != ".."
        && !name.to_ascii_lowercase().ends_with(".git")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    (owner_ok(owner) && name_ok).then_some((owner, name))
}

/// The value of `allowed_repo_owners` as stored: owners, lowercased, comma-joined; `None` when
/// one of them is not an owner name.
pub fn owners_value(v: &str) -> Option<String> {
    let owners: Vec<String> = v
        .split(',')
        .map(|o| o.trim().to_ascii_lowercase())
        .collect();
    owners.iter().all(|o| owner_ok(o)).then(|| owners.join(","))
}

/// The canonical origin of a repository, as the project row keeps it.
fn origin_of(owner: &str, name: &str) -> String {
    format!("https://github.com/{owner}/{name}")
}

/// Whether a `project` command for `slug` is still being carried out.
pub(super) fn under_way(w: &StateWriter, slug: &str) -> Result<bool> {
    Ok(w.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM commands WHERE verb = 'project' AND status = 'accepted'
                          AND json_extract(args_json, '$.slug') = ?1)",
        [slug],
        |r| r.get(0),
    )?)
}

/// The door's half, in its transaction: refuse at once, or accept (the command is recorded
/// `accepted` and a pass carries it out).
pub(super) fn accept(w: &StateWriter, role: Role, slug: &str, repo: &str) -> Response {
    let Some((owner, name)) = parse_repo(repo) else {
        return Err(refuse(
            ErrorKind::Usage,
            format!("repo is OWNER/NAME of a GitHub repository, not {repo:?}"),
            "",
        ));
    };
    let internal = |e: rusqlite::Error| refuse(ErrorKind::Internal, e.to_string(), "");
    if role != Role::Person {
        let allowed = super::language::get(w.conn(), "", OWNERS_KEY)
            .map_err(internal)?
            .unwrap_or_default();
        let owner_lc = owner.to_ascii_lowercase();
        if !allowed.split(',').any(|o| o == owner_lc) {
            return Err(refuse(
                ErrorKind::Denied,
                format!("the owner {owner} is not in the organization's {OWNERS_KEY}"),
                "ask the person: they may register it (makiba project) or allow the owner (makiba config set allowed_repo_owners)",
            ));
        }
    }
    let known: Option<Option<String>> = w
        .conn()
        .query_row(
            "SELECT origin_url FROM projects WHERE slug = ?1",
            [slug],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    let want = format!("{owner}/{name}").to_ascii_lowercase();
    if let Some(Some(url)) = known
        && super::triage::owner_repo_from_url(&url).as_deref() != Some(want.as_str())
    {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("project {slug} is the repository {url}, not {repo}"),
            "a slug names one repository; register this one under another slug",
        ));
    }
    if under_way(w, slug).map_err(|e| refuse(ErrorKind::Internal, e.to_string(), ""))? {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("project {slug} is being fetched already"),
            "wait for the notice that it is done",
        ));
    }
    Ok(json!({"slug": slug, "repo": repo, "status": "accepted"}))
}

// ---- the pass ---------------------------------------------------------------------------------

struct Job {
    id: i64,
    slug: String,
    repo: String,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rx: Receiver<std::result::Result<Value, DoorError>>,
}

impl Job {
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub struct Projects {
    repos: PathBuf,
    url: Option<String>,
    gh: String,
    jobs: Vec<Job>,
}

impl Projects {
    /// `repos` is the repositories folder; `$MAKIBA_GIT_URL` and `$MAKIBA_GH` are read here, once.
    pub fn from_env(repos: PathBuf) -> Projects {
        Projects {
            repos,
            url: std::env::var(GIT_URL_ENV).ok().filter(|v| !v.is_empty()),
            gh: super::gh::program(),
            jobs: Vec::new(),
        }
    }

    /// Finish what a thread answered; start a thread for every `accepted` command without one.
    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        let mut i = 0;
        while i < self.jobs.len() {
            let r = match self.jobs[i].rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(refuse(
                    ErrorKind::Internal,
                    "the fetch ended without an answer",
                    "",
                )),
            };
            let mut job = self.jobs.remove(i);
            job.stop();
            finish(w, &job, r)?;
        }
        let rows: Vec<(i64, String)> = {
            let mut st = w.conn().prepare(
                "SELECT id, args_json FROM commands
                 WHERE verb = 'project' AND status = 'accepted' ORDER BY id",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        for (id, args) in rows {
            if self.jobs.iter().any(|j| j.id == id) {
                continue;
            }
            let args: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
            let slug = args["slug"].as_str().unwrap_or_default().to_string();
            let repo = args["repo"].as_str().unwrap_or_default().to_string();
            let cancel = Arc::new(AtomicBool::new(false));
            let (tx, rx) = channel();
            let handle = {
                let (repos, url, gh) = (self.repos.clone(), self.url.clone(), self.gh.clone());
                let (slug, repo, cancel) = (slug.clone(), repo.clone(), Arc::clone(&cancel));
                std::thread::spawn(move || {
                    let r = fetch(&repos, url.as_deref(), &gh, id, &slug, &repo, &cancel);
                    if !cancel.load(Ordering::SeqCst) {
                        let _ = tx.send(r);
                    }
                })
            };
            self.jobs.push(Job {
                id,
                slug,
                repo,
                cancel,
                handle: Some(handle),
                rx,
            });
        }
        Ok(())
    }
}

impl Drop for Projects {
    /// A stop: every git under way is killed and its thread joined; the command stays `accepted`
    /// and the next start carries it out again.
    fn drop(&mut self) {
        for j in &mut self.jobs {
            j.stop();
        }
    }
}

/// The command `done` with the project row, or `failed`; and the notice. One transaction.
fn finish(w: &StateWriter, job: &Job, r: std::result::Result<Value, DoorError>) -> Result<()> {
    let line = super::door::line(&r).to_string();
    w.transaction(|w| {
        let body = match &r {
            Ok(v) => {
                let (owner, name) = parse_repo(&job.repo).unwrap_or_default();
                w.ensure_project(&job.slug, None, Some(&origin_of(owner, name)))?;
                w.conn().execute(
                    "UPDATE projects SET origin_url = ?2, default_branch = ?3 WHERE slug = ?1",
                    params![
                        job.slug,
                        origin_of(owner, name),
                        v["default_branch"].as_str()
                    ],
                )?;
                format!(
                    "Project {} ({}) is {}: {} is at {}. You may dispatch into it.",
                    job.slug,
                    job.repo,
                    if v["action"] == "clone" {
                        "cloned"
                    } else {
                        "fetched"
                    },
                    v["default_branch"].as_str().unwrap_or("?"),
                    v["head"].as_str().unwrap_or("?"),
                )
            }
            Err(e) => format!(
                "Project {} ({}) could not be fetched: {}",
                job.slug, job.repo, e.message
            ),
        };
        w.conn().execute(
            "UPDATE commands SET status = ?2, result_json = ?3, error_kind = ?4,
               finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1 AND status = 'accepted'",
            params![
                job.id,
                if r.is_ok() { "done" } else { "failed" },
                line,
                r.as_ref().err().map(|e| e.kind.as_str())
            ],
        )?;
        super::door::notice_untasked(w, AGENT_SECRETARY, &body)?;
        Ok(())
    })
}

// ---- the thread -------------------------------------------------------------------------------

/// Clone into `<repos>/<slug>` when it is missing, else fetch and fast-forward its default branch.
fn fetch(
    repos: &Path,
    url: Option<&str>,
    gh: &str,
    id: i64,
    slug: &str,
    repo: &str,
    cancel: &AtomicBool,
) -> std::result::Result<Value, DoorError> {
    let failed = |m: String| refuse(ErrorKind::Unavailable, m, "");
    let conflict = |m: String| refuse(ErrorKind::Conflict, m, "");
    let (owner, name) = parse_repo(repo).ok_or_else(|| failed(format!("bad repo {repo:?}")))?;
    let git = Git {
        gh,
        seam: url,
        cancel,
    };
    let remote = match url {
        Some(base) => format!("{}/{owner}/{name}.git", base.trim_end_matches('/')),
        None => {
            let ssh = super::gh::run_gh(
                gh,
                &["config", "get", "git_protocol", "-h", "github.com"],
                cancel,
            )
            .is_ok_and(|p| p.trim() == "ssh");
            if ssh {
                format!("git@github.com:{owner}/{name}.git")
            } else {
                format!("https://github.com/{owner}/{name}.git")
            }
        }
    };
    let dest = repos.join(slug);
    let action = if dest.exists() {
        let origin = git.run(Some(&dest), &["remote", "get-url", "origin"])?;
        let want = format!("{owner}/{name}").to_ascii_lowercase();
        if origin != remote
            && super::triage::owner_repo_from_url(&origin).as_deref() != Some(want.as_str())
        {
            return Err(conflict(format!(
                "{} is a clone of {origin}, not of {repo}; it is left as it is",
                dest.display()
            )));
        }
        git.run(
            Some(&dest),
            &["fetch", "--no-recurse-submodules", "--prune", "origin"],
        )?;
        // The remote's own default branch; a clone made by hand may not record it yet.
        let remote_head = || {
            git.run(
                Some(&dest),
                &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            )
        };
        let default = match remote_head() {
            Ok(r) => r,
            Err(_) => {
                git.run(Some(&dest), &["remote", "set-head", "origin", "--auto"])?;
                remote_head()?
            }
        };
        let default = default.strip_prefix("origin/").unwrap_or(&default);
        let head = git.run(Some(&dest), &["symbolic-ref", "--short", "HEAD"])?;
        if default != head {
            return Err(conflict(format!(
                "{} is on {head}, not on the default branch {default}",
                dest.display()
            )));
        }
        if !git
            .run(
                Some(&dest),
                &["status", "--porcelain", "--untracked-files=no"],
            )?
            .is_empty()
        {
            return Err(conflict(format!(
                "{} has uncommitted changes",
                dest.display()
            )));
        }
        git.run(
            Some(&dest),
            &["merge", "--ff-only", &format!("origin/{head}")],
        )?;
        "fetch"
    } else {
        std::fs::create_dir_all(repos).map_err(|e| failed(e.to_string()))?;
        let partial = repos.join(format!(".{slug}.partial-{id}"));
        // Left by an earlier try of this very command (a restart); it is ours.
        if partial.exists() {
            std::fs::remove_dir_all(&partial).map_err(|e| failed(e.to_string()))?;
        }
        let p = partial.to_string_lossy().into_owned();
        let cloned = git.run(
            None,
            &["clone", "--no-recurse-submodules", "--", &remote, &p],
        );
        if let Err(e) = cloned {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(e);
        }
        std::fs::rename(&partial, &dest).map_err(|e| failed(e.to_string()))?;
        "clone"
    };
    let default_branch = git.run(Some(&dest), &["symbolic-ref", "--short", "HEAD"])?;
    let head = git.run(Some(&dest), &["rev-parse", "HEAD"])?;
    Ok(json!({"slug": slug, "repo": repo, "action": action,
              "default_branch": default_branch, "head": head}))
}

struct Git<'a> {
    gh: &'a str,
    /// The test seam's URL: its scheme is allowed too.
    seam: Option<&'a str>,
    cancel: &'a AtomicBool,
}

impl Git<'_> {
    /// One git call: stdout trimmed, or the failure with git's own words.
    fn run(&self, dir: Option<&Path>, args: &[&str]) -> std::result::Result<String, DoorError> {
        let failed = |m: String| refuse(ErrorKind::Unavailable, m, "");
        let mut c = Command::new("git");
        if let Some(d) = dir {
            c.arg("-C").arg(d);
        }
        let mut cfg = vec![
            "core.hooksPath=/dev/null".to_string(),
            "protocol.allow=never".into(),
            "protocol.https.allow=always".into(),
            "protocol.ssh.allow=always".into(),
            "submodule.recurse=false".into(),
            // github.com only: the login never goes to a host a rewrite names.
            format!(
                "credential.https://github.com.helper=!\"{}\" auth git-credential",
                self.gh
            ),
        ];
        if let Some(scheme) = self.seam.and_then(|u| u.split_once("://")).map(|s| s.0) {
            cfg.push(format!("protocol.{scheme}.allow=always"));
        }
        for v in &cfg {
            c.arg("-c").arg(v);
        }
        let mut child = c
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| failed(format!("cannot run git: {e}")))?;
        let reader = |mut p: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut s = Vec::new();
                let _ = p.read_to_end(&mut s);
                String::from_utf8_lossy(&s).trim().to_string()
            })
        };
        let out = reader(Box::new(child.stdout.take().expect("piped")));
        let err = reader(Box::new(child.stderr.take().expect("piped")));
        let deadline = Instant::now() + GIT_TIMEOUT;
        let status = loop {
            if let Ok(Some(s)) = child.try_wait() {
                break s;
            }
            let stop = if self.cancel.load(Ordering::SeqCst) {
                Some("cancelled: makibad is stopping")
            } else if Instant::now() >= deadline {
                Some("git timed out")
            } else {
                None
            };
            if let Some(why) = stop {
                // SAFETY: a signal to the group of a child this function started.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                return Err(failed(format!("git {}: {why}", args[0])));
            }
            std::thread::sleep(super::gh::SLICE);
        };
        let (out, err) = (
            out.join().unwrap_or_default(),
            err.join().unwrap_or_default(),
        );
        if status.success() {
            Ok(out)
        } else {
            Err(failed(format!("git {} failed: {err}", args[0])))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owner_slash_name_is_a_repository() {
        assert_eq!(parse_repo("acme/widget"), Some(("acme", "widget")));
        assert_eq!(parse_repo("acme/.github"), Some(("acme", ".github")));
        for bad in [
            "acme",
            "acme/widget/x",
            "acme/widget.git",
            "-x/widget",
            "acme/..",
            "acme/-x",
            "https://github.com/acme/widget",
            "git@github.com:acme/widget",
            "../acme/widget",
            "acme/wid get",
        ] {
            assert_eq!(parse_repo(bad), None, "{bad}");
        }
        assert_eq!(owners_value("Acme, other").as_deref(), Some("acme,other"));
        assert_eq!(owners_value("acme,"), None);
        assert_eq!(owners_value("a/b"), None);
    }
}
