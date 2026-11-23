//! Issue 383: `makiba status` run by the person shows `makibad`'s merge approval mode, the one the
//! gate reads, and not the mode of the shell that runs it; the shell's value is shown beside it,
//! marked as the shell's.
//!
//! `makibad` records its mode in the payload of `dispatcher_started`. The person's status reads it
//! from the newest such event of the running `makibad` (its pid, written at or after its start),
//! only while the heartbeat is fresh; otherwise the mode is unknown (null). The reading process's
//! own value is `merge_approval_mode_here`.
//!
//! The first test runs `makibad` in this process on the stand-in base (no door); the others seed
//! `state.db` by hand. Every `makiba` child is given `MAKIBA_MERGE_APPROVAL_MODE` or has it removed,
//! so the test process's own value never reaches it.

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::db::writer::{NewEvent, StateWriter};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const MODE_ENV: &str = "MAKIBA_MERGE_APPROVAL_MODE";

/// Held while this process's environment is changed.
static ENV: Mutex<()> = Mutex::new(());

/// The person's `makiba` on the state folder `dir`, with no door, its mode variable set to `mode`
/// or removed, and `stdin` as its input.
fn cli(dir: &Path, args: &[&str], mode: Option<&str>, stdin: &str) -> Output {
    let mut c = Command::new(MAKIBA);
    c.args(args)
        .env("MAKIBA_STATE_DIR", dir)
        .env("MAKIBA_PERSON_KEY", dir.join("no-person.key"))
        .env("MAKIBA_GATE_KEY", dir.join("no-gate.key"))
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE", "stub")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match mode {
        Some(m) => c.env(MODE_ENV, m),
        None => c.env_remove(MODE_ENV),
    };
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn status_json(dir: &Path, mode: Option<&str>) -> Value {
    let out = cli(dir, &["status", "--json"], mode, "");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "status --json printed no JSON ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn status_text(dir: &Path, mode: Option<&str>) -> String {
    let out = cli(dir, &["status"], mode, "");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_line(text: &str, line: &str) {
    assert!(
        text.lines().any(|l| l == line),
        "no line {line:?} in:\n{text}"
    );
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

/// A `dispatcher_started` of `pid` recording `mode`, occurred `ago`.
fn started(w: &StateWriter, dir: &Path, pid: i64, mode: &str, ago: &str) {
    let when = at(dir, ago);
    let payload = json!({"pid": pid, "base": "stub", "base_version": "0",
                         "merge_approval_mode": mode});
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

/// 1. `makibad` runs under `acceptance`; the shell says `human` or nothing. The status shows
///    `acceptance` as `makibad`'s and the shell's value beside it.
#[test]
fn status_shows_makibads_mode_not_the_shells() {
    let (_tmp, dir, _w) = org();
    let _env = ENV.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: under the lock; the other tests of this file give every child its own value.
    unsafe { std::env::set_var(MODE_ENV, "acceptance") };
    let _d = Daemon::start(&dir);
    let db = Connection::open(dir.join("state.db")).unwrap();
    let payload = || -> Option<String> {
        db.query_row(
            "SELECT payload_json FROM events WHERE kind = 'dispatcher_started'
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
    };
    wait("makibad to start", || payload().is_some());
    let p: Value = serde_json::from_str(&payload().unwrap()).unwrap();
    assert_eq!(
        p["merge_approval_mode"],
        json!("acceptance"),
        "dispatcher_started does not record makibad's mode: {p}"
    );

    let v = status_json(&dir, Some("human"));
    assert_eq!(v["merge_approval_mode"], json!("acceptance"), "{v}");
    assert_eq!(v["merge_approval_mode_here"], json!("human"), "{v}");

    let v = status_json(&dir, None);
    assert_eq!(v["merge_approval_mode"], json!("acceptance"), "{v}");
    assert_eq!(v["merge_approval_mode_here"], json!("human"), "{v}");

    assert_line(
        &status_text(&dir, Some("human")),
        "merge approval mode: acceptance (makibad's); this shell says human, which the gate does not read",
    );
    assert_line(
        &status_text(&dir, Some("acceptance")),
        "merge approval mode: acceptance (makibad's)",
    );
}

/// 2. A `makibad` whose heartbeat is old is not running: no mode is in effect, whatever it
///    recorded; the shell's value is shown as the one `makiba up` passes on.
#[test]
fn a_stopped_makibad_has_no_mode() {
    let (_tmp, dir, w) = org();
    dispatcher(&dir, 4242, "-20 minutes", "-10 minutes");
    started(&w, &dir, 4242, "acceptance", "-15 minutes");

    let v = status_json(&dir, Some("human"));
    assert_eq!(v["merge_approval_mode"], Value::Null, "{v}");
    assert_eq!(v["merge_approval_mode_here"], json!("human"), "{v}");
    assert_line(
        &status_text(&dir, Some("human")),
        "merge approval mode: unknown (makibad is not running); this shell says human, which makiba up passes on",
    );
}

/// 3. A record of an earlier `makibad` (the same pid before this start, or another pid) is not
///    the running one's mode.
#[test]
fn an_earlier_makibads_record_is_not_read() {
    let (_tmp, dir, w) = org();
    dispatcher(&dir, 4242, "-1 minutes", "+0 seconds");
    started(&w, &dir, 4242, "acceptance", "-5 minutes");
    started(&w, &dir, 9999, "acceptance", "+0 seconds");

    let v = status_json(&dir, Some("human"));
    assert_eq!(v["merge_approval_mode"], Value::Null, "{v}");
    assert_eq!(v["merge_approval_mode_here"], json!("human"), "{v}");
    assert_line(
        &status_text(&dir, Some("human")),
        "merge approval mode: unknown (makibad has not recorded it); this shell says human, which the gate does not read",
    );
}

/// 4. `range approve` shows `makibad`'s mode before it asks, and the shell's when it differs.
#[test]
fn range_approve_shows_makibads_mode() {
    let (_tmp, dir, w) = org();
    dispatcher(&dir, 4242, "-1 minutes", "+0 seconds");
    started(&w, &dir, 4242, "acceptance", "+0 seconds");
    let project = w
        .ensure_project("p", None, Some("https://github.com/o/p"))
        .unwrap();
    let id: i64 = {
        let c = Connection::open(dir.join("state.db")).unwrap();
        c.execute(
            "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                 proposed_by)
             VALUES (?1, '[7]', 1, 1, 'Do it.', 'secretary')",
            [project],
        )
        .unwrap();
        c.last_insert_rowid()
    };
    w.append_event(&NewEvent {
        kind: "range_read",
        actor: Some("makibad"),
        payload: Some(&json!({"range_id": id, "complete": 1})),
        ..Default::default()
    })
    .unwrap();

    let out = cli(
        &dir,
        &["range", "approve", &id.to_string()],
        Some("human"),
        "n\n",
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_line(
        &text,
        "Merge approval mode: acceptance (makibad's); this shell says human",
    );
    assert!(
        !text.contains("this shell's; makibad records its own"),
        "the preview still shows the shell's mode as the mode:\n{text}"
    );
    assert!(
        !out.status.success(),
        "an answer n approved the range:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
