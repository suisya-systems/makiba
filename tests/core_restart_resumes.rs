//! Issue 579: a core restart under makiba suspends and resumes the runs instead of failing each
//! one. The base's core, on a stop and start, replaces every kept terminal's program with the
//! machine's shell (#560 reads such a terminal as gone). When makibad's view is rebuilt from a
//! snapshot in which no terminal of the organization still runs its program, at least two do not,
//! and at least one of those is newly lost, the core restarted: the runs at work are suspended and
//! resumed with new launches, as `makiba up` does, and none fails. One lost program while another
//! of the organization still runs fails its run as #560 makes it; so does a lone worker's restart
//! (it cannot be told from that worker's loss, a known limit asserted here so a change is seen).
//!
//! A stand-in core listens on a Unix socket in a temporary folder and answers makiba's real base
//! (`makiba::base::connect_as`), with the shapes of #560's test (tests/kept_terminal_replaced.rs):
//! a replaced terminal stays `live` with its labels and the shell's argv. Its restart drops every
//! connection, turns every live terminal into the shell, starts `seq` again and sends no notice.
//! makibad runs in this process with no secretary; the test writes `state.db` only before a
//! daemon starts, then reads. Every wait is a poll with a bound.

mod common;

use common::makibad;
use makiba::base::version::PINNED;
use makiba::base::{Base, LaunchSpec, connect_as};
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
const SHELL: &str = "/usr/bin/zsh";
const TASKS: [&str; 2] = ["t1", "t2"];

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

fn first_spawn(task: &str) -> String {
    format!("spawn-{task}")
}

// ---------------------------------------------------------------------------------------------
// The stand-in core.

#[derive(Default)]
struct World {
    /// Every terminal as the base reports it, finished ones until removed.
    terms: Vec<Value>,
    next: u64,
    pid: i64,
    seq: u64,
    /// The subscribers' connections.
    conns: Vec<UnixStream>,
    /// Down: a subscription is closed at once.
    down: bool,
    /// A terminal launched now reports `idle`, not `working`: its launch passes the startup
    /// dialogs (`dialog_passed`) and is not verified.
    idle: bool,
    /// The addresses `terminal.kill` or `terminal.rm` was called for.
    ended: Vec<String>,
}

impl World {
    fn find(&mut self, address: &str) -> Option<&mut Value> {
        self.terms.iter_mut().find(|t| t["address"] == address)
    }

    /// An `event` notice with the next `seq`, to every subscriber.
    fn emit(&mut self, kind: &str, address: &str, data: Value) {
        self.seq += 1;
        let msg = json!({"jsonrpc": "2.0", "method": "event",
            "params": {"seq": self.seq, "type": kind, "terminal": address, "data": data}});
        self.conns.retain_mut(|c| writeln!(c, "{msg}").is_ok());
    }

    fn new_pid(&mut self) -> i64 {
        self.pid += 1;
        100_000 + self.pid
    }

    /// The terminal at `address` now runs the machine's shell, as the base leaves a kept
    /// terminal whose program was lost: still `live`, its labels unchanged.
    fn replace(&mut self, address: &str) {
        let pid = self.new_pid();
        let t = self.find(address).expect("a terminal to replace");
        t["argv"] = json!([SHELL]);
        t["foreground"] = json!({"argv": [SHELL], "cwd": t["cwd"], "name": "zsh", "pid": pid});
        t["pid"] = json!(pid);
        t["status"] = Value::Null;
        t["status_ms"] = Value::Null;
        t["title"] = json!("user@host:folder");
    }

    fn drop_connections(&mut self) {
        for c in self.conns.drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn not_found(id: &str) -> Value {
    json!({"code": -32000, "message": format!("no terminal {id}"), "data": {"kind": "not_found"}})
}

struct Core {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    world: Arc<Mutex<World>>,
}

/// What the core does with one request other than `events.subscribe`.
fn handle(shared: &Arc<Mutex<World>>, method: &str, p: &Value) -> Result<Value, Value> {
    let mut w = shared.lock().unwrap();
    let id = p["terminal"].as_str().unwrap_or_default().to_string();
    match method {
        "hello" => Ok(json!({"version": PINNED, "generation": 1})),
        "terminal.run" => {
            w.next += 1;
            let address = format!("t{}", w.next);
            let pid = w.new_pid();
            let argv = p["argv"].clone();
            let cwd = p["cwd"].clone();
            let name0 = argv[0]
                .as_str()
                .and_then(|a| Path::new(a).file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let t = json!({
                "address": address, "argv": argv, "cwd": cwd, "labels": p["labels"],
                "name": p["name"], "state": "live", "reason": null, "status": null,
                "status_ms": null, "exit_code": null, "signal": null, "finished_ms": null,
                "keep": p["keep"], "reported": false, "pid": pid, "turn": 0, "title": "",
                "created_ms": now_ms(), "screen": "exact", "cols": 200, "rows": 120,
                "foreground": {"argv": argv, "cwd": cwd, "name": name0, "pid": pid},
            });
            w.terms.push(t.clone());
            w.emit("created", &address, t.clone());
            // The program starts and reports `working`, as an agent does (what verifies a launch),
            // or `idle` while the world says so.
            let state = if w.idle { "idle" } else { "working" };
            let (later, addr) = (Arc::clone(shared), address.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                let mut w = later.lock().unwrap();
                let status = json!({"state": state});
                let Some(t) = w.find(&addr) else { return };
                if t["state"] != "live" || t["argv"] == json!([SHELL]) {
                    return;
                }
                t["status"] = status.clone();
                t["status_ms"] = json!(now_ms());
                t["reported"] = json!(true);
                w.emit("status", &addr, json!({"status": status}));
            });
            Ok(t)
        }
        "terminal.list" => Ok(json!({"terminals": w.terms, "hidden_finished": 0, "machines": []})),
        "terminal.get" => w.find(&id).cloned().ok_or_else(|| not_found(&id)),
        "terminal.read" => match w.find(&id) {
            Some(_) => Ok(json!({"text": ""})),
            None => Err(not_found(&id)),
        },
        "terminal.keys" | "terminal.send" => match w.find(&id) {
            Some(_) => Ok(json!({})),
            None => Err(not_found(&id)),
        },
        "terminal.kill" => {
            w.ended.push(id.clone());
            let Some(t) = w.find(&id) else {
                return Err(not_found(&id));
            };
            t["state"] = json!("finished");
            t["reason"] = json!("killed");
            t["status"] = Value::Null;
            t["foreground"] = Value::Null;
            t["finished_ms"] = json!(now_ms());
            let t = t.clone();
            w.emit("finished", &id, json!({"exit_code": null}));
            Ok(t)
        }
        "terminal.rm" => {
            w.ended.push(id.clone());
            let Some(i) = w.terms.iter().position(|t| t["address"] == id.as_str()) else {
                return Err(not_found(&id));
            };
            w.terms.remove(i);
            w.emit("removed", &id, json!({}));
            Ok(json!({}))
        }
        other => Err(json!({"code": -32601, "message": format!("no method {other}")})),
    }
}

impl Core {
    fn start() -> Core {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("core.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let world = Arc::new(Mutex::new(World::default()));
        let shared = Arc::clone(&world);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || serve(&shared, stream));
            }
        });
        // A heartbeat every second to every subscriber.
        let beat = Arc::clone(&world);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Ok(mut w) = beat.lock() else { return };
                let msg = json!({"jsonrpc": "2.0", "method": "heartbeat", "params": {}});
                w.conns.retain_mut(|c| writeln!(c, "{msg}").is_ok());
            }
        });
        Core {
            _dir: dir,
            socket,
            world,
        }
    }

    fn world(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap()
    }

    /// The core restarts: every connection drops, every live terminal comes back as the
    /// machine's shell, `seq` starts again, and no notice is sent. `then` changes the world
    /// before the core takes subscriptions again.
    fn restart_then(&self, then: impl FnOnce(&mut World)) {
        {
            let mut w = self.world();
            w.down = true;
            w.drop_connections();
            let live: Vec<String> = w
                .terms
                .iter()
                .filter(|t| t["state"] == "live")
                .map(|t| t["address"].as_str().unwrap().to_string())
                .collect();
            for address in live {
                w.replace(&address);
            }
            w.seq = 0;
        }
        std::thread::sleep(Duration::from_millis(300));
        let mut w = self.world();
        then(&mut w);
        w.down = false;
    }

    fn restart(&self) {
        self.restart_then(|_| {});
    }

    /// The subscription breaks and, while makibad is not subscribed, only the program at
    /// `address` is lost and replaced by the shell. The core does not restart.
    fn outage_replacing(&self, address: &str) {
        {
            let mut w = self.world();
            w.down = true;
            w.drop_connections();
            w.replace(address);
        }
        std::thread::sleep(Duration::from_millis(300));
        self.world().down = false;
    }

    /// The program at `address` is lost while the subscription is up: the shell in its place
    /// and a live `lost` notice.
    fn lose_live(&self, address: &str) {
        let mut w = self.world();
        w.replace(address);
        w.emit("lost", address, json!({}));
    }

    fn address_of(&self, spawn: &str) -> String {
        let w = self.world();
        let t = w
            .terms
            .iter()
            .find(|t| t["labels"]["makiba.spawn"] == spawn)
            .unwrap_or_else(|| panic!("no terminal of {spawn} in the core"));
        t["address"].as_str().unwrap().to_string()
    }

    fn ended(&self, address: &str) -> bool {
        self.world().ended.iter().any(|a| a == address)
    }
}

/// One connection: requests answered one per line; a subscription is kept for notices.
fn serve(shared: &Arc<Mutex<World>>, stream: UnixStream) {
    let mut writer = stream.try_clone().unwrap();
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let r: Value = serde_json::from_str(&line).unwrap();
        let method = r["method"].as_str().unwrap_or_default().to_string();
        if method == "events.subscribe" {
            let mut w = shared.lock().unwrap();
            if w.down {
                return;
            }
            let reply = json!({"jsonrpc": "2.0", "id": r["id"],
                "result": {"seq": w.seq, "terminals": w.terms}});
            if writeln!(writer, "{reply}").is_err() {
                return;
            }
            // A subscriber that stops reading is dropped, never waited for under the lock.
            let conn = writer.try_clone().unwrap();
            conn.set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            w.conns.push(conn);
            continue;
        }
        let reply = match handle(shared, &method, &r["params"]) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": r["id"], "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": r["id"], "error": error}),
        };
        if writeln!(writer, "{reply}").is_err() {
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The organization.

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    root: PathBuf,
    dir: PathBuf,
    org: String,
    core: Core,
    daemon: Option<Daemon>,
    _tmp: tempfile::TempDir,
    _knowledge: makibad::Knowledge,
    _env: MutexGuard<'static, ()>,
}

impl Drop for Org {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An organization on a fresh core, without a secretary, with a worker at work for each task
/// in `tasks`.
fn org(tasks: &[&str]) -> Org {
    let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    drop(StateWriter::open(&dir.join("state.db")).unwrap());
    let held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
    // SAFETY: the lock above is held for the whole test; no daemon of an earlier test is running.
    unsafe {
        std::env::set_var(
            "MAKIBA_FAKE_AGENT",
            // The program and its record file: the stand-in runs no argv, so neither is used.
            json!(["makiba-test-agent", root.join("agent.record")]).to_string(),
        );
        std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        std::env::remove_var("MAKIBA_SECRETARY");
        std::env::remove_var("MAKIBA_WRITE_RETRY_MS");
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "60000");
        std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("config/person.key"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("MAKIBA_CURATOR_TIMEOUT_MS", "600000");
        std::env::remove_var("MAKIBA_SECRETARY_REMOTE_CONTROL");
    }
    let org = dir.to_string_lossy().into_owned();
    let o = Org {
        root,
        dir,
        org,
        core: Core::start(),
        daemon: None,
        _tmp: tmp,
        _knowledge: held_knowledge,
        _env: env,
    };
    for task in tasks {
        o.worker(task);
    }
    o
}

impl Org {
    fn base(&self) -> Box<dyn Base> {
        connect_as(&self.core.socket, Duration::from_secs(5), &self.org)
    }

    fn start(&mut self) {
        let started = self.started();
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, socket, org, flag) = (
            self.dir.clone(),
            self.core.socket.clone(),
            self.org.clone(),
            Arc::clone(&stop),
        );
        let daemon = std::thread::spawn(move || {
            makibad::run_until(
                &dir,
                &|| connect_as(&socket, Duration::from_secs(5), &org),
                flag,
            )
        });
        self.daemon = Some((stop, daemon));
        self.until("makibad subscribed", |o| o.started() > started);
        self.until("the first resync", |o| o.resyncs() >= 1);
    }

    /// How many `dispatcher_started` are recorded.
    fn started(&self) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'dispatcher_started'",
            [],
        )
    }

    fn resyncs(&self) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'base_resynced'",
            [],
        )
    }

    /// The `restart` of the latest `base_resynced`, as SQLite reads it (null when absent).
    fn last_restart(&self) -> Option<i64> {
        self.one(
            "SELECT json_extract(payload_json, '$.restart') FROM events
             WHERE kind = 'base_resynced' ORDER BY id DESC LIMIT 1",
            [],
        )
        .flatten()
    }

    fn stop(&mut self) {
        if let Some((stop, daemon)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
        }
    }

    fn db(&self) -> Connection {
        let c = Connection::open_with_flags(
            self.dir.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c
    }

    fn one<T: rusqlite::types::FromSql>(&self, sql: &str, p: impl rusqlite::Params) -> Option<T> {
        self.db()
            .query_row(sql, p, |r| r.get(0))
            .optional()
            .unwrap()
    }

    fn count(&self, sql: &str, p: impl rusqlite::Params) -> i64 {
        self.one(sql, p).unwrap_or(0)
    }

    fn until(&self, what: &str, f: impl Fn(&Org) -> bool) {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for: {what}");
    }

    fn status(&self, task: &str) -> Option<String> {
        self.one("SELECT status FROM runs WHERE task_id = ?1", [task])
    }

    /// The run's launches, oldest first: (spawn id, stage).
    fn launches(&self, task: &str) -> Vec<(String, String)> {
        let c = self.db();
        let mut q = c
            .prepare(
                "SELECT l.spawn_id, l.stage FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE r.task_id = ?1 ORDER BY l.id",
            )
            .unwrap();
        q.query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn gone_raised(&self, task: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'anomaly_raised'
               AND json_extract(payload_json, '$.rule') = 'gone'
               AND json_extract(payload_json, '$.task_id') = ?1",
            [task],
        )
    }

    fn spawn_failed(&self, task: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'worker_spawn_failed'
               AND json_extract(payload_json, '$.task_id') = ?1",
            [task],
        )
    }

    fn gone_recorded(&self, spawn: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'terminal_gone'
               AND json_extract(payload_json, '$.spawn_id') = ?1",
            [spawn],
        )
    }

    /// Run `task` in use, its launch `spawn-<task>` verified, and its worker terminal launched on
    /// the core by makiba's own base (so makiba's launch labels it), reporting `working`. Before
    /// makibad starts.
    fn worker(&self, task: &str) {
        let spawn = first_spawn(task);
        let w = StateWriter::open(&self.dir.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        let workplace = self.root.join("workers").join(task);
        std::fs::create_dir_all(&workplace).unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage,
                                         verified_at)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'document', 'fake',
                         'the brief', ?3, 'verified', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                rusqlite::params![task, spawn, workplace.to_string_lossy()],
            )
            .unwrap();
        drop(w);
        self.base()
            .launch(&LaunchSpec {
                argv: vec!["makiba-test-agent".into(), "--worker".into()],
                cwd: workplace,
                name: format!("w-{task}"),
                tags: BTreeMap::from([
                    ("makiba.role".to_string(), "worker".to_string()),
                    ("makiba.task_id".to_string(), task.to_string()),
                    ("makiba.spawn".to_string(), spawn.clone()),
                ]),
                env: BTreeMap::new(),
                unset: vec![],
            })
            .unwrap();
        self.until("the worker reports working", |o| {
            o.core
                .world()
                .terms
                .iter()
                .any(|t| t["labels"]["makiba.spawn"] == spawn && t["status"]["state"] == "working")
        });
    }

    /// The run's newest launch is not its first and is verified, and the run is in use.
    fn resumed(&self, task: &str) -> bool {
        let launches = self.launches(task);
        launches.len() >= 2
            && launches
                .last()
                .is_some_and(|(spawn, stage)| *spawn != first_spawn(task) && stage == "verified")
            && self.status(task).as_deref() == Some("in_use")
    }

    /// Nothing failed the run: no `gone` anomaly, no failed launch verification, not `failed`.
    fn assert_never_failed(&self, task: &str) {
        assert_eq!(self.gone_raised(task), 0, "{task} got the gone anomaly");
        assert_eq!(self.spawn_failed(task), 0, "a launch of {task} was failed");
        assert_ne!(
            self.status(task).as_deref(),
            Some("failed"),
            "{task} failed"
        );
    }

    /// A few more ticks for the anomaly and verification passes.
    fn settle(&self) {
        std::thread::sleep(Duration::from_millis(1500));
    }
}

// ---------------------------------------------------------------------------------------------
// The tests.

/// A core restart with two running workers: both runs are resumed with new launches, the shells
/// left in place of their programs are ended, and neither fails.
#[test]
fn a_core_restart_resumes_both_runs() {
    let mut o = org(&TASKS);
    o.start();
    let shells: Vec<String> = TASKS
        .iter()
        .map(|t| o.core.address_of(&first_spawn(t)))
        .collect();
    o.core.restart();
    o.until("a resync judged a restart", |o| {
        o.count(
            "SELECT count(*) FROM events WHERE kind = 'base_resynced'
               AND json_extract(payload_json, '$.restart') = 1",
            [],
        ) >= 1
    });
    for (task, shell) in TASKS.iter().zip(&shells) {
        o.until("the run resumed with a new verified launch", |o| {
            o.resumed(task)
        });
        o.until("the replaced shell ended", |o| o.core.ended(shell));
    }
    o.settle();
    for task in TASKS {
        o.assert_never_failed(task);
        assert_eq!(o.status(task).as_deref(), Some("in_use"));
    }
}

/// One program lost while makibad was not subscribed, the other still running: not a restart.
/// The lost one's run fails as #560 makes it; the other is left as it is.
#[test]
fn a_single_program_lost_in_a_reconnect_still_fails() {
    let mut o = org(&TASKS);
    o.start();
    let before = o.resyncs();
    let t1 = o.core.address_of(&first_spawn("t1"));
    o.core.outage_replacing(&t1);
    o.until("the resync after the reconnect", |o| o.resyncs() > before);
    o.until("t1's terminal_gone", |o| o.gone_recorded("spawn-t1") >= 1);
    o.until("the gone anomaly of t1", |o| o.gone_raised("t1") >= 1);
    o.until("t1 failed", |o| o.status("t1").as_deref() == Some("failed"));
    assert_eq!(
        o.last_restart(),
        Some(0),
        "the resync after the reconnect is not a restart"
    );
    o.settle();
    assert_eq!(o.status("t2").as_deref(), Some("in_use"));
    assert_eq!(o.launches("t2").len(), 1, "t2 was relaunched");
    assert_eq!(o.gone_raised("t2"), 0);
}

/// One program lost while the subscription is up (a live `lost` notice): its run fails, the
/// other is left as it is.
#[test]
fn a_single_program_lost_live_still_fails() {
    let mut o = org(&TASKS);
    o.start();
    let t1 = o.core.address_of(&first_spawn("t1"));
    o.core.lose_live(&t1);
    o.until("the gone anomaly of t1", |o| o.gone_raised("t1") >= 1);
    o.until("t1 failed", |o| o.status("t1").as_deref() == Some("failed"));
    o.settle();
    assert_eq!(o.status("t2").as_deref(), Some("in_use"));
    assert_eq!(o.launches("t2").len(), 1, "t2 was relaunched");
    assert_eq!(o.gone_raised("t2"), 0);
}

/// A second restart while the relaunches of the first are past their startup dialogs but not yet
/// verified (their agents report `idle`, never `working`): they are replaced too, and both runs
/// end resumed by their third launch, never failed.
#[test]
fn two_restarts_before_verification_resume_both_runs() {
    let mut o = org(&TASKS);
    o.start();
    o.core.world().idle = true;
    o.core.restart();
    for task in TASKS {
        o.until("the first relaunch past its dialogs, not verified", |o| {
            let l = o.launches(task);
            l.len() == 2 && l[1].1 == "dialog_passed"
        });
    }
    // The second restart; the relaunches it brings report `working`, as agents do.
    o.core.restart_then(|w| w.idle = false);
    for task in TASKS {
        o.until("the run resumed by its third launch", |o| {
            o.resumed(task) && o.launches(task).len() == 3
        });
    }
    o.settle();
    for task in TASKS {
        o.assert_never_failed(task);
        assert_eq!(o.status(task).as_deref(), Some("in_use"));
        assert_eq!(
            o.launches(task).len(),
            3,
            "{task}: one relaunch per restart"
        );
    }
}

/// The known limit: an organization whose only terminal is one worker (no secretary) cannot
/// tell a core restart from that worker's loss, and the run fails, as #560 makes it.
#[test]
fn a_lone_worker_restart_still_fails_its_run() {
    let mut o = org(&["t1"]);
    o.start();
    o.core.restart();
    o.until("t1's terminal_gone", |o| o.gone_recorded("spawn-t1") >= 1);
    o.until("the gone anomaly of t1", |o| o.gone_raised("t1") >= 1);
    o.until("t1 failed", |o| o.status("t1").as_deref() == Some("failed"));
    assert_eq!(
        o.last_restart(),
        Some(0),
        "a lone worker's resync is a restart"
    );
    assert_eq!(o.launches("t1").len(), 1, "t1 was relaunched");
}
