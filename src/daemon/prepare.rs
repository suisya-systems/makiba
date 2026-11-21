//! Preparing a taken run (Issue M3-2): its workplace and its per-task settings file.
//!
//! A launch is prepared once its `run_taken` event exists (M3-1). Code: a repository of the run's
//! own (Issue 241, `run_repo`) with its work tree `$MAKIBA_WORKERS_DIR/<project>/<task_id>` and
//! its git folder `<state>/git/<task_id>`, borrowing the project's own repository's objects, on a
//! new branch from the project's default branch. Document: `$MAKIBA_DOCS_DIR/<task_id>/out/`, no git. Then
//! the agent module writes `<state>/settings/<task_id>.json`, the worker role's settings with the
//! task's added, and the worker's rules file is written where the agent reads it (spec sections
//! 2 and 9, Issue 134). The stage
//! moves `planned`, `worktree_ready`, `settings_ready`, each recorded before the next step, and
//! a launch already past a stage is not touched for it. Nothing is written inside a worktree
//! after it is made; a code launch sets the project clone's and the run's hooks path to the
//! secret scanner's (Issue 136).
//!
//! Reading of the Issue where the spec is silent (reported to the secretary):
//! - (Owner) The project's own repository is `$MAKIBA_REPOS_DIR/<project>` (default
//!   `~/makiba-repos/<project>`); with no git repository there the launch fails. Preparation
//!   clones nothing: the verb `project` does (Issue 184).
//! - The new branch is named `runs.branch` when set, else `makiba/<task_id>`; it starts from
//!   `projects.default_branch`.
//! - (Owner) The workplace path is read from `run_launch.worktree`; `Places::workplace` computes
//!   it, for the dispatch verb to use when it makes the row.

use super::run_repo;
use crate::agents::{self, Agent, Role, TaskSettings};
use crate::base::BaseDenies;
use crate::db::Result;
use crate::db::writer::StateWriter;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The folders preparation writes into.
#[derive(Clone)]
pub struct Places {
    pub workers: PathBuf,
    pub docs: PathBuf,
    pub repos: PathBuf,
    pub settings: PathBuf,
    /// `<state>/state.db`: denied to agents, whose `status` goes through the door (M4-2).
    pub state_db: PathBuf,
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
            state_db: state.join("state.db"),
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

/// The person's git and GitHub CLI credential files, denied to a worker and the curator (Issue
/// 140): the ssh keys, git's plain credential store and the CLI's stored login, at their default
/// places and under `XDG_CONFIG_HOME` as this process has it. Folders, then files.
fn credentials() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut files = vec![
        PathBuf::from("~/.git-credentials"),
        PathBuf::from("~/.config/git/credentials"),
    ];
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        let xdg = PathBuf::from(xdg);
        files.push(xdg.join("git/credentials"));
        files.push(xdg.join("gh/hosts.yml"));
    }
    (vec![PathBuf::from("~/.ssh")], files)
}

pub struct Prepare {
    places: Places,
    /// What the base says a role must not reach (spec section 6.2).
    denies: BaseDenies,
}

struct Launch {
    id: i64,
    /// `None`: a launch without a run (Issue 166); `project` and `default_branch` are empty then.
    task: Option<String>,
    role: String,
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
    pub fn new(places: Places, denies: BaseDenies) -> Prepare {
        Prepare { places, denies }
    }

    /// One pass: moves every taken launch still `planned` or `worktree_ready` as far as it goes.
    /// Returns the task ids that reached `settings_ready`. Each stage is written only over the one
    /// read (Issue 163): a launch moved meanwhile by somebody else is left as it is. A launch that cannot be prepared goes
    /// through the failure path of M3-6 (Issue 124): `worker_spawn_failed` with the stage and the
    /// reason, the launch and its run `failed`, and a notice to the secretary, in one transaction.
    pub fn pass(&self, w: &StateWriter) -> Result<Vec<String>> {
        let launches: Vec<Launch> = {
            let mut st = w.conn().prepare(
                "SELECT l.id, r.task_id, l.role, COALESCE(p.slug, ''),
                        COALESCE(p.default_branch, 'main'),
                        r.branch, l.kind, l.agent, l.stage, l.worktree, l.spawn_id
                 FROM run_launch l LEFT JOIN runs r ON r.id = l.run_id
                 LEFT JOIN projects p ON p.id = r.project_id
                 WHERE l.stage IN ('planned', 'worktree_ready')
                   AND (l.run_id IS NULL
                        OR EXISTS (SELECT 1 FROM events e WHERE e.kind = 'run_taken'
                                   AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id))
                 ORDER BY l.id",
            )?;
            st.query_map([], |r| {
                Ok(Launch {
                    id: r.get(0)?,
                    task: r.get(1)?,
                    role: r.get(2)?,
                    project: r.get(3)?,
                    default_branch: r.get(4)?,
                    branch: r.get(5)?,
                    kind: r.get(6)?,
                    agent: r.get(7)?,
                    stage: r.get(8)?,
                    worktree: r.get(9)?,
                    spawn: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        let mut ready = Vec::new();
        for l in launches {
            match self.prepare(w, &l) {
                Ok(true) => ready.push(l.task.unwrap_or(l.role)),
                // Moved by somebody else (a close, `makiba down`): left as it is (Issue 163).
                Ok(false) => {}
                Err(why) => {
                    // The stage it got to: `prepare` may have moved it past the one read.
                    let stage: String = w.conn().query_row(
                        "SELECT stage FROM run_launch WHERE id = ?1",
                        [l.id],
                        |r| r.get(0),
                    )?;
                    if matches!(stage.as_str(), "planned" | "worktree_ready") {
                        let task = l.task.as_deref();
                        super::verify::fail(w, l.id, task, &l.spawn, &stage, &why, &[])?;
                    }
                }
            }
        }
        Ok(ready)
    }

    /// `Ok(false)`: a stage update found the launch moved by somebody else; nothing more is done.
    fn prepare(&self, w: &StateWriter, l: &Launch) -> std::result::Result<bool, String> {
        let db = |e: crate::db::Error| e.to_string();
        let one = |s: &str| !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\']);
        if let Some(task) = &l.task
            && (!one(task) || !one(&l.project))
        {
            return Err(format!("task id or project is not a plain name: {task:?}"));
        }
        let (_, role) = super::launch::roles(&l.role)
            .ok_or_else(|| format!("no launch role named {:?}", l.role))?;
        let workplace = PathBuf::from(&l.worktree);
        if l.stage == "planned" {
            // A launch without a run starts in its folder (Issue 166); a document task in its own.
            if l.task.is_none() {
                std::fs::create_dir_all(&workplace).map_err(|e| e.to_string())?;
            } else if l.kind == "document" {
                std::fs::create_dir_all(workplace.join("out")).map_err(|e| e.to_string())?;
            } else {
                self.add_worktree(l, &workplace)?;
            }
            super::crash::hold("worktree_ready");
            let n = w
                .conn()
                .execute(
                    "UPDATE run_launch SET stage = 'worktree_ready'
                     WHERE id = ?1 AND stage = 'planned'",
                    [l.id],
                )
                .map_err(|e| db(e.into()))?;
            if n == 0 {
                return Ok(false);
            }
            super::crash::point("worktree_ready");
        }
        let agent: Box<dyn Agent> = agents::by_name(&l.agent)
            .ok_or_else(|| format!("no agent module named {:?}", l.agent))?;
        std::fs::create_dir_all(&self.places.settings).map_err(|e| e.to_string())?;
        // The person key where it is now (`MAKIBA_PERSON_KEY` may move it); SQLite's side files
        // hold the same rows as `state.db`.
        let state_db = self.places.state_db.display().to_string();
        // A relative path (a relative `MAKIBA_PERSON_KEY`, `MAKIBA_STATE_DIR` or
        // `XDG_RUNTIME_DIR`) would be read from the agent's folder: each is made absolute here,
        // where it was meant. The `~/` form is the home's in both places and is kept.
        let absolute = |ps: Vec<PathBuf>| -> std::result::Result<Vec<PathBuf>, String> {
            ps.into_iter()
                .map(|p| {
                    if p.starts_with("~") {
                        Ok(p)
                    } else {
                        std::path::absolute(&p).map_err(|e| format!("{}: {e}", p.display()))
                    }
                })
                .collect()
        };
        // The test agent reads no rules file, and its tests leave the share folder at its default.
        if agent.name() != "fake" {
            agents::on_path("claude")
                .ok_or("no `claude` program on PATH, where `makiba up` checked it")?;
            agents::write_rules(role).map_err(|e| format!("the rules file: {e}"))?;
            agents::link_entry_point().map_err(|e| format!("the hooks' entry point: {e}"))?;
        }
        // The secret scanner (Issue 136): the clone's hooks path, outside the write surface.
        let mut deny_write_dirs = vec![crate::cli::share_dir()];
        // The workplace is the sandbox's write surface; its `.mcp.json` is not (Issue 137).
        let mut write_dirs = vec![workplace.clone()];
        let mut deny_write_files = vec![workplace.join(".mcp.json")];
        if let Some(task) = l.task.as_deref().filter(|_| l.kind != "document") {
            let repo = self.places.repos.join(&l.project);
            deny_write_dirs.push(self.install_scanner(&repo)?);
            // The configuration naming that hooks path, by makiba's own entry (Issue 140).
            deny_write_files.push(repo.join(".git").join("config"));
            // The run's own repository (Issue 241): every launch, a resume's too, is checked.
            let gitdir = run_repo::dir(&self.places.settings, task);
            if !run_repo::is_run(&gitdir, &workplace) {
                return Err(format!(
                    "the workplace {} has no repository of its own at {} (made before Issue 241, or removed); close the run with --abandon and dispatch it again",
                    workplace.display(),
                    gitdir.display()
                ));
            }
            // Its objects and refs are the worker's to write; its configuration is not.
            deny_write_files.push(gitdir.join("config"));
            write_dirs.push(gitdir);
        }
        write_dirs.push(std::env::temp_dir());
        // No credential to push with or to write to GitHub (Issue 140, line L-K2).
        let (cred_dirs, cred_files) = if role == Role::Secretary {
            (vec![], vec![])
        } else {
            credentials()
        };
        let settings = TaskSettings {
            role: Some(role),
            write_dirs,
            deny_dirs: absolute(
                [
                    &self.denies.resolved[..],
                    &self.denies.folders[..],
                    &cred_dirs[..],
                ]
                .concat(),
            )?,
            deny_files: absolute(
                [
                    &[
                        crate::cli::person_key_path(),
                        crate::cli::gate_key_path(),
                        PathBuf::from(&state_db),
                        PathBuf::from(format!("{state_db}-wal")),
                        PathBuf::from(format!("{state_db}-shm")),
                    ][..],
                    &cred_files[..],
                ]
                .concat(),
            )?,
            deny_write_dirs,
            deny_write_files: absolute(deny_write_files)?,
        };
        agent
            .write_task_settings(
                &super::launch::settings_file(
                    &self.places.settings,
                    l.task.as_deref(),
                    &l.role,
                    &l.spawn,
                ),
                &settings,
            )
            .map_err(|e| e.to_string())?;
        super::crash::hold("settings_ready");
        let n = w
            .conn()
            .execute(
                "UPDATE run_launch SET stage = 'settings_ready'
                 WHERE id = ?1 AND stage = 'worktree_ready'",
                [l.id],
            )
            .map_err(|e| db(e.into()))?;
        if n == 0 {
            return Ok(false);
        }
        super::crash::point("settings_ready");
        Ok(true)
    }

    /// The secret scanner's git hooks (Issue 136) in `<state>/git-hooks`, absolute, and the
    /// project clone's `core.hooksPath` set to that folder, so that every worktree of the clone
    /// runs them (the person's own commits in the clone too). A clone with per-worktree
    /// configuration is refused: a worktree's own file could set another hooks path. Returns the
    /// folder, for the run's settings to deny for write.
    fn install_scanner(&self, repo: &Path) -> std::result::Result<PathBuf, String> {
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .map_err(|e| e.to_string())
        };
        let per_worktree = git(&[
            "config",
            "--type=bool",
            "--get",
            "extensions.worktreeConfig",
        ])?;
        if String::from_utf8_lossy(&per_worktree.stdout).trim() == "true" {
            return Err(format!(
                "{} has extensions.worktreeConfig on, so a worktree could set its own hooks path past the secret scanner; turn it off",
                repo.display()
            ));
        }
        let dir = self.hooks_dir()?;
        let exe = std::env::current_exe().map_err(|e| format!("the scanner's program: {e}"))?;
        crate::hooks::git_hook::install(&dir, &exe)
            .map_err(|e| format!("the secret scanner's hooks in {}: {e}", dir.display()))?;
        let want = dir.to_string_lossy().into_owned();
        let now = git(&["config", "--get", "core.hooksPath"])?;
        if String::from_utf8_lossy(&now.stdout).trim_end_matches('\n') != want {
            let set = git(&["config", "core.hooksPath", &want])?;
            if !set.status.success() {
                return Err(format!(
                    "cannot set core.hooksPath in {}: {}",
                    repo.display(),
                    String::from_utf8_lossy(&set.stderr).trim()
                ));
            }
        }
        Ok(dir)
    }

    /// `<state>/git-hooks`, absolute.
    fn hooks_dir(&self) -> std::result::Result<PathBuf, String> {
        let dir = self.places.settings.with_file_name("git-hooks");
        std::path::absolute(&dir).map_err(|e| format!("{}: {e}", dir.display()))
    }

    /// A pass cut short before the stage was written leaves the workplace behind: it is kept only
    /// when it is a clean work tree of the run's own repository on the expected branch.
    fn check_existing(gitdir: &Path, path: &Path, branch: &str) -> std::result::Result<(), String> {
        let on_branch = run_repo::is_run(gitdir, path)
            && run_repo::read(gitdir, path, &["symbolic-ref", "--short", "HEAD"]).as_deref()
                == Ok(branch);
        if on_branch && run_repo::status(gitdir, path, true).as_deref() == Ok("") {
            Ok(())
        } else {
            Err(format!(
                "{} exists and is not a clean worktree of the project on {branch}",
                path.display()
            ))
        }
    }

    /// The run's own repository (Issue 241, `run_repo`) on `<branch>` from the project's default
    /// branch, its git folder `<state>/git/<task>`.
    fn add_worktree(&self, l: &Launch, path: &Path) -> std::result::Result<(), String> {
        let repo = Some(self.places.repos.join(&l.project)).filter(|r| r.join(".git").exists());
        let repo = repo.ok_or_else(|| {
            format!(
                "project {:?} has no git repository in the repositories folder; register it with the tool project",
                l.project
            )
        })?;
        let task = l.task.as_deref().unwrap_or_default();
        let branch = l.branch.clone().unwrap_or_else(|| format!("makiba/{task}"));
        let gitdir = run_repo::dir(&self.places.settings, task);
        let git_root = run_repo::root(&self.places.settings);
        if path.join(".git").symlink_metadata().is_ok() {
            return Self::check_existing(&gitdir, path, &branch);
        }
        // No `.git` file: a preparation cut short (it is written last), or nothing yet. At
        // `planned` no worker has had either folder, so both are made again.
        if path.exists() {
            if !gitdir.exists() {
                return Err(format!(
                    "{} exists and is not a clean worktree of the project on {branch}",
                    path.display()
                ));
            }
            run_repo::remove_under(&self.places.workers, path)?;
        }
        std::fs::create_dir_all(&git_root).map_err(|e| format!("{}: {e}", git_root.display()))?;
        run_repo::remove_under(&git_root, &gitdir)?;
        run_repo::create(
            &repo,
            &gitdir,
            path,
            &branch,
            &l.default_branch,
            &self.hooks_dir()?,
        )
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
            state_db: dir.path().join("state.db"),
        };
        assert!(
            Prepare::new(places, BaseDenies::default())
                .pass(&w)
                .unwrap()
                .is_empty()
        );
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
            state_db: dir.path().join("state.db"),
        };
        let l = Launch {
            id: 1,
            task: Some("t".into()),
            role: "worker".into(),
            project: "p".into(),
            default_branch: "main".into(),
            branch: None,
            kind: "code".into(),
            agent: "fake".into(),
            stage: "planned".into(),
            worktree: other.to_string_lossy().into(),
            spawn: "s".into(),
        };
        let why = Prepare::new(places, BaseDenies::default())
            .add_worktree(&l, &other)
            .unwrap_err();
        assert!(why.contains("not a clean worktree"), "{why}");
    }

    /// A relative deny path is written absolute, as the daemon meant it; `~/` is kept.
    #[test]
    fn relative_deny_paths_are_written_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        let places = Places {
            workers: dir.path().join("workers"),
            docs: dir.path().join("docs"),
            repos: dir.path().join("repos"),
            settings: dir.path().join("settings"),
            state_db: PathBuf::from("rel-state/state.db"),
        };
        let denies = BaseDenies {
            resolved: vec![PathBuf::from("rel-run/base")],
            folders: vec![PathBuf::from("~/.config/the-base")],
            ..Default::default()
        };
        let l = Launch {
            id: 1,
            task: Some("t".into()),
            role: "worker".into(),
            project: "p".into(),
            default_branch: "main".into(),
            branch: None,
            kind: "document".into(),
            agent: "fake".into(),
            stage: "worktree_ready".into(),
            worktree: dir.path().join("docs/t").to_string_lossy().into(),
            spawn: "s".into(),
        };
        Prepare::new(places, denies).prepare(&w, &l).unwrap();
        let text = std::fs::read_to_string(dir.path().join("settings/t.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let denied: Vec<&str> = v["sandbox"]["filesystem"]["denyRead"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p.as_str())
            .collect();
        for want in [
            cwd.join("rel-run/base"),
            cwd.join("rel-state/state.db"),
            cwd.join("rel-state/state.db-wal"),
        ] {
            assert!(denied.contains(&&*want.to_string_lossy()), "{text}");
        }
        assert!(denied.contains(&"~/.config/the-base"), "{text}");
        assert!(
            denied
                .iter()
                .all(|p| p.starts_with('/') || p.starts_with("~/")),
            "{text}"
        );
    }
}
