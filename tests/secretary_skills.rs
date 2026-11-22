//! Issue 182: the secretary's skills are placed where the secretary finds them.
//!
//! The design names the place: the plugin folder `<share>/plugins/makiba`, passed to the
//! secretary's Claude Code as `--plugin-dir`, its skills under `skills/<name>/` (named
//! `makiba:<name>` in the session). The daemon runs in this process on the stand-in base (which
//! runs nothing and keeps every launch spec), launching the secretary as `makiba up` asks it to,
//! with a stand-in `claude` on the path. No real `claude` and no real base is started.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    share: PathBuf,
    dir: PathBuf,
    base: Shared,
    daemon: Option<Daemon>,
    _tmp: tempfile::TempDir,
    _knowledge: makibad::Knowledge,
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

/// Poll `f` for up to `secs` seconds.
fn wait<T>(secs: u64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let until = Instant::now() + Duration::from_secs(secs);
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

/// Every regular file under `dir`, as (path relative to `dir`, content).
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => walk(root, &p, out),
                Ok(m) if m.is_file() => out.push((
                    p.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                )),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// The repository's skills: every file under `skills/`.
fn repository_skills() -> Vec<(PathBuf, Vec<u8>)> {
    let skills = files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("skills"));
    assert!(
        skills
            .iter()
            .any(|(p, _)| p == Path::new("delegate/SKILL.md")),
        "the repository has no skills/delegate/SKILL.md"
    );
    skills
}

impl Org {
    /// An organization with a stand-in `claude` on the path; a secretary launch fails its
    /// verification after one second, so a later start of the daemon launches a new one.
    fn new() -> Org {
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
        let share = root.join("share");
        let held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
        // SAFETY: the environment lock is held by the caller, and no daemon of this test runs yet.
        unsafe {
            std::env::set_var("PATH", &path);
            std::env::set_var("MAKIBA_SHARE_DIR", &share);
            std::env::set_var("MAKIBA_PERSON_KEY", root.join("elsewhere/person.key"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "1000");
            std::env::remove_var("MAKIBA_FAKE_AGENT");
            std::env::remove_var("MAKIBA_LAUNCH_AGENT");
            std::env::remove_var("MAKIBA_SECRETARY_REMOTE_CONTROL");
            std::env::remove_var("CLAUDE_CODE_SIMPLE");
            std::env::set_var("MAKIBA_SECRETARY", "1");
        }
        Org {
            share,
            dir,
            base: Shared(Arc::new(Recording::default())),
            daemon: None,
            _tmp: tmp,
            _knowledge: held_knowledge,
        }
    }

    /// Start the daemon as `makiba up` does, with the secretary.
    fn start(&mut self) {
        let (dir, base) = (self.dir.clone(), self.base.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, handle));
    }

    fn stop(&mut self) {
        if let Some((stop, daemon)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
        }
    }

    /// The launch specs of the secretary's terminals, in launch order.
    fn secretary_specs(&self) -> Vec<LaunchSpec> {
        let launched = self.base.0.launched.lock().unwrap();
        launched
            .iter()
            .filter(|s| s.name == "secretary")
            .cloned()
            .collect()
    }

    /// The place the design names.
    fn plugin(&self) -> PathBuf {
        self.share.join("plugins/makiba")
    }

    /// Wait until the secretary's launch spec number `n` (from 1) is recorded.
    fn wait_for_launch(&self, n: usize) -> LaunchSpec {
        wait(20, || self.secretary_specs().into_iter().nth(n - 1))
            .unwrap_or_else(|| panic!("secretary launch {n} not asked of the base within 20 s"))
    }

    /// Wait until `makibad` has recorded the secretary launch of `spawn` as failed.
    fn wait_failed(&self, spawn: &str) {
        let db = self.dir.join("state.db");
        wait(30, || {
            let stage: Option<String> =
                Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .and_then(|c| {
                        c.query_row(
                            "SELECT stage FROM run_launch WHERE spawn_id = ?1",
                            [spawn],
                            |r| r.get(0),
                        )
                    })
                    .ok();
            (stage.as_deref() == Some("failed")).then_some(())
        })
        .unwrap_or_else(|| panic!("the secretary launch {spawn} was not failed within 30 s"));
    }
}

fn spawn_of(spec: &LaunchSpec) -> String {
    spec.tags
        .get("makiba.spawn")
        .cloned()
        .expect("the launch spec has no makiba.spawn tag")
}

/// Every file under the repository's `skills/` is at `<plugin>/skills/` with the same content.
fn assert_placed(plugin: &Path) {
    for (rel, body) in repository_skills() {
        let placed = plugin.join("skills").join(&rel);
        let got = std::fs::read(&placed)
            .unwrap_or_else(|e| panic!("skill file {} not placed: {e}", placed.display()));
        assert!(
            got == body,
            "{} differs from skills/{}",
            placed.display(),
            rel.display()
        );
    }
}

/// After the secretary's launch, every skill of the repository is at the place, with the same
/// content.
#[test]
fn the_secretary_launch_places_every_skill_of_the_repository() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let mut org = Org::new();
    org.start();
    org.wait_for_launch(1);
    org.stop();
    assert_placed(&org.plugin());
}

/// The launch makes the place one Claude Code reads for skills: the secretary's argv passes it as
/// `--plugin-dir`, and neither its argv nor its environment turns skills off.
#[test]
fn the_secretary_argv_passes_the_place_as_a_plugin_folder() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let mut org = Org::new();
    // A person's environment that turns bare mode on (no skills) does not reach the secretary.
    // SAFETY: the environment lock is held, and no daemon of this test runs yet.
    unsafe { std::env::set_var("CLAUDE_CODE_SIMPLE", "1") };
    org.start();
    let spec = org.wait_for_launch(1);
    org.stop();
    unsafe { std::env::remove_var("CLAUDE_CODE_SIMPLE") };
    let plugin = org.plugin();
    assert_eq!(
        value(&spec.argv, "--plugin-dir"),
        Some(plugin.to_str().unwrap()),
        "argv: {:?}",
        spec.argv
    );
    for off in ["--bare", "--disable-slash-commands"] {
        assert!(
            !spec.argv.iter().any(|a| a == off),
            "the secretary's argv carries {off}: {:?}",
            spec.argv
        );
    }
    assert!(
        spec.unset.iter().any(|v| v == "CLAUDE_CODE_SIMPLE"),
        "CLAUDE_CODE_SIMPLE is not removed from the secretary's environment: {:?}",
        spec.unset
    );
    // A plugin folder holds its skills as `skills/<name>/SKILL.md`.
    assert!(
        plugin.join("skills/delegate/SKILL.md").is_file(),
        "no skills/delegate/SKILL.md in {}",
        plugin.display()
    );
}

/// A second launch after a changed skill leaves the binary's content, and a file the person added
/// beside the skills is still there.
#[test]
fn a_second_launch_replaces_a_changed_skill_and_keeps_the_persons_file() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let mut org = Org::new();
    org.start();
    let first = org.wait_for_launch(1);
    // The stand-in runs nothing: the launch fails its verification, and the next start of the
    // daemon launches a new secretary.
    org.wait_failed(&spawn_of(&first));
    org.stop();

    let plugin = org.plugin();
    assert_placed(&plugin);
    let changed = plugin.join("skills/delegate/SKILL.md");
    std::fs::write(&changed, "an older binary's delegate skill\n").unwrap();
    let persons = plugin.join("skills/delegate/my-notes.md");
    let persons_skill = plugin.join("skills/my-own/SKILL.md");
    std::fs::write(&persons, "the person's notes\n").unwrap();
    std::fs::create_dir_all(persons_skill.parent().unwrap()).unwrap();
    std::fs::write(&persons_skill, "the person's own skill\n").unwrap();

    org.start();
    let second = org.wait_for_launch(2);
    org.stop();
    assert_ne!(spawn_of(&second), spawn_of(&first));
    assert_placed(&plugin);
    assert_eq!(
        std::fs::read_to_string(&persons).ok().as_deref(),
        Some("the person's notes\n")
    );
    assert_eq!(
        std::fs::read_to_string(&persons_skill).ok().as_deref(),
        Some("the person's own skill\n")
    );
}
