//! Finishing a close (Issue M5-2, spec sections 5.2 and 12). The door accepts `close`: it decides
//! (a done report or `--abandon`, no unsaved work in a code worktree), sets the run `completed` or
//! `abandoned`, fails the launches under way and writes `run_closed`, and records the command
//! `accepted`. This pass, last in `makibad`'s loop, finishes every `accepted` close in order:
//!
//! 1. every terminal of this organization carrying the task or a spawn of the run is ended, and
//!    the pass goes on only once a new list shows none; every live worker key of the run or of a
//!    spawn of the run is revoked (kill point `close_ended`);
//! 2. each workplace of the run's launches is removed, only when it is strictly under the workers
//!    folder (code) or the documents folder (document): code by `git worktree remove` from the
//!    project's repository, after the unsaved-work check again unless `--abandon`; document by
//!    removing everything but `out/` (kill point `close_removed`);
//! 3. the per-task settings file is deleted and the command becomes `done`.
//!
//! The record of a half-done close is its `accepted` row. Every step reads what it has to do from
//! the world (the base's list, `agents`, the folders), so a restart runs the pass again and each
//! step finds its work done or does it. A step that cannot be done (a path outside those folders,
//! unsaved work, git refusing) makes the command `failed` and tells the secretary why; the door
//! then accepts another close of the run. A base that cannot list or end leaves the command
//! `accepted` for the next tick.

use super::prepare::Places;
use crate::base::Base;
use crate::db::Result;
use crate::db::writer::StateWriter;
use crate::spec::{AGENT_SECRETARY, TAG_ORG, TAG_SPAWN, TAG_TASK_ID};
use rusqlite::params;
use std::path::Path;
use std::process::Command;

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// `git -C <dir> <args>`: its trimmed output, or `None` when it fails.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output();
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Why the worktree at `path` holds work a removal would lose, if it does: uncommitted changes
/// (untracked files included, whatever git's settings say), or unpushed commits, that is commits
/// reachable from HEAD and from no remote-tracking branch. With an upstream that is `@{u}..HEAD`;
/// a branch with no upstream counts every commit on no remote branch, and in a repository with no
/// remote every commit counts. A worktree git cannot read counts as unsaved.
pub(super) fn unsaved(path: &Path) -> Option<String> {
    let shown = path.display();
    match git(path, &["status", "--porcelain", "--untracked-files=all"]) {
        None => return Some(format!("git cannot read the worktree {shown}")),
        Some(s) if !s.is_empty() => {
            return Some(format!("worktree {shown} has uncommitted changes"));
        }
        Some(_) => {}
    }
    match git(path, &["rev-list", "--count", "HEAD", "--not", "--remotes"]).as_deref() {
        Some("0") => None,
        Some(n) => Some(format!(
            "worktree {shown} has unpushed commits ({n} on no remote branch)"
        )),
        None => Some(format!("git cannot read the commits of {shown}")),
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
                "SELECT c.id, r.task_id, r.id, p.slug,
                        COALESCE(json_extract(c.args_json, '$.abandon'), 0) = 1
                 FROM commands c JOIN runs r ON r.task_id = json_extract(c.args_json, '$.task_id')
                 JOIN projects p ON p.id = r.project_id
                 WHERE c.verb = 'close' AND c.status = 'accepted' ORDER BY c.id",
            )?;
            st.query_map([], |r| {
                Ok(Job {
                    command: r.get(0)?,
                    task: r.get(1)?,
                    run: r.get(2)?,
                    project: r.get(3)?,
                    abandon: r.get(4)?,
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
        let prune = || {
            if kind != "document" {
                git(&repo, &["worktree", "prune"]);
            }
        };
        match path.symlink_metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                prune();
                return Ok(());
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
            Ok(_) => {}
        }
        let top = if kind == "document" {
            &self.places.docs
        } else {
            &self.places.workers
        };
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
        let real_s = real.to_string_lossy();
        if j.abandon {
            // A HEAD on no branch would be lost with the worktree: a ref keeps it.
            let lost = git(
                &real,
                &[
                    "rev-list",
                    "--count",
                    "HEAD",
                    "--not",
                    "--branches",
                    "--remotes",
                ],
            );
            if lost.is_some_and(|n| n != "0")
                && let Some(head) = git(&real, &["rev-parse", "HEAD"])
            {
                let r = format!("refs/makiba/closed/{}", j.task);
                git(&repo, &["update-ref", &r, &head])
                    .ok_or_else(|| format!("cannot keep HEAD {head} of {real_s} as {r}"))?;
            }
        } else if let Some(why) = unsaved(&real) {
            return Err(why);
        }
        let mut args = vec!["worktree", "remove"];
        if j.abandon {
            args.push("--force");
        }
        args.push(&real_s);
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(&args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "git worktree remove {real_s}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        prune();
        Ok(())
    }
}

/// The close could not be finished: the command `failed` and a notice to the secretary, together.
fn fail(w: &StateWriter, j: &Job, why: &str) -> Result<()> {
    eprintln!("makibad: close of {}: {why}", j.task);
    w.transaction(|w| {
        w.conn().execute(
            &format!(
                "UPDATE commands SET status = 'failed', error_kind = 'conflict', finished_at = {NOW}
                 WHERE id = ?1 AND status = 'accepted'"
            ),
            params![j.command],
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
