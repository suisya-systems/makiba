//! Issue 196: the organization's terminals carry the labels herdr-app places a terminal by, and
//! `makiba up` prints the line that registers the base's session on the app's side.
//!
//! The launches run through `makibad`'s own code in this process (`daemon::run_until`) on a
//! recording wrapper of the in-process stand-in base, so no real base and no socket are used
//! (`MAKIBA_DOOR=none`). `makiba up` runs as tests/up.rs runs it: a stand-in `hc` and `claude` on
//! PATH, the stand-in base for `makibad`, every folder a temporary one.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon::launch::add_launch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Recording {
    stub: Stub,
    launched: Mutex<Vec<LaunchSpec>>,
}

#[derive(Clone, Default)]
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

fn script(path: &Path, body: &str) {
    common::exe::write(path, format!("#!/bin/sh\n{body}\n"));
}

/// The first 32 lowercase hex digits of the SHA-256 of `text`.
fn id(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..32]
        .to_string()
}

#[test]
fn a_secretary_a_worker_and_a_curator_launch_with_the_app_labels_in_one_space() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["work", "secretary", "curator", "share", "knowledge"] {
        std::fs::create_dir_all(root.join(f)).unwrap();
    }
    let _held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
    // SAFETY: the only test of this binary that sets variables, before its daemon thread starts.
    unsafe {
        std::env::set_var("MAKIBA_DOOR", "none");
        // The test agent: a program and its record file. The stand-in base never runs it.
        std::env::set_var(
            "MAKIBA_FAKE_AGENT",
            serde_json::json!(["true", root.join("record")]).to_string(),
        );
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "600000");
        std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("person.key"));
    }

    {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'document',
                         'fake', 'the brief', ?1, 'settings_ready')",
                [root.join("work").to_string_lossy()],
            )
            .unwrap();
        add_launch(
            &w,
            Role::Secretary,
            &root.join("secretary"),
            "the brief",
            None,
            "fake",
        )
        .unwrap();
        add_launch(
            &w,
            Role::Curator,
            &root.join("curator"),
            "the brief",
            None,
            "fake",
        )
        .unwrap();
    }

    let base = Shared::default();
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (dir, base, stop) = (dir.clone(), base.clone(), stop.clone());
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), stop))
    };
    let names = ["secretary", "w-t1", "curator"];
    let specs = wait(30, || {
        let launched = base.0.launched.lock().unwrap();
        names
            .iter()
            .map(|n| launched.iter().find(|s| s.name == *n).cloned())
            .collect::<Option<Vec<_>>>()
    });
    stop.store(true, Ordering::SeqCst);
    let ended = daemon.join();
    let specs = specs.unwrap_or_else(|| {
        let launched = base.0.launched.lock().unwrap();
        panic!(
            "not every role was launched within 30 s; launched: {:?}; makibad ended: {ended:?}",
            launched.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });

    let org = dir.to_string_lossy().into_owned();
    let space = format!("sp{}", id(&org));
    let mut tabs = Vec::new();
    for (name, spec) in names.iter().zip(&specs) {
        let label = |k: &str| {
            spec.tags
                .get(k)
                .unwrap_or_else(|| panic!("the {name} launch has no {k} label: {:?}", spec.tags))
                .clone()
        };
        assert_eq!(
            label("herdr.space"),
            space,
            "{name}: not the organization's space"
        );
        assert_eq!(
            label("herdr.tab"),
            format!("tb{}", id(&format!("{org}\n{name}"))),
            "{name}: not its own tab"
        );
        assert_eq!(label("where"), format!("makiba / {name}"));
        tabs.push(label("herdr.tab"));
    }
    tabs.sort();
    tabs.dedup();
    assert_eq!(tabs.len(), 3, "two terminals share a tab");
}

/// `makiba up` as tests/up.rs runs it, with the base's session `session`.
fn up(root: &Path, session: &str) -> Output {
    let lowest = makiba::agents::version::LOWEST;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(
        &bin.join("claude"),
        &format!(
            "echo '{}.{}.{} (Claude Code)'",
            lowest.0, lowest.1, lowest.2
        ),
    );
    script(&bin.join("hc"), "exit 0");
    makibad::up(root)
        .arg("--no-secretary")
        .env_clear()
        .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(root))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("HC_BIN", bin.join("hc"))
        .env("MAKIBA_BASE", "stub")
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE_SESSION", session)
        .output()
        .unwrap()
}

/// Ends the `makibad` that `makiba up` started under `root`, passed or failed.
struct Makibad(PathBuf);

impl Drop for Makibad {
    fn drop(&mut self) {
        let lock = self.0.join("state/makiba/makibad.lock");
        let Some(pid) = std::fs::read_to_string(lock)
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok())
        else {
            return;
        };
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until && unsafe { libc::kill(pid, 0) } == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn makiba_up_prints_the_line_that_registers_its_session() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let _makibad = Makibad(root.clone());
    let session = "makiba-app-196";
    let want = format!("hc.exe machine add wsl --name {session} --session {session}");
    for round in ["first", "second"] {
        let out = up(&root, session);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{round} up failed: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.lines().any(|l| l.ends_with(&want)),
            "the {round} up did not print `{want}`:\n{stdout}"
        );
    }
}
