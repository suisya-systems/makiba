//! Finishing a `push` (Issue 190, spec section 12). The door accepts it for a run with a code
//! worktree; this pass gives every `accepted` push a thread of its own, which runs git outside
//! the door's transaction and touches no database, and writes what the thread found on a later
//! tick: the command `done` with `branch_pushed` and a notice naming the head, or `failed` with a
//! notice saying why, together.
//!
//! The thread checks, at the time it runs: the worktree is strictly under the workers folder and
//! a worktree of the project's repository, on the run's branch, with no uncommitted change to a
//! tracked file and nothing half-joined (a merge, rebase, cherry-pick or revert, one commit or a sequence). It pushes
//! `<head>:refs/heads/<branch>` to `origin`, never forced, with no terminal prompt, so a missing
//! credential fails at once with git's message. A restart runs an `accepted` push again; git
//! finds it pushed already or pushes it.
//!
//! `update` (Issue 183) runs here too, after the same checks: it fetches the base and the branch
//! from `origin` by their full refs, refuses a branch not at origin or one that moved there (not
//! an ancestor of the worktree's head), merges `origin/<base>` when the head does not hold it (a
//! merge commit, never a rebase; a failed merge is aborted and its files named), and pushes the
//! head when origin does not have it, never forced. Done: `branch_updated` with the old and new
//! head, or, when nothing was merged or pushed, a notice naming origin's head. A restart runs it
//! again: a merge not pushed yet is pushed, a push not recorded yet is found done. One job runs per
//! worktree at a time.

use super::prepare::Places;
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::AGENT_SECRETARY;
use serde_json::json;
use std::collections::HashSet;
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
}

/// What a thread did.
enum Done {
    /// `push`: the head pushed.
    Pushed(String),
    /// `update`: origin's head of the branch before and after.
    Updated { old: String, new: String },
    /// `update`: nothing to merge or push; origin's head of the branch.
    Current(String),
}

type Outcome = std::result::Result<Done, String>;

pub struct Push {
    places: Places,
    /// Commands whose thread runs or whose result is not written yet.
    running: HashSet<i64>,
    /// Their worktrees: one job per worktree at a time.
    busy: HashSet<String>,
    tx: Sender<(Job, Outcome)>,
    rx: Receiver<(Job, Outcome)>,
}

/// `git -C <dir> <args>` with no terminal prompt: its trimmed output, or git's message.
fn git(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
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
        .args(["merge-base", "--is-ancestor", a, b])
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
    let real = checked(places, j)?;
    let head = git(&real, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    match j.base.as_deref() {
        None => push_head(&real, &head, &j.branch).map(|()| Done::Pushed(head)),
        Some(base) => update(&real, j, base, &head),
    }
}

/// The commit first, so no branch name can read as a force (`+`).
fn push_head(dir: &Path, head: &str, branch: &str) -> std::result::Result<(), String> {
    git(
        dir,
        &["push", "origin", &format!("{head}:refs/heads/{branch}")],
    )
    .map(drop)
}

/// The path of `name` in the worktree's git folder exists (a merge or another operation under way).
fn under_way(dir: &Path, name: &str) -> std::result::Result<bool, String> {
    let p = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-path", name],
    )?;
    Ok(PathBuf::from(p).exists())
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
        return Err(format!(
            "the branch {} at origin moved to {old}, which the worktree does not hold; nothing \
             was merged or pushed",
            j.branch
        ));
    }
    if !is_ancestor(dir, &base_id, head)? {
        let message = format!("Merge origin/{base} into {}", j.branch);
        let merged = git(
            dir,
            &[
                "merge",
                "--no-edit",
                "--commit",
                "--no-squash",
                "--ff",
                "--no-overwrite-ignore",
                "-m",
                &message,
                &base_id,
            ],
        );
        if let Err(why) = merged {
            let files = git(dir, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let undone = if under_way(dir, "MERGE_HEAD")? {
                match git(dir, &["merge", "--abort"]) {
                    Ok(_) => "the merge is undone".to_string(),
                    Err(e) => {
                        format!("the merge could not be undone, the worktree needs a look: {e}")
                    }
                }
            } else {
                "nothing was merged".to_string()
            };
            return Err(if files.is_empty() {
                format!("the merge of origin/{base} failed, {undone}: {why}")
            } else {
                format!(
                    "the merge of origin/{base} conflicts in {}; {undone}, nothing was pushed",
                    files.lines().collect::<Vec<_>>().join(", ")
                )
            });
        }
        let clean = git(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty();
        if !is_ancestor(dir, &base_id, "HEAD")? || !clean || under_way(dir, "MERGE_HEAD")? {
            return Err(format!(
                "the merge of origin/{base} did not end in a commit; the worktree needs a look"
            ));
        }
    }
    let new = git(dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if new == old {
        return Ok(Done::Current(new));
    }
    push_head(dir, &new, &j.branch).map_err(|e| {
        format!(
            "the worktree is at {new}, but the push failed: {e}; nothing was reset, update or push \
             again"
        )
    })?;
    Ok(Done::Updated { old, new })
}

/// The checks of the worktree, before any write: its canonical path.
fn checked(places: &Places, j: &Job) -> std::result::Result<PathBuf, String> {
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
    let common = git(
        &real,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let repo = places.repos.join(&j.project).join(".git");
    if std::fs::canonicalize(&common).ok() != std::fs::canonicalize(&repo).ok() {
        return Err(format!(
            "{path} is not a worktree of {}",
            places.repos.join(&j.project).display()
        ));
    }
    let on = git(&real, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
    if on != j.branch {
        return Err(format!(
            "worktree {path} is on {}, not on {}",
            if on.is_empty() { "no branch" } else { &on },
            j.branch
        ));
    }
    if !git(&real, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
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
        if under_way(&real, half)? {
            return Err(format!(
                "worktree {path} is in the middle of an operation ({half})"
            ));
        }
    }
    Ok(real)
}

impl Push {
    pub fn new(places: Places) -> Push {
        let (tx, rx) = channel();
        Push {
            places,
            running: HashSet::new(),
            busy: HashSet::new(),
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
                          COALESCE(json_extract(c.args_json, '$.base'), '') END
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
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        for j in jobs {
            if self.running.contains(&j.command)
                || j.worktree.as_ref().is_some_and(|wt| self.busy.contains(wt))
            {
                continue;
            }
            self.running.insert(j.command);
            if let Some(wt) = &j.worktree {
                self.busy.insert(wt.clone());
            }
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
            Ok(Done::Updated { old, new }) => {
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
                format!(
                    "The branch {} of {} holds origin/{base} and is pushed to origin at {new} \
                     (was {old}). Record it with pr pushed and this head.",
                    j.branch, j.task
                )
            }
            Ok(Done::Current(head)) => format!(
                "The branch {} of {} already holds origin/{base}; nothing was merged. origin has \
                 it at {head}; record it with pr pushed and this head if it is not recorded yet.",
                j.branch, j.task
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
