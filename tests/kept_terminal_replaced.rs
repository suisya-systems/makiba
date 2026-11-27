//! Issue 560: a kept terminal whose program the base replaced with a shell is gone. herdr-core
//! launches makiba's terminals with `keep`, and gives a kept terminal whose program was lost (a
//! signal, a core stop and start) the machine's shell in place: the terminal keeps its address and
//! labels and is `live`. makibad must see that the program it launched no longer runs, also when
//! the loss happened while it was not subscribed, and record it, raise the anomaly and relaunch
//! the secretary or fail the worker's run. A loss whose `terminal_gone` the database refused past
//! the monitor's retry still raises the anomaly, from its `observation_lost`.
//!
//! A stand-in core listens on a Unix socket in a temporary folder and answers makiba's real base
//! (`makiba::base::connect_as`), so the wire is read as on the real base. The shapes are those
//! the person read on hc 0.1.22 (a kept `sh -c` killed by SIGKILL; a server stop and start; a
//! program never lost): a replaced terminal stays `state` `live` with `reason`, `exit_code`,
//! `signal` and `finished_ms` null and its labels unchanged, and its `argv` and `foreground` become
//! the shell's, with a new pid. Its "core restart" drops every connection, turns every live
//! terminal into that shell, starts `seq` again, and sends no `lost` or `restarted` notice: nobody
//! is subscribed. makibad runs in this process; the test writes `state.db` only before a daemon
//! starts, then reads. Every wait is a poll with a bound.

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
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const WAIT: Duration = Duration::from_secs(60);
const SHELL: &str = "/usr/bin/zsh";
const WORKER: &str = "spawn-t1";

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

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
    /// Restarting: a subscription is closed at once.
    down: bool,
    /// `terminal.list` answers `unavailable`.
    refuse_list: bool,
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
            // The program starts and reports `working`, as an agent does (what verifies a launch).
            let (later, addr) = (Arc::clone(shared), address.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                let mut w = later.lock().unwrap();
                let status = json!({"state": "working"});
                let Some(t) = w.find(&addr) else { return };
                if t["state"] != "live" {
                    return;
                }
                t["status"] = status.clone();
                t["status_ms"] = json!(now_ms());
                t["reported"] = json!(true);
                w.emit("status", &addr, json!({"status": status}));
            });
            Ok(t)
        }
        "terminal.list" if w.refuse_list => Err(json!({"code": -32000,
            "message": "the test holds the list", "data": {"kind": "unavailable"}})),
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
    /// machine's shell (the readings after `hc server stop` and `hc server start`), `seq` starts
    /// again, and no notice is sent.
    fn restart(&self) {
        {
            let mut w = self.world();
            w.down = true;
            for c in w.conns.drain(..) {
                let _ = c.shutdown(Shutdown::Both);
            }
            let mut terms = std::mem::take(&mut w.terms);
            for t in terms.iter_mut().filter(|t| t["state"] == "live") {
                let pid = w.new_pid();
                t["argv"] = json!([SHELL]);
                t["foreground"] =
                    json!({"argv": [SHELL], "cwd": t["cwd"], "name": "zsh", "pid": pid});
                t["pid"] = json!(pid);
                t["status"] = Value::Null;
                t["status_ms"] = Value::Null;
                t["title"] = json!("user@host:folder");
            }
            w.terms = terms;
            w.seq = 0;
        }
        std::thread::sleep(Duration::from_millis(300));
        self.world().down = false;
    }

    /// The connections break and are made again; nothing in the core changes.
    fn drop_connections(&self) {
        for c in self.world().conns.drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }

    /// A live `lost` notice for the terminal at `address`, while the subscription is up.
    fn lose(&self, address: &str) {
        self.world().emit("lost", address, json!({}));
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

/// An organization on a fresh core. `secretary`: makibad launches the secretary at its start.
/// `retry_ms`: the monitor's retry window.
fn org(secretary: bool, retry_ms: Option<u64>) -> Org {
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
        if secretary {
            std::env::set_var("MAKIBA_SECRETARY", "1");
        } else {
            std::env::remove_var("MAKIBA_SECRETARY");
        }
        match retry_ms {
            Some(ms) => std::env::set_var("MAKIBA_WRITE_RETRY_MS", ms.to_string()),
            None => std::env::remove_var("MAKIBA_WRITE_RETRY_MS"),
        }
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
    Org {
        root,
        dir,
        org,
        core: Core::start(),
        daemon: None,
        _tmp: tmp,
        _knowledge: held_knowledge,
        _env: env,
    }
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
    }

    /// How many `dispatcher_started` are recorded.
    fn started(&self) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'dispatcher_started'",
            [],
        )
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

    /// The secretary's launches, oldest first.
    fn secretaries(&self) -> Vec<String> {
        let c = self.db();
        let mut q = c
            .prepare("SELECT spawn_id FROM run_launch WHERE role = 'secretary' ORDER BY id")
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn stage(&self, spawn: &str) -> Option<String> {
        self.one("SELECT stage FROM run_launch WHERE spawn_id = ?1", [spawn])
    }

    /// The first secretary, launched and verified.
    fn verified_secretary(&self) -> String {
        self.until("the first secretary verified", |o| {
            o.secretaries()
                .first()
                .is_some_and(|s| o.stage(s).as_deref() == Some("verified"))
        });
        self.secretaries().remove(0)
    }

    /// A secretary launch other than `old` that is verified.
    fn wait_new_secretary(&self, old: &str) {
        self.until("a second secretary verified", |o| {
            o.secretaries()
                .iter()
                .any(|s| s != old && o.stage(s).as_deref() == Some("verified"))
        });
    }

    fn gone_recorded(&self, spawn: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'terminal_gone'
               AND json_extract(payload_json, '$.spawn_id') = ?1",
            [spawn],
        )
    }

    fn secretary_gone_raised(&self, spawn: &str) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'anomaly_raised'
               AND json_extract(payload_json, '$.rule') = 'secretary_gone'
               AND json_extract(payload_json, '$.generation') =
                   (SELECT id FROM agents WHERE spawn_id = ?1)",
            [spawn],
        )
    }

    fn worker_gone_raised(&self) -> i64 {
        self.count(
            "SELECT count(*) FROM events WHERE kind = 'anomaly_raised'
               AND json_extract(payload_json, '$.rule') = 'gone'
               AND json_extract(payload_json, '$.task_id') = 't1'",
            [],
        )
    }

    fn run_status(&self) -> Option<String> {
        self.one("SELECT status FROM runs WHERE task_id = 't1'", [])
    }

    /// Run `t1` in use, its launch `spawn-t1` verified, and its worker terminal launched on the
    /// core by makiba's own base (so makiba's launch labels it), reporting `working`. Before
    /// makibad starts.
    fn worker(&self) {
        let w = StateWriter::open(&self.dir.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        let workplace = self.root.join("workers/t1");
        std::fs::create_dir_all(&workplace).unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage,
                                         verified_at)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), ?1, 'document', 'fake',
                         'the brief', ?2, 'verified', strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                rusqlite::params![WORKER, workplace.to_string_lossy()],
            )
            .unwrap();
        drop(w);
        self.base()
            .launch(&LaunchSpec {
                argv: vec!["makiba-test-agent".into(), "--worker".into()],
                cwd: workplace,
                name: WORKER.into(),
                tags: BTreeMap::from([
                    ("makiba.role".to_string(), "worker".to_string()),
                    ("makiba.task_id".to_string(), "t1".to_string()),
                    ("makiba.spawn".to_string(), WORKER.to_string()),
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
                .any(|t| t["labels"]["makiba.spawn"] == WORKER && t["status"]["state"] == "working")
        });
    }

    /// `makiba status`, run by the person on this organization's folders.
    fn status_text(&self) -> String {
        let out = Command::new(MAKIBA)
            .arg("status")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("MAKIBA_STATE_DIR", &self.dir)
            .env("MAKIBA_PERSON_KEY", self.root.join("config/person.key"))
            .env("MAKIBA_DOOR", "none")
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }
}

// ---------------------------------------------------------------------------------------------
// The tests.

/// Definition of done, first point: a verified secretary whose program was lost and replaced by
/// a shell while makibad's subscription was down is gone; `secretary_gone` is raised and makibad
/// relaunches it.
#[test]
fn a_secretary_replaced_while_unsubscribed_is_gone_and_relaunched() {
    let mut o = org(true, None);
    o.start();
    let s1 = o.verified_secretary();
    let address = o.core.address_of(&s1);
    o.core.restart();
    o.until("the terminal_gone of the replaced secretary", |o| {
        o.count(
            "SELECT count(*) FROM events WHERE kind = 'terminal_gone'
               AND json_extract(payload_json, '$.role') = 'secretary'
               AND json_extract(payload_json, '$.spawn_id') = ?1",
            [&s1],
        ) >= 1
    });
    o.until("secretary_gone of the replaced secretary", |o| {
        o.secretary_gone_raised(&s1) >= 1
    });
    o.until("makibad's relaunch", |o| {
        o.count(
            "SELECT count(*) FROM commands WHERE actor = 'makibad' AND verb = 'renew'
               AND json_extract(args_json, '$.reason') = 'secretary_gone'",
            [],
        ) >= 1
    });
    o.wait_new_secretary(&s1);
    assert!(
        o.core.ended(&address),
        "the replaced secretary's shell was not ended"
    );
}

/// Definition of done, second point: the same scene with `makiba up` (makibad started with the
/// secretary asked for) after the core restarted while no makibad ran. While the base holds the
/// list, `makiba status` says the secretary is gone; then `decide` ends the shell and launches.
#[test]
fn up_after_a_core_restart_launches_a_secretary_and_status_says_gone() {
    let mut o = org(true, None);
    o.start();
    let s1 = o.verified_secretary();
    let address = o.core.address_of(&s1);
    o.stop();
    o.core.restart();
    o.core.world().refuse_list = true;
    o.start();
    o.until("the terminal_gone of the replaced secretary", |o| {
        o.count(
            "SELECT count(*) FROM events WHERE kind = 'terminal_gone'
               AND json_extract(payload_json, '$.role') = 'secretary'
               AND json_extract(payload_json, '$.spawn_id') = ?1",
            [&s1],
        ) >= 1
    });
    o.until("secretary_gone of the replaced secretary", |o| {
        o.secretary_gone_raised(&s1) >= 1
    });
    let text = o.status_text();
    assert!(
        text.lines()
            .any(|l| l.starts_with("secretary: gone; makibad relaunches it")),
        "makiba status does not say the secretary is gone:\n{text}"
    );
    o.core.world().refuse_list = false;
    o.until("the replaced secretary's shell ended", |o| {
        o.core.ended(&address)
    });
    o.wait_new_secretary(&s1);
}

/// Definition of done, third point: a worker whose program was replaced by a shell while makibad
/// was not subscribed: the run gets the `gone` anomaly and fails.
#[test]
fn a_worker_replaced_while_unsubscribed_fails_its_run() {
    let mut o = org(false, None);
    o.worker();
    o.start();
    o.core.restart();
    o.until("the worker's terminal_gone", |o| {
        o.count(
            "SELECT count(*) FROM events WHERE kind = 'terminal_gone'
               AND json_extract(payload_json, '$.task_id') = 't1'
               AND json_extract(payload_json, '$.spawn_id') = ?1",
            [WORKER],
        ) >= 1
    });
    o.until("the gone anomaly of t1", |o| o.worker_gone_raised() >= 1);
    o.until("t1 failed", |o| o.run_status().as_deref() == Some("failed"));
}

/// Definition of done, fourth point: the `terminal_gone` write is refused past the monitor's
/// retry (a trigger refuses every one; a retry window of 0 drops it at its first refusal). The
/// loss is recorded as `observation_lost`, and the anomalies are raised from it.
#[test]
fn a_refused_gone_write_still_raises_the_anomaly() {
    let mut o = org(true, Some(0));
    {
        let c = Connection::open(o.dir.join("state.db")).unwrap();
        c.execute_batch(
            "CREATE TRIGGER test_refuse_gone BEFORE INSERT ON events
             WHEN NEW.kind = 'terminal_gone'
             BEGIN SELECT RAISE(ABORT, 'refused by the test'); END;",
        )
        .unwrap();
    }
    o.worker();
    o.start();
    let s1 = o.verified_secretary();
    let (worker, secretary) = (o.core.address_of(WORKER), o.core.address_of(&s1));
    o.core.lose(&worker);
    o.core.lose(&secretary);
    for spawn in [WORKER, s1.as_str()] {
        o.until("the loss recorded as observation_lost", |o| {
            o.count(
                "SELECT count(*) FROM events WHERE kind = 'observation_lost'
                   AND json_extract(payload_json, '$.kind') = 'terminal_gone'
                   AND json_extract(payload_json, '$.spawn_id') = ?1",
                [spawn],
            ) >= 1
        });
    }
    o.until("the gone anomaly of t1", |o| o.worker_gone_raised() >= 1);
    o.until("t1 failed", |o| o.run_status().as_deref() == Some("failed"));
    o.until("secretary_gone of the lost secretary", |o| {
        o.secretary_gone_raised(&s1) >= 1
    });
}

/// No false loss: a reconnect while the launched program still runs records no gone.
#[test]
fn a_reconnect_while_the_program_runs_records_no_gone() {
    let mut o = org(false, None);
    o.worker();
    o.start();
    let resyncs = |o: &Org| {
        o.count(
            "SELECT count(*) FROM events WHERE kind = 'base_resynced'",
            [],
        )
    };
    o.until("the first resync", |o| resyncs(o) >= 1);
    o.core.drop_connections();
    o.until("the resync after the reconnect", |o| resyncs(o) >= 2);
    // A few more ticks for the anomaly pass.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        o.gone_recorded(WORKER),
        0,
        "a running worker was taken for gone"
    );
    assert_eq!(o.worker_gone_raised(), 0);
    assert_eq!(o.run_status().as_deref(), Some("in_use"));
}
