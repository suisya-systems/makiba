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
//! The other direction (Issue 567): a head whose own changes since that base touch a check
//! (`landing::is_check_path`, the list the merge order of Issue 331 uses) is also not ready while
//! a landing since added or changed a test (a file under `TESTS`, any test: the one that failed
//! PR 534 reached the checker through `include_str!` without naming it).

use super::run_repo::HOOKS_OFF;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The paths whose change by a landing makes a head not ready (docs/design/queue-ready.md
/// section 1); every change under `.github/` counts. For a head that itself changes a check, a
/// test added or changed under `TESTS` counts too (Issue 567).
pub const CHECKED_PATHS: &[&str] = &["tests/common/", "tools/", ".github/"];
/// Where a test lies (Issue 567): any file under it, of any language, fixtures included.
pub const TESTS: &str = "tests/";

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
    /// The base tip `base_tip`, the base the head holds `ci_base`, the paths of the head's own
    /// changes since `ci_base` that are checks (`own_checks`, Issue 567), and the landings since
    /// that count (none: ready).
    Read {
        base_tip: String,
        ci_base: String,
        own_checks: Vec<String>,
        landings: Vec<Landing>,
    },
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
        if status.success() {
            Ok(String::from_utf8_lossy(&stdout).trim_end().to_string())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&stderr).trim()
            ))
        }
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
    let g = Git::new(repo, cancel);
    let (base_ref, head_ref) = refs(task);
    for r in [&base_ref, &head_ref] {
        g.run(&["check-ref-format", r])
            .map_err(|_| format!("{r} is not a ref name git takes"))?;
    }
    g.run(&[
        "fetch",
        "-q",
        "--no-write-fetch-head",
        "--no-auto-gc",
        "--no-tags",
        // No opportunistic update of `refs/remotes/origin/*` from origin's configured refspec.
        "--refmap=",
        "origin",
        &format!("+refs/heads/{base}:{base_ref}"),
        &format!("+refs/heads/{branch}:{head_ref}"),
    ])?;
    let id = |r: &str| g.run(&["rev-parse", "--verify", &format!("{r}^{{commit}}")]);
    let at = id(&head_ref)?;
    if !at.eq_ignore_ascii_case(head) {
        return Ok(Reading::Stale);
    }
    let base_tip = id(&base_ref)?;
    if covered.is_some_and(|c| c.eq_ignore_ascii_case(&base_tip)) {
        return Ok(Reading::Unchanged);
    }
    let ci_base = g.run(&["merge-base", head, &base_tip])?;
    let own_checks: Vec<String> = g
        .run(&[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--no-commit-id",
            "--name-only",
            &ci_base,
            head,
        ])?
        .split('\0')
        .filter(|p| !p.is_empty() && super::landing::is_check_path(p))
        .map(String::from)
        .collect();
    let mut landings = Vec::new();
    let list = g.run(&[
        "rev-list",
        "--first-parent",
        "--reverse",
        &format!("{ci_base}..{base_tip}"),
    ])?;
    for sha in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let paths = landing_paths(&g, sha, !own_checks.is_empty())?;
        if !paths.is_empty() {
            landings.push(Landing {
                sha: sha.to_string(),
                paths,
            });
        }
    }
    Ok(Reading::Read {
        base_tip,
        ci_base,
        own_checks,
        landings,
    })
}

/// The paths of the landing `sha` (against its first parent) that count: under `CHECKED_PATHS`,
/// and, with `tests_count` (the head changes a check, Issue 567), a test added or changed under
/// `TESTS` (a deletion cannot fail, so it does not count).
fn landing_paths(g: &Git, sha: &str, tests_count: bool) -> Result<Vec<String>, String> {
    let parents = g.run(&["rev-list", "--parents", "-n", "1", sha])?;
    // (status, path) of each change.
    let changed: Vec<(String, String)> = match parents.split_whitespace().nth(1) {
        // A root commit on the base's line: every path is new.
        None => g
            .run(&["ls-tree", "-r", "-z", "--name-only", sha])?
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(|p| ("A".to_string(), p.to_string()))
            .collect(),
        Some(parent) => {
            let out = g.run(&[
                "diff-tree",
                "-r",
                "-z",
                "--no-renames",
                "--no-commit-id",
                "--name-status",
                parent,
                sha,
            ])?;
            // `-z --name-status`: each change is its status, NUL, its path, NUL.
            let fields: Vec<&str> = out.split('\0').filter(|f| !f.is_empty()).collect();
            fields
                .chunks(2)
                .filter_map(|c| match c {
                    [status, path] => Some((status.to_string(), path.to_string())),
                    _ => None,
                })
                .collect()
        }
    };
    Ok(changed
        .into_iter()
        .filter(|(status, p)| {
            checked(p) || (tests_count && p.starts_with(TESTS) && !status.starts_with('D'))
        })
        .map(|(_, p)| p)
        .collect())
}

/// Whether a change of `path` counts for every head: it lies under one of `CHECKED_PATHS`.
pub(super) fn checked(path: &str) -> bool {
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
        assert_eq!(
            landing_paths(&g, &sha, false).unwrap(),
            vec![path.to_string()]
        );
        assert_eq!(
            landing_paths(&g, &root, false).unwrap(),
            vec![path.to_string()]
        );
    }

    /// Issue 567: for a head that changes a check, a test added or changed counts and a test
    /// deleted does not; for any other head neither counts; a root commit's tests are added.
    #[test]
    fn a_landed_test_counts_only_for_a_check_changing_head_and_not_when_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git_t(d, &["init", "-q"]);
        std::fs::create_dir_all(d.join("tests/fixtures")).unwrap();
        std::fs::write(d.join("tests/old.rs"), "// old\n").unwrap();
        std::fs::write(d.join("tests/kept.py"), "# kept\n").unwrap();
        git_t(d, &["add", "-A"]);
        git_t(d, &["commit", "-q", "-m", "root"]);
        let root = git_t(d, &["rev-parse", "HEAD"]);
        std::fs::remove_file(d.join("tests/old.rs")).unwrap();
        std::fs::write(d.join("tests/kept.py"), "# changed\n").unwrap();
        std::fs::write(d.join("tests/fixtures/conftest.py"), "x = 1\n").unwrap();
        std::fs::write(d.join("README.md"), "docs\n").unwrap();
        git_t(d, &["add", "-A"]);
        git_t(d, &["commit", "-q", "-m", "two"]);
        let sha = git_t(d, &["rev-parse", "HEAD"]);
        let cancel = AtomicBool::new(false);
        let g = Git::new(d, &cancel);
        assert_eq!(
            landing_paths(&g, &sha, true).unwrap(),
            vec![
                "tests/fixtures/conftest.py".to_string(),
                "tests/kept.py".to_string()
            ]
        );
        assert!(landing_paths(&g, &sha, false).unwrap().is_empty());
        assert_eq!(
            landing_paths(&g, &root, true).unwrap(),
            vec!["tests/kept.py".to_string(), "tests/old.rs".to_string()]
        );
        assert!(landing_paths(&g, &root, false).unwrap().is_empty());
    }

    /// Issue 567: a head's own checks are its changes since the base it holds under
    /// `landing::CHECKS`, a check renamed away included (as a deletion); a landing of a test then
    /// counts for it.
    #[test]
    fn a_reading_names_the_heads_own_checks() {
        let dir = tempfile::tempdir().unwrap();
        let (origin, clone) = (dir.path().join("origin"), dir.path().join("clone"));
        std::fs::create_dir_all(origin.join("tools")).unwrap();
        git_t(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("tools/a.py"), "a").unwrap();
        git_t(&origin, &["add", "-A"]);
        git_t(&origin, &["commit", "-q", "-m", "a"]);
        git_t(&origin, &["checkout", "-q", "-b", "b"]);
        git_t(&origin, &["mv", "tools/a.py", "a.py"]);
        git_t(&origin, &["commit", "-q", "-m", "moved"]);
        let head = git_t(&origin, &["rev-parse", "b"]);
        git_t(&origin, &["checkout", "-q", "main"]);
        std::fs::create_dir_all(origin.join("tests")).unwrap();
        std::fs::write(origin.join("tests/t.rs"), "// t").unwrap();
        git_t(&origin, &["add", "-A"]);
        git_t(&origin, &["commit", "-q", "-m", "t"]);
        let landed = git_t(&origin, &["rev-parse", "main"]);
        git_t(
            dir.path(),
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        );
        let cancel = AtomicBool::new(false);
        let r = read(&clone, "t1", "main", "b", &head, None, &cancel).unwrap();
        let Reading::Read {
            own_checks,
            landings,
            ..
        } = r
        else {
            panic!("{r:?}");
        };
        assert_eq!(own_checks, vec!["tools/a.py".to_string()]);
        assert_eq!(
            landings,
            vec![Landing {
                sha: landed,
                paths: vec!["tests/t.rs".to_string()]
            }]
        );
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
