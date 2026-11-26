//! `makiba base upgrade` (spec section 12; docs/design/base-upgrade.md sections 3, 5, 6; Issue
//! 422): take the herdr-core release this makiba pins, with one [y/N] question asked right before
//! the only step that cannot be undone (stopping the core, which ends every terminal's program of
//! the session).
//!
//! It composes what exists: `up::agent_variable`, `up::check_claude` and `up::check_git` (the
//! start's prerequisites), `down::stop_daemon` and `down::run` (the stop), `up::run` (the start),
//! `version::compare_with_pinned` (the pin), `Base::list_session` (the inventory) and
//! `scripts/install-base.sh`, the only installer, carried here and run with `bash`.
//!
//! The order: S0 refusals with nothing changed; S1 the pinned release and a copy of the old one
//! staged and verified, the new file placed beside the one it replaces; S2 the inventory and the
//! question; S3 the stop; S4 one rename; S5 `up` and the check of the version.
//!
//! It owns three locks, OS locks on files that stay: the session's (`session_lock`, which
//! `makiba up` takes shared, so no `up` starts the old core between the stop and the rename), the
//! state folder's and the replaced file's folder's. It keeps nothing in memory beside the ledger
//! and writes no row of its own: the rows are those `down::run` and `up::run` write.

use super::version::{PINNED, compare_with_pinned};
use super::{BaseError, Terminal};
use crate::spec::{ErrorKind, TAG_ORG, TAG_ROLE};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: makiba base upgrade [--no-secretary]\n";

/// The installer, the same file `bash scripts/install-base.sh` runs.
const INSTALLER: &str = include_str!("../../scripts/install-base.sh");

type Failure = (ErrorKind, String);

fn fail(kind: ErrorKind) -> impl Fn(String) -> Failure {
    move |m| (kind, m)
}

/// `${XDG_RUNTIME_DIR:-/tmp}/makiba-upgrade-<session>.lock`.
fn session_lock_path(session: &str) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(format!("makiba-upgrade-{session}.lock"))
}

/// What a lock attempt found.
enum Lock {
    Held(File),
    Busy,
}

fn try_lock(path: &Path, exclusive: bool) -> std::io::Result<Lock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let got = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    match got {
        Ok(()) => Ok(Lock::Held(file)),
        Err(TryLockError::WouldBlock) => Ok(Lock::Busy),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// `makiba up`'s side of the session lock: shared, held by the returned file until it is dropped.
/// A `base upgrade` holding it refuses `up` (`conflict`).
pub(crate) fn up_lock(session: &str) -> Result<Option<File>, Failure> {
    up_lock_at(&session_lock_path(session), session)
}

/// `up_lock` on the lock file `path`. An existing file is opened for writing, else for reading (a
/// shared lock needs no more), and one that cannot be opened either way refuses `up`: a verb may
/// hold it. Only a file that does not exist and cannot be made lets `up` go on without the lock,
/// since a verb holds the lock only on a file that exists.
fn up_lock_at(path: &Path, session: &str) -> Result<Option<File>, Failure> {
    let busy = || {
        (
            ErrorKind::Conflict,
            format!(
                "a `makiba base upgrade` runs on session {session}; wait for it to end, then run \
                 `makiba up` again"
            ),
        )
    };
    let shared = |file: File| match file.try_lock_shared() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Err(busy()),
        Err(TryLockError::Error(e)) => Err((
            ErrorKind::Unavailable,
            format!("cannot lock {}: {e}", path.display()),
        )),
    };
    match try_lock(path, false) {
        Ok(Lock::Held(f)) => Ok(Some(f)),
        Ok(Lock::Busy) => Err(busy()),
        Err(_) if !path.exists() => Ok(None),
        Err(_) => match File::open(path) {
            Ok(f) => shared(f),
            Err(e) => Err((
                ErrorKind::Unavailable,
                format!(
                    "cannot open the upgrade lock {}: {e}; a `makiba base upgrade` may hold it. \
                     Make it readable or remove it once no upgrade runs, then run `makiba up` again",
                    path.display()
                ),
            )),
        },
    }
}

/// The first executable file `name` on `path`.
fn which(name: &str, path: &OsString) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The version a program's `--version` prints: the first word that is a plain
/// `major.minor.patch`, else the whole trimmed text (which then cannot be compared).
fn version_in(text: &str) -> String {
    text.split_whitespace()
        .find(|w| compare_with_pinned(w).is_some())
        .unwrap_or(text.trim())
        .to_string()
}

fn version_of(program: &Path) -> Result<String, String> {
    let out = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run `{} --version`: {e}", program.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`{} --version` failed: {}",
            program.display(),
            out.status
        ));
    }
    Ok(version_in(&String::from_utf8_lossy(&out.stdout)))
}

/// The file `hc server start` will run, and the one this verb replaces (open points 3 and 5).
struct Target {
    /// `$HC_BIN` or the first `hc` on `PATH`, as found.
    named: PathBuf,
    /// Its canonical path: the program itself, links followed.
    file: PathBuf,
}

impl Target {
    fn dir(&self) -> &Path {
        self.file.parent().unwrap_or(Path::new("/"))
    }
    fn name(&self) -> String {
        self.file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Find the file and refuse unless it is the only server `hc server start` could start: a
/// regular file named `herdr-core` or `hc` in a writable folder, and every `herdr-core` beside it
/// or first on `PATH` (and the first `hc` on `PATH` when `HC_BIN` names another) is that file.
fn target() -> Result<Target, Failure> {
    let refuse = fail(ErrorKind::Unavailable);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let hc_bin = std::env::var_os("HC_BIN");
    let named = match &hc_bin {
        Some(v) if v.is_empty() => {
            return Err(refuse(
                "HC_BIN is set and empty: point it at the hc program or unset it; nothing changed"
                    .into(),
            ));
        }
        Some(v) if Path::new(v).components().count() > 1 => PathBuf::from(v),
        Some(v) => which(&v.to_string_lossy(), &path).ok_or_else(|| {
            refuse(format!(
                "HC_BIN names {}, which PATH does not find; nothing changed",
                v.to_string_lossy()
            ))
        })?,
        None => which("hc", &path).ok_or_else(|| {
            refuse("no hc on PATH: install the base or set HC_BIN; nothing changed".into())
        })?,
    };
    let file = fs::canonicalize(&named).map_err(|e| {
        refuse(format!(
            "{} cannot be read ({e}): set HC_BIN to the hc program; nothing changed",
            named.display()
        ))
    })?;
    let t = Target { named, file };
    if !fs::metadata(&t.file).is_ok_and(|m| m.is_file())
        || !["herdr-core", "hc"].contains(&t.name().as_str())
    {
        return Err(refuse(format!(
            "{} is the program {} runs, and it is not a file named herdr-core or hc, the base's own \
             layout (bash scripts/install-base.sh PREFIX makes it); nothing changed",
            t.file.display(),
            t.named.display()
        )));
    }
    let dir = std::ffi::CString::new(t.dir().as_os_str().as_encoded_bytes()).unwrap_or_default();
    // SAFETY: access(2) on a NUL-terminated path.
    if unsafe { libc::access(dir.as_ptr(), libc::W_OK) } != 0 {
        return Err(refuse(format!(
            "{} cannot be written, so {} cannot be replaced; nothing changed",
            t.dir().display(),
            t.file.display()
        )));
    }
    let mut others: Vec<PathBuf> = Vec::new();
    for c in [
        t.named.parent().map(|d| d.join("herdr-core")),
        Some(t.dir().join("herdr-core")),
        which("herdr-core", &path),
        hc_bin.as_ref().and_then(|_| which("hc", &path)),
    ]
    .into_iter()
    .flatten()
    {
        if c.exists()
            && fs::canonicalize(&c).ok().as_deref() != Some(t.file.as_path())
            && !others.contains(&c)
        {
            others.push(c);
        }
    }
    if !others.is_empty() {
        let list: Vec<String> = others.iter().map(|p| p.display().to_string()).collect();
        return Err(refuse(format!(
            "{} is the file to replace, but {} is another program hc could start in its place \
             (#382): remove or rename it, or put {} first on PATH, or point HC_BIN at it; nothing changed",
            t.file.display(),
            list.join(" and "),
            t.dir().display()
        )));
    }
    Ok(t)
}

/// The core of the session, as `hello` finds it.
#[derive(PartialEq)]
enum Core {
    Running(String),
    Stopped,
}

/// Whether a failed connect to `socket` means that nothing listens: no socket file, or one that
/// refuses (a core that ended). Any other error (a socket that cannot be reached, for one)
/// says nothing of the core, and the core is not taken as stopped.
fn nothing_listens(socket: &Path) -> bool {
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => false,
        Err(e) => matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        ),
    }
}

fn core_of(session: &str) -> Result<Core, Failure> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let socket = super::herdr::socket_path(runtime.as_deref(), session);
    match super::connect_session_for_start(session, Duration::from_secs(10)).hello() {
        Ok(info) => Ok(Core::Running(info.version)),
        Err(BaseError::UnsupportedVersion { found, .. }) => Ok(Core::Running(found)),
        Err(BaseError::Unavailable(d)) if d.kind == "connect" && nothing_listens(&socket) => {
            Ok(Core::Stopped)
        }
        Err(e) => Err((
            ErrorKind::Unavailable,
            format!(
                "cannot read the version of the core of session {session}: {e}; nothing changed"
            ),
        )),
    }
}

/// What the core of `session` answers now, in words.
fn answers(session: &str) -> String {
    match core_of(session) {
        Ok(Core::Running(v)) => format!("the core of session {session} answers {v}"),
        Ok(Core::Stopped) => format!("nothing answers at the socket of session {session}"),
        Err((_, e)) => e,
    }
}

/// The files of one run: removed on every way out before S3 (and the new one up to S4).
struct Staging {
    dir: PathBuf,
    new_file: Option<PathBuf>,
}

impl Staging {
    fn clean(&self) {
        let _ = fs::remove_dir_all(&self.dir);
        if let Some(f) = &self.new_file {
            let _ = fs::remove_file(f);
        }
    }
}

/// Run the carried installer for `version` into `prefix`, alone: `HC_BIN` removed and the
/// prefix's own `bin` first on `PATH`, so its check after installing finds what it installed.
fn install(staging: &Path, prefix: &Path, version: &str) -> Result<PathBuf, String> {
    let script = staging.join("install-base.sh");
    if !script.exists() {
        fs::write(&script, INSTALLER)
            .map_err(|e| format!("cannot write {}: {e}", script.display()))?;
    }
    let mut path = OsString::from(prefix.join("bin"));
    if let Some(p) = std::env::var_os("PATH") {
        path.push(":");
        path.push(p);
    }
    let out = Command::new("bash")
        .arg(&script)
        .arg(prefix)
        .env_remove("HC_BIN")
        .env("PATH", path)
        .env("MAKIBA_BASE_PINNED", version)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run bash: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the installer of herdr-core {version} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let file = prefix.join("bin/herdr-core");
    expect_version(&file, version)?;
    Ok(file)
}

fn expect_version(file: &Path, version: &str) -> Result<(), String> {
    let found = version_of(file)?;
    if found == version {
        Ok(())
    } else {
        Err(format!(
            "{} prints the version {found}, not {version}",
            file.display()
        ))
    }
}

/// Remove what a run that no longer holds `lock` left: names in `dir` starting with `prefix`.
fn remove_leftovers(dir: &Path, prefix: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(prefix) {
            let p = e.path();
            let _ = if p.is_dir() {
                fs::remove_dir_all(&p)
            } else {
                fs::remove_file(&p)
            };
        }
    }
}

/// The runs a stop of the core suspends (the rows `down::suspend` takes), and those already
/// suspended, read only.
/// A ledger that does not exist yet holds none; one that cannot be read is an error, so that the
/// question never shows "none" for runs it could not read.
fn runs_to_suspend(dir: &Path) -> Result<Vec<String>, String> {
    let path = dir.join("state.db");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let c =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    c.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    c.prepare(
        "SELECT task_id FROM runs WHERE status IN ('in_use', 'review', 'suspended')
           OR (status = 'queued' AND EXISTS (SELECT 1 FROM run_launch l WHERE l.run_id = runs.id
               AND l.stage IN ('worktree_ready', 'settings_ready', 'launched', 'dialog_passed')))
         ORDER BY id",
    )
    .and_then(|mut q| {
        q.query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
    })
    .map_err(|e| e.to_string())
}

/// The state folders of the other organizations that have a worker or a secretary among
/// `terminals`: a stop of the core would end those agents without suspending their runs.
fn foreign_agents(terminals: &[Terminal], org: &str) -> BTreeSet<String> {
    terminals
        .iter()
        .filter(|t| {
            matches!(
                t.tags.get(TAG_ROLE).map(String::as_str),
                Some("worker" | "secretary")
            )
        })
        .filter_map(|t| t.tags.get(TAG_ORG).filter(|o| *o != org).cloned())
        .collect()
}

/// Every live terminal of `session`, refused when it cannot be read whole or holds another
/// organization's worker or secretary.
fn inventory(session: &str, org: &str) -> Result<Vec<Terminal>, Failure> {
    let terminals = super::connect_session_for_start(session, Duration::from_secs(10))
        .list_session()
        .map_err(|e| {
            (
                ErrorKind::Unavailable,
                format!("cannot list the terminals of session {session}: {e}; nothing changed"),
            )
        })?;
    let others = foreign_agents(&terminals, org);
    if !others.is_empty() {
        let names: Vec<&str> = others.iter().map(String::as_str).collect();
        return Err((
            ErrorKind::Conflict,
            format!(
                "session {session} holds a worker or secretary of another organization (state \
                 folder {}); stopping the core would end its agents without suspending its runs. \
                 Run that organization's `makiba down --secretary` (its own binary and \
                 MAKIBA_STATE_DIR), then run this again; nothing changed",
                names.join(", ")
            ),
        ));
    }
    Ok(terminals)
}

fn newest_start(dir: &Path) -> Option<i64> {
    rusqlite::Connection::open_with_flags(
        dir.join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?
    .query_row(
        "SELECT max(id) FROM events WHERE kind = 'dispatcher_started'",
        [],
        |r| r.get(0),
    )
    .ok()?
}

/// The lines of S2 about the terminals that end with the core.
fn inventory_lines(terminals: &[Terminal], org: &str) -> Vec<String> {
    let mut ours: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines = Vec::new();
    for t in terminals {
        let role = t
            .tags
            .get(TAG_ROLE)
            .cloned()
            .unwrap_or_else(|| "other".into());
        match t.tags.get(TAG_ORG) {
            Some(o) if o == org => *ours.entry(role).or_default() += 1,
            Some(_) => lines.push(format!("    another organization's {role}: {}", t.name)),
            None => lines.push(format!("    not makiba's: {}", t.name)),
        }
    }
    let counted: Vec<String> = ours.iter().map(|(r, n)| format!("{r} {n}")).collect();
    let mut out = vec![format!(
        "    makiba's: {}",
        if counted.is_empty() {
            "none".to_string()
        } else {
            counted.join(", ")
        }
    )];
    out.extend(lines);
    out
}

/// The canonical state folder: this organization's `makiba.org` tag.
fn organization(dir: &Path) -> String {
    fs::canonicalize(dir)
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn run(secretary: bool) -> Result<(), Failure> {
    // S0: refusals before anything changes.
    if let Some(v) = crate::up::agent_variable() {
        return Err((
            ErrorKind::Denied,
            format!(
                "{v} is set: this shell is inside an agent or a terminal of the base. Run `makiba \
                 base upgrade` from the person's login shell"
            ),
        ));
    }
    let session = crate::daemon::base_session();
    let lock_path = session_lock_path(&session);
    let _session_lock = match try_lock(&lock_path, true) {
        Ok(Lock::Held(f)) => f,
        Ok(Lock::Busy) => {
            return Err((
                ErrorKind::Conflict,
                format!(
                    "a base upgrade or makiba up runs on session {session}; wait for it to end; \
                     nothing changed"
                ),
            ));
        }
        Err(e) => {
            return Err((
                ErrorKind::Unavailable,
                format!("cannot lock {}: {e}; nothing changed", lock_path.display()),
            ));
        }
    };
    crate::up::check_claude()?;
    crate::up::check_git()?;
    if std::env::var("MAKIBA_BASE").as_deref() == Ok("stub") {
        return Err((
            ErrorKind::Unavailable,
            "MAKIBA_BASE=stub: the stand-in is no base to upgrade; nothing changed".into(),
        ));
    }
    // SAFETY: isatty(3) on the standard input.
    if unsafe { libc::isatty(0) } != 1 {
        return Err((
            ErrorKind::Usage,
            "the standard input is not a terminal: the question before the base's terminals end \
             cannot be asked (there is no --yes); nothing changed"
                .into(),
        ));
    }
    let dir = crate::daemon::state_dir();
    let base_dir = dir.join("base");
    fs::create_dir_all(&base_dir).map_err(|e| {
        (
            ErrorKind::Unavailable,
            format!("cannot create {}: {e}", base_dir.display()),
        )
    })?;
    let state_lock = base_dir.join("upgrade.lock");
    let _state_lock = match try_lock(&state_lock, true) {
        Ok(Lock::Held(f)) => f,
        Ok(Lock::Busy) => {
            return Err((
                ErrorKind::Conflict,
                format!(
                    "a base upgrade of the state folder {} runs; wait for it to end; nothing changed",
                    dir.display()
                ),
            ));
        }
        Err(e) => {
            return Err((
                ErrorKind::Unavailable,
                format!("cannot lock {}: {e}; nothing changed", state_lock.display()),
            ));
        }
    };
    let t = target()?;
    let file_lock = t.dir().join(format!(".{}.makiba-upgrade.lock", t.name()));
    let _file_lock = match try_lock(&file_lock, true) {
        Ok(Lock::Held(f)) => f,
        Ok(Lock::Busy) => {
            return Err((
                ErrorKind::Conflict,
                format!(
                    "a base upgrade of {} runs; wait for it to end; nothing changed",
                    t.file.display()
                ),
            ));
        }
        Err(e) => {
            return Err((
                ErrorKind::Unavailable,
                format!("cannot lock {}: {e}; nothing changed", file_lock.display()),
            ));
        }
    };
    // What a run that ended without cleaning left, under the locks that cover it.
    remove_leftovers(&base_dir, "staging-");
    remove_leftovers(t.dir(), &format!(".{}.makiba-new-", t.name()));

    let core = core_of(&session)?;
    let file_version = version_of(&t.file)
        .map_err(|e| (ErrorKind::Unavailable, format!("{e}; nothing changed")))?;
    let core_what = format!("the base of session {session}");
    let file_what = t.file.display().to_string();
    let mut checked: Vec<(&str, &str)> = vec![(file_what.as_str(), file_version.as_str())];
    if let Core::Running(v) = &core {
        checked.insert(0, (core_what.as_str(), v.as_str()));
    }
    for (what, v) in &checked {
        match compare_with_pinned(v) {
            None => {
                return Err((
                    ErrorKind::Unavailable,
                    format!(
                        "{what} is {v}, which this makiba cannot compare with {PINNED}; nothing changed"
                    ),
                ));
            }
            Some(Ordering::Greater) => {
                return Err((
                    ErrorKind::Unavailable,
                    format!(
                        "{what} is {v}, newer than {PINNED}, which this makiba pins: this makiba is \
                         older than the base. Run the makiba that pins {v} by its path; nothing changed"
                    ),
                ));
            }
            _ => {}
        }
    }
    let replace = compare_with_pinned(&file_version) == Some(Ordering::Less);
    let stop_core = match &core {
        Core::Running(v) => compare_with_pinned(v) == Some(Ordering::Less),
        Core::Stopped => false,
    };
    if !replace && !stop_core {
        println!(
            "nothing to upgrade: {} and {} is {PINNED}; run `makiba up`",
            match &core {
                Core::Running(_) => format!("{core_what} runs {PINNED}"),
                Core::Stopped => format!("{core_what} is stopped"),
            },
            t.file.display()
        );
        return Ok(());
    }
    // The release to keep a copy of: the core's when it is stopped here, else the file's.
    let old = match &core {
        Core::Running(v) if stop_core => v.clone(),
        _ => file_version.clone(),
    };

    // S1: stage, verify, place beside the file.
    let pid = std::process::id();
    let mut staging = Staging {
        dir: base_dir.join(format!("staging-{pid}")),
        new_file: None,
    };
    let staged = (|| -> Result<(), String> {
        fs::create_dir_all(staging.dir.join("old"))
            .map_err(|e| format!("cannot create {}: {e}", staging.dir.display()))?;
        if replace {
            let fresh = install(&staging.dir, &staging.dir.join("new"), PINNED)?;
            let beside = t.dir().join(format!(".{}.makiba-new-{pid}", t.name()));
            staging.new_file = Some(beside.clone());
            fs::copy(&fresh, &beside)
                .and_then(|_| fs::set_permissions(&beside, fs::Permissions::from_mode(0o755)))
                .map_err(|e| format!("cannot place {}: {e}", beside.display()))?;
            expect_version(&beside, PINNED)?;
        }
        let kept = staging.dir.join("old/herdr-core");
        if file_version == old {
            fs::copy(&t.file, &kept)
                .map_err(|e| format!("cannot copy {}: {e}", t.file.display()))?;
        } else {
            let fetched = install(&staging.dir, &staging.dir.join("old-prefix"), &old)
                .map_err(|e| format!("cannot keep a copy of {old}: {e}"))?;
            fs::copy(&fetched, &kept)
                .map_err(|e| format!("cannot copy {}: {e}", fetched.display()))?;
        }
        expect_version(&kept, &old)
    })();
    if let Err(e) = staged {
        staging.clean();
        return Err((ErrorKind::Unavailable, format!("{e}; nothing changed")));
    }
    let kept_at = base_dir.join(format!("herdr-core-{old}"));

    // S2: the inventory and the question.
    let org = organization(&dir);
    let mut lines = vec![format!("makiba base upgrade, session {session}:")];
    lines.push(match &core {
        Core::Running(v) if stop_core => {
            format!("  core: {v}, stopped, then started with {PINNED}")
        }
        Core::Running(v) => format!("  core: {v}, not stopped"),
        Core::Stopped => format!("  core: stopped, started with {PINNED}"),
    });
    lines.push(if replace {
        format!(
            "  file: {} ({file_version}) is replaced by {PINNED}; the old copy is kept at {}",
            t.file.display(),
            kept_at.display()
        )
    } else {
        format!(
            "  file: {} is {PINNED}: no file is replaced; a copy of {old} is kept at {}",
            t.file.display(),
            kept_at.display()
        )
    });
    if stop_core {
        let terminals = match inventory(&session, &org) {
            Ok(t) => t,
            Err(e) => {
                staging.clean();
                return Err(e);
            }
        };
        lines.push("  terminals whose programs end:".into());
        lines.extend(inventory_lines(&terminals, &org));
    } else {
        lines.push("  terminals: none ends".into());
    }
    if stop_core || matches!(core, Core::Stopped) {
        let runs = match runs_to_suspend(&dir) {
            Ok(r) => r,
            Err(e) => {
                staging.clean();
                return Err((
                    ErrorKind::Unavailable,
                    format!(
                        "cannot read the runs a stop would suspend from {}: {e}; nothing changed",
                        dir.join("state.db").display()
                    ),
                ));
            }
        };
        lines.push(format!(
            "  runs suspended and brought back: {}",
            if runs.is_empty() {
                "none".to_string()
            } else {
                runs.join(" ")
            }
        ));
    }
    let question = match (stop_core, replace, &core) {
        (true, true, _) => "End these terminals and replace herdr-core?",
        (true, false, _) => "End these terminals and restart the core?",
        (false, true, Core::Stopped) => "Replace herdr-core and start the core?",
        (false, _, _) => "Replace herdr-core?",
    };
    let mut out = std::io::stdout().lock();
    for l in &lines {
        let _ = writeln!(out, "{l}");
    }
    let _ = write!(out, "{question} [y/N] ");
    let _ = out.flush();
    drop(out);
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    if answer.trim() != "y" {
        staging.clean();
        println!("not upgraded: nothing changed");
        return Ok(());
    }
    // The question may have waited long: another organization's makibad may have launched an
    // agent meanwhile. The inventory is read again right before the stop; the window left is
    // between this read and `server.stop` (section 10, known limits).
    if stop_core && let Err(e) = inventory(&session, &org) {
        staging.clean();
        return Err(e);
    }
    // The file and the core may also have changed meanwhile (an install by hand, a restart): what
    // was decided at S0 holds only if the same file and the same two versions are found again.
    let same = (|| -> Result<bool, Failure> {
        let again = target()?;
        let file_now = version_of(&again.file).map_err(fail(ErrorKind::Unavailable))?;
        Ok(again.file == t.file && file_now == file_version && core_of(&session)? == core)
    })();
    match same {
        Ok(true) => {}
        Ok(false) => {
            staging.clean();
            return Err((
                ErrorKind::Conflict,
                format!(
                    "the base's file or the core of session {session} changed while the question \
                     waited; run this verb again; nothing changed"
                ),
            ));
        }
        Err(e) => {
            staging.clean();
            return Err(e);
        }
    }

    // S3: the stop. From here the old copy is kept.
    if let Err(e) = fs::rename(staging.dir.join("old/herdr-core"), &kept_at) {
        staging.clean();
        return Err((
            ErrorKind::Unavailable,
            format!(
                "cannot keep the old copy at {}: {e}; nothing changed",
                kept_at.display()
            ),
        ));
    }
    let _ = fs::remove_dir_all(&staging.dir);
    match crate::down::stop_daemon(&dir) {
        Ok(true) => println!("makibad: stopped"),
        Ok(false) => {}
        Err(e) => {
            staging.clean();
            return Err((ErrorKind::Unavailable, format!("{e}; nothing else changed")));
        }
    }
    if stop_core {
        let base = crate::daemon::connect_base(&dir);
        let stopped = crate::down::run(
            &dir,
            &*base,
            crate::down::Options {
                secretary: true,
                stop_base: true,
            },
        )
        .and_then(|_| {
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                if matches!(core_of(&session), Ok(Core::Stopped)) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(format!(
                "the core of session {session} still answers after 20 s"
            ))
        });
        if let Err(e) = stopped {
            staging.clean();
            return Err((
                ErrorKind::Unavailable,
                format!(
                    "{e}; {} was not replaced. What was done is what a failed `makiba down \
                     --secretary --stop-base` leaves: run this verb again, or the old makiba's \
                     `up` by its path (this makiba's `up` refuses the older core)",
                    t.file.display()
                ),
            ));
        }
        println!("base: stopped");
    }

    // S4: one rename within the file's folder.
    if let Some(new_file) = &staging.new_file
        && let Err(e) = fs::rename(new_file, &t.file)
    {
        staging.clean();
        return Err((
            ErrorKind::Unavailable,
            format!(
                "cannot replace {}: {e}; it is the old file and the core is stopped: run this verb \
                 again, or the old makiba's `up` by its path",
                t.file.display()
            ),
        ));
    }

    // S5: start and check.
    let started = crate::up::run(secretary);
    let now = core_of(&session);
    let ok = matches!(&now, Ok(Core::Running(v)) if v == PINNED);
    if let (Ok(()), true) = (&started, ok) {
        println!(
            "upgraded: herdr-core {old} -> {PINNED} on session {session}; {}",
            if replace {
                format!("{} replaced", t.file.display())
            } else {
                format!("{} kept as it was", t.file.display())
            }
        );
        println!("old copy: {}", kept_at.display());
        if let Some(id) = newest_start(&dir) {
            println!("dispatcher_started: event {id}");
        }
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "makiba".into());
        println!(
            "to go back: `{exe} down --secretary --stop-base`; put {} back over {}; then run the \
             old makiba's `up` by its path",
            kept_at.display(),
            t.file.display()
        );
        return Ok(());
    }
    let mut text = match started {
        Err((_, e)) => e,
        Ok(()) => "makiba up returned".into(),
    };
    text.push_str(&format!(
        "; {}; the file replaced was {}",
        answers(&session),
        t.file.display()
    ));
    Err((ErrorKind::Unavailable, text))
}

/// `makiba base upgrade [--no-secretary]`.
pub fn main(args: &[String]) -> ExitCode {
    let mut secretary = true;
    let (mut upgrade, mut bad) = (false, false);
    for a in args {
        match a.as_str() {
            "--no-secretary" => secretary = false,
            "upgrade" if !upgrade => upgrade = true,
            _ => bad = true,
        }
    }
    if !upgrade || bad {
        eprint!("makiba: bad arguments for base\n{USAGE}");
        return ExitCode::from(ErrorKind::Usage.exit_code() as u8);
    }
    match run(secretary) {
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
    use crate::base::TerminalRef;

    fn terminal(name: &str, tags: &[(&str, &str)]) -> Terminal {
        Terminal {
            id: TerminalRef(name.into()),
            name: name.into(),
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            status: None,
            turn: None,
            alive: true,
            exit_code: None,
        }
    }

    #[test]
    fn only_another_organizations_worker_or_secretary_is_foreign() {
        let terminals = [
            terminal("a", &[(TAG_ORG, "/mine"), (TAG_ROLE, "worker")]),
            terminal("b", &[(TAG_ORG, "/other"), (TAG_ROLE, "curator")]),
            terminal("c", &[]),
            terminal("d", &[(TAG_ORG, "/other"), (TAG_ROLE, "worker")]),
            terminal("e", &[(TAG_ORG, "/third"), (TAG_ROLE, "secretary")]),
        ];
        let found: Vec<String> = foreign_agents(&terminals, "/mine").into_iter().collect();
        assert_eq!(found, ["/other", "/third"]);
        assert!(foreign_agents(&terminals[..3], "/mine").is_empty());
    }

    #[test]
    fn an_absent_ledger_has_no_runs_and_an_unreadable_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(runs_to_suspend(dir.path()), Ok(Vec::new()));
        fs::write(dir.path().join("state.db"), "not a database").unwrap();
        assert!(runs_to_suspend(dir.path()).is_err());
    }

    #[test]
    fn up_is_refused_while_an_upgrade_holds_the_lock_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("makiba-upgrade-s.lock");
        // No file and no folder to make it in: no upgrade can hold it.
        assert!(matches!(
            up_lock_at(&dir.path().join("none/x.lock"), "s"),
            Ok(None)
        ));
        let held = match try_lock(&path, true).unwrap() {
            Lock::Held(f) => f,
            Lock::Busy => panic!("busy"),
        };
        assert_eq!(
            up_lock_at(&path, "s").map(|_| ()).unwrap_err().0,
            ErrorKind::Conflict
        );
        // SAFETY: geteuid(2) only reads.
        if unsafe { libc::geteuid() } != 0 {
            // An upgrade holds it and `up` can open it neither way: refused, never let through.
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            assert_eq!(
                up_lock_at(&path, "s").map(|_| ()).unwrap_err().0,
                ErrorKind::Unavailable
            );
            // Readable only: the shared lock is taken on a read-only open.
            drop(held);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
            assert!(matches!(up_lock_at(&path, "s"), Ok(Some(_))));
        }
    }

    #[test]
    fn a_socket_that_cannot_be_reached_is_not_a_stopped_core() {
        // SAFETY: geteuid(2) only reads.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let closed = dir.path().join("closed");
        fs::create_dir(&closed).unwrap();
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
        let unreachable = nothing_listens(&closed.join("makiba.sock"));
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!unreachable);
    }

    #[test]
    fn the_version_is_the_first_plain_word() {
        assert_eq!(version_in("herdr-core 0.1.22\n"), "0.1.22");
        assert_eq!(version_in("0.1.21"), "0.1.21");
        assert_eq!(version_in("herdr-core 0.1"), "herdr-core 0.1");
        assert_eq!(version_in("hc 0.1.22-rc.1"), "hc 0.1.22-rc.1");
    }
}
