//! Issue 243, what was left after the launch's own temporary folder: Claude Code's sandbox adds
//! `/tmp/claude` (and `/private/tmp/claude`) to every write surface on its own, beside what the
//! settings name (measured 2026-11-26, Claude Code 2.1.332; probes.md, run-tmp-dir). A folder
//! anyone keeps there would be writable by every launch. The settings of a prepared launch deny
//! writing both, for the sandbox (`denyWrite`) and for the file tools (an `Edit` deny rule), and
//! still hold the launch's own temporary folder in `allowWrite`.
//!
//! One organization per test process: `makibad` runs in this process (`daemon::run_until`,
//! `MAKIBA_DOOR=none`) on a recording wrapper of the stand-in base, with the `fake` agent (which
//! writes its settings through the same code as Claude Code's) and `TMPDIR` set to a scratch
//! folder `tmp/` beside the others. It prepares and launches a code run `t1`, a document run `t2`
//! and a secretary and a curator without a run.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon::launch::add_launch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

/// Claude Code's own shared folder, by the literal paths its sandbox adds.
const CLAUDE_SHARED: [&str; 2] = ["/tmp/claude", "/private/tmp/claude"];

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

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@e"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One prepared and launched launch: its settings file and its row id.
struct Launched {
    name: String,
    settings: PathBuf,
    id: i64,
}

fn wait<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

/// Make the organization, start `makibad` and wait until every launch is launched (its settings
/// are written before). Returns the state folder, the launches and the knowledge folder's hold,
/// which the caller keeps while the daemon runs.
fn launched() -> (PathBuf, Vec<Launched>, makibad::Knowledge) {
    let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
    let state = root.join("state");
    for f in [
        "state",
        "tmp",
        "share",
        "knowledge",
        "secretary",
        "curator",
        "workers",
        "docs",
    ] {
        std::fs::create_dir_all(root.join(f)).unwrap();
    }
    let repo = root.join("repos/p");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let origin = root.join("origin.git");
    git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    let held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
    // SAFETY: this file has one test, which sets these before it starts the daemon thread.
    unsafe {
        std::env::set_var("MAKIBA_DOOR", "none");
        std::env::set_var(
            "MAKIBA_FAKE_AGENT",
            json!(["true", root.join("record")]).to_string(),
        );
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "600000");
        std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("person.key"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("TMPDIR", root.join("tmp"));
    }

    let (secretary_spawn, curator_spawn) = {
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        for (task, kind) in [("t1", "code"), ("t2", "document")] {
            w.upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                status: Some("queued"),
                ..Default::default()
            })
            .unwrap();
            let workplace = if kind == "document" {
                root.join("docs").join(task)
            } else {
                root.join("workers/p").join(task)
            };
            w.conn()
                .execute(
                    "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                     VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, ?3, 'fake',
                             'the brief', ?4)",
                    rusqlite::params![
                        task,
                        format!("spawn-{task}"),
                        kind,
                        workplace.to_string_lossy()
                    ],
                )
                .unwrap();
        }
        let s = add_launch(
            &w,
            Role::Secretary,
            &root.join("secretary"),
            "the brief",
            None,
            "fake",
        )
        .unwrap();
        let c = add_launch(
            &w,
            Role::Curator,
            &root.join("curator"),
            "the brief",
            None,
            "fake",
        )
        .unwrap();
        (s, c)
    };

    let base = Shared::default();
    {
        let (dir, base) = (state.clone(), base.clone());
        let stop = Arc::new(AtomicBool::new(false));
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), stop));
    }
    let rows: [(&str, String, PathBuf); 4] = [
        ("w-t1", "spawn-t1".into(), state.join("settings/t1.json")),
        ("w-t2", "spawn-t2".into(), state.join("settings/t2.json")),
        (
            "secretary",
            secretary_spawn.clone(),
            state.join(format!("settings/secretary-{secretary_spawn}.json")),
        ),
        (
            "curator",
            curator_spawn.clone(),
            state.join(format!("settings/curator-{curator_spawn}.json")),
        ),
    ];
    wait(|| {
        let launched = base.0.launched.lock().unwrap();
        rows.iter()
            .all(|(n, _, _)| launched.iter().any(|s| s.name == *n))
            .then_some(())
    })
    .unwrap_or_else(|| {
        let launched = base.0.launched.lock().unwrap();
        panic!(
            "not every launch was started within {WAIT:?}; launched: {:?}",
            launched.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });
    let db = Connection::open_with_flags(state.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let all = rows
        .iter()
        .map(|(name, spawn, settings)| Launched {
            name: name.to_string(),
            settings: settings.clone(),
            id: db
                .query_row(
                    "SELECT id FROM run_launch WHERE spawn_id = ?1",
                    [spawn],
                    |r| r.get(0),
                )
                .unwrap(),
        })
        .collect();
    (state, all, held_knowledge)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// A settings path as the sandbox reads it: an existing path resolved.
fn resolved(entry: &str) -> PathBuf {
    let p = PathBuf::from(entry);
    std::fs::canonicalize(&p).unwrap_or(p)
}

#[test]
fn the_settings_of_a_prepared_run_deny_claude_codes_own_folder_in_the_shared_temporary_folder() {
    let (state, launches, _knowledge) = launched();
    for l in &launches {
        let text = std::fs::read_to_string(&l.settings)
            .unwrap_or_else(|e| panic!("{}: {e}", l.settings.display()));
        let v: Value = serde_json::from_str(&text).unwrap();
        let fs = &v["sandbox"]["filesystem"];
        let deny_write = strings(&fs["denyWrite"]);
        let rules = strings(&v["permissions"]["deny"]);
        for shared in CLAUDE_SHARED {
            // (1) The sandbox (Bash) does not write it.
            assert!(
                deny_write.iter().any(|p| p == shared),
                "{}: sandbox.filesystem.denyWrite does not name {shared}: {text}",
                l.name
            );
            // (2) Nor do the file tools, which run outside the sandbox.
            let rule = format!("Edit(/{shared}/**)");
            assert!(
                rules.contains(&rule),
                "{}: permissions.deny has no {rule}: {text}",
                l.name
            );
        }
        // (3) No write entry is the shared folder, Claude Code's folder in it, or inside that.
        let allow_write = strings(&fs["allowWrite"]);
        for e in &allow_write {
            for wide in ["/tmp", "/private/tmp"] {
                assert_ne!(
                    e, wide,
                    "{}: allowWrite names the shared temporary folder: {text}",
                    l.name
                );
            }
            for shared in CLAUDE_SHARED {
                assert!(
                    !Path::new(e).starts_with(shared),
                    "{}: allowWrite {e} is Claude Code's shared folder {shared} or inside it: {text}",
                    l.name
                );
            }
        }
        // (4) The launch's own temporary folder is still its to write.
        let own = std::fs::canonicalize(state.join("tmp").join(l.id.to_string()))
            .unwrap_or_else(|e| panic!("{}: no temporary folder of its own: {e}", l.name));
        assert!(
            allow_write.iter().any(|e| resolved(e) == own),
            "{}: allowWrite does not hold its own temporary folder {}: {text}",
            l.name,
            own.display()
        );
    }
}
