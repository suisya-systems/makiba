//! Issue 48 (M6-7): language settings.
//!
//! The person sets `conversation_language` and a project's `artifact_language` with `config set`
//! (through the door in this process, before `makibad` runs, so nothing races its writes); then
//! the daemon runs in this process on a stand-in base that keeps every launch spec, launching the
//! secretary as `makiba up` asks it to and two queued workers. The sentences are read from
//! `roles/locale/` (this file holds English only).

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The environment is the process's: one test at a time holds it.
static ENV: Mutex<()> = Mutex::new(());

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
    _knowledge: makibad::Knowledge,
    _env: MutexGuard<'static, ()>,
}

impl Drop for Org {
    fn drop(&mut self) {
        if let Some((stop, daemon)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = daemon.join();
        }
    }
}

impl Org {
    /// An organization with a stand-in `claude` on the path, in an English locale.
    fn new() -> Org {
        let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dir = root.join("state");
        let bin = root.join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        common::exe::write(&claude, "#!/bin/sh\nexit 1\n");
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
        // SAFETY: the environment lock is held, and no daemon of this test runs yet.
        unsafe {
            std::env::set_var("PATH", &path);
            std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
            std::env::set_var("MAKIBA_PERSON_KEY", root.join("elsewhere/person.key"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::remove_var("MAKIBA_FAKE_AGENT");
            std::env::remove_var("MAKIBA_LAUNCH_AGENT");
            std::env::remove_var("MAKIBA_SECRETARY_REMOTE_CONTROL");
            std::env::set_var("MAKIBA_SECRETARY", "1");
            std::env::set_var("LC_ALL", "C");
            std::env::set_var("LANG", "C");
            std::env::remove_var("LC_MESSAGES");
        }
        Org {
            root,
            dir,
            base: Shared(Arc::new(Recording::default())),
            daemon: None,
            _tmp: tmp,
            _knowledge: held_knowledge,
            _env: env,
        }
    }

    fn db(&self) -> PathBuf {
        self.dir.join("state.db")
    }

    /// `makiba config set` by the person, each accepted; then a queued worker run of each
    /// `(task, project)`. Done before `makibad` starts.
    fn prepare(&self, sets: &[serde_json::Value], runs: &[(&str, &str)]) {
        let w = StateWriter::open(&self.db()).unwrap();
        let key = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "claude_code".into());
        for args in sets {
            let r = door.handle(Some(&key), "config", args.clone(), None, false);
            assert!(r.is_ok(), "config {args}: {r:?}");
        }
        drop(door);
        let w = StateWriter::open(&self.db()).unwrap();
        for (task, project) in runs {
            w.ensure_project(project, None, None).unwrap();
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: project,
                status: Some("queued"),
                ..Default::default()
            })
            .unwrap();
            w.conn()
                .execute(
                    "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                     VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'document',
                             'claude_code', 'the brief', ?3)",
                    [
                        task.to_string(),
                        format!("spawn-{task}"),
                        self.root.join("docs").join(task).to_string_lossy().into(),
                    ],
                )
                .unwrap();
        }
    }

    fn start(&mut self) {
        let (dir, base) = (self.dir.clone(), self.base.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, handle));
    }

    /// The task text (the last argument) of the first launch of the terminal named `name`.
    fn task_text(&self, name: &str) -> String {
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            let launched = self.base.0.launched.lock().unwrap();
            if let Some(s) = launched.iter().find(|s| s.name == name) {
                return s.argv.last().cloned().unwrap_or_default();
            }
            drop(launched);
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("{name} was not launched within 20 s");
    }
}

/// The one sentence of `roles/locale/<kind>/<code>.md`.
fn sentence(kind: &str, code: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("roles/locale")
        .join(kind)
        .join(format!("{code}.md"));
    let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let s = s.trim().to_string();
    assert!(!s.is_empty() && !s.contains('\n'), "{}: {s:?}", p.display());
    s
}

/// Holds Hiragana, Katakana or CJK ideographs.
fn japanese(s: &str) -> bool {
    s.chars()
        .any(|c| ('\u{3040}'..='\u{30ff}').contains(&c) || ('\u{4e00}'..='\u{9fff}').contains(&c))
}

#[test]
fn conversation_language_ja_puts_the_japanese_sentence_in_the_secretary_launch_and_en_does_not() {
    for (code, want) in [("ja", true), ("en", false)] {
        let mut org = Org::new();
        org.prepare(
            &[json!({"action": "set", "key": "conversation_language", "value": code})],
            &[],
        );
        let ja = sentence("conversation", "ja");
        assert!(japanese(&ja), "{ja:?}");
        org.start();
        let text = org.task_text("secretary");
        assert_eq!(
            text.contains(&ja),
            want,
            "conversation_language={code}: {text:?}"
        );
    }
}

#[test]
fn a_project_without_artifact_language_gives_the_english_sentence_and_ja_the_japanese_one() {
    let mut org = Org::new();
    org.prepare(
        &[json!({"action": "set", "key": "artifact_language", "value": "ja", "project": "p-ja"})],
        &[("t-plain", "p-plain"), ("t-ja", "p-ja")],
    );
    let (en, ja) = (sentence("artifact", "en"), sentence("artifact", "ja"));
    assert!(japanese(&ja) && !japanese(&en), "{en:?} {ja:?}");
    org.start();
    let plain = org.task_text("w-t-plain");
    assert!(
        plain.starts_with("the brief") && plain.contains(&en) && !plain.contains(&ja),
        "{plain:?}"
    );
    let set = org.task_text("w-t-ja");
    assert!(
        set.starts_with("the brief") && set.contains(&ja) && !set.contains(&en),
        "{set:?}"
    );
}
