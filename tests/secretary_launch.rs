//! Issue 43 (M6-2): `makiba up` launches the secretary.
//!
//! The daemon runs in this process on the stand-in base (which runs nothing and keeps every launch
//! spec), launching the secretary as `makiba up` asks it to (`MAKIBA_SECRETARY=1` on the `makibad` it starts), with a
//! stand-in `claude` on the path; a second start of the daemon on the same base stands for a
//! second `up`. One test runs the `makiba` binary's `up` twice, its daemon on its own stand-in
//! (`MAKIBA_BASE=stub`, `MAKIBA_DOOR=none`), and reads what it recorded. No real `claude` and no
//! real base is started.

use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const REMOTE: &str = "--remote-control";

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

/// The stand-in, shared by the test and every daemon it starts, keeping every launch spec.
#[derive(Default)]
struct Recording {
    stub: Stub,
    launched: Mutex<Vec<LaunchSpec>>,
}

#[derive(Clone)]
struct Shared(Arc<Recording>);

impl Base for Shared {
    fn name(&self) -> &'static str {
        self.0.stub.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.0.stub.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.0.launched.lock().unwrap().push(spec.clone());
        self.0.stub.launch(spec)
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        self.0.stub.list()
    }
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.0.stub.get(t)
    }
    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.0.stub.read(t, lines)
    }
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.0.stub.keys(t, keys)
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        self.0.stub.send(t, text, only_if_idle)
    }
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        self.0.stub.end(t)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.0.stub.events()
    }
    fn start(&self) -> Result<(), BaseError> {
        self.0.stub.start()
    }
    fn worker_denies(&self) -> BaseDenies {
        self.0.stub.worker_denies()
    }
}

type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    root: PathBuf,
    dir: PathBuf,
    base: Shared,
    daemon: Option<Daemon>,
    _tmp: tempfile::TempDir,
    _env: MutexGuard<'static, ()>,
}

/// The daemon is stopped when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        self.stop();
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Poll `f` for up to 20 seconds.
fn wait<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

/// The value after `flag`.
fn value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let i = argv.iter().position(|a| a == flag)?;
    argv.get(i + 1).map(String::as_str)
}

fn count(db: &Path, sql: &str) -> i64 {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|c| c.query_row(sql, [], |r| r.get(0)))
        .unwrap_or(-1)
}

impl Org {
    /// An organization with a stand-in `claude` on the path; `remote_control` sets the setting.
    fn new(remote_control: bool) -> Org {
        let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dir = root.join("state");
        let bin = root.join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        script(&bin.join("claude"), "echo 'stand-in claude' >&2\nexit 1");
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        // SAFETY: the environment lock is held, and no daemon of this test runs yet.
        unsafe {
            std::env::set_var("PATH", &path);
            std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
            std::env::set_var("MAKIBA_PERSON_KEY", root.join("elsewhere/person.key"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_KNOWLEDGE_DIR", root.join("knowledge"));
            std::env::remove_var("MAKIBA_FAKE_AGENT");
            std::env::remove_var("MAKIBA_LAUNCH_AGENT");
            std::env::set_var("MAKIBA_SECRETARY", "1");
            if remote_control {
                std::env::set_var("MAKIBA_SECRETARY_REMOTE_CONTROL", "1");
            } else {
                std::env::remove_var("MAKIBA_SECRETARY_REMOTE_CONTROL");
            }
        }
        Org {
            root,
            dir,
            base: Shared(Arc::new(Recording::default())),
            daemon: None,
            _tmp: tmp,
            _env: env,
        }
    }

    /// Start the daemon as `makiba up` does, with the secretary.
    fn start(&mut self) {
        let (dir, base) = (self.dir.clone(), self.base.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle =
            std::thread::spawn(move || daemon::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, handle));
    }

    fn stop(&mut self) {
        if let Some((stop, daemon)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
        }
    }

    fn db(&self) -> PathBuf {
        self.dir.join("state.db")
    }

    fn secretaries(&self) -> Vec<Terminal> {
        self.base
            .list()
            .unwrap()
            .into_iter()
            .filter(|t| t.name == "secretary")
            .collect()
    }

    /// The launch specs of the terminal named `name`.
    fn specs(&self, name: &str) -> Vec<LaunchSpec> {
        let launched = self.base.0.launched.lock().unwrap();
        launched
            .iter()
            .filter(|s| s.name == name)
            .cloned()
            .collect()
    }

    fn secretary_launches(&self) -> i64 {
        count(
            &self.db(),
            "SELECT count(*) FROM run_launch WHERE role = 'secretary'",
        )
    }
}

/// Every regular file under `dir`, `target/` and hidden entries left out.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        let p = e.path();
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => out.extend(files(&p)),
            Ok(m) if m.is_file() => out.push(p),
            _ => {}
        }
    }
    out
}

/// After `up`, one terminal named `secretary` with `makiba.role=secretary` exists; a second `up`
/// launches no second one; its folder holds no file of the makiba checkout that its settings do
/// not name.
#[test]
fn up_launches_one_secretary_and_a_second_up_launches_none() {
    let mut org = Org::new(false);
    org.start();
    let first = wait(|| org.secretaries().into_iter().next())
        .expect("no terminal named secretary within 20 s of up");
    assert_eq!(org.secretaries().len(), 1);
    assert_eq!(
        first.tags.get("makiba.role").map(String::as_str),
        Some("secretary")
    );
    let canonical = std::fs::canonicalize(&org.dir).unwrap();
    assert_eq!(
        first.tags.get("makiba.org").map(String::as_str),
        Some(canonical.to_str().unwrap())
    );
    let spec = org.specs("secretary").remove(0);
    assert_eq!(spec.cwd, org.dir.join("secretary"));

    // A second `up`: makibad is started again, the secretary still runs.
    org.stop();
    org.start();
    wait(|| {
        (count(
            &org.db(),
            "SELECT count(*) FROM events WHERE kind = 'dispatcher_started'",
        ) >= 2)
            .then_some(())
    })
    .expect("the second makibad did not subscribe within 20 s");
    // Its passes run every tick: give them a few seconds to launch a second one, if they would.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(org.secretaries().len(), 1, "a second secretary runs");
    assert_eq!(org.specs("secretary").len(), 1, "a second launch was asked");
    assert_eq!(org.secretary_launches(), 1);
    org.stop();

    // The folder: made, and holding no file of the checkout unless the settings name it.
    let folder = org.dir.join("secretary");
    assert!(folder.is_dir(), "no secretary folder {}", folder.display());
    let settings = value(&spec.argv, "--settings").expect("no settings file");
    let settings = std::fs::read_to_string(settings).unwrap();
    let checkout: Vec<Vec<u8>> = files(Path::new(env!("CARGO_MANIFEST_DIR")))
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .collect();
    for f in files(&folder) {
        let body = std::fs::read(&f).unwrap();
        assert!(
            !checkout.contains(&body) || settings.contains(&f.display().to_string()),
            "{} is a file of the checkout the settings do not name",
            f.display()
        );
    }
}

/// Remote Control: on, the secretary's argv carries the option and a worker's does not; off,
/// neither does.
#[test]
fn remote_control_reaches_the_secretary_only_and_only_when_on() {
    for on in [true, false] {
        let mut org = Org::new(on);
        {
            let w = StateWriter::open(&org.db()).unwrap();
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
                             'claude_code', 'the brief', ?1)",
                    [org.root.join("docs/t1").to_string_lossy()],
                )
                .unwrap();
        }
        org.start();
        let (secretary, worker) = wait(|| {
            let s = org.specs("secretary").into_iter().next()?;
            let w = org.specs("w-t1").into_iter().next()?;
            Some((s, w))
        })
        .expect("the secretary and the worker were not both launched within 20 s");
        assert_eq!(
            secretary.argv.iter().any(|a| a == REMOTE),
            on,
            "secretary, setting {on}: {:?}",
            secretary.argv
        );
        assert!(
            !worker.argv.iter().any(|a| a.starts_with(REMOTE)),
            "worker, setting {on}: {:?}",
            worker.argv
        );
        assert!(
            !secretary
                .argv
                .iter()
                .any(|a| a.starts_with(REMOTE) && a != REMOTE)
        );
    }
}

/// `makiba up`, run twice as the person runs it: one secretary launch, in `<state>/secretary/`.
#[test]
fn the_up_command_asks_for_the_secretary_once() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let (a, b, c) = makiba::agents::version::LOWEST;
    script(
        &bin.join("claude"),
        &format!("echo '{a}.{b}.{c} (Claude Code)'"),
    );
    script(&bin.join("hc"), "exit 0");
    let up = || -> Output {
        Command::new(env!("CARGO_BIN_EXE_makiba"))
            .arg("up")
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("HC_BIN", bin.join("hc"))
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .output()
            .unwrap()
    };
    let state = root.join("state/makiba");
    /// Ends the `makibad` that `up` started, passed or failed.
    struct Kill(PathBuf);
    impl Drop for Kill {
        fn drop(&mut self) {
            let pid: Option<i32> = std::fs::read_to_string(&self.0)
                .ok()
                .and_then(|p| p.trim().parse().ok());
            if let Some(pid) = pid {
                unsafe { libc::kill(pid, libc::SIGTERM) };
                let until = Instant::now() + Duration::from_secs(10);
                while Instant::now() < until && unsafe { libc::kill(pid, 0) } == 0 {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    let _kill = Kill(state.join("makibad.lock"));
    let db = state.join("state.db");
    let launches = || {
        count(
            &db,
            "SELECT count(*) FROM run_launch WHERE role = 'secretary'",
        )
    };
    let out = up();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{said}");
    wait(|| (launches() >= 1).then_some(()))
        .unwrap_or_else(|| panic!("no secretary launch within 20 s of up: {said}"));
    let folder: String = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row(
            "SELECT worktree FROM run_launch WHERE role = 'secretary'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(PathBuf::from(folder), state.join("secretary"));

    let again = up();
    assert!(again.status.success());
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(launches(), 1, "a second up asked for a second secretary");
}
