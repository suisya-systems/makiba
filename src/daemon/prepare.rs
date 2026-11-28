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
//! after it is made; a code launch sets the run's hooks path to the secret scanner's (Issue 136),
//! whose scripts then run the project's own hooks (Issue 236), and leaves the clone's own.
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

/// The folder under the scanner's hooks folder that holds the design markers (Issue 337), one file
/// per task, read by the worker's commit hook.
pub const DESIGN_WAIT: &str = "design-wait";

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

/// `$var`, else `~/<default>` (spec section 2).
fn dir_from_env(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(default)
    })
}

/// The repositories folder: `$MAKIBA_REPOS_DIR`, else `~/makiba-repos`.
pub fn repos_from_env() -> PathBuf {
    dir_from_env("MAKIBA_REPOS_DIR", "makiba-repos")
}

impl Places {
    /// `$MAKIBA_WORKERS_DIR` and `$MAKIBA_DOCS_DIR` with the defaults of spec section 2;
    /// the settings folder is `<state>/settings`.
    pub fn from_env(state: &Path) -> Places {
        Places {
            workers: dir_from_env("MAKIBA_WORKERS_DIR", "makiba-workers"),
            docs: dir_from_env("MAKIBA_DOCS_DIR", "makiba-docs"),
            repos: repos_from_env(),
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
pub(crate) fn credentials() -> (Vec<PathBuf>, Vec<PathBuf>) {
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

/// `<state>/tmp`, where each launch's own temporary folder is (Issue 243).
pub fn tmp_root(settings: &Path) -> PathBuf {
    settings.with_file_name("tmp")
}

/// The temporary folder of launch `id` (`run_launch.id`): `<state>/tmp/<id>`, absolute. A row id
/// keeps the path short whatever the task id, and a resume, a new row, gets a new one.
pub fn tmp_dir(settings: &Path, id: i64) -> PathBuf {
    let dir = tmp_root(settings).join(id.to_string());
    std::path::absolute(&dir).unwrap_or(dir)
}

/// Make launch `id`'s temporary folder, mode 0700 (its parent too, when it makes it).
fn make_tmp_dir(settings: &Path, id: i64) -> std::result::Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = tmp_dir(settings, id);
    let shown = |e: std::io::Error| format!("{}: {e}", dir.display());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(shown)?;
    // Already there (a pass cut short): the mode as if made now.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(shown)?;
    Ok(dir)
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
    /// The `landed` of the run's `belt_dispatched` (Issue 490): the merges its start must hold.
    landed: Option<String>,
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
                        r.branch, l.kind, l.agent, l.stage, l.worktree, l.spawn_id,
                        (SELECT json_extract(b.payload_json, '$.landed') FROM events b
                         WHERE b.run_id = r.id AND b.kind = 'belt_dispatched'
                         ORDER BY b.id DESC LIMIT 1)
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
                    landed: r.get(11)?,
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
                        super::verify::fail(
                            w,
                            l.id,
                            task,
                            &l.spawn,
                            &stage,
                            &why,
                            &super::verify::Sight::none(),
                        )?;
                    }
                }
            }
        }
        Ok(ready)
    }

    /// At `makibad`'s start (Issue 243): the temporary folder of every launch that is over goes,
    /// decided by the rows alone (never by a terminal being absent, never by a folder's name):
    /// the launch `failed` while it was prepared (its `worker_spawn_failed` names `planned` or
    /// `worktree_ready`: no process was started), or a later launch of its run (of its role, for
    /// a launch without a run) `verified`. A launch that failed later may keep its terminal for
    /// inspection, so its folder stays until the run's close, which ends the terminals first. A
    /// folder no such row names is left.
    pub fn sweep(&self, w: &StateWriter) -> Result<()> {
        let ids: Vec<i64> = w
            .conn()
            .prepare(
                "SELECT l.id FROM run_launch l
                 WHERE (l.stage = 'failed'
                        AND EXISTS (SELECT 1 FROM events e
                                    WHERE e.kind = 'worker_spawn_failed'
                                      AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id
                                      AND json_extract(e.payload_json, '$.stage')
                                          IN ('planned', 'worktree_ready')))
                    OR EXISTS (SELECT 1 FROM run_launch n
                               WHERE n.id > l.id AND n.stage = 'verified'
                                 AND (n.run_id = l.run_id
                                      OR (n.run_id IS NULL AND l.run_id IS NULL
                                          AND n.role = l.role)))",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let root = tmp_root(&self.places.settings);
        for id in ids {
            if let Err(e) = run_repo::take_out(&root, &tmp_dir(&self.places.settings, id)) {
                eprintln!("makibad: the temporary folder of launch {id}: {e}");
            }
        }
        Ok(())
    }

    /// Issue 337 (line L-K13): the marker `<state>/git-hooks/design-wait/<task>` that the worker's
    /// commit hook reads (`hooks::test_commit::design_wait`), kept in agreement with the ledger
    /// for every run dispatched design-first: present, holding the reason, while
    /// `push::design_wait` gives one and the run is not completed or abandoned; absent otherwise.
    /// Run each pass after `pass`, from committed records only, so an approval is seen at the next
    /// pass after its transaction and a write that failed is tried again. The marker is a
    /// projection of the ledger: `makibad` never reads it for a decision; the push holds the line.
    pub fn design_markers(&self, w: &StateWriter) -> Result<()> {
        let tasks: Vec<String> = w
            .conn()
            .prepare(
                "SELECT DISTINCT r.task_id FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.kind = 'delegate_sent'
                   AND json_extract(e.payload_json, '$.design_first') = 1",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for task in tasks {
            if let Err(e) = self.design_marker(w, &task) {
                eprintln!("makibad: the design marker of {task}: {e}");
            }
        }
        Ok(())
    }

    /// The marker of `task` made to agree with the ledger (`design_markers`).
    fn design_marker(&self, w: &StateWriter, task: &str) -> std::result::Result<(), String> {
        let db = |e: rusqlite::Error| format!("the design approval of {task}: {e}");
        let open: bool = w
            .conn()
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM runs WHERE task_id = ?1
                                AND status NOT IN ('completed', 'abandoned'))",
                [task],
                |r| r.get(0),
            )
            .map_err(db)?;
        let why = if open {
            super::push::design_wait(w.conn(), task).map_err(|e| e.to_string())?
        } else {
            None
        };
        let dir = self.hooks_dir()?.join(DESIGN_WAIT);
        let marker = dir.join(task);
        match why {
            Some(why) => {
                if std::fs::read_to_string(&marker).ok().as_deref() != Some(why.as_str()) {
                    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    std::fs::write(&marker, why)
                        .map_err(|e| format!("{}: {e}", marker.display()))?;
                }
            }
            None => match std::fs::remove_file(&marker) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(format!("{}: {e}", marker.display()));
                }
                _ => {}
            },
        }
        Ok(())
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
        // The secret scanner (Issue 136): the run's hooks path, outside the write surface.
        let mut deny_write_dirs = vec![crate::cli::share_dir()];
        // The workplace is the sandbox's write surface; its `.mcp.json` is not (Issue 137).
        let mut write_dirs = vec![workplace.clone()];
        let mut deny_write_files = vec![workplace.join(".mcp.json")];
        if let Some(task) = l.task.as_deref().filter(|_| l.kind != "document") {
            let repo = self.places.repos.join(&l.project);
            deny_write_dirs.push(self.install_scanner(&repo)?);
            // Issue 337: the commit hook's marker is there before the worker starts.
            self.design_marker(w, task)?;
            // The clone's configuration, by makiba's own entry (Issue 140).
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
            say_project_hooks(task, &gitdir, &workplace);
            // Its objects and refs are the worker's to write; its configuration is not.
            deny_write_files.push(gitdir.join("config"));
            write_dirs.push(gitdir);
        }
        // The launch's own temporary folder (Issue 243), not the shared one: `TMPDIR` and
        // `CLAUDE_CODE_TMPDIR` name it in the settings and in the launch's environment.
        let tmp = make_tmp_dir(&self.places.settings, l.id)?;
        write_dirs.push(tmp.clone());
        // Claude Code's own shared folder, which its sandbox adds to the write surface itself.
        deny_write_dirs.extend(agents::CLAUDE_SHARED_TMP.map(PathBuf::from));
        // No credential to push with or to write to GitHub (Issue 140, line L-K2).
        let (cred_dirs, cred_files) = if role == Role::Secretary {
            (vec![], vec![])
        } else {
            credentials()
        };
        // The strict network allow-list (Issue 244, lines L-K2 and L-K7): a worker reaches its
        // project's listed hosts only, the curator none; the secretary keeps its role's.
        let allowed_hosts = match role {
            Role::Secretary => None,
            _ if l.task.is_none() => Some(vec![]),
            _ => Some(super::project::hosts_of(w.conn(), &l.project).map_err(|e| e.to_string())?),
        };
        let settings = TaskSettings {
            role: Some(role),
            allowed_hosts,
            write_dirs,
            deny_dirs: absolute(
                [
                    &self.denies.resolved[..],
                    &self.denies.folders[..],
                    &cred_dirs[..],
                    // The approval key's staging folder (Issue 590), for every role.
                    &[crate::cli::approval_staging_path()][..],
                ]
                .concat(),
            )?,
            deny_files: absolute(
                [
                    &[
                        crate::cli::person_key_path(),
                        crate::cli::gate_key_path(),
                        crate::cli::approval_key_path(),
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
        let settings_file = super::launch::settings_file(
            &self.places.settings,
            l.task.as_deref(),
            &l.role,
            &l.spawn,
        );
        agent
            .write_task_settings(&settings_file, &settings)
            .map_err(|e| e.to_string())?;
        agents::set_tmp_dir(&settings_file, &tmp).map_err(|e| e.to_string())?;
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

    /// The secret scanner's git hooks (Issue 136) in `<state>/git-hooks`, absolute, which a code
    /// run's `core.hooksPath` names (`run_repo::create`). The project clone's own hooks path is the
    /// person's (Issue 236): one that is still the scanner's folder, set before Issue 236, is unset,
    /// and the log says that a local value it replaced is lost. A clone with per-worktree
    /// configuration is refused. Returns the folder, for the run's settings to deny for write.
    fn install_scanner(&self, repo: &Path) -> std::result::Result<PathBuf, String> {
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["-c", run_repo::HOOKS_OFF])
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
        let exe = super::exe::started_path()
            .ok_or("the scanner's program: cannot tell the path of makiba's own executable")?;
        crate::hooks::git_hook::install(&dir, exe)
            .map_err(|e| format!("the secret scanner's hooks in {}: {e}", dir.display()))?;
        let want = dir.to_string_lossy().into_owned();
        let local = git(&["config", "--local", "--get", "core.hooksPath"])?;
        if String::from_utf8_lossy(&local.stdout).trim_end_matches('\n') == want {
            let unset = git(&["config", "--local", "--unset-all", "core.hooksPath"])?;
            if !unset.status.success() {
                return Err(format!(
                    "cannot unset core.hooksPath in {}: {}",
                    repo.display(),
                    String::from_utf8_lossy(&unset.stderr).trim()
                ));
            }
            eprintln!(
                "makibad: {}: core.hooksPath no longer names the secret scanner, which runs in each code run (Issue 236); a local core.hooksPath set before makiba cannot be recovered: set it again and dispatch again",
                repo.display()
            );
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
        let start = start_of(&repo, &l.default_branch, l.landed.as_deref())?;
        std::fs::create_dir_all(&git_root).map_err(|e| format!("{}: {e}", git_root.display()))?;
        run_repo::remove_under(&git_root, &gitdir)?;
        run_repo::create(&repo, &gitdir, path, &branch, &start, &self.hooks_dir()?)
    }
}

/// Where a run's branch starts in the clone `repo`: the default branch `branch`; for a range task
/// dispatched on its dependencies' landings (`landed`, Issue 490), the commit the clone's
/// `refs/remotes/origin/<branch>` names, which the belt's fetch moved and which must hold each
/// landed merge. Resolved once, so a fetch that moves the ref meanwhile cannot change the start.
fn start_of(
    repo: &Path,
    branch: &str,
    landed: Option<&str>,
) -> std::result::Result<String, String> {
    let landed: Vec<serde_json::Value> = landed
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();
    if landed.is_empty() {
        return Ok(branch.to_string());
    }
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(repo)
            .args(["-c", run_repo::HOOKS_OFF])
            .args(args)
            .stdin(std::process::Stdio::null());
        run_repo::out(c, &format!("git {}", args.join(" ")))
    };
    let start = git(&[
        "rev-parse",
        "--verify",
        &format!("refs/remotes/origin/{branch}^{{commit}}"),
    ])?;
    for l in &landed {
        let merge = l["merge_commit"].as_str().unwrap_or_default();
        if merge.is_empty() || git(&["merge-base", "--is-ancestor", merge, &start]).is_err() {
            return Err(format!(
                "the clone's origin/{branch} ({start}) does not hold the merge {merge} of #{}",
                l["issue"]
            ));
        }
    }
    Ok(start)
}

/// Say in the log which of the project's own hooks the run's scanner scripts run (Issue 236): the
/// executable ones named by the run's `git_hook::PROJECT_KEY`, a relative folder read in the
/// workplace as git would.
fn say_project_hooks(task: &str, gitdir: &Path, workplace: &Path) {
    use crate::hooks::git_hook::{FLAG_HOOK, FORWARDED, NAMES, PROJECT_KEY};
    use std::os::unix::fs::PermissionsExt;
    let Ok(dir) = run_repo::read(gitdir, workplace, &["config", "--get", PROJECT_KEY]) else {
        return;
    };
    let names: Vec<&str> = NAMES
        .iter()
        .chain([FLAG_HOOK].iter())
        .chain(FORWARDED.iter())
        .copied()
        .filter(|n| {
            std::fs::metadata(workplace.join(&dir).join(n))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .collect();
    if !names.is_empty() {
        eprintln!(
            "makibad: {task}: the project's own hooks in {dir} ({}) run after the secret scan",
            names.join(", ")
        );
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
            landed: None,
        };
        let why = Prepare::new(places, BaseDenies::default())
            .add_worktree(&l, &other)
            .unwrap_err();
        assert!(why.contains("not a clean worktree"), "{why}");
    }

    /// The start sweep removes the folders of launches that are over by their rows alone: one
    /// that failed while prepared and one with a later `verified` launch of its role; one that
    /// failed after its launch (its terminal may be kept), a live one and a folder no row names
    /// stay.
    #[test]
    fn the_sweep_removes_only_the_folders_of_launches_that_are_over() {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        let settings = dir.path().join("settings");
        for (spawn, role, stage) in [
            ("a", "curator", "failed"),
            ("b", "secretary", "verified"),
            ("c", "secretary", "verified"),
            ("d", "curator", "settings_ready"),
            ("e", "secretary", "failed"),
        ] {
            w.conn()
                .execute(
                    "INSERT INTO run_launch (spawn_id, role, agent, brief, worktree, stage)
                     VALUES (?1, ?2, 'fake', 'b', '/w', ?3)",
                    [spawn, role, stage],
                )
                .unwrap();
        }
        for (spawn, stage) in [("a", "worktree_ready"), ("e", "dialog_passed")] {
            w.append_event(&crate::db::writer::NewEvent {
                kind: "worker_spawn_failed",
                payload: Some(&serde_json::json!({"spawn_id": spawn, "stage": stage})),
                ..Default::default()
            })
            .unwrap();
        }
        for id in 1..=6 {
            make_tmp_dir(&settings, id).unwrap();
        }
        let places = Places {
            workers: dir.path().join("workers"),
            docs: dir.path().join("docs"),
            repos: dir.path().join("repos"),
            settings: settings.clone(),
            state_db: dir.path().join("state.db"),
        };
        Prepare::new(places, BaseDenies::default())
            .sweep(&w)
            .unwrap();
        let left: Vec<bool> = (1..=6).map(|id| tmp_dir(&settings, id).is_dir()).collect();
        assert_eq!(left, [false, false, true, true, true, true]);
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
            landed: None,
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
