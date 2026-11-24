//! Running `gh` (`$MAKIBA_GH`, the test seam), shared by the PR watches and the triage scan: a
//! process group of its own, a timeout, and a cancel flag that ends the call and what it started.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A `gh` call that takes longer than this is killed and counts as unreadable.
const GH_TIMEOUT: Duration = Duration::from_secs(120);
/// How often a waiting call looks at its cancel flag.
pub(crate) const SLICE: Duration = Duration::from_millis(20);

/// The program `$MAKIBA_GH` names, default `gh`.
pub(crate) fn program() -> String {
    std::env::var("MAKIBA_GH")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "gh".into())
}

pub(crate) enum GhError {
    /// The program is not there: nothing will change by waiting.
    Missing,
    Cancelled,
    Failed(String),
}

/// Run `gh args`, give its stdout. The child is killed at the timeout and when `cancel` is set.
pub(crate) fn run_gh(gh: &str, args: &[&str], cancel: &AtomicBool) -> Result<String, GhError> {
    run_gh_for(gh, args, cancel, GH_TIMEOUT)
}

/// `run_gh` with the caller's timeout: a read someone waits for (Issue 317).
pub(crate) fn run_gh_for(
    gh: &str,
    args: &[&str],
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<String, GhError> {
    let out = run_bounded(gh, args, None, cancel, timeout, "gh")?;
    String::from_utf8(out).map_err(|_| GhError::Failed("gh wrote text that is not UTF-8".into()))
}

/// Run `program args` in `dir` (else the current folder) and give its stdout: `gh`, and the `git`
/// that finds a project's repository (Issue 355). The child is ended, with what it started, at
/// `timeout` and when `cancel` is set; `name` is what the errors call it.
pub(crate) fn run_bounded(
    program: &str,
    args: &[&str],
    dir: Option<&Path>,
    cancel: &AtomicBool,
    timeout: Duration,
    name: &str,
) -> Result<Vec<u8>, GhError> {
    run_kept(program, args, dir, cancel, timeout, name, None).map(|(out, _)| out)
}

/// `run_gh` that keeps only the last `keep` bytes of the output (Issue 327: a job's log can be
/// large, and only its end is read), and whether anything before them was dropped.
pub(crate) fn run_gh_tail(
    gh: &str,
    args: &[&str],
    cancel: &AtomicBool,
    keep: usize,
) -> Result<(Vec<u8>, bool), GhError> {
    run_kept(gh, args, None, cancel, GH_TIMEOUT, "gh", Some(keep))
}

/// Read `pipe` to its end, keeping all of it, or only its last `keep` bytes, so that memory
/// holds at most twice `keep` at any time; and whether anything was dropped.
fn read_kept(pipe: &mut impl Read, keep: Option<usize>) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let Some(keep) = keep else {
        let _ = pipe.read_to_end(&mut out);
        return (out, false);
    };
    let (mut chunk, mut dropped) = (vec![0u8; 64 * 1024], false);
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if out.len() > keep.saturating_mul(2) {
            out.drain(..out.len() - keep);
            dropped = true;
        }
    }
    if out.len() > keep {
        out.drain(..out.len() - keep);
        dropped = true;
    }
    (out, dropped)
}

/// `run_bounded`, keeping all of the output or only its end (`read_kept`).
fn run_kept(
    program: &str,
    args: &[&str],
    dir: Option<&Path>,
    cancel: &AtomicBool,
    timeout: Duration,
    name: &str,
    keep: Option<usize>,
) -> Result<(Vec<u8>, bool), GhError> {
    // A process group of its own, so that what the program started goes with it when it is ended.
    let mut command = Command::new(program);
    command
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(d) = dir {
        command.current_dir(d);
    }
    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => GhError::Missing,
        _ => GhError::Failed(format!("cannot run {program}: {e}")),
    })?;
    let mut pipe = child.stdout.take().expect("stdout is piped");
    let reader = std::thread::spawn(move || read_kept(&mut pipe, keep));
    let deadline = Instant::now() + timeout;
    // The deadline and the cancel flag hold until the output is collected as well: a descendant
    // may keep the pipe open after the child has gone.
    let mut exited = None;
    let status = loop {
        if exited.is_none() {
            match child.try_wait() {
                Ok(s) => exited = s,
                Err(e) => return Err(GhError::Failed(format!("cannot wait for {program}: {e}"))),
            }
        }
        if let Some(s) = exited
            && reader.is_finished()
        {
            break s;
        }
        let stop = if cancel.load(Ordering::SeqCst) {
            Some(GhError::Cancelled)
        } else if Instant::now() >= deadline {
            Some(GhError::Failed(format!("{name} timed out")))
        } else {
            None
        };
        if let Some(e) = stop {
            // The whole group: a descendant that holds the pipe would hold the reader too.
            // SAFETY: a signal to the group of a child this function started.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
            std::thread::sleep(SLICE);
            if reader.is_finished() {
                let _ = reader.join();
            }
            return Err(e);
        }
        std::thread::sleep(SLICE);
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(GhError::Failed(format!("{name} exited with {status}")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    /// Issue 355: the working folder given is the program's.
    #[test]
    fn a_bounded_program_runs_in_the_folder_given() {
        let dir = tempfile::tempdir().unwrap();
        let never = std::sync::atomic::AtomicBool::new(false);
        let out = super::run_bounded(
            "pwd",
            &["-P"],
            Some(dir.path()),
            &never,
            std::time::Duration::from_secs(10),
            "pwd",
        )
        .unwrap_or_else(|_| panic!("pwd did not run"));
        let want = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap().trim_end(),
            want.to_str().unwrap()
        );
    }

    /// Issue 327: a kept read holds only the last bytes of a long output and says so; a short
    /// output is whole.
    #[test]
    fn a_kept_read_holds_the_end_of_the_output() {
        let long: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (out, dropped) = super::read_kept(&mut long.as_slice(), Some(1000));
        assert!(dropped);
        assert_eq!(out, long[long.len() - 1000..]);
        let (out, dropped) = super::read_kept(&mut &b"short"[..], Some(1000));
        assert!(!dropped);
        assert_eq!(out, b"short");
        let (out, dropped) = super::read_kept(&mut long.as_slice(), None);
        assert!(!dropped);
        assert_eq!(out, long);
        let never = std::sync::atomic::AtomicBool::new(false);
        let (out, dropped) = super::run_kept(
            "printf",
            &["0123456789"],
            None,
            &never,
            std::time::Duration::from_secs(10),
            "printf",
            Some(4),
        )
        .unwrap_or_else(|_| panic!("printf did not run"));
        assert!(dropped);
        assert_eq!(out, b"6789");
    }
}
