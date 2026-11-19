//! Issue 131: after a launch adopts a terminal that appeared late, exactly one key of the spawn is
//! live and it is the one the adopted terminal was started with. The setup is that of
//! tests/launch_in_flight.rs (Issue 122): the daemon runs in this process on `Local`, the first
//! `launch` call keeps its request and ends the calling thread (the daemon dies with the call in
//! flight, its key already issued), and a second daemon starts on the same folder. Two cases, by
//! what the second daemon's own launch does:
//!
//! - `late`: the kept request is carried out just before the second daemon's own reaches the
//!   base, which refuses it (`name_taken`). The adopted terminal carries the dead daemon's key.
//! - `lost_reply`: the kept request is never carried out; the second daemon's launch starts the
//!   terminal and the reply is lost (an error). The adopted terminal carries the second daemon's
//!   key, and the dead daemon's key was never delivered.
//!
//! The key a terminal carries is read from its argv, as the child would read it.

mod common;

use common::local::Local;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, Detail, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(30);
const SPAWN: &str = "spawn-adopted-key";

#[derive(Clone, Copy)]
enum Second {
    /// The dead daemon's request is carried out first; the second launch is refused.
    Late,
    /// Only the second launch starts a terminal, and it returns an error.
    LostReply,
}

/// `Local`, whose first `launch` is the one the daemon dies in.
#[derive(Clone)]
struct InFlight {
    local: Local,
    second: Second,
    calls: Arc<AtomicUsize>,
    /// The request of the call the daemon died in.
    kept: Arc<Mutex<Option<LaunchSpec>>>,
    /// The request of the terminal that was started, whichever call started it.
    started: Arc<Mutex<Option<LaunchSpec>>>,
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
        match self.second {
            Second::Late => {
                let late = self.kept.lock().unwrap().take().unwrap();
                self.local.launch(&late)?;
                *self.started.lock().unwrap() = Some(late);
                self.local.launch(spec)
            }
            Second::LostReply => {
                self.local.launch(spec)?;
                *self.started.lock().unwrap() = Some(spec.clone());
                Err(BaseError::Unavailable(Detail {
                    kind: "lost".into(),
                    message: "the reply was lost".into(),
                    hint: None,
                }))
            }
        }
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

fn org(second: Second, task: &str) -> Org {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets it, once.
        unsafe { std::env::set_var("MAKIBA_FAKE_AGENT", agent) };
    });
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let work = root.join(task);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("steps.json"),
        json!(["working", "hang"]).to_string(),
    )
    .unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: task,
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
            [task, SPAWN, &work.to_string_lossy()],
        )
        .unwrap();
    drop(w);
    Org {
        dir,
        base: InFlight {
            local: Local::new(),
            second,
            calls: Arc::new(AtomicUsize::new(0)),
            kept: Arc::new(Mutex::new(None)),
            started: Arc::new(Mutex::new(None)),
        },
        _tmp: tmp,
    }
}

impl Org {
    fn daemon(&self) -> JoinHandle<Result<(), String>> {
        let (dir, base) = (self.dir.clone(), self.base.clone());
        std::thread::spawn(move || daemon::run(&dir, &|| Box::new(base.clone())))
    }

    fn db(&self) -> Option<Connection> {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
    }

    fn until(&self, what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + WAIT;
        while Instant::now() < until && !done() {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(done(), "timed out waiting for {what}");
    }

    /// The hashes of the spawn's keys that are not revoked.
    fn live(&self) -> Vec<String> {
        let Some(db) = self.db() else { return vec![] };
        let Ok(mut q) = db.prepare(
            "SELECT key_sha256 FROM agents WHERE spawn_id = ?1 AND revoked_at IS NULL
             ORDER BY id",
        ) else {
            return vec![];
        };
        q.query_map([SPAWN], |r| r.get(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    fn stage(&self) -> Option<String> {
        self.db()?
            .query_row("SELECT stage FROM run_launch", [], |r| r.get(0))
            .ok()
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

/// Runs both daemons and checks the keys; returns the dead daemon's request and the started one.
fn run(second: Second, task: &str) -> (LaunchSpec, LaunchSpec) {
    let org = org(second, task);
    let first = org.daemon();
    org.until("the first daemon to die in the launch call", || {
        first.is_finished()
    });
    first.join().expect_err("makibad ended without dying");
    assert!(org.base.list().unwrap().is_empty(), "nothing is up yet");
    let dead = org.base.kept.lock().unwrap().clone().unwrap();

    let _restarted = org.daemon();
    org.until("the launch to be verified", || {
        org.stage().as_deref() == Some("verified")
    });
    let tagged = org
        .base
        .list()
        .unwrap()
        .iter()
        .filter(|t| t.tags.get("makiba.spawn").map(String::as_str) == Some(SPAWN))
        .count();
    assert_eq!(tagged, 1, "exactly one terminal carries the spawn tag");
    let started = org.base.started.lock().unwrap().clone().unwrap();
    let carried = daemon::door::sha256_hex(&token(&started));
    assert_eq!(
        org.live(),
        [carried],
        "one key of the spawn is live: the one the adopted terminal carries"
    );
    (dead, started)
}

#[test]
fn late_terminal_keeps_the_dead_daemons_key_only() {
    let (dead, started) = run(Second::Late, "t-late");
    assert_eq!(
        token(&dead),
        token(&started),
        "the late terminal is the dead daemon's"
    );
}

#[test]
fn lost_reply_keeps_the_second_daemons_key_only() {
    let (dead, started) = run(Second::LostReply, "t-lost-reply");
    assert_ne!(
        token(&dead),
        token(&started),
        "the dead daemon's key was never started"
    );
}
