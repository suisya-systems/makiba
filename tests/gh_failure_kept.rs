//! Issue 430 (docs/design/merge-act.md, section 8): a failed gh call keeps what it said.
//!
//! - On a non-zero exit, `GhError::Failed` carries the exit code, the response body gh wrote to
//!   stdout and the error line it wrote to stderr, each kept apart, and today's message.
//! - What is kept is bounded: the first 16 KiB of stdout and the first 4 KiB of stderr, and a
//!   child that writes more than that is still read to its end.
//! - A success still gives stdout alone.
//!
//! The runner is called in this process with a stand-in gh written to a temporary folder; no test
//! reaches the network.

use makiba::daemon::gh::{Failure, GhError, run_gh};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Write `body` as an executable shell script in a fresh folder and give its path.
fn fake(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

fn call(path: &PathBuf) -> Result<String, GhError> {
    let never = AtomicBool::new(false);
    run_gh(
        path.to_str().unwrap(),
        &["api", "repos/o/r/pulls/1/merge"],
        &never,
    )
}

fn failure(r: Result<String, GhError>) -> Failure {
    match r {
        Err(GhError::Failed(f)) => f,
        Err(GhError::Missing) => panic!("the stand-in was not found"),
        Err(GhError::Cancelled) => panic!("the call was cancelled"),
        Ok(out) => panic!("the call succeeded: {out}"),
    }
}

#[test]
fn a_failed_call_keeps_its_body_and_its_error() {
    let (_dir, path) = fake(
        "printf '%s\\n' '{\"message\":\"Pull Request is not mergeable\",\"status\":\"405\"}'\n\
         printf '%s\\n' 'gh: Pull Request is not mergeable (HTTP 405)' >&2\n\
         exit 1\n",
    );
    let f = failure(call(&path));
    assert_eq!(f.exit, Some(1));
    assert!(
        f.stdout
            .contains("\"Pull Request is not mergeable\",\"status\":\"405\""),
        "stdout kept: {:?}",
        f.stdout
    );
    assert!(
        f.stderr
            .contains("gh: Pull Request is not mergeable (HTTP 405)"),
        "stderr kept: {:?}",
        f.stderr
    );
    assert!(
        !f.stdout.contains("gh: "),
        "the streams are kept apart: {:?}",
        f.stdout
    );
    assert_eq!(f.message, "gh exited with exit status: 1");
}

#[test]
fn what_is_kept_is_bounded() {
    let (_dir, path) = fake(
        "head -c 100000 /dev/zero | tr '\\0' 'a'\n\
         head -c 100000 /dev/zero | tr '\\0' 'b' >&2\n\
         exit 2\n",
    );
    let started = Instant::now();
    let f = failure(call(&path));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the call returned in {:?}",
        started.elapsed()
    );
    assert_eq!(f.exit, Some(2));
    assert_eq!(f.stdout, "a".repeat(16384));
    assert_eq!(f.stderr, "b".repeat(4096));
}

#[test]
fn a_success_still_gives_stdout_alone() {
    let (_dir, path) = fake("echo ok\necho warning >&2\nexit 0\n");
    match call(&path) {
        Ok(out) => assert_eq!(out, "ok\n"),
        Err(GhError::Failed(f)) => panic!("the call failed: {}", f.message),
        Err(_) => panic!("the call failed"),
    }
}
