//! `makiba up` (spec section 12): start the base cleanly. It runs locally, before `makibad`
//! exists, from the person's login shell: it refuses inside an agent, checks the Claude Code
//! version, creates the person key, starts the base's own place and `makibad`. The secretary is
//! M6-2: `up` starts none and says so.

use crate::agents::version;
use crate::base;
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
fn agent_variable() -> Option<String> {
    std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .find(|k| k.starts_with("CLAUDE") || BASE_VARS.contains(&k.as_str()))
}

type Failure = (ErrorKind, String);

fn unavailable<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> Failure + '_ {
    move |e| (ErrorKind::Unavailable, format!("{what}: {e}"))
}

/// `claude --version` must be inside the supported range.
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
    version::check(&String::from_utf8_lossy(&out.stdout)).map_err(|e| {
        (
            ErrorKind::Unavailable,
            format!("{e}; no secretary or worker is started"),
        )
    })
}

/// Whether a `makibad` holds the lock of the state folder `dir`.
fn daemon_running(dir: &Path) -> Result<bool, Failure> {
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
/// holds its lock.
fn start_daemon(dir: &Path) -> Result<(), Failure> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(unavailable("cannot find makiba"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("makibad.log"))
        .map_err(unavailable("cannot open makibad.log"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(unavailable("makibad.log"))?)
        .stderr(log)
        .process_group(0);
    for v in UNSET {
        cmd.env_remove(v);
    }
    let mut child = cmd.spawn().map_err(unavailable("cannot start makibad"))?;
    let until = Instant::now() + Duration::from_secs(10);
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
        if daemon_running(dir)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err((
        ErrorKind::Unavailable,
        "makibad did not take its lock in 10 s".into(),
    ))
}

fn run() -> Result<(), Failure> {
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
    base::connect_session_for_start(&session, Duration::from_secs(10))
        .start()
        .map_err(|e| {
            (
                ErrorKind::Unavailable,
                format!("cannot start the base: {e}"),
            )
        })?;
    if daemon_running(&dir)? {
        println!("makibad: already running");
    } else {
        start_daemon(&dir)?;
        println!("makibad: started");
    }
    // ponytail: the secretary is M6-2; `--no-secretary` has nothing to switch off until then.
    println!("secretary: not started (M6-2)");
    Ok(())
}

/// `makiba up [--no-secretary]`.
pub fn main(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a != "--no-secretary") {
        eprint!("makiba: bad arguments for up\n{USAGE}");
        return ExitCode::from(ErrorKind::Usage.exit_code() as u8);
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((kind, message)) => {
            eprintln!("makiba: {}: {message}", kind.as_str());
            ExitCode::from(kind.exit_code() as u8)
        }
    }
}
