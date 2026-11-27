//! Whether a pull request's head is ready for the merge queue (Issue 475, docs/design/queue-ready.md):
//! the one place that holds which landings make a head not ready, and the git reading the PR
//! watch's readiness readers run. It owns no thread, table or timer: the PR watch
//! (`prwatch.rs`) decides when to read and writes what it decides.
//!
//! A head is ready when its CI passed, it does not conflict with its base, and no landing since
//! the base it holds (`git merge-base`, a conservative stand-in for the base its CI ran on) changed
//! a file under one of `CHECKED_PATHS`. Every change under `.github/` counts: the person chose it
//! on 2026-11-25 over a line reader of the workflows (the design's Q2), which kept letting YAML
//! edges through review after review; telling scheduling changes apart waits for a YAML parser.
//!
//! A reading may also read the head against its peers (Issue 573, docs/design/queue-overlap.md):
//! the followed pull requests of the same repository before it in the queue's order, which the
//! PR watch lists. Each peer's branch is fetched in the same fetch, and a peer whose head
//! conflicts with the head by `git merge-tree` is an overlap.

use super::run_repo::HOOKS_OFF;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The paths whose change by a landing makes a head not ready (docs/design/queue-ready.md
/// section 1). `.github/` counts only for what `scheduling_only` cannot clear.
pub const CHECKED_PATHS: &[&str] = &["tests/common/", "tools/", ".github/"];

/// A landing on the base that changed what the checks check, with the paths that count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub sha: String,
    pub paths: Vec<String>,
}

/// One reading of a head against its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// The branch at origin is not at the head read for: a newer head is coming.
    Stale,
    /// The base tip is the one already covered: nothing to read.
    Unchanged,
    /// The base tip `base_tip`, the base the head holds `ci_base`, and the landings since that
    /// count (none: ready).
    Read {
        base_tip: String,
        ci_base: String,
        landings: Vec<Landing>,
    },
    /// As `Read`, with the head read against `peers` too (Issue 573): the base is read whole,
    /// whatever was covered, and `overlaps` are the peers whose heads conflict with it.
    Overlaps {
        base_tip: String,
        ci_base: String,
        landings: Vec<Landing>,
        peers: Vec<Peer>,
        overlaps: Vec<Overlap>,
    },
}

/// A followed pull request before the head in the queue's order (Issue 573): in the merge queue
/// (`queued`), or told ready before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub pr: i64,
    pub task: String,
    pub branch: String,
    pub head: String,
    pub queued: bool,
}

/// A peer whose head conflicts with the head, with the files `git merge-tree` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub pr: i64,
    pub files: Vec<String>,
}

/// How long one git call may run before it is stopped (as `gh.rs` bounds a gh call).
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// git in one repository, every call bounded by a timeout and stopped by `cancel`: a stalled
/// fetch ends as a failure, which counts against the reader's budget, and never holds a stop.
pub struct Git<'a> {
    dir: &'a Path,
    cancel: &'a AtomicBool,
    program: &'a str,
    timeout: Duration,
}

impl<'a> Git<'a> {
    pub fn new(dir: &'a Path, cancel: &'a AtomicBool) -> Git<'a> {
        Git {
            dir,
            cancel,
            program: "git",
            timeout: GIT_TIMEOUT,
        }
    }

    /// `git -C <dir> <args>` with no hook, prompt or replace object, in a process group of its
    /// own (a stop kills what it started too): its trimmed output, or git's message.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let (code, stdout, stderr) = self.exec(args)?;
        if code == Some(0) {
            Ok(stdout)
        } else {
            Err(format!("git {}: {}", args.join(" "), stderr.trim()))
        }
    }

    /// As `run`, for a command whose exit status says something (`git merge-tree` exits 1 on a
    /// conflict and names the files on its output): the exit code and the trimmed output, or a
    /// message when git could not run, was stopped, or ended by a signal.
    fn run_status(&self, args: &[&str]) -> Result<(i32, String), String> {
        let (code, stdout, stderr) = self.exec(args)?;
        match code {
            Some(c) => Ok((c, stdout)),
            None => Err(format!(
                "git {}: ended by a signal: {}",
                args.join(" "),
                stderr.trim()
            )),
        }
    }

    /// The call itself, under the timeout and the cancel: its exit code (none: a signal), its
    /// trimmed output and its error output.
    fn exec(&self, args: &[&str]) -> Result<(Option<i32>, String, String), String> {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(self.program);
        if self.program == "git" {
            cmd.arg("-C").arg(self.dir).args(["-c", HOOKS_OFF]);
        }
        let mut child = cmd
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot run git: {e}"))?;
        // Each pipe is read on a thread of its own, which sends what it read once the pipe closes.
        let drain = |mut r: Box<dyn Read + Send>| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut b = Vec::new();
                let _ = r.read_to_end(&mut b);
                let _ = tx.send(b);
            });
            rx
        };
        let out = child.stdout.take().map(|o| drain(Box::new(o)));
        let err = child.stderr.take().map(|e| drain(Box::new(e)));
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {}
                Err(e) => return Err(format!("git {}: {e}", args.join(" "))),
            }
            let cancelled = self.cancel.load(Ordering::SeqCst);
            if cancelled || started.elapsed() >= self.timeout {
                // SAFETY: the group is the child's own (`process_group(0)`).
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                // The readers are let go: a descendant may still hold the output open.
                return Err(if cancelled {
                    "cancelled".to_string()
                } else {
                    format!(
                        "git {} did not end within {} s and was stopped",
                        args.join(" "),
                        self.timeout.as_secs()
                    )
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // The output under the same deadline and cancel: a descendant that keeps a pipe open
        // after git exited is killed with the group, and the call fails.
        let take = |rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>| {
            let Some(rx) = rx else {
                return Ok(Vec::new());
            };
            loop {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(b) => return Ok(b),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(Vec::new()),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                let cancelled = self.cancel.load(Ordering::SeqCst);
                if cancelled || started.elapsed() >= self.timeout {
                    // SAFETY: the group is the child's own (`process_group(0)`).
                    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                    return Err(if cancelled {
                        "cancelled".to_string()
                    } else {
                        format!(
                            "git {}: its output was still open {} s after it began; it was stopped",
                            args.join(" "),
                            self.timeout.as_secs()
                        )
                    });
                }
            }
        };
        let (stdout, stderr) = (take(out)?, take(err)?);
        Ok((
            status.code(),
            String::from_utf8_lossy(&stdout).trim_end().to_string(),
            String::from_utf8_lossy(&stderr).to_string(),
        ))
    }
}

/// The refs a reading of `task` fetches into: its own, never `refs/remotes/origin/*`, which the
/// `update` job fetches into (two fetches of one ref collide on git's ref lock, Issue 369).
pub fn refs(task: &str) -> (String, String) {
    (
        format!("refs/makiba/ready/{task}/base"),
        format!("refs/makiba/ready/{task}/head"),
    )
}

/// Read `head` of `branch` against `base` in the project's repository `repo`: fetch both from
/// `origin`, then, unless the base tip is `covered`, the landings since the base the head holds.
pub fn read(
    repo: &Path,
    task: &str,
    base: &str,
    branch: &str,
    head: &str,
    covered: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Reading, String> {
    read_with_peers(repo, task, base, branch, head, covered, None, cancel)
}

/// The ref a reading of `task` fetches the branch of its peer `pr` into (Issue 573).
pub fn peer_ref(task: &str, pr: i64) -> String {
    format!("refs/makiba/ready/{task}/peer/{pr}")
}

/// As `read`; with `peers` given (Issue 573), their branches are fetched in the same fetch, the
/// base is read whole whatever `covered` says, and the reading is `Overlaps`. A peer whose
/// branch at origin is not at its head makes the reading `Stale`.
#[allow(clippy::too_many_arguments)]
pub fn read_with_peers(
    repo: &Path,
    task: &str,
    base: &str,
    branch: &str,
    head: &str,
    covered: Option<&str>,
    peers: Option<&[Peer]>,
    cancel: &AtomicBool,
) -> Result<Reading, String> {
    let g = Git::new(repo, cancel);
    let (base_ref, head_ref) = refs(task);
    let listed: &[Peer] = peers.unwrap_or_default();
    let peer_refs: Vec<String> = listed.iter().map(|p| peer_ref(task, p.pr)).collect();
    for r in [&base_ref, &head_ref].into_iter().chain(&peer_refs) {
        g.run(&["check-ref-format", r])
            .map_err(|_| format!("{r} is not a ref name git takes"))?;
    }
    let (base_spec, head_spec) = (
        format!("+refs/heads/{base}:{base_ref}"),
        format!("+refs/heads/{branch}:{head_ref}"),
    );
    let peer_specs: Vec<String> = listed
        .iter()
        .zip(&peer_refs)
        .map(|(p, r)| format!("+refs/heads/{}:{r}", p.branch))
        .collect();
    let mut fetch: Vec<&str> = vec![
        "fetch",
        "-q",
        "--no-write-fetch-head",
        "--no-auto-gc",
        "--no-tags",
        // No opportunistic update of `refs/remotes/origin/*` from origin's configured refspec.
        "--refmap=",
        "origin",
        &base_spec,
        &head_spec,
    ];
    fetch.extend(peer_specs.iter().map(String::as_str));
    g.run(&fetch)?;
    let id = |r: &str| g.run(&["rev-parse", "--verify", &format!("{r}^{{commit}}")]);
    let at = id(&head_ref)?;
    if !at.eq_ignore_ascii_case(head) {
        return Ok(Reading::Stale);
    }
    for (p, r) in listed.iter().zip(&peer_refs) {
        if !id(r)?.eq_ignore_ascii_case(&p.head) {
            return Ok(Reading::Stale);
        }
    }
    let base_tip = id(&base_ref)?;
    if peers.is_none() && covered.is_some_and(|c| c.eq_ignore_ascii_case(&base_tip)) {
        return Ok(Reading::Unchanged);
    }
    let ci_base = g.run(&["merge-base", head, &base_tip])?;
    let mut landings = Vec::new();
    let list = g.run(&[
        "rev-list",
        "--first-parent",
        "--reverse",
        &format!("{ci_base}..{base_tip}"),
    ])?;
    for sha in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let paths = landing_paths(&g, sha)?;
        if !paths.is_empty() {
            landings.push(Landing {
                sha: sha.to_string(),
                paths,
            });
        }
    }
    let Some(peers) = peers else {
        return Ok(Reading::Read {
            base_tip,
            ci_base,
            landings,
        });
    };
    let mut overlaps = Vec::new();
    for p in peers {
        let files = conflicts(&g, &p.head, head)?;
        if !files.is_empty() {
            overlaps.push(Overlap { pr: p.pr, files });
        }
    }
    Ok(Reading::Overlaps {
        base_tip,
        ci_base,
        landings,
        peers: peers.to_vec(),
        overlaps,
    })
}

/// The files in which the heads `theirs` and `ours` conflict by `git merge-tree` (none: they
/// merge).
fn conflicts(g: &Git, theirs: &str, ours: &str) -> Result<Vec<String>, String> {
    let args = [
        "merge-tree",
        "--write-tree",
        "--name-only",
        "--no-messages",
        theirs,
        ours,
    ];
    match g.run_status(&args)? {
        (0, _) => Ok(Vec::new()),
        (1, out) => {
            let mut files: Vec<String> = out
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect();
            files.sort();
            files.dedup();
            Ok(files)
        }
        (code, _) => Err(format!("git {} exited {code}", args.join(" "))),
    }
}

/// The paths of the landing `sha` (against its first parent) that count.
fn landing_paths(g: &Git, sha: &str) -> Result<Vec<String>, String> {
    let parents = g.run(&["rev-list", "--parents", "-n", "1", sha])?;
    let changed = match parents.split_whitespace().nth(1) {
        // A root commit on the base's line: every path is new.
        None => g.run(&["ls-tree", "-r", "-z", "--name-only", sha])?,
        Some(parent) => g.run(&[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--no-commit-id",
            "--name-only",
            parent,
            sha,
        ])?,
    };
    Ok(changed
        .split('\0')
        .filter(|p| !p.is_empty() && checked(p))
        .map(String::from)
        .collect())
}

/// Whether a change of `path` counts: it lies under one of `CHECKED_PATHS`.
fn checked(path: &str) -> bool {
    CHECKED_PATHS.iter().any(|p| path.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `.github/workflows/ci.yml` before PR 473 (4cefa34), the part it changed.
    const BEFORE_473: &str = "name: CI
on: [push, pull_request, merge_group]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # rustup installs the toolchain of rust-toolchain.toml on the first cargo call.
      - run: cargo test --locked
";

    /// The same after PR 473: triggers, concurrency and a cache step.
    const AFTER_473: &str = "name: CI
on:
  push:
    branches: [main]
  pull_request:
  merge_group:
# One group per pull request: a newer head cancels the older head's run.
concurrency:
  group: ${{ github.workflow }}-${{ github.event_name }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # The Rust build cache, one per job (#471).
      - uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            target
          key: ${{ runner.os }}-cargo-${{ github.job }}
          restore-keys: |
            ${{ runner.os }}-cargo-
      # rustup installs the toolchain of rust-toolchain.toml on the first cargo call.
      - run: cargo test --locked
";

    #[test]
    fn the_paths_are_one_list() {
        assert!(checked("tests/common/mod.rs"));
        assert!(checked("tools/check_language.py"));
        assert!(checked(".github/workflows/ci.yml"));
        assert!(!checked("tests/other.rs"));
        assert!(!checked("src/daemon/prwatch.rs"));
        assert!(!checked("tests/commonplace.rs"));
    }

    /// Every change under `.github/` counts, PR 473's scheduling and caching change included
    /// (the person's rule of 2026-11-25); a change under `tests/` outside `tests/common/` does
    /// not.
    #[test]
    fn every_change_under_github_counts() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git_t(d, &["init", "-q"]);
        std::fs::create_dir_all(d.join(".github/workflows")).unwrap();
        let path = ".github/workflows/ci.yml";
        std::fs::write(d.join(path), BEFORE_473).unwrap();
        git_t(d, &["add", "-A"]);
        git_t(d, &["commit", "-q", "-m", "one"]);
        let root = git_t(d, &["rev-parse", "HEAD"]);
        std::fs::write(d.join(path), AFTER_473).unwrap();
        std::fs::create_dir_all(d.join("tests")).unwrap();
        std::fs::write(d.join("tests/other.rs"), "// other\n").unwrap();
        git_t(d, &["add", "-A"]);
        git_t(d, &["commit", "-q", "-m", "two"]);
        let sha = git_t(d, &["rev-parse", "HEAD"]);
        let cancel = AtomicBool::new(false);
        let g = Git::new(d, &cancel);
        assert_eq!(landing_paths(&g, &sha).unwrap(), vec![path.to_string()]);
        assert_eq!(landing_paths(&g, &root).unwrap(), vec![path.to_string()]);
    }

    /// `git` in `dir` for the tests: a fixed identity, no hook, no signing.
    fn git_t(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false", "-c", HOOKS_OFF])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A call whose process exits while a descendant keeps its output open is stopped at the
    /// deadline, not waited for.
    #[test]
    fn an_output_held_open_by_a_descendant_is_not_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let held = Git {
            dir: dir.path(),
            cancel: &cancel,
            program: "sh",
            timeout: Duration::from_millis(300),
        };
        let started = Instant::now();
        let e = held.run(&["-c", "sleep 30 & exit 0"]).unwrap_err();
        assert!(e.contains("still open"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_reading_fetches_into_its_own_refs_only() {
        let dir = tempfile::tempdir().unwrap();
        let (origin, clone) = (dir.path().join("origin"), dir.path().join("clone"));
        std::fs::create_dir_all(&origin).unwrap();
        git_t(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("a"), "a").unwrap();
        git_t(&origin, &["add", "a"]);
        git_t(&origin, &["commit", "-q", "-m", "a"]);
        git_t(&origin, &["branch", "b"]);
        let head = git_t(&origin, &["rev-parse", "b"]);
        git_t(
            dir.path(),
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        );
        let tracked = git_t(&clone, &["rev-parse", "refs/remotes/origin/main"]);
        std::fs::write(origin.join("t"), "t").unwrap();
        git_t(&origin, &["add", "t"]);
        git_t(&origin, &["commit", "-q", "-m", "t"]);
        let main = git_t(&origin, &["rev-parse", "main"]);
        let cancel = AtomicBool::new(false);
        let r = read(&clone, "t1", "main", "b", &head, None, &cancel).unwrap();
        assert!(
            matches!(r, Reading::Read { ref base_tip, .. } if *base_tip == main),
            "{r:?}"
        );
        assert_eq!(
            git_t(&clone, &["rev-parse", "refs/remotes/origin/main"]),
            tracked,
            "the remote-tracking ref moved"
        );
        assert_eq!(
            git_t(&clone, &["rev-parse", "refs/makiba/ready/t1/base"]),
            main
        );
    }

    #[test]
    fn a_stalled_or_cancelled_call_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let slow = Git {
            dir: dir.path(),
            cancel: &cancel,
            program: "sleep",
            timeout: Duration::from_millis(200),
        };
        let started = Instant::now();
        let e = slow.run(&["30"]).unwrap_err();
        assert!(e.contains("did not end"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let stopped = AtomicBool::new(true);
        let cancelled = Git {
            cancel: &stopped,
            timeout: Duration::from_secs(60),
            ..slow
        };
        let started = Instant::now();
        assert_eq!(cancelled.run(&["30"]).unwrap_err(), "cancelled");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
