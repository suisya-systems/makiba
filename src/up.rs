//! `makiba up` (spec section 12): start the base cleanly. It runs locally, before `makibad`
//! exists, from the person's login shell: it refuses inside an agent, checks the Claude Code
//! version and the git version, creates the person key, starts the base's own place and `makibad`, which it asks to
//! launch the secretary unless `--no-secretary` (Issue 43): `makibad` holds the base and decides
//! whether one already runs. A `makibad` already running is asked by a `commands` row (actor
//! `person`, verb `up`, `accepted`) that its secretary pass takes up (Issue 46).
//!
//! After `makiba down` or a stop of the base (Issue 41) it brings the work back before `makibad`
//! starts: a base that was not running had its programs stopped, so the runs at work are suspended
//! first (`down::suspend`); then every `suspended` run gets a new launch (`resume`) that the
//! daemon's passes take on.
//!
//! A core that answers with another version than the pinned one stops `up` at once, before the
//! ledger is opened and before the base is started, naming what to run (Issue 418): `up` never stops a core.
//! A core of another version that the base's own start started (an older server found first,
//! Issue 382) stops `up` too, before a run is resumed or `makibad` started; the message names the
//! server's file (Linux) and its version, and the core is left running.

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

/// How long `up` waits in all for the ledger's write lock when another process (a running
/// `makibad`, a `makiba` command) holds it, longer than one connection's 5 s busy timeout (Issue 288).
const LEDGER_WAIT: Duration = Duration::from_secs(30);

/// Run `f` on `state.db` in `dir` inside one write transaction, committed when `f` succeeds. The
/// write lock is waited for up to `LEDGER_WAIT`, each try's busy timeout cut to what is left of
/// it; when the wait runs out the ledger is said to be busy, not an internal error.
fn with_ledger<T>(
    dir: &Path,
    mut f: impl FnMut(&StateWriter) -> Result<T, Failure>,
) -> Result<T, Failure> {
    let path = dir.join("state.db");
    let until = Instant::now() + LEDGER_WAIT;
    loop {
        let locked = StateWriter::open(&path).and_then(|w| {
            let left = until.saturating_duration_since(Instant::now());
            w.conn()
                .busy_timeout(left.min(Duration::from_secs(5)))
                .map_err(crate::db::Error::from)?;
            w.begin()?;
            Ok(w)
        });
        let w = match locked {
            Ok(w) => w,
            Err(e) if crate::db::is_busy(&e) && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) if crate::db::is_busy(&e) => {
                return Err((
                    ErrorKind::Unavailable,
                    format!(
                        "the ledger {} is busy: another makiba process has held its write lock for {} s; \
                         wait a moment and run `makiba up` again (`makiba status` shows what runs)",
                        path.display(),
                        LEDGER_WAIT.as_secs()
                    ),
                ));
            }
            Err(e) => {
                return Err((ErrorKind::Internal, format!("cannot open state.db: {e}")));
            }
        };
        let out = f(&w);
        match &out {
            Ok(_) => w
                .commit()
                .map_err(|e| (ErrorKind::Internal, format!("cannot write state.db: {e}")))?,
            Err(_) => {
                let _ = w.rollback();
            }
        }
        return out;
    }
}

/// `claude --version` must be at or above the lowest supported version and not refused by name;
/// a version newer than the newest checked one is said so in one line and accepted.
pub(crate) fn check_claude() -> Result<(), Failure> {
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

/// `git --version` must be at or above `run_repo::GIT_LOWEST` (Issue 241: `update` merges with
/// `git merge-tree --write-tree`).
pub(crate) fn check_git() -> Result<(), Failure> {
    let out = Command::new("git")
        .arg("--version")
        .output()
        .map_err(unavailable("cannot run `git --version`"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (a, b) = daemon::run_repo::GIT_LOWEST;
    match daemon::run_repo::git_new_enough(&text) {
        Some(true) => Ok(()),
        found => Err((
            ErrorKind::Unavailable,
            format!(
                "{} git {a}.{b} or newer is needed (makibad merges with git merge-tree --write-tree); no secretary or worker is started",
                if found.is_none() {
                    format!("cannot read a git version from {:?};", text.trim())
                } else {
                    format!("{} is too old:", text.trim())
                }
            ),
        )),
    }
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

/// The refusal of a core of session `session` that runs `found`, not the pinned release
/// (docs/design/base-upgrade.md section 4). An older core names `base upgrade` by the path of
/// this binary, so that the right build is named even when `PATH` finds another.
fn other_version(session: &str, found: &str) -> Failure {
    use std::cmp::Ordering;
    let pinned = base::version::PINNED;
    let head = format!("the base of session {session} runs {found}");
    let text = match base::version::compare_with_pinned(found) {
        Some(Ordering::Less) => {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "makiba".into());
            format!(
                "{head}, this makiba pins {pinned}. To take {pinned}: `{exe} base upgrade` (it asks \
                 before ending the base's terminals)"
            )
        }
        Some(Ordering::Greater) => format!(
            "{head}, this makiba pins {pinned}: this makiba is older than the base. Run the makiba \
             that pins {found} by its path"
        ),
        _ => format!("{head}, which this makiba cannot compare with {pinned}"),
    };
    (ErrorKind::Unavailable, text)
}

/// After the base's start: the version of the core that answers, when it is not the pinned one
/// (Issue 382). While nothing listens yet it asks again every 100 ms for up to 5 s; anything
/// else (the pinned version, still nothing, another error) is `None`, and `up` goes on as before.
fn started_version(the_base: &dyn base::Base) -> Option<String> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match the_base.hello() {
            Err(BaseError::UnsupportedVersion { found, .. }) => return Some(found),
            Err(BaseError::Unavailable(d)) if d.kind == "connect" && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => return None,
        }
    }
}

/// The refusal of a core of session `session` that the base's own start started and that runs
/// `found`, not the pinned release (Issue 382). The text is the base's
/// (`base::started_other_version`); the way out is named by the path of this binary. It says
/// what `up` left undone, which `base upgrade` (whose last step is `run`) passes on in its own
/// failure.
fn started_other_version(session: &str, found: &str) -> Failure {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "makiba".into());
    (
        ErrorKind::Unavailable,
        format!(
            "{}. `up` stopped there: no run was resumed and makibad was not started",
            base::started_other_version(session, found, &exe)
        ),
    )
}

/// Everything `up` does but take the session lock, which `main` takes shared and
/// `base upgrade` (which calls this at its last step) holds exclusively.
pub(crate) fn run(secretary: bool) -> Result<(), Failure> {
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
    check_git()?;
    let session = std::env::var("MAKIBA_BASE_SESSION").unwrap_or_else(|_| "makiba".into());
    let the_base = base::connect_session_for_start(&session, Duration::from_secs(10));
    // With the stand-in (`MAKIBA_BASE=stub`, the switch makibad reads) the base makibad uses is not
    // the one probed here, and the stand-in never stops: no probe.
    let probe = (std::env::var("MAKIBA_BASE").as_deref() != Ok("stub")).then(|| the_base.hello());
    // A core of another version is refused before anything is started or the ledger is opened.
    if let Some(Err(BaseError::UnsupportedVersion { found, .. })) = &probe {
        return Err(other_version(&session, found));
    }
    // Nothing listening at the base's socket: the base is stopped, and its programs with it.
    let stopped = matches!(&probe, Some(Err(BaseError::Unavailable(d))) if d.kind == "connect");
    let dir = daemon::state_dir();
    std::fs::create_dir_all(&dir).map_err(unavailable("cannot create the state folder"))?;
    // Issue 563: a stalled makibad may hold the ledger's write lock, so it is named before any
    // ledger write, and `up` writes and starts nothing.
    refuse_stalled(&dir)?;
    // The key file is written under the ledger's lock; a try that waited finds it there and keeps it.
    with_ledger(&dir, |w| {
        person_key::ensure(w, &crate::cli::person_key_path()).map_err(|e| (ErrorKind::Internal, e))
    })?;
    // Issue 590: outside the ledger's lock, so ssh-keygen never lengthens a hold of it. Every agent
    // runs in a terminal of a base on this machine, of this organization or of another that may
    // share the key's place. With the stand-in (`MAKIBA_BASE=stub`) nothing was probed and no
    // base is read.
    let on_base = match &probe {
        None => Ok(Vec::new()),
        Some(_) => base_agents_that_may_read(&dir),
    };
    approval_key(&dir, on_base);
    let internal = |e: crate::db::Error| (ErrorKind::Internal, e.to_string());
    // Suspended before the base is back: a makibad still running would otherwise see the stopped
    // workers' terminals missing on its reconnect and fail their runs first.
    // A stopped core left a shell in place of a kept secretary terminal: its launch is failed, so
    // `makibad` ends that terminal and launches a new secretary.
    if stopped {
        with_ledger(&dir, |w| {
            crate::down::suspend(w).map_err(internal)?;
            w.conn()
                .execute(
                    "UPDATE run_launch SET stage = 'failed', failed_reason = 'the base was stopped'
                     WHERE role = 'secretary' AND stage != 'failed'",
                    [],
                )
                .map_err(|e| internal(e.into()))
        })?;
    }
    the_base.start().map_err(|e| {
        (
            ErrorKind::Unavailable,
            format!("cannot start the base: {e}"),
        )
    })?;
    // The core the base's start started may be an older server found first (Issue 382):
    // refused before a run is resumed or `makibad` started. Not stopped: `up` never stops a core.
    if probe.is_some()
        && let Some(found) = started_version(&*the_base)
    {
        return Err(started_other_version(&session, &found));
    }
    // Printed only: the app's machine list is the person's (Issue 196).
    println!(
        "app: to see this organization in the app, run on the app's machine: {}",
        base::app_register_command(&session)
    );
    let resumed = with_ledger(&dir, |w| resume(w).map_err(internal))?;
    if !resumed.is_empty() {
        println!("resumed: {}", resumed.join(" "));
    }
    if daemon_running(&dir)? {
        match stalled(&dir) {
            Some((pid, age)) => println!("{}", stall_line(pid, age)),
            None => println!("makibad: already running"),
        }
        if secretary {
            // The running makibad's secretary pass takes the row up (Issue 46): it launches one
            // unless one runs, as at its start.
            with_ledger(&dir, |w| {
                w.conn()
                    .execute(
                        "INSERT INTO commands (actor, verb, args_json, status)
                         VALUES ('person', 'up', '{\"secretary\":true}', 'accepted')",
                        [],
                    )
                    .map_err(|e| internal(e.into()))
            })?;
            println!(
                "secretary: asked of the running makibad, launched unless one runs (see makiba status)"
            );
        } else {
            println!("secretary: not asked (--no-secretary)");
        }
    } else {
        if let Some(u) = unrecorded(&dir) {
            println!(
                "makibad: the previous makibad (pid {}) stopped without recording it; its last \
                 heartbeat was {}. How it stopped is not recorded: a kill, the machine stopping, \
                 or a crash (see makibad.log and the system journal)",
                u.pid,
                u.heartbeat_at.as_deref().unwrap_or("unknown")
            );
        }
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

/// The approval key (Issue 590, docs/design/acceptance-approval.md section 3): kept private when
/// it exists, made when it is absent and nothing that could read it runs, its line printed. `up`
/// goes on whatever happens here: without the key no approval can be signed. `on_base`: the
/// agents running on a base of this machine that may read it (`base_agents_that_may_read`).
fn approval_key(dir: &Path, on_base: Result<Vec<String>, String>) {
    let key = crate::cli::approval_key_path();
    let staging = crate::cli::approval_staging_path();
    let way = "run makiba down --secretary, then makiba up";
    // The live agents whose settings do not deny the key, as the ledger knows them.
    let lacking = match read_only(dir) {
        None => Ok(Vec::new()),
        Some(c) => person_key::agents_without_approval_deny(&c, &dir.join("settings"), &key)
            .map_err(|e| format!("cannot read the agents of the ledger: {e}")),
    };
    let print = |how: &str| match person_key::approval_signer_line(&key) {
        Ok(line) => println!(
            "approval key: {} ({how}); its line for .github/acceptance-approvers: {line}",
            key.display()
        ),
        Err(e) => eprintln!("approval key: {} ({how}); {e}", key.display()),
    };
    match person_key::keep_approval(&key, &staging) {
        Ok(true) => {
            print("kept");
            match &lacking {
                Ok(ids) if ids.is_empty() => {}
                Ok(ids) => println!(
                    "approval key: warning: agents launched without its deny rule: {}; {way}",
                    ids.join(" ")
                ),
                Err(e) => println!("approval key: warning: {e}"),
            }
        }
        Ok(false) => {
            // A makibad already running may be one from before the key, whose launches would
            // not deny it: the key is made only by an `up` that starts makibad itself.
            let not_made = if !matches!(daemon_running(dir), Ok(false)) {
                Some("makibad is running".to_string())
            } else if let Err(e) = &on_base {
                Some(e.clone())
            } else if let Ok(agents) = &on_base
                && !agents.is_empty()
            {
                // Their launches may come from a makiba before the key, or from another
                // organization, whose ledger this `up` does not read.
                Some(format!(
                    "agents on the base may read it: {}",
                    agents.join(", ")
                ))
            } else {
                match &lacking {
                    Ok(ids) if ids.is_empty() => None,
                    Ok(ids) => Some(format!(
                        "agents launched without its deny rule: {}",
                        ids.join(" ")
                    )),
                    Err(e) => Some(e.clone()),
                }
            };
            match not_made {
                Some(why) => println!("approval key: not made: {why}; {way}"),
                None => match person_key::make_approval(&key, &staging) {
                    Ok(()) => print("created"),
                    Err(e) => eprintln!(
                        "approval key: cannot make {}: {e}; makiba merge approve cannot sign an \
                         acceptance approval until it exists",
                        key.display()
                    ),
                },
            }
        }
        Err(e) => eprintln!("approval key: {e}"),
    }
}

/// This organization's `makiba.org` tag: the state folder's absolute path, as `makibad` and
/// `down` write and read it.
fn organization(dir: &Path) -> String {
    std::fs::canonicalize(dir)
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The makiba agents running in the base's terminals, of every session on this machine, that may
/// read the approval key: a terminal is what runs, whatever the ledger says of its key (`down`
/// revokes a key before it ends the terminal). A session nothing listens at runs none; any other
/// failure to list one is an error, so the key is not made.
fn base_agents_that_may_read(dir: &Path) -> Result<Vec<String>, String> {
    let mut per_session = Vec::new();
    for session in base::sessions()? {
        match base::connect_session(&session, Duration::from_secs(5)).list() {
            Ok(ts) => per_session.push((session, ts)),
            Err(BaseError::Unavailable(d)) if d.kind == "connect" => {}
            Err(e) => {
                return Err(format!(
                    "cannot list the terminals of session {session}: {e}"
                ));
            }
        }
    }
    let conn = read_only(dir);
    let key = crate::cli::approval_key_path();
    let settings = dir.join("settings");
    let denies = |spawn: &str| {
        conn.as_ref().is_some_and(|c| {
            person_key::launch_denies_approval(c, &settings, spawn, &key).unwrap_or(false)
        })
    };
    Ok(agents_that_may_read(
        &per_session,
        &organization(dir),
        &denies,
    ))
}

/// Of the terminals of each session, the makiba agents (a live program in a terminal with a
/// `makiba.role` tag) that may read the approval key: every one of another organization (another
/// `makiba.org` tag, or none), and one of `org` whose launch (`makiba.spawn`) `denies` says does
/// not deny it. Each as `<session>: <name>`, with the organization when it is another.
fn agents_that_may_read(
    per_session: &[(String, Vec<base::Terminal>)],
    org: &str,
    denies: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for (session, terminals) in per_session {
        for t in terminals {
            if !t.alive || !t.tags.contains_key(crate::spec::TAG_ROLE) {
                continue;
            }
            match t.tags.get(crate::spec::TAG_ORG) {
                Some(theirs) if theirs == org => {
                    let ok = t
                        .tags
                        .get(crate::spec::TAG_SPAWN)
                        .is_some_and(|s| denies(s));
                    if !ok {
                        out.push(format!("{session}: {}", t.name));
                    }
                }
                theirs => out.push(format!(
                    "{session}: {} of organization {}",
                    t.name,
                    theirs.map_or("(none)", String::as_str)
                )),
            }
        }
    }
    out
}

fn read_only(dir: &Path) -> Option<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(
        dir.join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()
}

/// A stalled `makibad` may hold the ledger's write lock, so it is named before any ledger write
/// and `up` writes and starts nothing (Issue 563). Reads only.
fn refuse_stalled(dir: &Path) -> Result<(), Failure> {
    if daemon_running(dir)?
        && let Some((pid, age)) = stalled(dir)
    {
        println!("{}", stall_line(pid, age));
        return Err((
            ErrorKind::Unavailable,
            "makibad is stalled: up wrote and started nothing".into(),
        ));
    }
    Ok(())
}

/// What `up` says of a stalled `makibad` and the way out (Issue 563).
fn stall_line(pid: i64, age: f64) -> String {
    format!(
        "makibad: pid {pid} holds its lock but its heartbeat is {age:.0} s old: its tick has \
         stopped. Run makiba down; if it says makibad is still running after 20 s, kill -KILL \
         {pid}; then makiba up"
    )
}

/// The running `makibad`'s pid and heartbeat age when the heartbeat is stale (Issue 563). Only
/// when the dispatcher row names the pid in the lock file: a `makibad` that has taken the lock
/// but not yet written its row is starting, not stalled, and the row's pid may be an old one.
fn stalled(dir: &Path) -> Option<(i64, f64)> {
    let holder: i64 = std::fs::read_to_string(dir.join("makibad.lock"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let conn = read_only(dir)?;
    let age = crate::status_local::heartbeat_age(&conn)?;
    if age < crate::status_local::STALE.as_secs_f64() {
        return None;
    }
    let pid: i64 = conn
        .query_row("SELECT pid FROM dispatcher WHERE id = 1", [], |r| r.get(0))
        .ok()?;
    (pid == holder).then_some((pid, age))
}

/// The previous `makibad` when it stopped without recording it (Issue 563); asked only while no
/// `makibad` holds the lock.
fn unrecorded(dir: &Path) -> Option<crate::status_local::Unrecorded> {
    crate::status_local::unrecorded_stop(&read_only(dir)?)
        .ok()
        .flatten()
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
            daemon::launch::add_relaunch(w, *from, conversation.as_deref(), "person")?;
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
    // Held until `up` ends: no `base upgrade` stops or replaces the base meanwhile, and none
    // that runs lets this `up` start the old core between its stop and its rename (Issue 422).
    let session = crate::daemon::base_session();
    let _lock = match crate::base::upgrade::up_lock(&session) {
        Ok(lock) => lock,
        Err((kind, message)) => {
            eprintln!("makiba: {}: {message}", kind.as_str());
            return ExitCode::from(kind.exit_code() as u8);
        }
    };
    match run(!args.iter().any(|a| a == "--no-secretary")) {
        Ok(()) => ExitCode::SUCCESS,
        Err((kind, message)) => {
            eprintln!("makiba: {}: {message}", kind.as_str());
            ExitCode::from(kind.exit_code() as u8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state folder whose dispatcher row names `row_pid` with a heartbeat a minute old, and
    /// whose lock file holds `lock_pid`.
    fn folder(row_pid: i64, lock_pid: i64) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.conn()
            .execute(
                "INSERT INTO dispatcher (id, pid, started_at, heartbeat_at, subscribed)
                 VALUES (1, ?1, strftime('%Y-%m-%dT%H:%M:%fZ','now','-120 seconds'),
                         strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 seconds'), 1)",
                [row_pid],
            )
            .unwrap();
        std::fs::write(dir.path().join("makibad.lock"), lock_pid.to_string()).unwrap();
        dir
    }

    /// Issue 590: every agent of another organization, in any session, and every agent of this
    /// one whose launch does not deny the approval key, is named, whatever the ledger says of its
    /// key; this organization's agents that deny it, a shell a stopped core left in a kept
    /// terminal, and a terminal that is no makiba agent are not.
    #[test]
    fn the_agents_on_a_base_that_may_read_the_approval_key_are_named() {
        use crate::base::{Terminal, TerminalRef};
        let t = |name: &str, alive: bool, tags: &[(&str, &str)]| Terminal {
            id: TerminalRef(name.into()),
            name: name.into(),
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            status: None,
            turn: None,
            alive,
            exit_code: None,
        };
        use crate::spec::{TAG_ORG as ORG, TAG_ROLE as ROLE, TAG_SPAWN as SPAWN};
        let ours = vec![
            t(
                "secretary",
                true,
                &[(ROLE, "secretary"), (ORG, "/org/a"), (SPAWN, "s-new")],
            ),
            t(
                "w-old",
                true,
                &[(ROLE, "worker"), (ORG, "/org/a"), (SPAWN, "s-old")],
            ),
            t("w-nospawn", true, &[(ROLE, "worker"), (ORG, "/org/a")]),
            t(
                "w-shell",
                false,
                &[(ROLE, "worker"), (ORG, "/org/a"), (SPAWN, "s-old")],
            ),
            t("a shell", true, &[]),
        ];
        let theirs = vec![
            t(
                "secretary",
                true,
                &[(ROLE, "secretary"), (ORG, "/org/b"), (SPAWN, "s-new")],
            ),
            t("curator", true, &[(ROLE, "curator")]),
        ];
        let denies = |spawn: &str| spawn == "s-new";
        let per_session = vec![("makiba".to_string(), ours), ("other".to_string(), theirs)];
        assert_eq!(
            agents_that_may_read(&per_session, "/org/a", &denies),
            vec![
                "makiba: w-old".to_string(),
                "makiba: w-nospawn".to_string(),
                "other: secretary of organization /org/b".to_string(),
                "other: curator of organization (none)".to_string(),
            ]
        );
        assert_eq!(
            agents_that_may_read(&per_session[..1], "/org/a", &|_: &str| true),
            vec!["makiba: w-nospawn".to_string()]
        );
    }

    /// Issue 563: a stale row is a stall only when it is the lock holder's; a makibad that has
    /// taken the lock but not yet written its row is starting, and the row's pid is not named.
    #[test]
    fn a_stale_row_of_another_pid_than_the_lock_holder_is_no_stall() {
        let starting = folder(111, 222);
        assert_eq!(stalled(starting.path()), None);
        let stalled_one = folder(333, 333);
        let (pid, age) = stalled(stalled_one.path()).expect("a stall");
        assert_eq!(pid, 333);
        assert!(age >= 59.0, "{age}");
    }

    /// Issue 563: while a stalled makibad holds its lock and the ledger's write lock, up names it
    /// at once, before any ledger write could wait on that lock.
    #[test]
    fn a_stall_is_named_while_the_ledger_is_locked() {
        let dir = folder(444, 444);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("makibad.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let writer = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let asked = Instant::now();
        let refused = refuse_stalled(dir.path()).expect_err("a stalled makibad is refused");
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "it waited on the ledger"
        );
        assert_eq!(refused.0, ErrorKind::Unavailable);
        writer.execute_batch("ROLLBACK").unwrap();
    }
}
