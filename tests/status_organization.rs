//! Issue 411: `makiba status --json` names the organization: the base session name
//! (`MAKIBA_BASE_SESSION`, default `makiba`).
//!
//! `makibad` records its session as `base_session` in the payload of `dispatcher_started` and
//! `dispatcher_stopped`. The person's status reads it from the newest `dispatcher_started` of the
//! pid in `dispatcher`, at or after its `started_at`, whether or not the heartbeat is fresh (so the
//! name stays readable when `makibad` is dead); null when there is none, when that event has no
//! `base_session`, or when the newest start predates the dispatcher row's start (a restart that
//! has not subscribed, or failed). Through the door (`Reader::Makibad`) it is `makibad`'s own
//! environment.
//!
//! The first test runs `makibad` in this process on the stand-in base (no door); the others seed
//! `state.db` by hand. Every `makiba` child is given its own `MAKIBA_BASE_SESSION`, so the test
//! process's value never reaches it; this process's environment is changed under `ENV`.

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::db::writer::{NewEvent, StateWriter};
use makiba::status_local::{Reader, read_as};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const SESSION_ENV: &str = "MAKIBA_BASE_SESSION";

/// Held while this process's environment is changed.
static ENV: Mutex<()> = Mutex::new(());

/// The person's `makiba` on the state folder `dir`, with no door, its session variable `session`.
fn cli(dir: &Path, args: &[&str], session: &str) -> Output {
    Command::new(MAKIBA)
        .args(args)
        .env("MAKIBA_STATE_DIR", dir)
        .env("MAKIBA_PERSON_KEY", dir.join("no-person.key"))
        .env("MAKIBA_GATE_KEY", dir.join("no-gate.key"))
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE", "stub")
        .env(SESSION_ENV, session)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn status_json(dir: &Path, session: &str) -> Value {
    let out = cli(dir, &["status", "--json"], session);
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "status --json printed no JSON ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// The `organization` field, which must be present.
fn organization(v: &Value) -> &Value {
    v.get("organization")
        .unwrap_or_else(|| panic!("status has no field organization: {v}"))
}

/// A fresh state folder and its `state.db`, made with the current schema.
fn org() -> (tempfile::TempDir, PathBuf, StateWriter) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    (tmp, dir, w)
}

/// The `dispatcher` row: `pid`, started `started_ago` and last beating `beat_ago` (SQLite time
/// modifiers such as `-10 minutes`), subscribed.
fn dispatcher(dir: &Path, pid: i64, started_ago: &str, beat_ago: &str) {
    let c = Connection::open(dir.join("state.db")).unwrap();
    c.execute(
        "INSERT INTO dispatcher (id, pid, started_at, heartbeat_at, base, base_version, subscribed)
         VALUES (1, ?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2),
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3), 'stub', '0', 1)",
        rusqlite::params![pid, started_ago, beat_ago],
    )
    .unwrap();
}

/// The time `ago` before now, in the ledger's format.
fn at(dir: &Path, ago: &str) -> String {
    let c = Connection::open(dir.join("state.db")).unwrap();
    c.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
        [ago],
        |r| r.get(0),
    )
    .unwrap()
}

/// A `dispatcher_started` of `pid` recording `session` (none: an earlier build's payload),
/// occurred `ago`.
fn started(w: &StateWriter, dir: &Path, pid: i64, session: Option<&str>, ago: &str) {
    let when = at(dir, ago);
    let mut payload = json!({"pid": pid, "base": "stub", "base_version": "0",
                             "merge_approval_mode": "human"});
    if let Some(s) = session {
        payload["base_session"] = json!(s);
    }
    w.append_event(&NewEvent {
        kind: "dispatcher_started",
        actor: Some("makibad"),
        payload: Some(&payload),
        occurred_at: Some(&when),
        ..Default::default()
    })
    .unwrap();
}

/// `makibad` in this process, stopped when dropped.
struct Daemon {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl Daemon {
    fn start(dir: &Path) -> Daemon {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (dir, stop) = (dir.to_path_buf(), Arc::clone(&stop));
            std::thread::spawn(move || {
                makibad::run_until(&dir, &|| Box::new(Stub::new()) as Box<dyn Base>, stop)
            })
        };
        Daemon {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// The payload of the newest event of `kind`, if any.
fn newest_payload(db: &Connection, kind: &str) -> Option<Value> {
    db.query_row(
        "SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1",
        [kind],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .map(|p| serde_json::from_str(&p).unwrap())
}

/// T1. `makibad` runs with the session `org-live`; the shell says `shell-other`. Both its events
/// record `org-live`, and the status names `makibad`'s session, not the shell's.
#[test]
fn status_names_makibads_session_not_the_shells() {
    let (_tmp, dir, _w) = org();
    let _env = ENV.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: under the lock; every child of this file is given its own value.
    unsafe { std::env::set_var(SESSION_ENV, "org-live") };
    let d = Daemon::start(&dir);
    let db = Connection::open(dir.join("state.db")).unwrap();
    wait("makibad to start", || {
        newest_payload(&db, "dispatcher_started").is_some()
    });
    let p = newest_payload(&db, "dispatcher_started").unwrap();
    assert_eq!(
        p["base_session"],
        json!("org-live"),
        "dispatcher_started does not record makibad's session: {p}"
    );

    let v = status_json(&dir, "shell-other");
    assert_eq!(organization(&v), &json!("org-live"), "{v}");

    drop(d);
    let p = newest_payload(&db, "dispatcher_stopped")
        .expect("makibad stopped without a dispatcher_stopped");
    assert_eq!(
        p["base_session"],
        json!("org-live"),
        "dispatcher_stopped does not record makibad's session: {p}"
    );
    // SAFETY: under the lock.
    unsafe { std::env::remove_var(SESSION_ENV) };
}

/// T2. A `makibad` whose heartbeat is old still names the organization: the newest start of its
/// pid after its row's start.
#[test]
fn a_stopped_makibad_still_names_the_organization() {
    let (_tmp, dir, w) = org();
    dispatcher(&dir, 4242, "-20 minutes", "-10 minutes");
    started(&w, &dir, 4242, Some("org-a"), "-19 minutes");
    started(&w, &dir, 4242, Some("org-b"), "-18 minutes");

    let v = status_json(&dir, "shell-other");
    assert_eq!(v["makibad"]["running"], json!(false), "{v}");
    assert_eq!(organization(&v), &json!("org-b"), "{v}");
}

/// T3. The newest start has no `base_session` (an earlier build): an older record is not read.
#[test]
fn a_newest_start_without_a_session_names_none() {
    let (_tmp, dir, w) = org();
    dispatcher(&dir, 4242, "-20 minutes", "+0 seconds");
    started(&w, &dir, 4242, Some("org-a"), "-19 minutes");
    started(&w, &dir, 4242, None, "-18 minutes");

    let v = status_json(&dir, "shell-other");
    assert_eq!(organization(&v), &Value::Null, "{v}");
}

/// T4. A fresh organization, with no `dispatcher` row and no event, names none.
#[test]
fn a_fresh_organization_names_none() {
    let (_tmp, dir, _w) = org();
    let v = status_json(&dir, "shell-other");
    assert_eq!(organization(&v), &Value::Null, "{v}");
}

/// T5. Through the door, `makibad` names its own environment's session, `makiba` when unset.
#[test]
fn through_the_door_makibads_environment_is_read() {
    let (_tmp, dir, _w) = org();
    let path = dir.join("state.db");
    let _env = ENV.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: under the lock; every child of this file is given its own value.
    unsafe { std::env::set_var(SESSION_ENV, "door-org") };
    let v = read_as(&path, Reader::Makibad).unwrap();
    assert_eq!(organization(&v), &json!("door-org"), "{v}");
    // SAFETY: under the lock.
    unsafe { std::env::remove_var(SESSION_ENV) };
    let v = read_as(&path, Reader::Makibad).unwrap();
    assert_eq!(organization(&v), &json!("makiba"), "{v}");
}

/// T6. A restart that has not recorded its start (failed, or not yet subscribed) names none: the
/// only start predates the row's `started_at`, so a session renamed by the restart is never shown
/// stale.
#[test]
fn a_start_before_the_rows_start_is_not_read() {
    let (_tmp, dir, w) = org();
    dispatcher(&dir, 4242, "-1 minutes", "-1 minutes");
    started(&w, &dir, 4242, Some("org-old"), "-5 minutes");

    let v = status_json(&dir, "shell-other");
    assert_eq!(organization(&v), &Value::Null, "{v}");
}
