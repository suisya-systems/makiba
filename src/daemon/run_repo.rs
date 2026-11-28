//! The repository of a code run (Issue 241). A code run does not share the project clone's `.git`:
//! its workplace `<workers>/<project>/<task_id>` holds a `.git` file naming a git folder of its
//! own, `<state>/git/<task_id>`, outside the workplace, whose objects borrow the clone's as a
//! read-only alternate. The run's settings let the worker write the workplace and that folder,
//! never the clone, and deny the folder's `config`, which `makibad` writes here before the worker
//! exists.
//!
//! After that `makibad` never writes into a run's repository and runs no network or merge command
//! there: the worker can turn any folder under its git folder into a link to somewhere else, so a
//! write would land there. It reads the repository with the git folder, the common folder and the
//! work tree given (never found from the workplace's `.git` file or a `commondir` file, which the
//! worker can write), with no optional lock, no fsmonitor and no submodule recursion (a worker's
//! submodule's own configuration is never run). A head leaves the run only by `deliver`: a push
//! from the run into the clone, whose own receive-pack does the writing.
//!
//! No git `makibad` runs, on the run or on the clone, runs a hook (`core.hooksPath=/dev/null`,
//! Issue 236): a project's own hook is code the person or a worker wrote, and `makibad` runs
//! unsandboxed with the person's credentials. The worker's own git runs them, after the secret
//! scanner, from the folder `create` records.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The lowest git that has `git merge-tree --write-tree`, which `update` merges with.
pub const GIT_LOWEST: (u64, u64) = (2, 38);

/// `Some(false)` when `git --version`'s text names a version below `GIT_LOWEST`; `None` when it
/// names none.
pub fn git_new_enough(text: &str) -> Option<bool> {
    let v = text.split_whitespace().nth(2)?;
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    let (major, minor) = (parts.next()??, parts.next()??);
    Some((major, minor) >= GIT_LOWEST)
}

/// `git` on the run's repository at `gitdir` with the work tree `workplace`, read only.
pub fn git(gitdir: &Path, workplace: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(gitdir)
        .env("GIT_DIR", gitdir)
        .env("GIT_COMMON_DIR", gitdir)
        .env("GIT_WORK_TREE", workplace)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "submodule.recurse=false",
            "-c",
            HOOKS_OFF,
        ])
        .stdin(Stdio::null());
    c
}

/// The configuration that runs no hook, for every git `makibad` runs (Issue 236).
pub const HOOKS_OFF: &str = "core.hooksPath=/dev/null";

/// Run `cmd`: its trimmed output, or git's message after `what`.
pub fn out(mut cmd: Command, what: &str) -> Result<String, String> {
    let o = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(format!(
            "{what}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ))
    }
}

/// `git` with `args` on the run's repository.
pub fn read(gitdir: &Path, workplace: &Path, args: &[&str]) -> Result<String, String> {
    let mut c = git(gitdir, workplace);
    c.args(args);
    out(c, &format!("git {}", args.join(" ")))
}

/// Whether `gitdir` is a run repository and `workplace` a work tree of it.
pub fn is_run(gitdir: &Path, workplace: &Path) -> bool {
    gitdir.join("HEAD").is_file() && workplace.join(".git").is_file()
}

/// The commit `rev` names in the run's repository, if any.
pub fn commit(gitdir: &Path, workplace: &Path, rev: &str) -> Option<String> {
    read(
        gitdir,
        workplace,
        &["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")],
    )
    .ok()
}

/// Uncommitted changes in the work tree (untracked files when `untracked`), submodules not read.
pub fn status(gitdir: &Path, workplace: &Path, untracked: bool) -> Result<String, String> {
    let u = if untracked {
        "--untracked-files=all"
    } else {
        "--untracked-files=no"
    };
    read(
        gitdir,
        workplace,
        &["status", "--porcelain", u, "--ignore-submodules=all"],
    )
}

/// The submodules of the index whose folder in the work tree is not empty: their work is never
/// read (their own configuration would run), so it counts as unsaved. Only the index is read.
pub fn checked_out_submodules(gitdir: &Path, workplace: &Path) -> Result<Vec<String>, String> {
    let staged = read(gitdir, workplace, &["ls-files", "--stage", "-z"])?;
    Ok(staged
        .split('\0')
        .filter_map(|e| e.strip_prefix("160000 ")?.split_once('\t'))
        .map(|(_, p)| p.to_string())
        .filter(|p| std::fs::read_dir(workplace.join(p)).is_ok_and(|mut d| d.next().is_some()))
        .collect())
}

/// Push the commit `oid` of the run into the clone `repo` as `refname` (`force`: replacing what is
/// there). The clone's own receive-pack writes; the run is only read.
pub fn deliver(
    gitdir: &Path,
    workplace: &Path,
    repo: &Path,
    oid: &str,
    refname: &str,
    force: bool,
) -> Result<(), String> {
    let repo = std::path::absolute(repo).map_err(|e| format!("{}: {e}", repo.display()))?;
    let spec = format!("{}{oid}:{refname}", if force { "+" } else { "" });
    let mut c = git(gitdir, workplace);
    c.args([
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.file.allow=always",
        "push",
        "-q",
        "--no-verify",
        "--no-recurse-submodules",
        // The clone's receive-pack is a process of its own: `-c` on the push does not reach it.
        "--receive-pack=git -c core.hooksPath=/dev/null receive-pack",
    ])
    .arg(&repo)
    .arg(&spec);
    out(c, &format!("handing {oid} to {}", repo.display())).map(drop)
}

/// The ids of the clone's `refs/remotes/origin/*`: what the clone last saw at origin.
pub fn at_origin(repo: &Path) -> Result<Vec<String>, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(["-c", HOOKS_OFF]).args([
        "for-each-ref",
        "--format=%(objectname)",
        "refs/remotes/origin",
    ]);
    Ok(out(c, "reading the clone's remote branches")?
        .lines()
        .map(String::from)
        .collect())
}

/// The commits reachable from `revs` of the run (a missing one skipped) or from any of its local
/// branches and tags, and from none of `not`.
pub fn count_not(
    gitdir: &Path,
    workplace: &Path,
    revs: &[&str],
    not: &[String],
) -> Result<u64, String> {
    let input: String = revs
        .iter()
        .map(|r| format!("{r}\n"))
        .chain(std::iter::once("--not\n".to_string()))
        .chain(not.iter().map(|r| format!("{r}\n")))
        .collect();
    let mut child = git(gitdir, workplace)
        .args([
            "rev-list",
            "--count",
            "--ignore-missing",
            "--branches",
            "--tags",
            "--stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("git has no input")?;
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let o = child
        .wait_with_output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let _ = writer.join();
    let text = String::from_utf8_lossy(&o.stdout);
    if !o.status.success() {
        return Err(format!(
            "git rev-list: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    text.trim()
        .parse()
        .map_err(|_| format!("git rev-list: {text:?}"))
}

/// Make the run's repository at `gitdir` and its work tree `workplace` on a new `branch` at the
/// commit `start` of the clone `repo`, with `hooks` as its hooks path and the project's own hooks
/// folder recorded as `git_hook::PROJECT_KEY` (`project_hooks`). The `.git` file is written last:
/// a workplace without one is a preparation cut short.
pub fn create(
    repo: &Path,
    gitdir: &Path,
    workplace: &Path,
    branch: &str,
    start: &str,
    hooks: &Path,
) -> Result<(), String> {
    let clone = |args: &[&str]| {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(repo)
            .args(["-c", HOOKS_OFF])
            .args(args)
            .stdin(Stdio::null());
        out(c, &format!("git {}", args.join(" ")))
    };
    let project_hooks = project_hooks(repo, hooks)?;
    let repo = std::path::absolute(repo).map_err(|e| format!("{}: {e}", repo.display()))?;
    let objects = clone(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ])?;
    let start = clone(&["rev-parse", "--verify", &format!("{start}^{{commit}}")])?;
    // The clone's object format (sha1 or sha256), or its objects could not be borrowed.
    let format = clone(&["rev-parse", "--show-object-format"])?;
    for d in [gitdir, workplace] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut init = Command::new("git");
    init.env("GIT_DIR", gitdir)
        .args(["init", "-q", &format!("--object-format={format}")])
        .stdin(Stdio::null());
    out(init, "git init")?;
    std::fs::write(
        gitdir.join("objects/info/alternates"),
        format!("{objects}\n"),
    )
    .map_err(|e| format!("the run's alternates: {e}"))?;
    let repo_s = repo.to_string_lossy();
    let hooks_s = hooks.to_string_lossy();
    // `git init` without a work tree wrote `core.bare = true`: replaced, not added beside it.
    read(gitdir, workplace, &["config", "core.bare", "false"])?;
    let mut config = vec![
        ("core.logAllRefUpdates", "true".to_string()),
        ("core.hooksPath", hooks_s.into_owned()),
        (crate::hooks::git_hook::PROJECT_KEY, project_hooks),
        ("remote.origin.url", repo_s.into_owned()),
        (
            "remote.origin.fetch",
            "+refs/remotes/origin/*:refs/remotes/origin/*".to_string(),
        ),
    ];
    // The worker's own commits carry the clone's identity, as in a worktree of the clone.
    for key in ["user.name", "user.email"] {
        if let Ok(v) = clone(&["config", "--get", key]) {
            config.push((key, v));
        }
    }
    // A push from the workplace goes where the clone's would (a person's own; a worker has no
    // credential and its push is refused); a fetch reads the clone.
    let pushurls = clone(&["config", "--get-all", "remote.origin.pushurl"])
        .or_else(|_| clone(&["config", "--get", "remote.origin.url"]))
        .unwrap_or_default();
    for url in pushurls.lines() {
        config.push(("remote.origin.pushurl", url.to_string()));
    }
    for (k, v) in &config {
        read(gitdir, workplace, &["config", "--add", k, v])?;
    }
    read(gitdir, workplace, &["fetch", "-q", "--no-tags", "origin"])?;
    read(
        gitdir,
        workplace,
        &["update-ref", &format!("refs/heads/{branch}"), &start, ""],
    )?;
    read(
        gitdir,
        workplace,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
    )?;
    read(gitdir, workplace, &["reset", "-q", "--hard"])?;
    std::fs::write(
        workplace.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .map_err(|e| format!("{}: {e}", workplace.display()))
}

/// The hooks folder the clone `repo` would use without `makibad`: the last `core.hooksPath` it
/// reads that is not the scanner's folder `scanner` (expanded as git does, `~/` with this
/// process's home; a relative one kept, for the hook's work tree), or else its `hooks` folder.
/// Read without `HOOKS_OFF`, whose value would be the last.
pub fn project_hooks(repo: &Path, scanner: &Path) -> Result<String, String> {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(repo)
        .args(["config", "--path", "--get-all", "core.hooksPath"])
        .stdin(Stdio::null());
    let o = c.output().map_err(|e| format!("cannot run git: {e}"))?;
    // Status 1: no value is set.
    if !o.status.success() && o.status.code() != Some(1) {
        return Err(format!(
            "reading the clone's core.hooksPath: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    let set = String::from_utf8_lossy(&o.stdout).into_owned();
    if let Some(p) = set.lines().rev().find(|p| Path::new(p) != scanner) {
        return Ok(p.to_string());
    }
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args([
        "-c",
        HOOKS_OFF,
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]);
    Ok(Path::new(&out(c, "reading the clone's git folder")?)
        .join("hooks")
        .to_string_lossy()
        .into_owned())
}

/// Remove `path`, which must be strictly under `top` (both resolved); nothing when it is gone.
pub fn remove_under(top: &Path, path: &Path) -> Result<(), String> {
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
    std::fs::remove_dir_all(&real).map_err(|e| format!("{}: {e}", real.display()))
}

/// The name of the holding folder in each folder a close removes from (Issue 644). It cannot be a
/// project slug (letters, digits and `-`), a task id (never starting with `.`) or a launch id.
pub const HOLDING: &str = ".makiba-removing";

/// `<top>/.makiba-removing`, where `take_out` puts what it takes out of `top`.
pub fn holding(top: &Path) -> PathBuf {
    top.join(HOLDING)
}

/// Take `path`, which must be strictly under `top` (both resolved), out of its place (Issue 644):
/// renamed into `top`'s holding folder, which `empty_holding` empties off the tick; nothing when it
/// is gone. On another file system than the holding folder it is removed here, as before, and
/// that is said.
pub fn take_out(top: &Path, path: &Path) -> Result<(), String> {
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
        Ok(_) => {}
    }
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let top_real = std::fs::canonicalize(top).map_err(|e| format!("{}: {e}", top.display()))?;
    if !real.starts_with(&top_real) || real == top_real {
        return Err(format!(
            "{} is not under {}: it is not removed",
            path.display(),
            top.display()
        ));
    }
    move_out(&top_real, &real)
}

/// Take `entry`, a child of the folder `parent`, out of its place as itself (a link is moved as a
/// link, never followed; Issue 644): `parent` resolved must be strictly under `top` resolved.
pub fn take_out_entry(top: &Path, parent: &Path, entry: &Path) -> Result<(), String> {
    let parent_real =
        std::fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let top_real = std::fs::canonicalize(top).map_err(|e| format!("{}: {e}", top.display()))?;
    if !parent_real.starts_with(&top_real) || parent_real == top_real {
        return Err(format!(
            "{} is not under {}: it is not removed",
            parent.display(),
            top.display()
        ));
    }
    let Some(name) = entry.file_name() else {
        return Err(format!("{} names no entry", entry.display()));
    };
    let entry = parent_real.join(name);
    match entry.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", entry.display())),
        Ok(_) => move_out(&top_real, &entry),
    }
}

/// Rename `entry` (under `top`, both resolved) into `top`'s holding folder, made when absent; on
/// another file system, remove it in place.
fn move_out(top: &Path, entry: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let hold = holding(top);
    match std::fs::DirBuilder::new().mode(0o700).create(&hold) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(format!("{}: {e}", hold.display()));
        }
        _ => {}
    }
    let m = hold
        .symlink_metadata()
        .map_err(|e| format!("{}: {e}", hold.display()))?;
    if !m.file_type().is_dir() {
        return Err(format!("{} is not a folder", hold.display()));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let to = hold.join(format!(
        "{nanos}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    match std::fs::rename(entry, &to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            eprintln!(
                "makibad: {} is on another file system than {}: removed on the tick",
                entry.display(),
                hold.display()
            );
            remove_entry(entry)
        }
        Err(e) => Err(format!("{} -> {}: {e}", entry.display(), to.display())),
    }
}

/// Remove `entry` itself: a folder with all it holds (links in it are not followed), anything else
/// as a file. Gone already counts as removed. A test's hold (`removal`) waits here once.
fn remove_entry(entry: &Path) -> Result<(), String> {
    super::crash::hold("removal");
    let gone = match entry.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", entry.display())),
        Ok(m) if m.file_type().is_dir() => std::fs::remove_dir_all(entry),
        Ok(_) => std::fs::remove_file(entry),
    };
    match gone {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", entry.display()))
        }
        _ => Ok(()),
    }
}

/// Sets the emptier's stop flag when dropped: on `makibad`'s return, an error or an unwind.
pub struct Emptier(Arc<AtomicBool>);

impl Drop for Emptier {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Start the emptier (Issue 644): a thread that, until the returned guard is dropped, empties each
/// of `folders` (holding folders) every second, off the tick. A holding folder that is not a real
/// folder (a link) is said and never listed. Each entry removed is said with its time; a failure
/// is said once per entry and error and tried again on the next round. The thread is not joined:
/// a removal under way is not cut short, and the next start goes on with what is left. `state`
/// arms a test's hold (`crash::arm`).
pub fn empty_holding(state: &Path, folders: Vec<PathBuf>) -> Emptier {
    let stop = Arc::new(AtomicBool::new(false));
    let (flag, state) = (stop.clone(), state.to_path_buf());
    let spawned = std::thread::Builder::new()
        .name("emptier".into())
        .spawn(move || {
            super::crash::arm(&state);
            let mut said: std::collections::HashSet<(PathBuf, String)> = Default::default();
            while !flag.load(Ordering::SeqCst) {
                for hold in &folders {
                    match hold.symlink_metadata() {
                        Err(_) => continue,
                        Ok(m) if !m.file_type().is_dir() => {
                            let why = "not a folder".to_string();
                            if said.insert((hold.clone(), why)) {
                                eprintln!(
                                    "makibad: {} is not a folder: it is not emptied",
                                    hold.display()
                                );
                            }
                            continue;
                        }
                        Ok(_) => {}
                    }
                    let Ok(entries) = std::fs::read_dir(hold) else {
                        continue;
                    };
                    for e in entries.flatten() {
                        if flag.load(Ordering::SeqCst) {
                            return;
                        }
                        let (entry, at) = (e.path(), std::time::Instant::now());
                        match remove_entry(&entry) {
                            Ok(()) => eprintln!(
                                "makibad: removed {} off the tick in {:.1} s",
                                entry.display(),
                                at.elapsed().as_secs_f64()
                            ),
                            Err(why) => {
                                if said.insert((entry.clone(), why.clone())) {
                                    eprintln!(
                                        "makibad: cannot remove {why} off the tick; tried again"
                                    );
                                }
                            }
                        }
                    }
                }
                for _ in 0..10 {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("makibad: cannot start the emptier of the holding folders: {e}");
    }
    Emptier(stop)
}

/// The run's git folder for `task` in the state folder of `settings` (`<state>/settings`).
pub fn dir(settings: &Path, task: &str) -> PathBuf {
    root(settings).join(task)
}

/// `<state>/git`.
pub fn root(settings: &Path) -> PathBuf {
    settings.with_file_name("git")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_git_version_is_read_from_its_text() {
        assert_eq!(git_new_enough("git version 2.43.0"), Some(true));
        assert_eq!(git_new_enough("git version 2.38.0.windows.1"), Some(true));
        assert_eq!(git_new_enough("git version 2.37.9"), Some(false));
        assert_eq!(git_new_enough("git version 3.0"), Some(true));
        assert_eq!(git_new_enough("nonsense"), None);
    }

    /// A run of a sha256 clone is a sha256 repository, whatever git's default format is.
    #[test]
    fn a_run_takes_the_object_format_of_its_clone() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@e"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        std::fs::create_dir(&repo).unwrap();
        git(&["init", "-q", "--object-format=sha256", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "m"]);
        let (gitdir, wt) = (dir.path().join("git"), dir.path().join("work"));
        create(&repo, &gitdir, &wt, "b", "main", &dir.path().join("hooks")).unwrap();
        assert_eq!(
            read(&gitdir, &wt, &["rev-parse", "--show-object-format"]).unwrap(),
            "sha256"
        );
        assert!(commit(&gitdir, &wt, "HEAD").is_some());
    }

    /// A submodule counts only when its folder holds something: an empty one is not checked out.
    #[test]
    fn a_checked_out_submodule_is_found_from_the_index_alone() {
        let dir = tempfile::tempdir().unwrap();
        // As a run is laid out: the git folder outside the work tree.
        let wt = &dir.path().join("work");
        let gitdir = dir.path().join("git");
        std::fs::create_dir(wt).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(wt)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        git(&[
            "init",
            "-q",
            "--separate-git-dir",
            &gitdir.to_string_lossy(),
        ]);
        let oid = "1111111111111111111111111111111111111111";
        for p in ["sub", "empty"] {
            git(&[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{oid},{p}"),
            ]);
            std::fs::create_dir(wt.join(p)).unwrap();
        }
        assert!(checked_out_submodules(&gitdir, wt).unwrap().is_empty());
        std::fs::write(wt.join("sub/work.txt"), "unsaved\n").unwrap();
        assert_eq!(checked_out_submodules(&gitdir, wt).unwrap(), ["sub"]);
    }

    /// Issue 644: a folder strictly under its top goes into the top's holding folder; one gone is
    /// nothing; one not under the top, or a holding folder that is a link, is refused.
    #[test]
    fn take_out_moves_into_the_holding_folder_only_under_its_top() {
        let t = tempfile::tempdir().unwrap();
        let top = t.path().join("top");
        std::fs::create_dir_all(top.join("a/b")).unwrap();
        std::fs::write(top.join("a/b/f"), "x").unwrap();
        take_out(&top, &top.join("a")).unwrap();
        assert!(!top.join("a").exists());
        let held: Vec<_> = std::fs::read_dir(holding(&top))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(held.len(), 1);
        assert!(held[0].path().join("b/f").is_file());
        take_out(&top, &top.join("a")).unwrap();
        assert!(take_out(&top, &top).is_err());
        let other = t.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(take_out(&top, &other).is_err());
        assert!(other.exists());

        let linked = t.path().join("linked");
        std::fs::create_dir_all(linked.join("c")).unwrap();
        std::os::unix::fs::symlink(&other, holding(&linked)).unwrap();
        assert!(take_out(&linked, &linked.join("c")).is_err());
        assert!(linked.join("c").is_dir());
    }

    /// Issue 644: a document's entry is moved as itself: a link to `out/` moves as a link and
    /// `out/` stays; a dangling link moves too.
    #[test]
    fn take_out_entry_moves_a_link_as_a_link() {
        let t = tempfile::tempdir().unwrap();
        let docs = t.path().join("docs");
        let doc = docs.join("d");
        std::fs::create_dir_all(doc.join("out")).unwrap();
        std::fs::write(doc.join("out/report.md"), "kept").unwrap();
        std::os::unix::fs::symlink(doc.join("out"), doc.join("to-out")).unwrap();
        std::os::unix::fs::symlink(t.path().join("nowhere"), doc.join("dangling")).unwrap();
        take_out_entry(&docs, &doc, &doc.join("to-out")).unwrap();
        take_out_entry(&docs, &doc, &doc.join("dangling")).unwrap();
        assert!(doc.join("out/report.md").is_file());
        assert!(doc.join("to-out").symlink_metadata().is_err());
        assert!(doc.join("dangling").symlink_metadata().is_err());
        for e in std::fs::read_dir(holding(&docs)).unwrap().flatten() {
            assert!(e.file_type().unwrap().is_symlink());
            remove_entry(&e.path()).unwrap();
        }
        assert!(doc.join("out/report.md").is_file());
        assert!(take_out_entry(&docs, &docs, &docs.join("d")).is_err());
    }
}
