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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        ])
        .stdin(Stdio::null());
    c
}

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
    ])
    .arg(&repo)
    .arg(&spec);
    out(c, &format!("handing {oid} to {}", repo.display())).map(drop)
}

/// The ids of the clone's `refs/remotes/origin/*`: what the clone last saw at origin.
pub fn at_origin(repo: &Path) -> Result<Vec<String>, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args([
        "for-each-ref",
        "--format=%(objectname)",
        "refs/remotes/origin",
    ]);
    Ok(out(c, "reading the clone's remote branches")?
        .lines()
        .map(String::from)
        .collect())
}

/// The commits reachable from `revs` of the run (a missing one skipped) and from none of `not`.
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
        .args(["rev-list", "--count", "--ignore-missing", "--stdin"])
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
/// commit `start` of the clone `repo`, with `hooks` as its hooks path. The `.git` file is written
/// last: a workplace without one is a preparation cut short.
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
        c.arg("-C").arg(repo).args(args).stdin(Stdio::null());
        out(c, &format!("git {}", args.join(" ")))
    };
    let repo = std::path::absolute(repo).map_err(|e| format!("{}: {e}", repo.display()))?;
    let objects = clone(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ])?;
    let start = clone(&["rev-parse", "--verify", &format!("{start}^{{commit}}")])?;
    for d in [gitdir, workplace] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut init = Command::new("git");
    init.env("GIT_DIR", gitdir)
        .args(["init", "-q"])
        .stdin(Stdio::null());
    out(init, "git init")?;
    std::fs::write(
        gitdir.join("objects/info/alternates"),
        format!("{objects}\n"),
    )
    .map_err(|e| format!("the run's alternates: {e}"))?;
    let repo_s = repo.to_string_lossy();
    let hooks_s = hooks.to_string_lossy();
    let mut config = vec![
        ("core.bare", "false".to_string()),
        ("core.logAllRefUpdates", "true".to_string()),
        ("core.hooksPath", hooks_s.into_owned()),
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
}
