//! Finishing a close (Issue M5-2, spec sections 5.2 and 12). The door accepts `close`: it decides
//! (a done report or `--abandon`, no unsaved work in a code worktree), sets the run `completed` or
//! `abandoned`, fails the launches under way and writes `run_closed`, and records the command
//! `accepted`. This pass, first in each tick of `makibad`'s loop, finishes every `accepted` close in order:
//!
//! 1. every terminal of this organization carrying the task or a spawn of the run is ended, and
//!    the pass goes on only once a new list shows none; every live worker key of the run or of a
//!    spawn of the run is revoked (kill point `close_ended`);
//! 2. each workplace of the run's launches is removed, only when it is strictly under the workers
//!    folder (code) or the documents folder (document): code after the unsaved-work check again
//!    unless `--abandon`, by keeping the run's branch (and a HEAD off it) in the project's
//!    repository, then removing the work tree and the run's git folder `<state>/git/<task>`
//!    (Issue 241), each only under its own folder; document by removing everything but `out/`
//!    (kill point `close_removed`);
//! 3. the per-task settings file is deleted and the command becomes `done`.
//!
//! The record of a half-done close is its `accepted` row. Every step reads what it has to do from
//! the world (the base's list, `agents`, the folders), so a restart runs the pass again and each
//! step finds its work done or does it. A step that cannot be done (a path outside those folders,
//! unsaved work, git refusing) makes the command `failed` and tells the secretary why; the door
//! then accepts another close of the run. A base that cannot list or end leaves the command
//! `accepted` for the next tick.

use super::prepare::Places;
use super::run_repo;
use crate::base::Base;
use crate::db::Result;
use crate::db::writer::StateWriter;
use crate::spec::{AGENT_SECRETARY, TAG_ORG, TAG_SPAWN, TAG_TASK_ID};
use rusqlite::params;
use std::path::Path;
use std::process::Command;

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// `git -C <dir> <args>`, running no hook (Issue 236): its trimmed output, or `None` when it fails.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", run_repo::HOOKS_OFF])
        .args(args)
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Why the run's work tree at `path` holds work a removal would lose, if it does: uncommitted
/// changes (untracked files included, whatever git's settings say), a submodule checked out, or
/// unpushed commits (`unpushed`). A work tree without a repository of its own, or
/// one git cannot read, counts as unsaved. The run's repository is only read (`run_repo`).
pub(super) fn unsaved(gitdir: &Path, path: &Path, repo: &Path, branch: &str) -> Option<String> {
    let shown = path.display();
    if !run_repo::is_run(gitdir, path) {
        return Some(format!(
            "worktree {shown} has no repository of its own at {} (made before Issue 241)",
            gitdir.display()
        ));
    }
    match run_repo::status(gitdir, path, true) {
        Err(_) => return Some(format!("git cannot read the worktree {shown}")),
        Ok(s) if !s.is_empty() => {
            return Some(format!("worktree {shown} has uncommitted changes"));
        }
        Ok(_) => {}
    }
    match run_repo::checked_out_submodules(gitdir, path) {
        Err(_) => return Some(format!("git cannot read the worktree {shown}")),
        Ok(subs) if !subs.is_empty() => {
            return Some(format!(
                "worktree {shown} has submodules checked out ({}), whose work makibad does not read",
                subs.join(", ")
            ));
        }
        Ok(_) => {}
    }
    unpushed(gitdir, path, repo, branch)
}

/// Unpushed commits of the run's repository: commits reachable from HEAD, the run's `branch` or
/// any local branch or tag, and from none of the project repository's `refs/remotes/origin/*`
/// (Issue 241: judged from the clone, which the worker cannot write); in a repository with no
/// remote every commit counts. Read without the work tree, which may be gone.
fn unpushed(gitdir: &Path, path: &Path, repo: &Path, branch: &str) -> Option<String> {
    let shown = path.display();
    let tip = format!("refs/heads/{branch}");
    let count = run_repo::at_origin(repo)
        .and_then(|saved| run_repo::count_not(gitdir, path, &["HEAD", &tip], &saved));
    match count {
        Ok(0) => None,
        Ok(n) => Some(format!(
            "worktree {shown} has unpushed commits ({n} on no remote branch)"
        )),
        Err(_) => Some(format!("git cannot read the commits of {shown}")),
    }
}

pub struct Close {
    places: Places,
    /// This organization's `makiba.org` tag.
    org: String,
}

struct Job {
    command: i64,
    task: String,
    branch: String,
    run: i64,
    project: String,
    abandon: bool,
}

impl Close {
    pub fn new(places: Places, org: String) -> Close {
        Close { places, org }
    }

    /// One pass over the `accepted` closes, oldest first.
    pub fn pass(&self, w: &StateWriter, base: &dyn Base) -> Result<()> {
        let jobs: Vec<Job> = {
            let mut st = w.conn().prepare(
                "SELECT c.id, r.task_id, COALESCE(r.branch, 'makiba/' || r.task_id), r.id, p.slug,
                        COALESCE(json_type(c.args_json, '$.abandon') = 'true', 0)
                 FROM commands c JOIN runs r ON r.task_id = json_extract(c.args_json, '$.task_id')
                 JOIN projects p ON p.id = r.project_id
                 WHERE c.verb = 'close' AND c.status = 'accepted' ORDER BY c.id",
            )?;
            st.query_map([], |r| {
                Ok(Job {
                    command: r.get(0)?,
                    task: r.get(1)?,
                    branch: r.get(2)?,
                    run: r.get(3)?,
                    project: r.get(4)?,
                    abandon: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        for j in jobs {
            match self.finish(w, base, &j) {
                Ok(true) => {
                    w.conn().execute(
                        &format!(
                            "UPDATE commands SET status = 'done', finished_at = {NOW}
                             WHERE id = ?1 AND status = 'accepted'"
                        ),
                        [j.command],
                    )?;
                }
                Ok(false) => {}
                Err(why) => fail(w, &j, &why)?,
            }
        }
        Ok(())
    }

    /// `Ok(false)`: the base could not list or end; tried again on the next tick.
    fn finish(
        &self,
        w: &StateWriter,
        base: &dyn Base,
        j: &Job,
    ) -> std::result::Result<bool, String> {
        let db = |e: rusqlite::Error| e.to_string();
        let launches: Vec<(String, String, String)> = w
            .conn()
            .prepare("SELECT spawn_id, kind, worktree FROM run_launch WHERE run_id = ?1")
            .map_err(db)?
            .query_map([j.run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(db)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db)?;
        let spawns: Vec<&str> = launches.iter().map(|l| l.0.as_str()).collect();

        if !self.end_terminals(base, &j.task, &spawns) {
            return Ok(false);
        }
        w.conn()
            .execute(
                &format!(
                    "UPDATE agents SET revoked_at = {NOW}
                     WHERE role = 'worker' AND revoked_at IS NULL
                       AND (run_id = ?1 OR spawn_id IN
                            (SELECT spawn_id FROM run_launch WHERE run_id = ?1))"
                ),
                [j.run],
            )
            .map_err(db)?;
        super::crash::point("close_ended");

        let mut done: Vec<&str> = Vec::new();
        for (_, kind, path) in &launches {
            if !done.contains(&path.as_str()) {
                self.remove(j, kind, Path::new(path))?;
                done.push(path);
            }
        }
        super::crash::point("close_removed");

        if j.task.is_empty() || j.task.starts_with('.') || j.task.contains(['/', '\\']) {
            return Err(format!("task id {:?} is not a plain name", j.task));
        }
        match std::fs::remove_file(self.places.settings.join(format!("{}.json", j.task))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(true),
        }
    }

    /// End every terminal of this organization carrying `task` or one of `spawns`. `true` once a
    /// new list shows none of them.
    fn end_terminals(&self, base: &dyn Base, task: &str, spawns: &[&str]) -> bool {
        let ours = |base: &dyn Base| -> Option<Vec<crate::base::TerminalRef>> {
            let list = base
                .list()
                .map_err(|e| eprintln!("makibad: close of {task}: cannot list: {e}"))
                .ok()?;
            Some(
                list.into_iter()
                    .filter(|t| t.tags.get(TAG_ORG) == Some(&self.org))
                    .filter(|t| {
                        t.tags.get(TAG_TASK_ID).map(String::as_str) == Some(task)
                            || t.tags
                                .get(TAG_SPAWN)
                                .is_some_and(|s| spawns.contains(&s.as_str()))
                    })
                    .map(|t| t.id)
                    .collect(),
            )
        };
        let Some(found) = ours(base) else {
            return false;
        };
        for id in &found {
            if let Err(e) = base.end(id) {
                eprintln!("makibad: close of {task}: cannot end {}: {e}", id.0);
            }
        }
        found.is_empty() || ours(base).is_some_and(|left| left.is_empty())
    }

    /// Remove one workplace of the run, only under the folder of its kind.
    fn remove(&self, j: &Job, kind: &str, path: &Path) -> std::result::Result<(), String> {
        let repo = self.places.repos.join(&j.project);
        let gitdir = run_repo::dir(&self.places.settings, &j.task);
        let git_root = run_repo::root(&self.places.settings);
        let top = if kind == "document" {
            &self.places.docs
        } else {
            &self.places.workers
        };
        match path.symlink_metadata() {
            // Gone (a restart after the removal, or removed by hand): the run's git folder goes
            // too, once its commits are saved or kept as with a work tree.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if kind == "document" {
                    return Ok(());
                }
                if gitdir.join("HEAD").is_file() {
                    if !j.abandon
                        && let Some(why) = unpushed(&gitdir, path, &repo, &j.branch)
                    {
                        return Err(why);
                    }
                    keep(&gitdir, path, &repo, j)?;
                }
                return run_repo::remove_under(&git_root, &gitdir);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
            Ok(_) => {}
        }
        let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let under = std::fs::canonicalize(top).is_ok_and(|t| real.starts_with(&t) && real != t);
        if !under {
            return Err(format!(
                "{} is not under {}: it is not removed",
                path.display(),
                top.display()
            ));
        }
        if kind == "document" {
            let entries = std::fs::read_dir(&real).map_err(|e| e.to_string())?;
            for e in entries {
                let e = e.map_err(|e| e.to_string())?;
                if e.file_name() == "out" {
                    continue;
                }
                let gone = if e.file_type().map_err(|e| e.to_string())?.is_dir() {
                    std::fs::remove_dir_all(e.path())
                } else {
                    std::fs::remove_file(e.path())
                };
                gone.map_err(|err| format!("{}: {err}", e.path().display()))?;
            }
            return Ok(());
        }
        if !j.abandon
            && let Some(why) = unsaved(&gitdir, &real, &repo, &j.branch)
        {
            return Err(why);
        }
        // The git folder decides, not the work tree's `.git` file, which the worker can delete.
        if gitdir.join("HEAD").is_file() {
            keep(&gitdir, &real, &repo, j)?;
        }
        run_repo::remove_under(top, &real)?;
        run_repo::remove_under(&git_root, &gitdir)?;
        // A worktree of the clone from before Issue 241 leaves an entry there.
        git(&repo, &["worktree", "prune"]);
        Ok(())
    }
}

/// The run's commits, kept in the project's repository before the run's own repository goes: the
/// branch as `refs/heads/<branch>` when the clone has none or it moves forward, else as
/// `refs/makiba/closed/<task>/<commit>`, and HEAD and every other local branch or tag that holds a
/// commit nothing kept holds as `refs/makiba/closed/<task>/<commit>` (one ref per commit, so two
/// workplaces of the run never overwrite each other's). Each is checked in the clone; the
/// hand-over ref goes.
fn keep(gitdir: &Path, real: &Path, repo: &Path, j: &Job) -> std::result::Result<(), String> {
    let closed = |c: &str| format!("refs/makiba/closed/{}/{c}", j.task);
    let tip = run_repo::commit(gitdir, real, &format!("refs/heads/{}", j.branch));
    let mut kept = Vec::new();
    if let Some(tip) = &tip {
        let on_branch = format!("refs/heads/{}", j.branch);
        if run_repo::deliver(gitdir, real, repo, tip, &on_branch, false).is_err() {
            run_repo::deliver(gitdir, real, repo, tip, &closed(tip), true)?;
        }
        kept.push(tip.clone());
    }
    let others = run_repo::read(
        gitdir,
        real,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/heads",
            "refs/tags",
        ],
    )?;
    for r in std::iter::once("HEAD").chain(others.lines()) {
        let Some(c) = run_repo::commit(gitdir, real, r) else {
            continue;
        };
        let held = kept
            .iter()
            .any(|k| run_repo::read(gitdir, real, &["merge-base", "--is-ancestor", &c, k]).is_ok());
        if !held {
            run_repo::deliver(gitdir, real, repo, &c, &closed(&c), true)?;
            kept.push(c);
        }
    }
    for c in &kept {
        git(repo, &["cat-file", "-e", &format!("{c}^{{commit}}")]).ok_or_else(|| {
            format!(
                "{c} of {} is not kept in {}",
                real.display(),
                repo.display()
            )
        })?;
    }
    let hand_over = format!("refs/makiba/runs/{}", j.task);
    if git(repo, &["rev-parse", "--verify", "-q", &hand_over]).is_some() {
        git(repo, &["update-ref", "-d", &hand_over])
            .ok_or_else(|| format!("cannot delete {hand_over} in {}", repo.display()))?;
    }
    Ok(())
}

/// The close could not be finished: the command `failed` and a notice to the secretary, together.
fn fail(w: &StateWriter, j: &Job, why: &str) -> Result<()> {
    eprintln!("makibad: close of {}: {why}", j.task);
    w.transaction(|w| {
        w.conn().execute(
            &format!(
                "UPDATE commands SET status = 'failed', error_kind = 'conflict', finished_at = {NOW},
                   result_json = json_set(COALESCE(result_json, '{{}}'), '$.error', json(?2))
                 WHERE id = ?1 AND status = 'accepted'"
            ),
            params![
                j.command,
                serde_json::json!({"kind": "conflict", "message": why, "hint": ""}).to_string()
            ],
        )?;
        let body = format!(
            "The close of {} could not be finished: {why}. Close it again (with --abandon to \
             remove unsaved work) once the cause is dealt with.",
            j.task
        );
        super::door::notice(w, AGENT_SECRETARY, &j.task, &body)?;
        Ok(())
    })
}
