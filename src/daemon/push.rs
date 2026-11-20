//! Finishing a `push` (Issue 190, spec section 12). The door accepts it for a run with a code
//! worktree; this pass gives every `accepted` push a thread of its own, which runs git outside
//! the door's transaction and touches no database, and writes what the thread found on a later
//! tick: the command `done` with `branch_pushed` and a notice naming the head, or `failed` with a
//! notice saying why, together.
//!
//! The thread checks, at the time it runs: the worktree is strictly under the workers folder and
//! a worktree of the project's repository, on the run's branch, with no uncommitted change to a
//! tracked file and nothing half-joined (a merge, rebase, cherry-pick or revert). It pushes
//! `<head>:refs/heads/<branch>` to `origin`, never forced, with no terminal prompt, so a missing
//! credential fails at once with git's message. A restart runs an `accepted` push again; git
//! finds it pushed already or pushes it.

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
    task: String,
    branch: String,
    project: String,
    worktree: Option<String>,
}

pub struct Push {
    places: Places,
    /// Commands whose thread runs or whose result is not written yet.
    running: HashSet<i64>,
    tx: Sender<(Job, std::result::Result<String, String>)>,
    rx: Receiver<(Job, std::result::Result<String, String>)>,
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

/// The checks and the push; returns the head pushed.
fn work(places: &Places, j: &Job) -> std::result::Result<String, String> {
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
    ] {
        let p = PathBuf::from(git(
            &real,
            &["rev-parse", "--path-format=absolute", "--git-path", half],
        )?);
        if p.exists() {
            return Err(format!(
                "worktree {path} is in the middle of an operation ({half})"
            ));
        }
    }
    let head = git(&real, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    // The commit first, so no branch name can read as a force (`+`).
    git(
        &real,
        &["push", "origin", &format!("{head}:refs/heads/{}", j.branch)],
    )?;
    Ok(head)
}

impl Push {
    pub fn new(places: Places) -> Push {
        let (tx, rx) = channel();
        Push {
            places,
            running: HashSet::new(),
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
                }
                Err(e) => {
                    eprintln!("makibad: push of {}: cannot record its result: {e}", j.task);
                    // Tried again on the next tick.
                    let _ = self.tx.send((j, outcome));
                    break;
                }
            }
        }
        let jobs: Vec<Job> = {
            let mut st = w.conn().prepare(
                "SELECT c.id, r.task_id, COALESCE(r.branch, 'makiba/' || r.task_id),
                        COALESCE(p.slug, ''),
                        (SELECT worktree FROM run_launch WHERE run_id = r.id AND kind = 'code'
                         ORDER BY id DESC LIMIT 1)
                 FROM commands c JOIN runs r ON r.task_id = json_extract(c.args_json, '$.task')
                 LEFT JOIN projects p ON p.id = r.project_id
                 WHERE c.verb = 'push' AND c.status = 'accepted' ORDER BY c.id",
            )?;
            st.query_map([], |r| {
                Ok(Job {
                    command: r.get(0)?,
                    task: r.get(1)?,
                    branch: r.get(2)?,
                    project: r.get(3)?,
                    worktree: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        for j in jobs {
            if !self.running.insert(j.command) {
                continue;
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
fn finish(w: &StateWriter, j: &Job, outcome: &std::result::Result<String, String>) -> Result<()> {
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
        let body = match outcome {
            Ok(head) => {
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
            Err(why) => {
                eprintln!("makibad: push of {}: {why}", j.task);
                format!(
                    "The push of {} was not done: {why}. Nothing was forced. Push again once \
                     the cause is dealt with.",
                    j.task
                )
            }
        };
        super::door::notice(w, AGENT_SECRETARY, &j.task, &body)?;
        Ok(())
    })
}
