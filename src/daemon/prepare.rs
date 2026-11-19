//! Preparing a taken run (Issue M3-2): its workplace and its per-task settings file.
//!
//! A launch is prepared once its `run_taken` event exists (M3-1). Code: `git worktree add` from
//! the project's own repository into `$MAKIBA_WORKERS_DIR/<project>/<task_id>` on a new branch
//! from the project's default branch. Document: `$MAKIBA_DOCS_DIR/<task_id>/out/`, no git. Then
//! the agent module writes `<state>/settings/<task_id>.json` (spec sections 2 and 9). The stage
//! moves `planned`, `worktree_ready`, `settings_ready`, each recorded before the next step, and
//! a launch already past a stage is not touched for it. Nothing is written inside a worktree.
//!
//! Reading of the Issue where the spec is silent (reported to the secretary):
//! - (Owner) The project's own repository is `$MAKIBA_REPOS_DIR/<project>` (default
//!   `~/makiba-repos/<project>`); with no git repository there the launch fails. Nothing is cloned.
//! - The new branch is named `runs.branch` when set, else `makiba/<task_id>`; it starts from
//!   `projects.default_branch`.
//! - (Owner) The workplace path is read from `run_launch.worktree`; `Places::workplace` computes
//!   it, for the dispatch verb to use when it makes the row.

use crate::agents::{self, Agent, TaskSettings};
use crate::db::Result;
use crate::db::writer::StateWriter;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The folders preparation writes into.
pub struct Places {
    pub workers: PathBuf,
    pub docs: PathBuf,
    pub repos: PathBuf,
    pub settings: PathBuf,
}

impl Places {
    /// `$MAKIBA_WORKERS_DIR` and `$MAKIBA_DOCS_DIR` with the defaults of spec section 2;
    /// the settings folder is `<state>/settings`.
    pub fn from_env(state: &Path) -> Places {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let dir = |var: &str, default: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(default))
        };
        Places {
            workers: dir("MAKIBA_WORKERS_DIR", "makiba-workers"),
            docs: dir("MAKIBA_DOCS_DIR", "makiba-docs"),
            repos: dir("MAKIBA_REPOS_DIR", "makiba-repos"),
            settings: state.join("settings"),
        }
    }

    /// Where the workplace of a launch is (spec section 2): the worktree of a code task, or the
    /// working folder of a document task.
    pub fn workplace(&self, kind: &str, project: &str, task: &str) -> PathBuf {
        if kind == "document" {
            self.docs.join(task)
        } else {
            self.workers.join(project).join(task)
        }
    }
}

pub struct Prepare {
    places: Places,
}

struct Launch {
    id: i64,
    task: String,
    project: String,
    default_branch: String,
    branch: Option<String>,
    kind: String,
    agent: String,
    stage: String,
    worktree: String,
    spawn: String,
}

impl Prepare {
    pub fn new(places: Places) -> Prepare {
        Prepare { places }
    }

    /// One pass: moves every taken launch still `planned` or `worktree_ready` as far as it goes.
    /// Returns the task ids that reached `settings_ready`. A launch that cannot be prepared goes
    /// through the failure path of M3-6 (Issue 124): `worker_spawn_failed` with the stage and the
    /// reason, the launch and its run `failed`, and a notice to the secretary, in one transaction.
    pub fn pass(&self, w: &StateWriter) -> Result<Vec<String>> {
        let launches: Vec<Launch> = {
            let mut st = w.conn().prepare(
                "SELECT l.id, r.task_id, p.slug, COALESCE(p.default_branch, 'main'),
                        r.branch, l.kind, l.agent, l.stage, l.worktree, l.spawn_id
                 FROM run_launch l JOIN runs r ON r.id = l.run_id
                 JOIN projects p ON p.id = r.project_id
                 WHERE l.stage IN ('planned', 'worktree_ready')
                   AND EXISTS (SELECT 1 FROM events e WHERE e.kind = 'run_taken'
                               AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id)
                 ORDER BY l.id",
            )?;
            st.query_map([], |r| {
                Ok(Launch {
                    id: r.get(0)?,
                    task: r.get(1)?,
                    project: r.get(2)?,
                    default_branch: r.get(3)?,
                    branch: r.get(4)?,
                    kind: r.get(5)?,
                    agent: r.get(6)?,
                    stage: r.get(7)?,
                    worktree: r.get(8)?,
                    spawn: r.get(9)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        let mut ready = Vec::new();
        for l in launches {
            match self.prepare(w, &l) {
                Ok(()) => ready.push(l.task),
                Err(why) => {
                    // The stage it got to: `prepare` may have moved it past the one read.
                    let stage: String = w.conn().query_row(
                        "SELECT stage FROM run_launch WHERE id = ?1",
                        [l.id],
                        |r| r.get(0),
                    )?;
                    super::verify::fail(w, l.id, &l.task, &l.spawn, &stage, &why, &[])?;
                }
            }
        }
        Ok(ready)
    }

    fn prepare(&self, w: &StateWriter, l: &Launch) -> std::result::Result<(), String> {
        let db = |e: crate::db::Error| e.to_string();
        let one = |s: &str| !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\']);
        if !one(&l.task) || !one(&l.project) {
            return Err(format!(
                "task id or project is not a plain name: {:?}",
                l.task
            ));
        }
        let workplace = PathBuf::from(&l.worktree);
        if l.stage == "planned" {
            if l.kind == "document" {
                std::fs::create_dir_all(workplace.join("out")).map_err(|e| e.to_string())?;
            } else {
                self.add_worktree(l, &workplace)?;
            }
            w.conn()
                .execute(
                    "UPDATE run_launch SET stage = 'worktree_ready' WHERE id = ?1",
                    [l.id],
                )
                .map_err(|e| db(e.into()))?;
            super::crash::point("worktree_ready");
        }
        let agent: Box<dyn Agent> = agents::by_name(&l.agent)
            .ok_or_else(|| format!("no agent module named {:?}", l.agent))?;
        std::fs::create_dir_all(&self.places.settings).map_err(|e| e.to_string())?;
        let settings = TaskSettings {
            write_dirs: vec![workplace, std::env::temp_dir()],
        };
        agent
            .write_task_settings(
                &self.places.settings.join(format!("{}.json", l.task)),
                &settings,
            )
            .map_err(|e| e.to_string())?;
        w.conn()
            .execute(
                "UPDATE run_launch SET stage = 'settings_ready' WHERE id = ?1",
                [l.id],
            )
            .map_err(|e| db(e.into()))?;
        super::crash::point("settings_ready");
        Ok(())
    }

    /// A pass cut short before the stage was written leaves the worktree behind: it is kept only
    /// when it is a clean worktree of this repository on the expected branch.
    fn check_existing(repo: &Path, path: &Path, branch: &str) -> std::result::Result<(), String> {
        let git = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(path).args(args).output();
            out.ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        let same = common.is_some_and(|c| {
            std::fs::canonicalize(c).ok() == std::fs::canonicalize(repo.join(".git")).ok()
        });
        let on_branch = git(&["symbolic-ref", "--short", "HEAD"]).as_deref() == Some(branch);
        let clean = git(&["status", "--porcelain", "--untracked-files=all"]).as_deref() == Some("");
        if same && on_branch && clean {
            Ok(())
        } else {
            Err(format!(
                "{} exists and is not a clean worktree of the project on {branch}",
                path.display()
            ))
        }
    }

    /// `git worktree add -b <branch> <path> <default branch>` in the project's repository.
    fn add_worktree(&self, l: &Launch, path: &Path) -> std::result::Result<(), String> {
        let repo = Some(self.places.repos.join(&l.project)).filter(|r| r.join(".git").exists());
        let repo = repo.ok_or_else(|| {
            format!(
                "project {:?} has no git repository in the repositories folder",
                l.project
            )
        })?;
        let branch = l
            .branch
            .clone()
            .unwrap_or_else(|| format!("makiba/{}", l.task));
        if path.exists() {
            return Self::check_existing(&repo, path, &branch);
        }
        std::fs::create_dir_all(path.parent().unwrap_or(path)).map_err(|e| e.to_string())?;
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add", "-b", &branch])
            .arg(path)
            .arg(&l.default_branch)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::dispatch::Dispatch;
    use crate::db::writer::RunUpsert;

    /// A code launch whose project has no git repository in the repositories folder fails with a
    /// reason, and nothing is cloned or created.
    #[test]
    fn a_project_without_a_repository_fails_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        let wt = dir.path().join("workers/p/t");
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't'), 's', 'fake', 'b', ?1)",
                [wt.to_string_lossy()],
            )
            .unwrap();
        Dispatch::new(4).pass(&w).unwrap();
        let places = Places {
            workers: dir.path().join("workers"),
            docs: dir.path().join("docs"),
            repos: dir.path().join("repos"),
            settings: dir.path().join("settings"),
        };
        assert!(Prepare::new(places).pass(&w).unwrap().is_empty());
        let (stage, why): (String, String) = w
            .conn()
            .query_row("SELECT stage, failed_reason FROM run_launch", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(stage, "failed");
        assert!(why.contains("no git repository"), "{why}");
        assert!(!wt.exists());
    }

    /// A destination that is not a worktree of the project is never taken for a prepared one.
    #[test]
    fn an_unrelated_folder_at_the_destination_fails_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repos/p");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |d: &Path, a: &[&str]| {
            let st = Command::new("git")
                .arg("-C")
                .arg(d)
                .args(a)
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "m",
            ],
        );
        let other = dir.path().join("workers/p/t");
        std::fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "-q"]);
        let places = Places {
            workers: dir.path().join("workers"),
            docs: dir.path().join("docs"),
            repos: dir.path().join("repos"),
            settings: dir.path().join("settings"),
        };
        let l = Launch {
            id: 1,
            task: "t".into(),
            project: "p".into(),
            default_branch: "main".into(),
            branch: None,
            kind: "code".into(),
            agent: "fake".into(),
            stage: "planned".into(),
            worktree: other.to_string_lossy().into(),
            spawn: "s".into(),
        };
        let why = Prepare::new(places).add_worktree(&l, &other).unwrap_err();
        assert!(why.contains("not a clean worktree"), "{why}");
    }
}
