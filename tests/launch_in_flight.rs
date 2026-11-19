//! Issue 122: a launch that `makibad` dies in the middle of (`Base::launch` in flight) ends with
//! exactly one terminal. The daemon runs in this process on `Local` (as tests/launch_idempotent.rs).
//! The wrapper answers the first `launch` call by keeping its spec and ending the calling thread
//! (the daemon dies with the call in flight: the base has the request and has not started the
//! child). A second daemon then starts on the same folder; just before its own launch reaches the
//! base, the kept request is carried out (the delayed terminal appears), so the base refuses the
//! second one with a name that is taken. The launch is adopted: one terminal carries the spawn tag,
//! it carries a key that is still live, and the run ends `in_use`.

mod common;

use common::local::Local;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(30);
const TASK: &str = "t-in-flight";

/// `Local`, whose first `launch` is the one the daemon dies in.
#[derive(Clone)]
struct InFlight {
    local: Local,
    calls: Arc<AtomicUsize>,
    /// The request of the call the daemon died in, not yet carried out.
    kept: Arc<Mutex<Option<LaunchSpec>>>,
}

impl Base for InFlight {
    fn name(&self) -> &'static str {
        self.local.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.local.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            *self.kept.lock().unwrap() = Some(spec.clone());
            std::panic::resume_unwind(Box::new("makibad died in flight".to_string()));
        }
        let kept = self.kept.lock().unwrap().take();
        if let Some(late) = kept {
            self.local.launch(&late)?;
        }
        self.local.launch(spec)
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        self.local.list()
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
    base: InFlight,
    _tmp: tempfile::TempDir,
}

/// Ends every terminal when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

const SPAWN: &str = "spawn-in-flight";

fn org() -> Org {
    let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
    // SAFETY: this file has one test; nothing else runs a thread yet.
    unsafe { std::env::set_var("MAKIBA_FAKE_AGENT", agent) };
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let work = root.join(TASK);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("steps.json"),
        json!(["working", "hang"]).to_string(),
    )
    .unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: TASK,
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'document', 'fake',
                     'the brief', ?3, 'planned')",
            [TASK, SPAWN, &work.to_string_lossy()],
        )
        .unwrap();
    drop(w);
    Org {
        dir,
        base: InFlight {
            local: Local::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            kept: Arc::new(Mutex::new(None)),
        },
        _tmp: tmp,
    }
}

impl Org {
    fn daemon(&self) -> JoinHandle<Result<(), String>> {
        let (dir, base) = (self.dir.clone(), self.base.clone());
        std::thread::spawn(move || daemon::run(&dir, &|| Box::new(base.clone())))
    }

    fn one<T: rusqlite::types::FromSql>(&self, sql: &str) -> Option<T> {
        let conn = Connection::open_with_flags(
            self.dir.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;
        conn.query_row(sql, [], |r| r.get(0)).ok()
    }

    fn until(&self, what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + WAIT;
        while Instant::now() < until && !done() {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(done(), "timed out waiting for {what}");
    }
}

/// The token in the door's `Authorization` header of a launch request.
fn token(spec: &LaunchSpec) -> String {
    let config = spec
        .argv
        .iter()
        .find_map(|a| serde_json::from_str::<serde_json::Value>(a).ok())
        .expect("the argv carries the MCP configuration");
    let header = config["mcpServers"]["makiba"]["headers"]["Authorization"]
        .as_str()
        .unwrap();
    header.strip_prefix("Bearer ").unwrap().to_string()
}

#[test]
fn killed_while_the_launch_is_in_flight() {
    let org = org();
    let first = org.daemon();
    org.until("the first daemon to die in the launch call", || {
        first.is_finished()
    });
    let payload = first.join().expect_err("makibad ended without dying");
    assert_eq!(
        payload.downcast_ref::<String>().map(String::as_str),
        Some("makibad died in flight")
    );
    assert!(org.base.list().unwrap().is_empty(), "nothing is up yet");
    let late_key = token(org.base.kept.lock().unwrap().as_ref().unwrap());

    let _restarted = org.daemon();
    org.until("the launch to be verified", || {
        org.one::<String>("SELECT stage FROM run_launch").as_deref() == Some("verified")
    });
    let tagged = org
        .base
        .list()
        .unwrap()
        .iter()
        .filter(|t| t.tags.get("makiba.spawn").map(String::as_str) == Some(SPAWN))
        .count();
    assert_eq!(tagged, 1, "exactly one terminal carries the spawn tag");
    assert_eq!(
        org.one::<String>("SELECT status FROM runs").as_deref(),
        Some("in_use")
    );
    assert_eq!(org.base.calls.load(Ordering::SeqCst), 2);
    let hash = daemon::door::sha256_hex(&late_key);
    assert_eq!(
        org.one::<i64>(&format!(
            "SELECT count(*) FROM agents WHERE key_sha256 = '{hash}' AND revoked_at IS NULL"
        )),
        Some(1),
        "the key the adopted terminal carries is live"
    );
}
