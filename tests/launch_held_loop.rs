//! Issue 333: a loop of makibad held past the verify timeout does not fail a launch whose known
//! startup dialog is still on the screen (the incident of 2026-11-22T19:06Z).
//!
//! The screen is the incident's own (event 3068): Claude Code's dev-channel dialog, on the policy
//! list, cursor on the line to choose. From the first list of the base that shows it, every list
//! waits 5 s (the verify timeout is 2 s): makibad's loop is held before the dialog step answers.
//! The dialog is then answered, the curator starts its turn and the launch is verified.
//!
//! Turned around from the reproduction (commit 9ee5b22, tests/curator_launch_silent.rs,
//! `a_held_loop_fails_the_launch_before_the_dialog_step_answers`). The daemon runs in this
//! process on `Local` with the test agent, as tests/curator.rs.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, State, Terminal, TerminalRef,
};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const VERIFY_MS: u64 = 2000;
const LIMIT_MS: u64 = 12000;
const HOLD_MS: u64 = VERIFY_MS + 3000;
const WAIT: Duration = Duration::from_secs(40);

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

/// `Local`, whose lists, once one has seen the dev-channel dialog, wait until `hold` after it
/// (milliseconds): a loop of makibad held up before its dialog step answers.
#[derive(Clone)]
struct Held {
    local: Local,
    hold: Arc<AtomicU64>,
    /// When lists stop waiting, set at the first sighting.
    until: Arc<Mutex<Option<Instant>>>,
}

impl Base for Held {
    fn name(&self) -> &'static str {
        self.local.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.local.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.local.launch(spec)
    }
    /// Every pass that could press the dialog's key lists before it reads, so a hold here comes
    /// before any key. From the first list that sees the dialog, every list, on whatever thread,
    /// waits until `hold` after that sighting.
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        let list = self.local.list()?;
        let shown = list.iter().any(|t| {
            self.local
                .read(&t.id, 0)
                .is_ok_and(|s| s.iter().any(|l| l.contains("Loading development channels")))
        });
        let deadline = {
            let mut until = self.until.lock().unwrap();
            let ms = self.hold.load(Ordering::SeqCst);
            if shown && ms > 0 && until.is_none() {
                *until = Some(Instant::now() + Duration::from_millis(ms));
            }
            *until
        };
        if let Some(d) = deadline {
            std::thread::sleep(d.saturating_duration_since(Instant::now()));
        }
        Ok(list)
    }
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.local.get(t)
    }
    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.local.read(t, lines)
    }
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.local.keys(t, keys)
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        self.local.send(t, text, only_if_idle)
    }
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        self.local.end(t)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.local.events()
    }
    fn start(&self) -> Result<(), BaseError> {
        self.local.start()
    }
    fn worker_denies(&self) -> BaseDenies {
        self.local.worker_denies()
    }
}

struct Org {
    dir: PathBuf,
    knowledge: PathBuf,
    base: Held,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
    _held: makibad::Knowledge,
    _tmp: tempfile::TempDir,
    _env: MutexGuard<'static, ()>,
}

/// The daemon is stopped and every terminal ended when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(d) = self.daemon.take() {
            let _ = d.join();
        }
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

impl Org {
    fn curators(&self) -> Vec<Terminal> {
        self.base
            .list()
            .unwrap()
            .into_iter()
            .filter(|t| t.tags.get("makiba.role").map(String::as_str) == Some("curator"))
            .collect()
    }

    fn count(&self, sql: &str) -> i64 {
        db(&self.dir).query_row(sql, [], |r| r.get(0)).unwrap()
    }
}

fn db(dir: &Path) -> Connection {
    Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn until(what: &str, f: impl Fn() -> bool) {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for: {what}");
}

/// A daemon over five raw notes whose curator runs `steps`, and one close recorded; every list is
/// held for `hold_ms` from the first that shows the dev-channel dialog.
fn held_org(steps: Value, hold_ms: u64) -> Org {
    let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (dir, knowledge) = (root.join("state"), root.join("knowledge"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(knowledge.join("raw")).unwrap();
    for i in 0..5 {
        std::fs::write(knowledge.join(format!("raw/n{i}.md")), "a note").unwrap();
    }
    std::fs::write(knowledge.join("steps.json"), steps.to_string()).unwrap();
    drop(StateWriter::open(&dir.join("state.db")).unwrap());
    let held = makibad::use_knowledge(&knowledge);
    // SAFETY: the lock above is held for the whole test.
    unsafe {
        std::env::set_var(
            "MAKIBA_FAKE_AGENT",
            json!([AGENT, "record", "steps:steps.json"]).to_string(),
        );
        std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        std::env::set_var("MAKIBA_CURATOR_TIMEOUT_MS", LIMIT_MS.to_string());
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", VERIFY_MS.to_string());
    }
    let base = Held {
        local: Local::new(),
        hold: Arc::new(AtomicU64::new(hold_ms)),
        until: Arc::new(Mutex::new(None)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (dir, base, stop) = (dir.clone(), base.clone(), Arc::clone(&stop));
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), stop))
    };
    // A close, as the door writes it; the daemon writes too, so a busy database is tried again.
    let close = || -> Result<(), makiba::db::Error> {
        let w = StateWriter::open(&dir.join("state.db"))?;
        w.transaction(|w| {
            w.upsert_run(&RunUpsert {
                task_id: "t1",
                project_slug: "p",
                status: Some("completed"),
                ..Default::default()
            })?;
            w.append_event(&NewEvent {
                kind: "run_closed",
                actor: Some("person"),
                payload: Some(&json!({"task_id": "t1"})),
                run_task_id: Some("t1"),
                ..Default::default()
            })
            .map(|_| ())
        })
    };
    until("a close recorded", || close().is_ok());
    Org {
        dir,
        knowledge,
        base,
        stop,
        daemon: Some(daemon),
        _held: held,
        _tmp: tmp,
        _env: env,
    }
}

/// The dev-channel dialog as event 3068 recorded it, then a turn.
fn dev_channel_then_work() -> Value {
    let menu = json!({
        "above": [
            "WARNING: Loading development channels",
            "",
            "Channels: server:makiba-channel",
            "",
        ],
        "options": ["1. I am using this for local development", "2. Exit"],
        "at": 0,
        "indent": "  ",
        "below": ["", "Enter to confirm \u{b7} Esc to cancel"],
    });
    json!([format!("menu:{menu}"), "working", "hang"])
}

#[test]
fn a_held_loop_does_not_fail_the_launch_the_dialog_is_answered_and_verified() {
    let org = held_org(dev_channel_then_work(), HOLD_MS);
    until("the launch verified", || {
        org.count("SELECT count(*) FROM run_launch WHERE role = 'curator' AND stage = 'verified'")
            == 1
    });
    assert_eq!(
        org.count("SELECT count(*) FROM run_launch WHERE role = 'curator' AND stage = 'failed'"),
        0
    );
    assert_eq!(
        org.count("SELECT count(*) FROM events WHERE kind = 'worker_spawn_failed'"),
        0
    );
    // The loop was held: a list saw the dialog and every list waited after it.
    assert!(org.base.until.lock().unwrap().is_some());

    let t = org.curators().remove(0);
    let spawn = t.tags["makiba.spawn"].clone();
    // The launch outlived its verify timeout by the wall clock.
    assert_eq!(
        org.count(&format!(
            "SELECT (julianday(v.occurred_at) - julianday(s.occurred_at)) * 86400000 > {VERIFY_MS}
             FROM events v, events s
             WHERE v.kind = 'worker_spawn_verified' AND s.kind = 'worker_spawned'
               AND json_extract(v.payload_json, '$.spawn_id') = '{spawn}'
               AND json_extract(s.payload_json, '$.spawn_id') = '{spawn}'"
        )),
        1
    );
    // The dialog was answered on its line to choose.
    let record = std::fs::read_to_string(org.knowledge.join("record")).unwrap_or_default();
    assert!(record.lines().any(|l| l == "chose 1"), "{record}");
    // Verified by a working status or a turn of this launch's own terminal.
    assert_eq!(
        org.count(&format!(
            "SELECT count(*) FROM events v JOIN events e
               ON e.id = json_extract(v.payload_json, '$.event_id')
             WHERE v.kind = 'worker_spawn_verified'
               AND json_extract(v.payload_json, '$.spawn_id') = '{spawn}'
               AND json_extract(e.payload_json, '$.spawn_id') = '{spawn}'
               AND (e.kind = 'terminal_turn' OR (e.kind = 'terminal_status'
                    AND json_extract(e.payload_json, '$.state') = 'working'))"
        )),
        1
    );
    assert!(t.alive);
    assert_eq!(t.status.map(|s| s.state), Some(State::Working));
}
