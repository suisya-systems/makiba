//! M3-7 (Issue 30): a launch survives the dispatcher dying at any step. The daemon runs in this
//! process on `Local` (as tests/spawn_verified.rs), wrapped so that every `Base::launch` call is
//! counted; the test keeps the terminals, so they outlive a daemon that dies. The step at which a
//! daemon must die is written into `<state>/makibad.die-at` before it starts; a debug build of
//! `makibad` reads it at start and its thread ends right after that step with the payload
//! `makibad died at <step>`. A second daemon is then started on the same folder and terminals.
//!
//! One document launch per test: its run is `queued`, its row `planned`, its workplace a folder
//! that holds the test agent's `steps.json`; the test agent reports `working` and waits.

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
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const WAIT: Duration = Duration::from_secs(30);

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets it, once.
        unsafe { std::env::set_var("MAKIBA_FAKE_AGENT", agent) };
    });
}

/// `Local`, counting the calls of `launch`.
#[derive(Clone)]
struct Counted {
    local: Local,
    launches: Arc<AtomicUsize>,
}

impl Base for Counted {
    fn name(&self) -> &'static str {
        self.local.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.local.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.launches.fetch_add(1, Ordering::SeqCst);
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
    spawn: String,
    base: Counted,
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

fn org(task: &str) -> Org {
    setup();
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
        // Not `in_use`: an `in_use` run with no terminal is recorded as lost (M2-5).
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    let spawn = format!("spawn-{task}");
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'document', 'fake',
                     'the brief', ?3, 'planned')",
            [task, &spawn, &work.to_string_lossy()],
        )
        .unwrap();
    drop(w);
    Org {
        dir,
        spawn,
        base: Counted {
            local: Local::new(),
            launches: Arc::new(AtomicUsize::new(0)),
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

    fn stage(&self) -> Option<String> {
        self.one("SELECT stage FROM run_launch")
    }

    /// The terminals that carry the launch's `makiba.spawn` tag.
    fn tagged(&self) -> usize {
        self.base
            .list()
            .unwrap()
            .iter()
            .filter(|t| t.tags.get("makiba.spawn") == Some(&self.spawn))
            .count()
    }

    /// Start a daemon that dies at `step`, and wait for it to die there.
    fn die_at(&self, step: &str) {
        let file = self.dir.join("makibad.die-at");
        std::fs::write(&file, step).unwrap();
        let daemon = self.daemon();
        let until = Instant::now() + WAIT;
        while !daemon.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            daemon.is_finished(),
            "makibad did not die at {step} (stage {:?})",
            self.stage()
        );
        let payload = daemon.join().expect_err("makibad ended without dying");
        assert_eq!(
            payload.downcast_ref::<String>().map(String::as_str),
            Some(format!("makibad died at {step}").as_str())
        );
        std::fs::remove_file(&file).unwrap();
    }

    /// Wait for the launch to reach `stage`.
    fn reaches(&self, stage: &str) {
        let until = Instant::now() + WAIT;
        while Instant::now() < until && self.stage().as_deref() != Some(stage) {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(self.stage().as_deref(), Some(stage));
    }
}

/// Kill at `step`, restart, and the launch ends with exactly one terminal carrying its spawn tag
/// and its run `in_use`.
fn survives(step: &str, stage_left: &str) -> Org {
    let org = org(&format!("t-{}", step.replace('_', "-")));
    org.die_at(step);
    assert_eq!(org.stage().as_deref(), Some(stage_left));
    let _restarted = org.daemon();
    org.reaches("verified");
    assert_eq!(
        org.tagged(),
        1,
        "exactly one terminal carries the spawn tag"
    );
    assert_eq!(
        org.one::<String>("SELECT status FROM runs").as_deref(),
        Some("in_use")
    );
    assert_eq!(
        org.base.launches.load(Ordering::SeqCst),
        1,
        "the base was asked to launch once"
    );
    org
}

#[test]
fn killed_after_the_run_was_taken() {
    survives("run_taken", "planned");
}

#[test]
fn killed_after_worktree_ready() {
    survives("worktree_ready", "worktree_ready");
}

#[test]
fn killed_after_settings_ready() {
    survives("settings_ready", "settings_ready");
}

#[test]
fn killed_after_launched() {
    survives("launched", "launched");
}

#[test]
fn killed_after_dialog_passed() {
    survives("dialog_passed", "dialog_passed");
}

/// Killed after `Base::launch` returned and before `launched` was written: on restart the
/// terminal is adopted, not launched again, and no second key is issued for it.
#[test]
fn killed_after_the_launch_returned_before_its_stage_was_written() {
    let org = survives("launch_returned", "settings_ready");
    assert_eq!(
        org.one::<i64>(&format!(
            "SELECT count(*) FROM agents WHERE spawn_id = '{}'",
            org.spawn
        )),
        Some(1),
        "one key was issued for the launch"
    );
    assert_eq!(
        org.one::<i64>("SELECT count(*) FROM events WHERE kind = 'worker_spawned'"),
        Some(1)
    );
}
