//! Running `gh` (`$MAKIBA_GH`, the test seam), shared by the PR watches and the triage scan: a
//! process group of its own, a timeout, and a cancel flag that ends the call and what it started.

use std::io::Read;
use std::os::unix::process::CommandExt;
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
    // A process group of its own, so that what the program started goes with it when it is ended.
    let mut child = Command::new(gh)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => GhError::Missing,
            _ => GhError::Failed(format!("cannot run {gh}: {e}")),
        })?;
    let mut pipe = child.stdout.take().expect("stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = pipe.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + GH_TIMEOUT;
    // The deadline and the cancel flag hold until the output is collected as well: a descendant
    // may keep the pipe open after the child has gone.
    let mut exited = None;
    let status = loop {
        if exited.is_none() {
            match child.try_wait() {
                Ok(s) => exited = s,
                Err(e) => return Err(GhError::Failed(format!("cannot wait for {gh}: {e}"))),
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
            Some(GhError::Failed("gh timed out".into()))
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
        return Err(GhError::Failed(format!("gh exited with {status}")));
    }
    String::from_utf8(out).map_err(|_| GhError::Failed("gh wrote text that is not UTF-8".into()))
}
