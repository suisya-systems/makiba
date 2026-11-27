//! `makiba down` (spec section 12, Issue 41): stop the organization so that `makiba up` brings
//! its work back. It runs locally, like `up`: it stops `makibad`, records the state, ends the
//! worker terminals of this organization (the secretary's only with `--secretary`) and stops the
//! base only with `--stop-base`.
//!
//! Recording the state is one transaction (`suspend`): every run `in_use` or `review`, and every
//! run with a launch under way, becomes `suspended`; launches under way are failed with the reason
//! `makiba down`; the workers' keys of those runs are revoked; the session row says `SUSPENDED`
//! with the suspended task ids. A terminal is ended only when its `makiba.org` tag is this
//! organization's, checked here whatever the base's `list` returns.

use crate::base::Base;
use crate::db::Result;
use crate::db::writer::StateWriter;
use crate::spec::{ErrorKind, TAG_ORG, TAG_ROLE};
use rusqlite::params;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: makiba down [--secretary] [--stop-base]\n";

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// End the secretary's terminal too.
    pub secretary: bool,
    /// Stop the base after the terminals are ended.
    pub stop_base: bool,
}

#[derive(Debug, Default)]
pub struct Summary {
    /// The task ids of the runs that are `suspended`.
    pub suspended: Vec<String>,
    /// The names of the terminals ended.
    pub ended: Vec<String>,
}

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// A run at work, for a suspend, over `runs r`: `in_use`, `review`, or `queued` with a launch
/// under way.
pub const AT_WORK: &str = "(r.status IN ('in_use', 'review')
    OR (r.status = 'queued' AND EXISTS (SELECT 1 FROM run_launch l WHERE l.run_id = r.id
        AND l.stage IN ('worktree_ready', 'settings_ready', 'launched', 'dialog_passed'))))";

/// The runs `ids` become `suspended`, their launches under way are failed with `reason`, and
/// their workers' keys are revoked. No other run is touched, and the session row is not written:
/// `suspend` writes it for `makiba down`; a core restart that `makibad` saw (Issue 579,
/// `daemon/recover.rs`) suspends only the runs it replaces.
pub fn suspend_runs(w: &StateWriter, ids: &[i64], reason: &str) -> Result<()> {
    let ids = serde_json::json!(ids).to_string();
    w.conn().execute(
        "UPDATE runs SET status = 'suspended' WHERE id IN (SELECT value FROM json_each(?1))",
        [&ids],
    )?;
    w.conn().execute(
        "UPDATE run_launch SET stage = 'failed', failed_reason = ?2
         WHERE stage IN ('worktree_ready', 'settings_ready', 'launched', 'dialog_passed')
           AND run_id IN (SELECT value FROM json_each(?1))",
        params![ids, reason],
    )?;
    w.conn().execute(
        &format!(
            "UPDATE agents SET revoked_at = {NOW} WHERE role = 'worker' AND revoked_at IS NULL
             AND run_id IN (SELECT value FROM json_each(?1))"
        ),
        [&ids],
    )?;
    Ok(())
}

/// Record the state: the runs at work become `suspended` (see the module comment). Returns the
/// task ids of every suspended run. Also used by `makiba up` after a stop of the base.
pub fn suspend(w: &StateWriter) -> Result<Vec<String>> {
    w.transaction(|w| {
        // The runs at work, and every run already suspended: their launches under way fail and
        // their workers' keys are revoked too.
        let ids: Vec<i64> = w
            .conn()
            .prepare(&format!(
                "SELECT id FROM runs r WHERE {AT_WORK} OR r.status = 'suspended' ORDER BY id"
            ))?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        suspend_runs(w, &ids, "makiba down")?;
        let tasks: Vec<String> = w
            .conn()
            .prepare("SELECT task_id FROM runs WHERE status = 'suspended' ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        w.conn().execute(
            &format!(
                "UPDATE org_sessions SET status = 'SUSPENDED', suspended_at = {NOW},
                   updated_at = {NOW}, resume_instructions = ?1, last_writer_at = {NOW}
                 WHERE id = 1"
            ),
            params![serde_json::json!({ "suspended": tasks }).to_string()],
        )?;
        Ok(tasks)
    })
}

/// Record the state, end this organization's terminals as `opts` says, stop the base if asked.
/// `makibad` must not be running (`main` stops it first).
pub fn run(dir: &Path, base: &dyn Base, opts: Options) -> std::result::Result<Summary, String> {
    let org = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let org = org.to_string_lossy().into_owned();
    let w = StateWriter::open(&dir.join("state.db")).map_err(|e| e.to_string())?;
    let suspended = suspend(&w).map_err(|e| e.to_string())?;
    if opts.secretary {
        w.conn()
            .execute(
                &format!(
                    "UPDATE agents SET revoked_at = {NOW}
                     WHERE role = 'secretary' AND revoked_at IS NULL"
                ),
                [],
            )
            .map_err(|e| e.to_string())?;
    }
    let terminals = base
        .list()
        .map_err(|e| format!("cannot list the base's terminals: {e}"))?;
    let mut ended = Vec::new();
    for t in terminals {
        // Never a terminal without this organization's tag, whatever `list` returned.
        if t.tags.get(TAG_ORG) != Some(&org) {
            continue;
        }
        match t.tags.get(TAG_ROLE).map(String::as_str) {
            Some("worker") => {}
            Some("secretary") if opts.secretary => {}
            _ => continue,
        }
        base.end(&t.id)
            .map_err(|e| format!("cannot end {:?}: {e}", t.name))?;
        ended.push(t.name);
    }
    if opts.stop_base {
        base.stop()
            .map_err(|e| format!("cannot stop the base: {e}"))?;
    }
    Ok(Summary { suspended, ended })
}

/// SIGTERM to the `makibad` holding the lock of `dir`, and wait until it has let the lock go.
pub(crate) fn stop_daemon(dir: &Path) -> std::result::Result<bool, String> {
    let running = |d: &Path| crate::up::daemon_running(d).map_err(|(_, e)| e);
    if !running(dir)? {
        return Ok(false);
    }
    let pid: i32 = std::fs::read_to_string(dir.join("makibad.lock"))
        .ok()
        .and_then(|p| p.trim().parse().ok())
        .ok_or("makibad holds its lock but its pid is not in the lock file")?;
    // SAFETY: a plain kill(2) of the pid makibad wrote into its own lock file.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if !running(dir)? {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("makibad (pid {pid}) is still running after 20 s"))
}

/// `makiba down [--secretary] [--stop-base]`.
pub fn main(args: &[String]) -> ExitCode {
    let mut opts = Options::default();
    for a in args {
        match a.as_str() {
            "--secretary" => opts.secretary = true,
            "--stop-base" => opts.stop_base = true,
            _ => {
                eprint!("makiba: bad arguments for down\n{USAGE}");
                return ExitCode::from(ErrorKind::Usage.exit_code() as u8);
            }
        }
    }
    let fail = |kind: ErrorKind, message: String| {
        eprintln!("makiba: {}: {message}", kind.as_str());
        ExitCode::from(kind.exit_code() as u8)
    };
    if let Some(v) = crate::up::agent_variable() {
        return fail(
            ErrorKind::Denied,
            format!("{v} is set: run `makiba down` from the person's login shell"),
        );
    }
    let dir = crate::daemon::state_dir();
    match stop_daemon(&dir) {
        Ok(true) => println!("makibad: stopped"),
        Ok(false) => println!("makibad: not running"),
        Err(e) => return fail(ErrorKind::Unavailable, e),
    }
    let base = crate::daemon::connect_base(&dir);
    match run(&dir, &*base, opts) {
        Ok(s) => {
            println!("suspended: {}", s.suspended.join(" "));
            println!("ended: {}", s.ended.join(" "));
            if opts.stop_base {
                println!("base: stopped");
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(ErrorKind::Unavailable, e),
    }
}
