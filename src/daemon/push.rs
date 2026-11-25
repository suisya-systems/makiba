//! Finishing a `push` (Issue 190, spec section 12). The door accepts it for a run with a code
//! worktree; this pass gives every `accepted` push a thread of its own, which runs git outside
//! the door's transaction and touches no database, and writes what the thread found on a later
//! tick: the command `done` with `branch_pushed` and a notice naming the head, or `failed` with a
//! notice saying why, together.
//!
//! The thread checks, at the time it runs, reading the run's own repository only (Issue 241,
//! `run_repo`): the worktree is strictly under the workers folder and a work tree of the run's
//! repository `<state>/git/<task>`, on the run's branch, with no uncommitted change to a tracked
//! file and nothing half-joined (a merge, rebase, cherry-pick or revert, one commit or a
//! sequence). It hands the head to the project's repository as `refs/makiba/runs/<task>` and,
//! from there, pushes `<head>:refs/heads/<branch>` to `origin`, never forced, with no terminal
//! prompt, so a missing credential fails at once with git's message. A restart runs an
//! `accepted` push again; git finds it pushed already or pushes it.
//!
//! `update` (Issue 183) runs here too, after the same checks and the same hand-over, in the
//! project's repository: it fetches the base and the branch from `origin` by their full refs,
//! refuses a branch not at origin or one that moved there (not an ancestor of the worktree's
//! head), merges `origin/<base>` when the head does not hold it (a merge commit made by
//! `git merge-tree --write-tree` and `commit-tree`, as git's own identity or, where git has none,
//! makiba <makiba@localhost>; never a rebase; a conflict names its files and
//! leaves nothing behind), and pushes the result when origin does not have it, never forced. The
//! worktree is not touched: the worker brings the new head in by a fast-forward of its own. Done:
//! `branch_updated` with the old and new head, or, when nothing was merged or pushed, a notice
//! naming origin's head. A restart runs it again. One job runs per worktree and per project's
//! repository at a time (Issue 369): two jobs of one project would fetch the same
//! `refs/remotes/origin/<base>` at once, and git's ref lock lets only one through.

use super::gate;
use super::prepare::Places;
use super::run_repo;
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::hooks::git_hook::find;
use crate::spec::AGENT_SECRETARY;
use serde_json::json;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

#[derive(Clone)]
struct Job {
    command: i64,
    /// `push` or `update`.
    verb: String,
    task: String,
    branch: String,
    project: String,
    worktree: Option<String>,
    /// `update`'s base branch.
    base: Option<String>,
    /// `push`'s `head`: the branch's test commit, pushed alone (Issue 307).
    head: Option<String>,
    /// What the check before CI needs from `state.db`, read when the job starts.
    before: Before,
    /// An `update` makibad started on its queue readiness judgement (Issue 475): the
    /// `queue_readiness` event, and the pull request and head it judged, whose new head `finish`
    /// records.
    readiness: Option<Readiness>,
}

#[derive(Clone)]
struct Readiness {
    repo: String,
    pr: i64,
    head: String,
}

/// Issue 307: the project's default branch and the heads with a failed CI run on any of the task's
/// pull requests (`door::failed_heads`, the gate's reader).
#[derive(Clone, Default)]
struct Before {
    default_branch: String,
    failed: BTreeSet<String>,
}

impl Before {
    fn read(w: &StateWriter, task: &str, default_branch: String) -> Result<Before> {
        let c = w.conn();
        let mut st = c.prepare(
            "SELECT p.repo, p.pr FROM pr_watches p JOIN runs r ON r.id = p.run_id
             WHERE r.task_id = ?1 ORDER BY p.id DESC",
        )?;
        let prs: Vec<(String, i64)> = st
            .query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mut failed = BTreeSet::new();
        for (repo, pr) in &prs {
            failed.extend(super::door::failed_heads(c, repo, *pr)?);
        }
        Ok(Before {
            default_branch,
            failed,
        })
    }
}

/// The base the check before CI judges against, fetched from origin first: the project's default
/// branch, the run's starting point. Nothing the secretary passes or records steers it: neither
/// `update`'s base, which is only what is merged in, nor a recorded pull request's base, either of
/// which could name a branch that already holds the test commit and so hide it. Its id; what
/// cannot be read refuses.
fn judged_base(dir: &Path, j: &Job) -> std::result::Result<String, String> {
    let b = &j.before.default_branch;
    let r = format!("refs/remotes/origin/{b}");
    git(
        dir,
        &["fetch", "-q", "origin", &format!("+refs/heads/{b}:{r}")],
    )
    .map_err(|e| {
        format!(
            "the base {b} cannot be fetched from origin ({e}), so the branch cannot be checked \
             before CI (Issue 307); nothing was pushed, push again"
        )
    })?;
    git(dir, &["rev-parse", "--verify", &format!("{r}^{{commit}}")])
}

/// Issue 307: `sent` may go to origin only when it is the branch's test commit or a failed CI run
/// is recorded at that commit on one of the task's pull requests (`gate::before_ci`).
fn before_ci(dir: &Path, j: &Job, base: &str, sent: &str) -> std::result::Result<(), String> {
    gate::before_ci(
        dir,
        base,
        sent,
        &|sha| j.before.failed.contains(sha),
        "on this task's pull requests",
    )
    .map_err(|e| format!("{}; nothing was pushed", e.0))
}

/// What a thread did.
enum Done {
    /// `push`: the head pushed.
    Pushed(String),
    /// `update`: origin's head of the branch before and after; `base_merge` (a readiness update,
    /// Issue 475): `new` is a clean merge of the base onto the judged head.
    Updated {
        old: String,
        new: String,
        base_merge: bool,
    },
    /// `update`: nothing to merge or push; origin's head of the branch, and `base_merge` as above.
    Current { head: String, base_merge: bool },
}

type Outcome = std::result::Result<Done, String>;

pub struct Push {
    places: Places,
    /// Commands whose thread runs or whose result is not written yet.
    running: HashSet<i64>,
    /// Their worktrees: one job per worktree at a time.
    busy: HashSet<String>,
    /// Their projects: one job per project's repository at a time (Issue 369).
    projects: HashSet<String>,
    tx: Sender<(Job, Outcome)>,
    rx: Receiver<(Job, Outcome)>,
}

/// `git -C <dir> <args>` with no terminal prompt and no hook (Issue 236): its trimmed output, or
/// git's message.
fn git(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
    git_in(dir, args, String::new())
}

/// `git` with `input` on its standard input.
fn git_in(dir: &Path, args: &[&str], input: String) -> std::result::Result<String, String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", run_repo::HOOKS_OFF])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("git has no input")?;
    // Written beside the read, so a long output cannot stall a long input.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let _ = writer.join();
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Whether `a` is an ancestor of (or is) `b`.
fn is_ancestor(dir: &Path, a: &str, b: &str) -> std::result::Result<bool, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            run_repo::HOOKS_OFF,
            "merge-base",
            "--is-ancestor",
            a,
            b,
        ])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git merge-base --is-ancestor {a} {b}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn work(places: &Places, j: &Job) -> Outcome {
    let (gitdir, real) = checked(places, j)?;
    let repo = places.repos.join(&j.project);
    let head = run_repo::read(&gitdir, &real, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    // Issue 241: the run is only read; the project's repository pushes and merges.
    let to = format!("refs/makiba/runs/{}", j.task);
    run_repo::deliver(&gitdir, &real, &repo, &head, &to, true)?;
    match j.base.as_deref() {
        None => {
            let base = judged_base(&repo, j)?;
            let sent = match &j.head {
                None => head,
                Some(m) => {
                    test_commit_of(&repo, &base, &head, m)?;
                    m.clone()
                }
            };
            before_ci(&repo, j, &base, &sent)?;
            push_head(&repo, &sent, &j.branch).map(|()| Done::Pushed(sent))
        }
        Some(base) => {
            judged_head(j, &head)?;
            update(&repo, j, base, &head).map(|d| judged_merge(&repo, j, base, d))
        }
    }
}

/// A readiness update (Issue 475) merges only the head makibad judged: a worktree that has moved
/// on since holds the worker's own commits, which go to origin with `push`, never with it.
fn judged_head(j: &Job, head: &str) -> std::result::Result<(), String> {
    match &j.readiness {
        Some(r) if !r.head.eq_ignore_ascii_case(head) => Err(format!(
            "the worktree of {} is at {head}, not at {}, the head makibad judged for the queue; \
             nothing was merged or pushed (the worker's newer commits go to origin with push)",
            j.task, r.head
        )),
        _ => Ok(()),
    }
}

/// For a readiness update (Issue 475): whether the head it ends with is a clean merge of the base
/// onto the judged head (`gate::pushed_base_merge`, as the door judges `pr pushed`).
fn judged_merge(dir: &Path, j: &Job, base: &str, d: Done) -> Done {
    let Some(r) = &j.readiness else { return d };
    let merged = |head: &str| {
        gate::pushed_base_merge(dir, &format!("refs/remotes/origin/{base}"), &r.head, head)
    };
    match d {
        Done::Updated { old, new, .. } => {
            let base_merge = merged(&new);
            Done::Updated {
                old,
                new,
                base_merge,
            }
        }
        Done::Current { head, .. } => {
            let base_merge = merged(&head);
            Done::Current { head, base_merge }
        }
        other => other,
    }
}

/// `push` with `head` (Issue 307): it must be the test commit of the branch at `head` (the commit
/// with the `Acceptance-Test` trailer, as the gate finds it).
fn test_commit_of(dir: &Path, base: &str, head: &str, m: &str) -> std::result::Result<(), String> {
    let found = gate::find_manifest(dir, base, head).map_err(|e| e.0)?;
    match found {
        Some((t, _)) if t == m => Ok(()),
        Some((t, _)) => Err(format!(
            "head {m} is not the branch's test commit {t}: push names only the test commit with \
             head; nothing was pushed"
        )),
        None => Err(format!(
            "head {m} was given, but the branch declares no acceptance test: push without head; \
             nothing was pushed"
        )),
    }
}

/// The commit first, so no branch name can read as a force (`+`). Scanned first (Issue 231); no
/// tag follows.
fn push_head(dir: &Path, head: &str, branch: &str) -> std::result::Result<(), String> {
    scan(dir, head)?;
    git(
        dir,
        &[
            "push",
            "--no-follow-tags",
            "origin",
            &format!("{head}:refs/heads/{branch}"),
        ],
    )
    .map(drop)
}

/// `git` for a command whose arguments hold origin's URL: git's message without the arguments,
/// which could carry a credential.
fn remote(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
    git(dir, args).map_err(|e| {
        let why = e
            .strip_prefix(&format!("git {}: ", args.join(" ")))
            .unwrap_or("");
        format!(
            "origin cannot be read ({why}), so the outgoing commits cannot be scanned; nothing \
             was pushed"
        )
    })
}

/// Issue 231: what pushing `head` would send (every object it reaches that no branch or tag of
/// the remote reaches, as the remote itself says), read with the pre-commit hook's scanner: each
/// new blob raw and each new commit's message. A match refuses naming the commit, the file, the
/// line and the shape, never the text. Anything that cannot be read refuses too.
fn scan(dir: &Path, head: &str) -> std::result::Result<(), String> {
    let urls = remote(dir, &["remote", "get-url", "--push", "--all", "origin"])?;
    let mut urls = urls.lines();
    let (Some(url), None) = (urls.next(), urls.next()) else {
        return Err(
            "origin has more than one push URL, so the outgoing commits cannot be \
                    scanned against one remote; nothing was pushed"
                .into(),
        );
    };
    // `push origin` connects to `url` as it is; `ls-remote <url>` would rewrite it once more.
    if remote(dir, &["ls-remote", "--get-url", url])? != url {
        return Err(
            "origin's push URL is rewritten again by a url insteadOf rule, so the remote \
                    the scan would read is not the one pushed to; nothing was pushed"
                .into(),
        );
    }
    let refs = remote(dir, &["ls-remote", "--heads", "--tags", url])?;
    // On standard input: a remote with many branches and tags would not fit in the arguments.
    let range: String = [head, "--not"]
        .into_iter()
        .chain(refs.lines().filter_map(|l| l.split_whitespace().next()))
        .map(|r| format!("{r}\n"))
        .collect();
    let objects = git_in(
        dir,
        &["rev-list", "--objects", "--ignore-missing", "--stdin"],
        range.clone(),
    )?;
    let Some((oid, path, shape, line)) = first_match(dir, &objects)? else {
        return Ok(());
    };
    let refuse = "a secret must not reach the remote, so nothing was pushed. A person who finds it \
                  a false positive pushes the branch with their own git; once the remote has it, \
                  push goes on";
    match path {
        None => Err(format!(
            "the message of commit {oid} holds a line shaped like {shape} (line {line}); {refuse}"
        )),
        Some(path) => {
            // The oldest new commit that brings the blob in; a merge's own content too (`-m`).
            let log = [
                "log",
                "-m",
                "--format=%H",
                "--find-object",
                &oid,
                "--ignore-missing",
                "--stdin",
            ];
            let commit = git_in(dir, &log, range)
                .ok()
                .and_then(|l| l.lines().last().map(str::to_string))
                .map_or("a new commit".to_string(), |c| format!("commit {c}"));
            Err(format!(
                "{commit} adds {path}, which holds a line shaped like {shape} (line {line}); \
                 {refuse}"
            ))
        }
    }
}

/// An object's id, its path (none for a commit), the shape and the line.
type Match = (String, Option<String>, &'static str, usize);

/// The first object of `objects` (`rev-list --objects` lines: an id, and a path for a tree or a
/// blob) whose blob, or commit message, holds a token shape. One `cat-file --batch` reads them,
/// one at a time.
fn first_match(dir: &Path, objects: &str) -> std::result::Result<Option<Match>, String> {
    use std::io::{BufRead, BufReader, Read, Write};
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["cat-file", "--batch"])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let ids: String = objects
        .lines()
        .map(|l| format!("{}\n", l.split(' ').next().unwrap_or_default()))
        .collect();
    let mut stdin = child.stdin.take().ok_or("git cat-file has no input")?;
    let writer = std::thread::spawn(move || stdin.write_all(ids.as_bytes()));
    let mut out = BufReader::new(child.stdout.take().ok_or("git cat-file has no output")?);
    let mut read = || -> std::result::Result<Option<Match>, String> {
        for l in objects.lines() {
            let (oid, path) = l.split_once(' ').map_or((l, None), |(o, p)| (o, Some(p)));
            let mut header = String::new();
            out.read_line(&mut header)
                .map_err(|e| format!("cannot read git cat-file: {e}"))?;
            let parts: Vec<&str> = header.split_whitespace().collect();
            let [_, kind, size] = parts[..] else {
                return Err(format!("cannot read object {oid}: {}", header.trim()));
            };
            let size: usize = size
                .parse()
                .map_err(|_| format!("cannot read object {oid}: {}", header.trim()))?;
            let mut body = vec![0; size + 1];
            out.read_exact(&mut body)
                .map_err(|e| format!("cannot read object {oid}: {e}"))?;
            body.pop();
            let hit = match kind {
                "blob" => find(&body),
                "commit" => body
                    .windows(2)
                    .position(|w| w == b"\n\n")
                    .and_then(|at| find(&body[at + 2..])),
                _ => None,
            };
            if let Some((shape, line)) = hit {
                let path = (kind == "blob").then(|| path.unwrap_or_default().to_string());
                return Ok(Some((oid.to_string(), path, shape, line)));
            }
        }
        Ok(None)
    };
    let found = read();
    drop(out);
    let _ = child.kill();
    let _ = child.wait();
    let _ = writer.join();
    found
}

fn update(dir: &Path, j: &Job, base: &str, head: &str) -> Outcome {
    let fetch = |b: &str| {
        git(
            dir,
            &[
                "fetch",
                "-q",
                "origin",
                &format!("+refs/heads/{b}:refs/remotes/origin/{b}"),
            ],
        )
    };
    fetch(base).map_err(|e| format!("the base {base} cannot be fetched from origin: {e}"))?;
    fetch(&j.branch).map_err(|e| {
        format!(
            "the branch {} cannot be fetched from origin (a branch not pushed yet is the \
             worker's to rebase): {e}",
            j.branch
        )
    })?;
    let id = |r: String| git(dir, &["rev-parse", "--verify", &format!("{r}^{{commit}}")]);
    let base_id = id(format!("refs/remotes/origin/{base}"))?;
    let old = id(format!("refs/remotes/origin/{}", j.branch))?;
    if !is_ancestor(dir, &old, head)? {
        // A restart after the merge was pushed and before it was recorded: origin holds the
        // worktree's head and the base, which the worktree does not (it is not written).
        if is_ancestor(dir, head, &old)? && is_ancestor(dir, &base_id, &old)? {
            return Ok(Done::Current {
                head: old,
                base_merge: false,
            });
        }
        return Err(format!(
            "the branch {} at origin moved to {old}, which the worktree does not hold; nothing \
             was merged or pushed",
            j.branch
        ));
    }
    let new = if is_ancestor(dir, &base_id, head)? {
        head.to_string()
    } else if is_ancestor(dir, head, &base_id)? {
        base_id.clone()
    } else {
        merge(dir, j, base, head, &base_id)?
    };
    if new == old {
        return Ok(Done::Current {
            head: new,
            base_merge: false,
        });
    }
    let judged = judged_base(dir, j)?;
    before_ci(dir, j, &judged, &new)?;
    push_head(dir, &new, &j.branch).map_err(|e| {
        format!("the merged head {new} was not pushed: {e}; nothing changed at origin, update or push again")
    })?;
    Ok(Done::Updated {
        old,
        new,
        base_merge: false,
    })
}

/// The merge commit of `head` and the base commit `base_id`, made in the project's repository with
/// no work tree; a conflict names its files.
fn merge(
    dir: &Path,
    j: &Job,
    base: &str,
    head: &str,
    base_id: &str,
) -> std::result::Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            head,
            base_id,
        ])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let tree = lines.next().unwrap_or_default().to_string();
    match out.status.code() {
        Some(0) => {}
        Some(1) => {
            return Err(format!(
                "the merge of origin/{base} conflicts in {}; nothing was merged or pushed",
                lines.collect::<Vec<_>>().join(", ")
            ));
        }
        _ => {
            return Err(format!(
                "the merge of origin/{base} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    let message = format!("Merge origin/{base} into {}", j.branch);
    // The merge commit is written as git's own identity (the repository's or the person's
    // config). Only where git has none (a CI runner, a bare host) it is makiba <makiba@localhost>,
    // so the merge is made instead of failing with "Author identity unknown".
    let known = ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"]
        .iter()
        .all(|v| git(dir, &["var", v]).is_ok());
    let fallback: &[&str] = if known {
        &[]
    } else {
        &[
            "-c",
            "user.name=makiba",
            "-c",
            "user.email=makiba@localhost",
        ]
    };
    git(
        dir,
        &[
            fallback,
            &[
                "commit-tree",
                &tree,
                "-p",
                head,
                "-p",
                base_id,
                "-m",
                &message,
            ],
        ]
        .concat(),
    )
}

/// The checks of the worktree, reading only: the run's git folder and the worktree's canonical
/// path.
fn checked(places: &Places, j: &Job) -> std::result::Result<(PathBuf, PathBuf), String> {
    let path = j
        .worktree
        .as_deref()
        .ok_or_else(|| format!("task {} has no code worktree", j.task))?;
    let real = std::fs::canonicalize(path).map_err(|e| format!("worktree {path}: {e}"))?;
    let under =
        std::fs::canonicalize(&places.workers).is_ok_and(|t| real.starts_with(&t) && real != t);
    if !under {
        return Err(format!(
            "{path} is not under the workers folder {}",
            places.workers.display()
        ));
    }
    let gitdir = run_repo::dir(&places.settings, &j.task);
    if !run_repo::is_run(&gitdir, &real) {
        return Err(format!(
            "{path} has no repository of its own at {} (made before Issue 241)",
            gitdir.display()
        ));
    }
    let on = run_repo::read(&gitdir, &real, &["symbolic-ref", "--short", "-q", "HEAD"])
        .unwrap_or_default();
    if on != j.branch {
        return Err(format!(
            "worktree {path} is on {}, not on {}",
            if on.is_empty() { "no branch" } else { &on },
            j.branch
        ));
    }
    if !run_repo::status(&gitdir, &real, false)?.is_empty() {
        return Err(format!(
            "worktree {path} has uncommitted changes to tracked files"
        ));
    }
    for half in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        if gitdir.join(half).symlink_metadata().is_ok() {
            return Err(format!(
                "worktree {path} is in the middle of an operation ({half})"
            ));
        }
    }
    Ok((gitdir, real))
}

impl Push {
    pub fn new(places: Places) -> Push {
        let (tx, rx) = channel();
        Push {
            places,
            running: HashSet::new(),
            busy: HashSet::new(),
            projects: HashSet::new(),
            tx,
            rx,
        }
    }

    /// Write what the threads found, then start a thread for every `accepted` push without one.
    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        while let Ok((j, outcome)) = self.rx.try_recv() {
            match finish(w, &j, &outcome) {
                Ok(()) => {
                    self.running.remove(&j.command);
                    if let Some(wt) = &j.worktree {
                        self.busy.remove(wt);
                    }
                    self.projects.remove(&j.project);
                }
                Err(e) => {
                    eprintln!(
                        "makibad: {} of {}: cannot record its result: {e}",
                        j.verb, j.task
                    );
                    // Tried again on the next tick.
                    let _ = self.tx.send((j, outcome));
                    break;
                }
            }
        }
        let jobs: Vec<Job> = {
            let mut st = w.conn().prepare(
                "SELECT c.id, c.verb, r.task_id, COALESCE(r.branch, 'makiba/' || r.task_id),
                        COALESCE(p.slug, ''),
                        (SELECT worktree FROM run_launch WHERE run_id = r.id AND kind = 'code'
                         ORDER BY id DESC LIMIT 1),
                        CASE c.verb WHEN 'update' THEN
                          COALESCE(json_extract(c.args_json, '$.base'), '') END,
                        CASE c.verb WHEN 'push' THEN json_extract(c.args_json, '$.head') END,
                        COALESCE(p.default_branch, 'main'),
                        CASE c.verb WHEN 'update' THEN json_extract(c.args_json, '$.readiness') END,
                        json_extract(c.args_json, '$.repo'), json_extract(c.args_json, '$.pr'),
                        json_extract(c.args_json, '$.head')
                 FROM commands c JOIN runs r ON r.task_id = json_extract(c.args_json, '$.task')
                 LEFT JOIN projects p ON p.id = r.project_id
                 WHERE c.verb IN ('push', 'update') AND c.status = 'accepted' ORDER BY c.id",
            )?;
            st.query_map([], |r| {
                Ok(Job {
                    command: r.get(0)?,
                    verb: r.get(1)?,
                    task: r.get(2)?,
                    branch: r.get(3)?,
                    project: r.get(4)?,
                    worktree: r.get(5)?,
                    base: r.get(6)?,
                    head: r.get(7)?,
                    // The rest of what the check before CI needs is read when the job starts.
                    before: Before {
                        default_branch: r.get(8)?,
                        ..Default::default()
                    },
                    readiness: match (
                        r.get::<_, Option<i64>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<i64>>(11)?,
                        r.get::<_, Option<String>>(12)?,
                    ) {
                        (Some(_), Some(repo), Some(pr), Some(head)) => {
                            Some(Readiness { repo, pr, head })
                        }
                        _ => None,
                    },
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        for mut j in jobs {
            if self.running.contains(&j.command)
                || j.worktree.as_ref().is_some_and(|wt| self.busy.contains(wt))
                || self.projects.contains(&j.project)
            {
                continue;
            }
            let default_branch = std::mem::take(&mut j.before.default_branch);
            j.before = Before::read(w, &j.task, default_branch)?;
            self.running.insert(j.command);
            if let Some(wt) = &j.worktree {
                self.busy.insert(wt.clone());
            }
            self.projects.insert(j.project.clone());
            let (places, tx) = (self.places.clone(), self.tx.clone());
            // ponytail: no time limit on a push; a hung remote leaves the command `accepted`
            // until makibad restarts. Add a timeout if hosts show it.
            std::thread::spawn(move || {
                let outcome = work(&places, &j);
                let _ = tx.send((j, outcome));
            });
        }
        Ok(())
    }
}

/// What the secretary asks of the worker after an `update`: the fast-forward of `branch`, ended by
/// a one-line report, so the turn raises no `done_no_report` (Issue 386).
pub fn sync_request(branch: &str) -> String {
    format!(
        "tell the worker to run git fetch origin and git merge --ff-only origin/{branch}, then \
         report done in one line (\"fast-forwarded to <head>, nothing committed\"); if the \
         worker has committed since, the fast-forward fails: it then runs git merge \
         origin/{branch}, reports done again and you push again."
    )
}

/// For an update makibad started on its queue readiness judgement (Issue 475): `head` recorded as
/// the `pr` verb records a `pushed`, in the caller's transaction, so the new head is watched with
/// no step of the secretary. The sentence for the notice; `None` for any other update.
fn record_head(w: &StateWriter, j: &Job, head: &str, base_merge: bool) -> Result<Option<String>> {
    let Some(r) = &j.readiness else {
        return Ok(None);
    };
    // Only while the run's current pull request is still the one judged, at the head judged: a
    // head or a pull request recorded while the update ran is never overwritten by its result.
    let current: Option<(Option<String>, Option<i64>)> = w
        .conn()
        .query_row(
            &format!(
                "SELECT {}, {} FROM runs r WHERE r.task_id = ?1",
                super::belt::current_pr_sql("r", "repo"),
                super::belt::current_pr_sql("r", "pr")
            ),
            [&j.task],
            |q| Ok((q.get(0)?, q.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    let same_pr = matches!(&current, Some((Some(cr), Some(cp)))
        if cr.eq_ignore_ascii_case(&r.repo) && *cp == r.pr);
    let same_head = super::prwatch::newest_head(w.conn(), &j.task)?
        .is_some_and(|h| h.eq_ignore_ascii_case(&r.head));
    if !(same_pr && same_head) {
        return Ok(Some(format!(
            "makibad did not record it: {} no longer has {}#{} at {}, the head it judged, as its \
             newest record; record it with pr pushed if it is still wanted.",
            j.task, r.repo, r.pr, r.head
        )));
    }
    let args = json!({"state": "pushed", "repo": r.repo, "pr": r.pr, "head": head,
                      "task": j.task});
    Ok(Some(match super::door::pr(w, &args, base_merge) {
        Ok(_) => format!(
            "makibad recorded it for {}#{} (pr pushed) and watches its CI; you need not record it.",
            r.repo, r.pr
        ),
        // A failed write: the whole transaction is rolled back and tried again on the next tick.
        Err(e) if e.kind == crate::spec::ErrorKind::Internal => {
            return Err(crate::db::Error::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(e.message),
            )));
        }
        Err(e) => format!(
            "makibad could not record it ({}): record it with pr pushed and this head.",
            e.message
        ),
    }))
}

/// The command `done` with `branch_pushed`, or `failed`, and the notice to the secretary, together.
fn finish(w: &StateWriter, j: &Job, outcome: &Outcome) -> Result<()> {
    w.transaction(|w| {
        let (status, kind) = match outcome {
            Ok(_) => ("done", None),
            Err(_) => ("failed", Some("conflict")),
        };
        let n = w.conn().execute(
            &format!(
                "UPDATE commands SET status = ?1, error_kind = ?2, finished_at = {NOW}
                 WHERE id = ?3 AND status = 'accepted'"
            ),
            rusqlite::params![status, kind, j.command],
        )?;
        if n == 0 {
            return Ok(());
        }
        let base = j.base.as_deref().unwrap_or_default();
        let body = match outcome {
            Ok(Done::Pushed(head)) => {
                w.append_event(&NewEvent {
                    kind: "branch_pushed",
                    actor: Some("makibad"),
                    payload: Some(&json!({"task_id": j.task, "branch": j.branch, "head": head})),
                    run_task_id: Some(&j.task),
                    ..Default::default()
                })?;
                format!(
                    "The branch {} of {} is pushed to origin at {head}. Record it with pr \
                     (opened or pushed) and this head.",
                    j.branch, j.task
                )
            }
            Ok(Done::Updated {
                old,
                new,
                base_merge,
            }) => {
                w.append_event(&NewEvent {
                    kind: "branch_updated",
                    actor: Some("makibad"),
                    payload: Some(&json!({
                        "task_id": j.task, "branch": j.branch, "base": base,
                        "old": old, "new": new,
                    })),
                    run_task_id: Some(&j.task),
                    ..Default::default()
                })?;
                let record = match record_head(w, j, new, *base_merge)? {
                    Some(said) => said,
                    None => "Record it with pr pushed and this head.".to_string(),
                };
                format!(
                    "The branch {} of {} holds origin/{base} and is pushed to origin at {new} \
                     (was {old}). {record} The worker's tree is not changed: {}",
                    j.branch,
                    j.task,
                    sync_request(&j.branch)
                )
            }
            Ok(Done::Current { head, base_merge })
                if j.readiness
                    .as_ref()
                    .is_some_and(|r| !r.head.eq_ignore_ascii_case(head)) =>
            {
                let record = record_head(w, j, head, *base_merge)?.unwrap_or_default();
                format!(
                    "The branch {} of {} already holds origin/{base}; nothing was merged. origin \
                     has it at {head}. {record} If the worker's tree is behind it, {}",
                    j.branch,
                    j.task,
                    sync_request(&j.branch)
                )
            }
            Ok(Done::Current { head, .. }) => format!(
                "The branch {} of {} already holds origin/{base}; nothing was merged. origin has \
                 it at {head}; record it with pr pushed and this head if it is not recorded yet. \
                 If the worker's tree is behind it, {}",
                j.branch,
                j.task,
                sync_request(&j.branch)
            ),
            Err(why) => {
                eprintln!("makibad: {} of {}: {why}", j.verb, j.task);
                format!(
                    "The {} of {} was not done: {why}. Nothing was forced. Call {} again once \
                     the cause is dealt with (a conflict or uncommitted work goes to the worker \
                     with send).",
                    j.verb, j.task, j.verb
                )
            }
        };
        super::door::notice(w, AGENT_SECRETARY, &j.task, &body)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::writer::RunUpsert;

    fn job(task: &str, judged: Option<&str>) -> Job {
        Job {
            command: 1,
            verb: "update".into(),
            task: task.into(),
            branch: format!("makiba/{task}"),
            project: "p".into(),
            worktree: None,
            base: Some("main".into()),
            head: None,
            before: Before::default(),
            readiness: judged.map(|h| Readiness {
                repo: "o/r".into(),
                pr: 5,
                head: h.into(),
            }),
        }
    }

    /// Issue 475: a readiness update of a worktree the worker committed on since the judgement
    /// merges and pushes nothing; any other update is not held to a head.
    #[test]
    fn a_readiness_update_merges_only_the_judged_head() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        assert!(judged_head(&job("t", Some(&a)), &a).is_ok());
        let e = judged_head(&job("t", Some(&a)), &b).unwrap_err();
        assert!(e.contains("nothing was merged or pushed"), "{e}");
        assert!(judged_head(&job("t", None), &b).is_ok());
    }

    /// Issue 475: an update that ends after a newer head was recorded does not record its own
    /// result over it; one that ends on the head it judged does.
    #[test]
    fn a_late_readiness_update_does_not_record_over_a_newer_head() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t",
            project_slug: "p",
            status: Some("review"),
            ..Default::default()
        })
        .unwrap();
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        super::super::prwatch::register(&w, "pr_opened", "t", "o/r", 5, &a).unwrap();
        super::super::prwatch::register(&w, "fix_pushed", "t", "o/r", 5, &b).unwrap();
        let said = record_head(&w, &job("t", Some(&a)), &c, true)
            .unwrap()
            .unwrap();
        assert!(said.contains("did not record"), "{said}");
        assert_eq!(
            super::super::prwatch::newest_head(w.conn(), "t").unwrap(),
            Some(b.clone())
        );
        let said = record_head(&w, &job("t", Some(&b)), &c, true)
            .unwrap()
            .unwrap();
        assert!(said.contains("makibad recorded it"), "{said}");
        assert_eq!(
            super::super::prwatch::newest_head(w.conn(), "t").unwrap(),
            Some(c)
        );
    }
}
