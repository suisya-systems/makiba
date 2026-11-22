//! Issue 46 (M6-5): renew the secretary without typing into it, and relaunch it when it is gone.
//!
//! The daemon runs in this process on `Local` (tests/common/local.rs), which really runs each
//! terminal's argv: every agent is the test agent, whose steps are read from one steps file when
//! it is launched, so a test changes what the next launch does by rewriting that file. The base is
//! wrapped to keep every launch spec: an agent's key is its token in the spec's MCP configuration,
//! which is how the test calls the door as that agent (`Door::handle`, the handler the MCP server
//! calls). The test writes `state.db` only before the daemon starts; after that it reads, and its
//! requests go through the door, as an agent's do.
//!
//! Rows are named, never counted in total: the gap message by its id, the renewal by its spawn
//! ids, `secretary_gone` by its generation (the secretary's `agents` row).

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const WAIT: Duration = Duration::from_secs(60);
/// An agent that starts, reports a turn and waits.
const RUNS: &[&str] = &["working", "idle", "hang"];
/// An agent that takes 6 s to start: the gap is held open for that long.
const SLOW: &[&str] = &["sleep:6000", "working", "idle", "hang"];
/// An agent that exits before it starts: its launch is never verified.
const DIES: &[&str] = &["exit:3"];

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

/// `Local`, keeping every launch spec.
#[derive(Clone)]
struct Rec {
    local: Local,
    specs: Arc<Mutex<Vec<LaunchSpec>>>,
}

impl Base for Rec {
    fn name(&self) -> &'static str {
        self.local.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.local.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.specs.lock().unwrap().push(spec.clone());
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

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    root: PathBuf,
    dir: PathBuf,
    steps: PathBuf,
    base: Rec,
    door: Option<Door>,
    person: String,
    daemon: Option<Daemon>,
    _tmp: tempfile::TempDir,
    _knowledge: makibad::Knowledge,
    _env: MutexGuard<'static, ()>,
}

/// The daemon is stopped and every terminal ended when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        if let Some((stop, daemon)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
        }
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// An organization whose agents follow `steps`, with the verification limit `verify_ms`, and a
/// worker run `t1` when `worker`. Open and answered pending entries are recorded before the start.
fn org(verify_ms: u64, worker: bool) -> Org {
    let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let steps = root.join("steps.json");
    let person;
    {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        if worker {
            w.ensure_project("p", None, None).unwrap();
            w.upsert_run(&RunUpsert {
                task_id: "t1",
                project_slug: "p",
                status: Some("queued"),
                ..Default::default()
            })
            .unwrap();
            w.conn()
                .execute(
                    "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                     VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'document',
                             'fake', 'the brief', ?1)",
                    [root.join("docs/t1").to_string_lossy()],
                )
                .unwrap();
            for (question, status) in [("Merge it?", "open"), ("Keep it?", "answered")] {
                w.conn()
                    .execute(
                        "INSERT INTO pending (run_id, kind, question, status)
                         VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'question', ?1, ?2)",
                        [question, status],
                    )
                    .unwrap();
            }
        }
    }
    let agent = json!([AGENT, "record", format!("steps:{}", steps.display())]).to_string();
    let held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
    // SAFETY: the lock above is held for the whole test; no daemon of an earlier test is running.
    unsafe {
        std::env::set_var("MAKIBA_FAKE_AGENT", agent);
        std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        std::env::set_var("MAKIBA_SECRETARY", "1");
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", verify_ms.to_string());
        std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("config/person.key"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("MAKIBA_CURATOR_TIMEOUT_MS", "600000");
        std::env::remove_var("MAKIBA_SECRETARY_REMOTE_CONTROL");
    }
    let o = Org {
        root,
        dir,
        steps,
        base: Rec {
            local: Local::new(),
            specs: Arc::default(),
        },
        door: None,
        person,
        daemon: None,
        _tmp: tmp,
        _knowledge: held_knowledge,
        _env: env,
    };
    o.set_steps(RUNS);
    o
}

impl Org {
    fn set_steps(&self, steps: &[&str]) {
        std::fs::write(&self.steps, json!(steps).to_string()).unwrap();
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.dir.clone(), self.base.clone(), Arc::clone(&stop));
        let daemon =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, daemon));
        // The daemon opens the database as it starts: a busy open is tried again.
        let until = Instant::now() + WAIT;
        let w = loop {
            match StateWriter::open(&self.dir.join("state.db")) {
                Ok(w) => break w,
                Err(e) if Instant::now() < until => {
                    eprintln!("door open: {e}; again");
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("cannot open the door's database: {e}"),
            }
        };
        self.door = Some(Door::new(w, Arc::new(self.base.clone()), "fake".into()));
    }

    /// A request through the door, as the agent or person holding `key`.
    fn call(&self, key: &str, verb: &str, args: Value) -> Result<Value, String> {
        self.door
            .as_ref()
            .unwrap()
            .handle(Some(key), verb, args, None, true)
            .map_err(|e| format!("{e:?}"))
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

    fn until(&self, what: &str, f: impl Fn(&Org) -> bool) {
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for: {what}");
    }

    /// How many times a terminal named `name` was launched.
    fn launches(&self, name: &str) -> usize {
        self.base
            .specs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.name == name)
            .count()
    }

    /// The live terminals named `name`.
    fn live(&self, name: &str) -> Vec<Terminal> {
        self.base
            .list()
            .unwrap()
            .into_iter()
            .filter(|t| t.name == name && t.alive)
            .collect()
    }

    /// The token of the newest launch of `name`, from its MCP configuration.
    fn token(&self, name: &str) -> String {
        let specs = self.base.specs.lock().unwrap();
        let spec = specs.iter().rev().find(|s| s.name == name).unwrap();
        let i = spec.argv.iter().position(|a| a == "--mcp-config").unwrap();
        let config: Value = serde_json::from_str(&spec.argv[i + 1]).unwrap();
        config["mcpServers"]["makiba"]["headers"]["Authorization"]
            .as_str()
            .unwrap()
            .strip_prefix("Bearer ")
            .unwrap()
            .to_string()
    }

    fn stage(&self, spawn: &str) -> Option<String> {
        self.one("SELECT stage FROM run_launch WHERE spawn_id = ?1", [spawn])
    }

    /// The newest secretary launch's spawn id.
    fn newest_secretary(&self) -> Option<String> {
        self.one(
            "SELECT spawn_id FROM run_launch WHERE role = 'secretary' ORDER BY id DESC LIMIT 1",
            [],
        )
    }

    /// A secretary that runs: its launch verified, its terminal live and idle. Returns its spawn.
    fn running_secretary(&self) -> String {
        self.until("a verified, idle secretary", |o| {
            let Some(spawn) = o.newest_secretary() else {
                return false;
            };
            o.stage(&spawn).as_deref() == Some("verified")
                && o.live("secretary").iter().any(|t| {
                    t.tags.get("makiba.spawn") == Some(&spawn)
                        && t.status.as_ref().map(|s| s.state) == Some(makiba::base::State::Idle)
                })
        });
        self.newest_secretary().unwrap()
    }

    /// The delivery state of message `id` to the secretary.
    fn delivery(&self, id: i64) -> Option<String> {
        self.one(
            "SELECT d.status FROM event_deliveries d JOIN messages m ON m.event_id = d.source_event_id
             WHERE m.id = ?1 AND d.recipient = 'secretary'",
            [id],
        )
    }

    /// The ids of the messages `inbox` returns to the holder of `token`.
    fn inbox(&self, token: &str) -> Vec<i64> {
        let r = self.call(token, "inbox", json!({})).unwrap();
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_i64().unwrap())
            .collect()
    }

    /// The person's message to the secretary; its id.
    fn send(&self, text: &str) -> i64 {
        let r = self
            .call(
                &self.person,
                "send",
                json!({"to": "secretary", "text": text}),
            )
            .unwrap();
        r["id"].as_i64().unwrap()
    }

    /// `secretary_gone` raises for the generation of the key of `spawn`.
    fn gone_raised(&self, spawn: &str) -> i64 {
        self.one(
            "SELECT count(*) FROM events WHERE kind = 'anomaly_raised'
               AND json_extract(payload_json, '$.rule') = 'secretary_gone'
               AND json_extract(payload_json, '$.generation') =
                   (SELECT id FROM agents WHERE spawn_id = ?1)",
            [spawn],
        )
        .unwrap()
    }

    /// `makiba` run by the person, with this organization's folders and nothing of this process's
    /// environment.
    fn makiba(&self, args: &[&str]) -> Output {
        let mut c = Command::new(MAKIBA);
        c.args(args);
        self.env(&mut c);
        c.output().unwrap()
    }

    /// `makiba up`, which starts makibad, with the knowledge folder of the helper.
    fn up(&self) -> Output {
        let mut c = makibad::up(&self.root);
        c.with(|c| self.env(c))
            .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(&self.root));
        c.output().unwrap()
    }

    /// Only what is set here: the person's environment is cleared.
    fn env(&self, cmd: &mut Command) {
        let bin = self.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let (a, b, c) = makiba::agents::version::LOWEST;
        script(
            &bin.join("claude"),
            &format!("echo '{a}.{b}.{c} (Claude Code)'"),
        );
        script(&bin.join("hc"), "exit 0");
        cmd.env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("MAKIBA_STATE_DIR", &self.dir)
            .env("MAKIBA_PERSON_KEY", self.root.join("config/person.key"))
            .env("HC_BIN", bin.join("hc"))
            .env("MAKIBA_DOOR", "none");
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A renewal: the terminal named `secretary` is another one, every worker terminal is the same, a
/// message queued while no secretary ran reaches the new one, and the open pending entries and
/// the `in_use` runs are the same rows before and after.
#[test]
fn a_renewal_replaces_the_secretary_and_nothing_else() {
    let mut o = org(60_000, true);
    o.start();
    let old = o.running_secretary();
    o.until("the worker's run in use", |o| {
        o.one::<String>("SELECT status FROM runs WHERE task_id = 't1'", [])
            .as_deref()
            == Some("in_use")
    });
    // A terminal is told by its launch identity (the base names a terminal by its name).
    let terminals = |o: &Org, name: &str| -> BTreeSet<(TerminalRef, Option<String>)> {
        o.live(name)
            .into_iter()
            .map(|t| (t.id, t.tags.get("makiba.spawn").cloned()))
            .collect()
    };
    let workers = terminals(&o, "w-t1");
    assert_eq!((workers.len(), o.launches("w-t1")), (1, 1));
    let ids = |o: &Org, sql: &str| -> Vec<i64> {
        let c = o.db();
        let mut q = c.prepare(sql).unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    const OPEN: &str = "SELECT id FROM pending WHERE status IN ('open', 'escalated') ORDER BY id";
    const IN_USE: &str = "SELECT id FROM runs WHERE status = 'in_use' ORDER BY id";
    let (open, in_use) = (ids(&o, OPEN), ids(&o, IN_USE));
    assert_eq!((open.len(), in_use.len()), (1, 1));

    // The next secretary takes 6 s to start: the gap stays open while the message is queued.
    o.set_steps(SLOW);
    let old_token = o.token("secretary");
    let r = o.call(
        &old_token,
        "renew",
        json!({"handover": "Waiting on t1's report."}),
    );
    assert!(r.is_ok(), "renew: {r:?}");
    o.until("the old secretary ended and its key revoked", |o| {
        o.one::<i64>(
            "SELECT count(*) FROM agents WHERE spawn_id = ?1 AND revoked_at IS NULL",
            [&old],
        ) == Some(0)
            && !o
                .live("secretary")
                .iter()
                .any(|t| t.tags.get("makiba.spawn") == Some(&old))
    });
    let gap = o.send("Sent while no secretary ran.");
    let new = o.newest_secretary().unwrap();
    assert_ne!(new, old);
    assert_ne!(
        o.stage(&new).as_deref(),
        Some("verified"),
        "the message was not queued during the gap"
    );
    assert_eq!(o.delivery(gap).as_deref(), Some("pending"));

    o.until("secretary_renewed", |o| {
        o.one::<i64>(
            "SELECT count(*) FROM events WHERE kind = 'secretary_renewed'
               AND json_extract(payload_json, '$.old') = ?1
               AND json_extract(payload_json, '$.new') = ?2",
            [&old, &new],
        ) == Some(1)
    });
    // The terminal named secretary is another one; every worker terminal is the same one.
    let now: Vec<Terminal> = o.live("secretary");
    assert_eq!(now.len(), 1);
    assert_eq!(now[0].tags.get("makiba.spawn"), Some(&new));
    assert_eq!(terminals(&o, "w-t1"), workers);
    assert_eq!(o.launches("w-t1"), 1, "a worker was launched again");
    // The gap's message reaches the new secretary, and the old key reaches nothing.
    assert!(o.call(&old_token, "inbox", json!({})).is_err());
    let new_token = o.token("secretary");
    assert!(o.inbox(&new_token).contains(&gap));
    assert_eq!(o.delivery(gap).as_deref(), Some("delivered"));
    // Open pending entries and in_use runs, by id.
    assert_eq!(ids(&o, OPEN), open);
    assert_eq!(ids(&o, IN_USE), in_use);
    // The renewal was no gone.
    assert_eq!(o.gone_raised(&old), 0);
}

/// A secretary terminal killed with no renewal: `secretary_gone` once, a relaunch, and the
/// message queued while it was gone stays pending and reaches the new terminal once.
#[test]
fn a_gone_secretary_is_relaunched_and_gets_its_mail_once() {
    let mut o = org(60_000, false);
    o.start();
    let old = o.running_secretary();
    let old_terminal = o.live("secretary").remove(0).id;
    o.set_steps(SLOW);
    o.base.end(&old_terminal).unwrap();
    o.until("secretary_gone of the killed terminal", |o| {
        o.gone_raised(&old) == 1
    });
    let mail = o.send("Sent while the secretary was gone.");
    assert_eq!(o.delivery(mail).as_deref(), Some("pending"));
    let new = o.running_secretary();
    assert_ne!(new, old);
    // Never failed while it was gone; once, and only to the new terminal.
    assert_eq!(o.delivery(mail).as_deref(), Some("pending"));
    let token = o.token("secretary");
    assert_eq!(o.inbox(&token).iter().filter(|id| **id == mail).count(), 1);
    assert_eq!(o.delivery(mail).as_deref(), Some("delivered"));
    assert!(!o.inbox(&token).contains(&mail), "delivered twice");
    assert_eq!(o.gone_raised(&old), 1);
    o.until("the relaunch done", |o| {
        o.one::<String>(
            "SELECT status FROM commands WHERE actor = 'makibad' AND verb = 'renew'
               AND json_extract(result_json, '$.result.new') = ?1",
            [&new],
        )
        .as_deref()
            == Some("done")
    });
}

/// Two relaunches that fail in the window: no third, `makiba status` shows the secretary gone and
/// says to run `makiba up`; a plain `makiba up` with makibad running launches it again.
#[test]
fn after_two_failed_relaunches_none_until_up() {
    let mut o = org(3_000, false);
    o.start();
    let first = o.running_secretary();
    let terminal = o.live("secretary").remove(0).id;
    o.set_steps(DIES);
    o.base.end(&terminal).unwrap();
    let relaunches = |o: &Org, status: &str| -> i64 {
        o.one(
            "SELECT count(*) FROM commands WHERE actor = 'makibad' AND verb = 'renew'
               AND status = ?1",
            [status],
        )
        .unwrap()
    };
    o.until("two failed relaunches and the stop", |o| {
        relaunches(o, "failed") == 2 && relaunches(o, "rejected") == 1
    });
    let launched_after = |o: &Org| -> i64 {
        o.one(
            "SELECT count(*) FROM run_launch WHERE role = 'secretary'
               AND id > (SELECT id FROM run_launch WHERE spawn_id = ?1)",
            [&first],
        )
        .unwrap()
    };
    assert_eq!(launched_after(&o), 2);
    // Its passes run every tick: a few seconds for a third, if it would.
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(launched_after(&o), 2, "a third relaunch was tried");
    assert_eq!(relaunches(&o, "accepted"), 0);
    let status = o.makiba(&["status"]);
    let said = text(&status);
    let line = said
        .lines()
        .find(|l| l.starts_with("secretary:"))
        .unwrap_or_else(|| panic!("no secretary line: {said}"));
    assert!(
        line.contains("gone") && line.contains("makiba up"),
        "{said}"
    );

    // The person runs `makiba up`: makibad runs, so it is asked by a row and launches one.
    o.set_steps(RUNS);
    let up = o.up();
    assert!(up.status.success(), "{}", text(&up));
    let back = o.running_secretary();
    assert_eq!(launched_after(&o), 3);
    assert_eq!(
        o.one::<String>(
            "SELECT status FROM commands WHERE verb = 'up' ORDER BY id DESC LIMIT 1",
            []
        )
        .as_deref(),
        Some("done")
    );
    let said = text(&o.makiba(&["status"]));
    assert!(!said.contains("gone"), "{said}");
    assert_ne!(back, first);
}
