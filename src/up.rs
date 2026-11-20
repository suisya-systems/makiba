//! `makiba up` (spec section 12): start the base cleanly. It runs locally, before `makibad`
//! exists, from the person's login shell: it refuses inside an agent, checks the Claude Code
//! version, creates the person key, starts the base's own place and `makibad`, which it asks to
//! launch the secretary unless `--no-secretary` (Issue 43): `makibad` holds the base and decides
//! whether one already runs. A `makibad` already running is asked by a `commands` row (actor
//! `person`, verb `up`, `accepted`) that its secretary pass takes up (Issue 46).
//!
//! After `makiba down` or a stop of the base (Issue 41) it brings the work back before `makibad`
//! starts: a base that was not running had its programs stopped, so the runs at work are suspended
//! first (`down::suspend`); then every `suspended` run gets a new launch (`resume`) that the
//! daemon's passes take on.

use crate::agents::version;
use crate::base::{self, BaseError};
use crate::daemon::{self, launch::UNSET, person_key};
use crate::db::writer::StateWriter;
use crate::spec::ErrorKind;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: makiba up [--no-secretary]\n";

/// The base's own terminal variables (spec section 6.4).
const BASE_VARS: &[&str] = &[
    "HC_SESSION",
    "HC_SOCKET",
    "HC_SCOPE",
    "HC_TERMINAL_ID",
    "HC_TERMINAL_SOCKET",
];

/// The variable that shows `up` runs inside an agent or in a terminal of the base, if any.
pub(crate) fn agent_variable() -> Option<String> {
    std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .find(|k| k.starts_with("CLAUDE") || BASE_VARS.contains(&k.as_str()))
}

type Failure = (ErrorKind, String);

fn unavailable<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> Failure + '_ {
    move |e| (ErrorKind::Unavailable, format!("{what}: {e}"))
}

/// `claude --version` must be at or above the lowest supported version and not refused by name;
/// a version newer than the newest checked one is said so in one line and accepted.
fn check_claude() -> Result<(), Failure> {
    let out = Command::new("claude")
        .arg("--version")
        .output()
        .map_err(unavailable("cannot run `claude --version`"))?;
    if !out.status.success() {
        return Err((
            ErrorKind::Unavailable,
            format!("`claude --version` failed: {}", out.status),
        ));
    }
    let note = version::check(&String::from_utf8_lossy(&out.stdout)).map_err(|e| {
        (
            ErrorKind::Unavailable,
            format!("{e}; no secretary or worker is started"),
        )
    })?;
    if let Some(line) = note {
        println!("claude: {line}");
    }
    Ok(())
}

/// Whether a `makibad` holds the lock of the state folder `dir`.
pub(crate) fn daemon_running(dir: &Path) -> Result<bool, Failure> {
    let path = dir.join("makibad.lock");
    let file: File = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(unavailable("cannot open the lock file"))?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => Err((ErrorKind::Internal, format!("cannot lock: {e}"))),
    }
}

/// Start `makiba daemon` detached, with the variables of section 6.4 removed, and wait until it
/// has subscribed to the base. With `secretary` it launches the secretary unless one runs.
fn start_daemon(dir: &Path, secretary: bool) -> Result<(), Failure> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(unavailable("cannot find makiba"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("makibad.log"))
        .map_err(unavailable("cannot open makibad.log"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon");
    if secretary {
        cmd.env(daemon::secretary::SECRETARY_ENV, "1");
    } else {
        cmd.env_remove(daemon::secretary::SECRETARY_ENV);
    }
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone().map_err(unavailable("makibad.log"))?)
        .stderr(log)
        .process_group(0);
    for v in UNSET {
        cmd.env_remove(v);
    }
    let mut child = cmd.spawn().map_err(unavailable("cannot start makibad"))?;
    // Ready = the dispatcher row names this child and says it is subscribed (its lock is taken
    // earlier, before the base has answered).
    let pid = child.id();
    let ready = || -> bool {
        rusqlite::Connection::open_with_flags(
            dir.join("state.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .and_then(|c| {
            c.query_row(
                "SELECT subscribed FROM dispatcher WHERE id = 1 AND pid = ?1",
                [pid],
                |r| r.get::<_, i64>(0),
            )
        })
        .is_ok_and(|s| s == 1)
    };
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if let Some(status) = child.try_wait().map_err(unavailable("makibad"))? {
            return Err((
                ErrorKind::Unavailable,
                format!(
                    "makibad exited at once ({status}); see {}",
                    dir.join("makibad.log").display()
                ),
            ));
        }
        if ready() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err((
        ErrorKind::Unavailable,
        format!(
            "makibad is not subscribed to the base after 20 s; see {}",
            dir.join("makibad.log").display()
        ),
    ))
}

fn run(secretary: bool) -> Result<(), Failure> {
    if let Some(v) = agent_variable() {
        return Err((
            ErrorKind::Denied,
            format!(
                "{v} is set: this shell is inside an agent or a terminal of the base, and what \
                 starts the base hands its environment to every terminal. Run `makiba up` from a login shell"
            ),
        ));
    }
    check_claude()?;
    let dir = daemon::state_dir();
    std::fs::create_dir_all(&dir).map_err(unavailable("cannot create the state folder"))?;
    {
        let w = StateWriter::open(&dir.join("state.db"))
            .map_err(|e| (ErrorKind::Internal, format!("cannot open state.db: {e}")))?;
        person_key::ensure(&w, &crate::cli::person_key_path())
            .map_err(|e| (ErrorKind::Internal, e))?;
    }
    let session = std::env::var("MAKIBA_BASE_SESSION").unwrap_or_else(|_| "makiba".into());
    let the_base = base::connect_session_for_start(&session, Duration::from_secs(10));
    // Nothing listening at the base's socket: the base is stopped, and its programs with it.
    // With the stand-in (`MAKIBA_BASE=stub`, the switch makibad reads) the base makibad uses is not
    // the one probed here, and the stand-in never stops.
    let stopped = std::env::var("MAKIBA_BASE").as_deref() != Ok("stub")
        && matches!(the_base.hello(), Err(BaseError::Unavailable(d)) if d.kind == "connect");
    let internal = |e: crate::db::Error| (ErrorKind::Internal, e.to_string());
    // Suspended before the base is back: a makibad still running would otherwise see the stopped
    // workers' terminals missing on its reconnect and fail their runs first.
    // A stopped core left a shell in place of a kept secretary terminal: its launch is failed, so
    // `makibad` ends that terminal and launches a new secretary.
    if stopped {
        let w = StateWriter::open(&dir.join("state.db")).map_err(internal)?;
        crate::down::suspend(&w).map_err(internal)?;
        w.conn()
            .execute(
                "UPDATE run_launch SET stage = 'failed', failed_reason = 'the base was stopped'
                 WHERE role = 'secretary' AND stage != 'failed'",
                [],
            )
            .map_err(|e| internal(e.into()))?;
    }
    the_base.start().map_err(|e| {
        (
            ErrorKind::Unavailable,
            format!("cannot start the base: {e}"),
        )
    })?;
    {
        let w = StateWriter::open(&dir.join("state.db")).map_err(internal)?;
        let resumed = resume(&w).map_err(internal)?;
        if !resumed.is_empty() {
            println!("resumed: {}", resumed.join(" "));
        }
    }
    if daemon_running(&dir)? {
        println!("makibad: already running");
        if secretary {
            // The running makibad's secretary pass takes the row up (Issue 46): it launches one
            // unless one runs, as at its start.
            let w = StateWriter::open(&dir.join("state.db")).map_err(internal)?;
            w.conn()
                .execute(
                    "INSERT INTO commands (actor, verb, args_json, status)
                     VALUES ('person', 'up', '{\"secretary\":true}', 'accepted')",
                    [],
                )
                .map_err(|e| internal(e.into()))?;
            println!(
                "secretary: asked of the running makibad, launched unless one runs (see makiba status)"
            );
        } else {
            println!("secretary: not asked (--no-secretary)");
        }
    } else {
        start_daemon(&dir, secretary)?;
        println!("makibad: started");
        println!(
            "secretary: {} (see makiba status)",
            if secretary {
                "launching unless one runs"
            } else {
                "not started (--no-secretary)"
            }
        );
    }
    Ok(())
}

/// A new launch for every `suspended` run with no launch under way (Issue 41): copied from its
/// newest launch, in its workplace as it is, resuming the conversation of its newest `verified`
/// launch if that has one. A resume that is not verified is replaced by a fresh launch
/// (`verify::fail`). The session row goes back to `ACTIVE`. Returns the task ids relaunched; a
/// second call adds nothing.
pub fn resume(w: &StateWriter) -> crate::db::Result<Vec<String>> {
    w.transaction(|w| {
        let runs: Vec<(String, i64, Option<String>)> = w
            .conn()
            .prepare(
                "SELECT r.task_id, l.id,
                   (SELECT v.conversation_id FROM run_launch v WHERE v.run_id = r.id
                    AND v.stage = 'verified' ORDER BY v.id DESC LIMIT 1)
                 FROM runs r
                 JOIN run_launch l ON l.id = (SELECT max(id) FROM run_launch WHERE run_id = r.id)
                 WHERE r.status = 'suspended' AND l.stage IN ('verified', 'failed')
                 ORDER BY r.id",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (_, from, conversation) in &runs {
            daemon::launch::add_relaunch(w, *from, conversation.as_deref())?;
        }
        w.conn().execute(
            "UPDATE org_sessions SET status = 'ACTIVE',
               resumed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = 1 AND status = 'SUSPENDED'",
            [],
        )?;
        Ok(runs.into_iter().map(|(task, _, _)| task).collect())
    })
}

/// `makiba up [--no-secretary]`.
pub fn main(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a != "--no-secretary") {
        eprint!("makiba: bad arguments for up\n{USAGE}");
        return ExitCode::from(ErrorKind::Usage.exit_code() as u8);
    }
    match run(!args.iter().any(|a| a == "--no-secretary")) {
        Ok(()) => ExitCode::SUCCESS,
        Err((kind, message)) => {
            eprintln!("makiba: {}: {message}", kind.as_str());
            ExitCode::from(kind.exit_code() as u8)
        }
    }
}
