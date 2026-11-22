//! Issue 243: a launch has a temporary folder of its own. The settings of a prepared launch give
//! no write to the shared temporary folder (`makibad`'s own `TMPDIR`): the launch writes
//! `<state>/tmp/<launch id>`, made at preparation with mode 0700, which its launch environment
//! and its settings environment name as `TMPDIR` and `CLAUDE_CODE_TMPDIR`. No launch can write
//! another's, and close removes the run's.
//!
//! One organization per test process: `makibad` runs in this process (`daemon::run_until`,
//! `MAKIBA_DOOR=none`) on a recording wrapper of the stand-in base, with the `fake` agent and
//! `TMPDIR` set to a scratch folder `tmp/` beside the others. It prepares and launches a code run
//! `t1`, a document run `t2`, a code run `t3` (the one closed) and a secretary and a curator
//! without a run. The door runs in this process too, on the same `state.db`, with the person's
//! key.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::launch::add_launch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

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

/// One prepared and launched launch: its settings file, its row id and what the base was given.
#[derive(Clone)]
struct Launched {
    name: String,
    settings: PathBuf,
    id: i64,
    spec: LaunchSpec,
}

struct Org {
    root: PathBuf,
    state: PathBuf,
    t1: Launched,
    t2: Launched,
    t3: Launched,
    secretary: Launched,
    curator: Launched,
    _knowledge: makibad::Knowledge,
}

impl Org {
    /// The shared temporary folder: `makibad`'s `TMPDIR`.
    fn shared_tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Where a launch's own temporary folder is meant to be.
    fn own_tmp(&self, l: &Launched) -> PathBuf {
        self.state.join("tmp").join(l.id.to_string())
    }

    fn conn(&self) -> Connection {
        let c = Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c
    }
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

/// The organization, made, started and launched once per process.
fn org() -> &'static Org {
    static ORG: OnceLock<Org> = OnceLock::new();
    ORG.get_or_init(|| {
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
        // SAFETY: set once, by the first test to get here, before this process starts its
        // daemon thread; every other test waits on this initialisation.
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
            for (task, kind) in [("t1", "code"), ("t2", "document"), ("t3", "code")] {
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
        let rows: [(&str, String, PathBuf); 5] = [
            ("w-t1", "spawn-t1".into(), state.join("settings/t1.json")),
            ("w-t2", "spawn-t2".into(), state.join("settings/t2.json")),
            ("w-t3", "spawn-t3".into(), state.join("settings/t3.json")),
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
        let specs = wait(|| {
            let launched = base.0.launched.lock().unwrap();
            rows.iter()
                .map(|(n, _, _)| launched.iter().find(|s| s.name == *n).cloned())
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_else(|| {
            let launched = base.0.launched.lock().unwrap();
            panic!(
                "not every launch was started within {WAIT:?}; launched: {:?}",
                launched.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });
        let db =
            Connection::open_with_flags(state.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let mut all: Vec<Launched> = rows
            .iter()
            .zip(specs)
            .map(|((name, spawn, settings), spec)| Launched {
                name: name.to_string(),
                settings: settings.clone(),
                id: db
                    .query_row(
                        "SELECT id FROM run_launch WHERE spawn_id = ?1",
                        [spawn],
                        |r| r.get(0),
                    )
                    .unwrap(),
                spec,
            })
            .collect();
        let curator = all.pop().unwrap();
        let secretary = all.pop().unwrap();
        let t3 = all.pop().unwrap();
        let t2 = all.pop().unwrap();
        let t1 = all.pop().unwrap();
        Org {
            root,
            state,
            t1,
            t2,
            t3,
            secretary,
            curator,
            _knowledge: held_knowledge,
        }
    })
}

/// A settings path as the sandbox reads it: `~/` is the home folder; an existing path resolved.
fn resolved(entry: &str) -> PathBuf {
    let p = match entry.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap()).join(rest),
        None => PathBuf::from(entry),
    };
    std::fs::canonicalize(&p).unwrap_or(p)
}

fn settings(l: &Launched) -> (Value, String) {
    let text = std::fs::read_to_string(&l.settings)
        .unwrap_or_else(|e| panic!("{}: {e}", l.settings.display()));
    (serde_json::from_str(&text).unwrap(), text)
}

/// The resolved `allowWrite` entries of a launch's settings, and the text.
fn allow_write(l: &Launched) -> (Vec<PathBuf>, String) {
    let (v, text) = settings(l);
    let entries = v["sandbox"]["filesystem"]["allowWrite"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: no allowWrite list: {text}", l.name))
        .iter()
        .map(|e| resolved(e.as_str().unwrap()))
        .collect();
    (entries, text)
}

/// Whether `a` is `b` or above it.
fn reaches(a: &Path, b: &Path) -> bool {
    b.starts_with(a)
}

/// The launch's own temporary folder: it exists, is a folder of mode 0700 and is in its
/// `allowWrite`.
fn own_folder(org: &Org, l: &Launched) -> PathBuf {
    let own = org.own_tmp(l);
    let meta = std::fs::symlink_metadata(&own).unwrap_or_else(|e| {
        panic!(
            "{}: no temporary folder of its own at {}: {e}",
            l.name,
            own.display()
        )
    });
    assert!(
        meta.is_dir(),
        "{}: {} is not a folder",
        l.name,
        own.display()
    );
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o700,
        "{}: {} is not mode 0700",
        l.name,
        own.display()
    );
    let own = std::fs::canonicalize(own).unwrap();
    let (entries, text) = allow_write(l);
    assert!(
        entries.contains(&own),
        "{}: allowWrite does not hold its own temporary folder {}: {text}",
        l.name,
        own.display()
    );
    own
}

/// The run's settings give no write to the shared temporary folder: no `allowWrite` entry is it,
/// above it or inside it.
fn no_shared_tmp(org: &Org, l: &Launched) {
    let shared = std::fs::canonicalize(org.shared_tmp()).unwrap();
    let (entries, text) = allow_write(l);
    for e in &entries {
        assert!(
            !reaches(e, &shared) && !e.starts_with(&shared),
            "{}: allowWrite {} reaches the shared temporary folder {}: {text}",
            l.name,
            e.display(),
            shared.display()
        );
    }
}

#[test]
fn the_settings_of_a_prepared_run_allow_no_write_to_the_shared_temporary_folder() {
    let org = org();
    for l in [&org.t1, &org.t2] {
        no_shared_tmp(org, l);
        own_folder(org, l);
    }
}

#[test]
fn a_prepared_run_has_a_temporary_folder_of_its_own_in_its_write_surface_and_environment() {
    let org = org();
    for l in [&org.t1, &org.t2] {
        let own = own_folder(org, l);
        let (v, text) = settings(l);
        for var in ["TMPDIR", "CLAUDE_CODE_TMPDIR"] {
            let in_spec = l.spec.env.get(var).unwrap_or_else(|| {
                panic!(
                    "{}: the launch environment has no {var}: {:?}",
                    l.name, l.spec.env
                )
            });
            assert_eq!(
                resolved(in_spec),
                own,
                "{}: the launch environment's {var}",
                l.name
            );
            let in_settings = v["env"][var]
                .as_str()
                .unwrap_or_else(|| panic!("{}: the settings' env has no {var}: {text}", l.name));
            assert_eq!(
                resolved(in_settings),
                own,
                "{}: the settings' env {var}: {text}",
                l.name
            );
            assert!(
                !l.spec.unset.iter().any(|u| u == var),
                "{}: {var} is unset by the launch",
                l.name
            );
        }
    }
}

#[test]
fn no_run_can_write_another_runs_temporary_folder() {
    let org = org();
    let launches = [&org.t1, &org.t2, &org.secretary, &org.curator];
    let tmp_root = org.state.join("tmp");
    for l in launches {
        let own = own_folder(org, l);
        let (entries, text) = allow_write(l);
        for e in &entries {
            assert!(
                !reaches(e, &std::fs::canonicalize(&tmp_root).unwrap()),
                "{}: allowWrite {} reaches every launch's temporary folder: {text}",
                l.name,
                e.display()
            );
        }
        for other in launches.iter().filter(|o| o.id != l.id) {
            let theirs = std::fs::canonicalize(org.own_tmp(other)).unwrap_or(org.own_tmp(other));
            assert_ne!(own, theirs, "{} and {} share one", l.name, other.name);
            for e in &entries {
                assert!(
                    !reaches(e, &theirs),
                    "{}: allowWrite {} reaches the temporary folder of {} at {}: {text}",
                    l.name,
                    e.display(),
                    other.name,
                    theirs.display()
                );
            }
        }
    }
}

#[test]
fn close_removes_the_runs_temporary_folder() {
    let org = org();
    let t3 = own_folder(org, &org.t3);
    std::fs::write(t3.join("left-behind"), "scratch").unwrap();
    let t1 = own_folder(org, &org.t1);
    let w = StateWriter::open(&org.state.join("state.db")).unwrap();
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
    let r = door.handle(
        Some(&person),
        "close",
        json!({"task_id": "t3", "abandon": true}),
        None,
        false,
    );
    assert!(r.is_ok(), "{r:?}");
    let sql = "SELECT status FROM commands WHERE verb = 'close'
               AND json_extract(args_json, '$.task_id') = 't3'
               AND status IN ('done', 'failed') ORDER BY id DESC LIMIT 1";
    let status = wait(|| {
        org.conn()
            .query_row(sql, [], |r| r.get::<_, String>(0))
            .ok()
    })
    .expect("the close of t3 is not finished");
    assert_eq!(status, "done");
    assert!(
        !t3.exists(),
        "close left the run's temporary folder {}",
        t3.display()
    );
    assert!(t1.is_dir(), "close removed another run's {}", t1.display());
    assert!(org.state.join("tmp").is_dir(), "close removed <state>/tmp");
}

#[test]
fn the_curator_and_the_secretary_get_one_too() {
    let org = org();
    for l in [&org.secretary, &org.curator] {
        no_shared_tmp(org, l);
        let own = own_folder(org, l);
        let (v, text) = settings(l);
        for var in ["TMPDIR", "CLAUDE_CODE_TMPDIR"] {
            assert_eq!(
                l.spec.env.get(var).map(|s| resolved(s)),
                Some(own.clone()),
                "{}: the launch environment's {var}: {:?}",
                l.name,
                l.spec.env
            );
            assert_eq!(
                v["env"][var].as_str().map(resolved),
                Some(own.clone()),
                "{}: the settings' env {var}: {text}",
                l.name
            );
        }
    }
}
