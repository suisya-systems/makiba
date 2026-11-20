//! `makibad`, run as `makiba daemon` (spec sections 5.7 and 14): one instance per state folder,
//! a heartbeat, a clean stop. It says it is subscribed only once the base's event stream has
//! yielded its first `Resync`, and only then writes `dispatcher_started`.

pub mod anomalies;
mod close;
mod crash;
pub mod curator;
pub mod delivery;
mod dialogs;
pub mod dispatch;
pub mod door;
mod gh;
pub mod inbox;
pub mod launch;
mod monitor;
pub mod notify_gate;
pub mod person_key;
pub mod prepare;
pub mod prwatch;
mod recover;
mod resume;
pub mod secretary;
mod triage;
mod verify;

use self::anomalies::Anomalies;
use self::delivery::Delivery;
use self::dispatch::Dispatch;
use self::monitor::Monitor;
use self::prepare::{Places, Prepare};
use crate::base::stub::Stub;
use crate::base::{Base, BaseEvent};
use crate::db::writer::{NewEvent, StateWriter};
use serde_json::json;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// How often `dispatcher.heartbeat_at` is written.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
/// How often the stop flag is looked at.
const TICK: Duration = Duration::from_millis(100);

/// `$MAKIBA_STATE_DIR`, default `${XDG_STATE_HOME:-~/.local/state}/makiba` (spec section 2).
pub fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("MAKIBA_STATE_DIR") {
        return d.into();
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("makiba")
}

/// The base this process uses: the stand-in when `MAKIBA_BASE=stub` (tests; its first `Resync`
/// comes after `MAKIBA_STUB_SUBSCRIBE_DELAY_MS`), otherwise the real one, scoped to this state
/// folder so two organizations on one base do not adopt each other's terminals.
pub(crate) fn connect_base(state: &Path) -> Box<dyn Base> {
    if std::env::var("MAKIBA_BASE").as_deref() == Ok("stub") {
        let ms = std::env::var("MAKIBA_STUB_SUBSCRIBE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        return Box::new(Stub::new().with_subscribe_delay(Duration::from_millis(ms)));
    }
    let session = std::env::var("MAKIBA_BASE_SESSION").unwrap_or_else(|_| "makiba".into());
    let org = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
    crate::base::connect_session_as(&session, Duration::from_secs(10), &org.to_string_lossy())
}

pub fn main() -> ExitCode {
    let dir = state_dir();
    let door = match door::Config::from_env() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("makibad: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The channel servers reach the door's port; delivery pushes through their held requests.
    let db = dir.join("state.db");
    let hub = crate::channel::Hub::new(move |token| door::agent_of(&db, token));
    let delivery = Delivery::new(Box::new(hub.clone()));
    match serve(&dir, &|| connect_base(&dir), delivery, Some((door, hub))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("makibad: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Take `makibad.lock` or say who holds it. The holder's pid is the file's content; it is emptied
/// on a clean stop, so a stale pid is only ever left by a process that was killed.
fn take_lock(dir: &Path) -> Result<File, String> {
    let path = dir.join("makibad.lock");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0).map_err(|e| e.to_string())?;
            file.rewind().map_err(|e| e.to_string())?;
            write!(file, "{}", std::process::id()).map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            Ok(file)
        }
        Err(TryLockError::WouldBlock) => {
            // The winner writes its pid right after locking; wait for it a moment.
            let until = Instant::now() + Duration::from_secs(1);
            let mut pid = String::new();
            while Instant::now() < until {
                pid.clear();
                let _ = file.rewind();
                let _ = file.read_to_string(&mut pid);
                if !pid.trim().is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(format!(
                "makibad is already running on {} (pid {})",
                dir.display(),
                if pid.trim().is_empty() {
                    "unknown"
                } else {
                    pid.trim()
                }
            ))
        }
        Err(TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}

fn beat(w: &StateWriter) -> Result<(), String> {
    w.conn()
        .execute(
            "UPDATE dispatcher SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = 1",
            [],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn event(w: &StateWriter, kind: &str, base: &str, version: &str) -> crate::db::Result<()> {
    let payload = json!({"pid": std::process::id(), "base": base, "base_version": version});
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(&payload),
        ..Default::default()
    })
    .map(|_| ())
}

/// Run until SIGTERM or SIGINT. `connect` makes the base once the lock and the database are held.
/// No door is opened and no channel server can connect, so nothing is pushed (`main` opens both).
pub fn run(dir: &Path, connect: &dyn Fn() -> Box<dyn Base>) -> Result<(), String> {
    run_with(dir, connect, Delivery::new(Box::new(delivery::NoChannel)))
}

/// As `run`, with the channel `delivery` pushes through. No door is opened.
pub fn run_with(
    dir: &Path,
    connect: &dyn Fn() -> Box<dyn Base>,
    delivery: Delivery,
) -> Result<(), String> {
    serve(dir, connect, delivery, None)
}

/// As `run`, until `stop` is set as well as on a signal (a daemon in a test's own process).
pub fn run_until(
    dir: &Path,
    connect: &dyn Fn() -> Box<dyn Base>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let delivery = Delivery::new(Box::new(delivery::NoChannel));
    serve_until(dir, connect, delivery, None, stop)
}

/// As `run_with`, and with `door`, the door opened once the base has answered.
fn serve(
    dir: &Path,
    connect: &dyn Fn() -> Box<dyn Base>,
    delivery: Delivery,
    door: Option<(door::Config, crate::channel::Hub)>,
) -> Result<(), String> {
    serve_until(
        dir,
        connect,
        delivery,
        door,
        Arc::new(AtomicBool::new(false)),
    )
}

/// Open `state.db`. Another process opening it at the same moment (the door's request, a test)
/// can make the open itself answer "database is locked" before the busy timeout applies, so a
/// busy answer is tried again for a few seconds.
fn open_state(path: &Path) -> Result<StateWriter, String> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match StateWriter::open(path) {
            Err(crate::db::Error::Sqlite(rusqlite::Error::SqliteFailure(e, _)))
                if matches!(
                    e.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) && Instant::now() < until =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            other => return other.map_err(|e| e.to_string()),
        }
    }
}

fn serve_until(
    dir: &Path,
    connect: &dyn Fn() -> Box<dyn Base>,
    mut delivery: Delivery,
    door: Option<(door::Config, crate::channel::Hub)>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let lock = take_lock(dir)?;
    crash::arm(dir);
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, Arc::clone(&stop)).map_err(|e| e.to_string())?;
    }
    let w = open_state(&dir.join("state.db"))?;
    let base = connect();
    // Not subscribed yet: the row says so, and the heartbeat already runs.
    w.conn()
        .execute(
            "INSERT INTO dispatcher (id, pid, started_at, heartbeat_at, base, base_version, subscribed)
             VALUES (1, ?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?2, NULL, 0)
             ON CONFLICT(id) DO UPDATE SET pid = ?1, started_at = excluded.started_at,
               heartbeat_at = excluded.heartbeat_at, base = ?2, base_version = NULL, subscribed = 0",
            rusqlite::params![std::process::id(), base.name()],
        )
        .map_err(|e| e.to_string())?;

    let info = base
        .hello()
        .map_err(|e| format!("the base refused: {e:?}"))?;
    // The subscription is established on its own thread: `events()` may block, and a
    // real stream never ends, so that thread is left to die with the process.
    // What the thread passes on: whether the subscription exists, then every event of it.
    enum Msg {
        Subscribed(Result<(), String>),
        Event(BaseEvent),
    }
    let (tx, rx) = channel();
    let events_base = Arc::<dyn Base>::from(base);
    let _door = match door {
        Some((cfg, hub)) => Some(door::start(
            &cfg,
            &dir.join("state.db"),
            Arc::clone(&events_base),
            hub,
        )?),
        None => None,
    };
    // Agents reach the door where it listens; without one (`run`), at the configured port.
    let door_addr = _door.as_ref().map_or_else(
        || {
            format!(
                "127.0.0.1:{}",
                crate::cli::port().unwrap_or(crate::cli::DEFAULT_PORT)
            )
        },
        |d| d.addr.to_string(),
    );
    let thread_base = Arc::clone(&events_base);
    std::thread::spawn(move || match thread_base.events() {
        Err(e) => {
            let _ = tx.send(Msg::Subscribed(Err(format!(
                "cannot subscribe to the base: {e:?}"
            ))));
        }
        Ok(mut stream) => {
            let first = stream.next();
            let ok = matches!(first, Some(BaseEvent::Resync(_)));
            let _ = tx.send(Msg::Subscribed(if ok {
                Ok(())
            } else {
                Err(format!("the first event was not a Resync: {first:?}"))
            }));
            if let (true, Some(first)) = (ok, first) {
                // The blocking read stays on this thread; the monitor gets what it yields.
                for event in std::iter::once(first).chain(stream) {
                    if tx.send(Msg::Event(event)).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let org = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut monitor = Monitor::new(&org.to_string_lossy());
    let mut anomalies = Anomalies::new();
    let dispatch = Dispatch::new(dispatch::capacity_from_env());
    let prepare = Prepare::new(Places::from_env(dir), events_base.worker_denies());
    let launch = launch::Launch::new(dir, door_addr);
    let mut curator = curator::Curator::from_env(dir);
    // `makiba up` asks for the secretary (Issue 43); read once, at start.
    let mut secretary = (std::env::var(secretary::SECRETARY_ENV).as_deref() == Ok("1"))
        .then(|| secretary::Secretary::from_env(dir));
    let mut dialogs = dialogs::Dialogs::new();
    let mut verify = verify::Verify::new(verify::timeout_from_env());
    let mut prwatch = prwatch::PrWatch::from_env();
    let mut triage =
        triage::Triage::from_env(Places::from_env(dir).repos, dispatch::capacity_from_env());
    let close = close::Close::new(Places::from_env(dir), org.to_string_lossy().into_owned());
    let mut next_beat = Instant::now() + HEARTBEAT;
    let mut subscribed = false;
    let mut resumed = false;
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(TICK) {
            Ok(Msg::Subscribed(Ok(()))) => {
                w.transaction(|w| {
                    w.conn()
                        .execute(
                            "UPDATE dispatcher SET subscribed = 1, base_version = ?1 WHERE id = 1",
                            [&info.version],
                        )
                        .map_err(crate::db::Error::from)?;
                    event(w, "dispatcher_started", &info.name, &info.version)
                })
                .map_err(|e| e.to_string())?;
                subscribed = true;
            }
            Ok(Msg::Subscribed(Err(e))) => return Err(e),
            Ok(Msg::Event(e)) => {
                // A failed write is reported and the daemon goes on: the monitor keeps its
                // record of what was written, so the next event tries again.
                if let Err(e) = monitor.handle(&w, e) {
                    eprintln!("makibad: cannot record a terminal event: {e}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if !subscribed => {
                return Err("the subscription thread ended before it subscribed".into());
            }
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(TICK),
        }
        // The rules and the delivery read what is committed: not before the first Resync is
        // reconciled. Order: recover, monitor, then anomalies and delivery.
        if monitor.reconciled() {
            // First: a close accepted while a pass of the last tick was mid-step is finished only
            // after that pass is (they share this thread), and before any pass starts more work.
            if let Err(e) = close.pass(&w, &*events_base) {
                eprintln!("makibad: close: {e}");
            }
            if let Err(e) = anomalies.pass(&w) {
                eprintln!("makibad: anomaly pass failed: {e}");
            }
            if let Err(e) = dispatch.pass(&w) {
                eprintln!("makibad: dispatch: {e}");
            }
            if let Err(e) = prepare.pass(&w) {
                eprintln!("makibad: prepare: {e}");
            }
            // No launch step runs until the start pass has looked for every launch's terminal.
            if !resumed {
                match resume::pass(&w, &*events_base) {
                    Ok(done) => resumed = done,
                    Err(e) => eprintln!("makibad: resume: {e}"),
                }
            }
            if resumed {
                if let Err(e) = launch.pass(&w, &*events_base) {
                    eprintln!("makibad: launch: {e}");
                }
                if let Err(e) = curator.pass(&w, &*events_base) {
                    eprintln!("makibad: curator: {e}");
                }
                if let Some(Err(e)) = secretary.as_mut().map(|s| s.pass(&w, &*events_base)) {
                    eprintln!("makibad: secretary: {e}");
                }
                // Before the dialogs: a launch they move on is looked at on a later tick.
                if let Err(e) = verify.pass(&w, &*events_base) {
                    eprintln!("makibad: verify: {e}");
                }
                if let Err(e) = dialogs.pass(&w, &*events_base) {
                    eprintln!("makibad: dialogs: {e}");
                }
            }
            if let Err(e) = prwatch.pass(&w) {
                eprintln!("makibad: pr watch: {e}");
            }
            if let Err(e) = triage.pass(&w) {
                eprintln!("makibad: triage: {e}");
            }
            if let Err(e) = delivery.pass(&w) {
                eprintln!("makibad: delivery: {e}");
            }
            if let Err(e) = delivery.wake_pass(&w, &*events_base) {
                eprintln!("makibad: wake: {e}");
            }
        }
        if Instant::now() >= next_beat {
            beat(&w)?;
            next_beat = Instant::now() + HEARTBEAT;
        }
    }

    // A stop: end the pollers (their `gh` children too), say so, release the lock, leave no pid behind.
    drop(prwatch);
    drop(triage);
    event(&w, "dispatcher_stopped", &info.name, &info.version).map_err(|e| e.to_string())?;
    w.conn()
        .execute("UPDATE dispatcher SET subscribed = 0 WHERE id = 1", [])
        .map_err(|e| e.to_string())?;
    lock.set_len(0).map_err(|e| e.to_string())?;
    drop(lock);
    Ok(())
}
