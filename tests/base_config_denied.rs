//! Issue 133: the configuration folder `makiba up` gives the base, `${XDG_CONFIG_HOME:-~/.config}/
//! makiba/base` (spec section 2), is part of `base::worker_denies()`, so it is denied for read and
//! write by every role's settings file (at its default place, the only one a static file can name)
//! and by the per-task settings written for a run (at the place resolved when they are written).
//! One test: it changes the environment in turn, before and while the daemon runs.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::db::writer::{RunUpsert, StateWriter};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ROLES: [&str; 3] = ["secretary", "worker", "curator"];
/// The folder at its default place, as the role settings name it.
const DEFAULT: &str = "~/.config/makiba/base";

/// The stand-in, with the denies of the real base as they are resolved when asked.
struct RealDenies(Stub);

impl Base for RealDenies {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.0.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.0.launch(spec)
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        self.0.list()
    }
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.0.get(t)
    }
    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.0.read(t, lines)
    }
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.0.keys(t, keys)
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        self.0.send(t, text, only_if_idle)
    }
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        self.0.end(t)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.0.events()
    }
    fn start(&self) -> Result<(), BaseError> {
        self.0.start()
    }
    fn worker_denies(&self) -> BaseDenies {
        makiba::base::worker_denies()
    }
}

fn strings(v: &Value, pointer: &str) -> Vec<String> {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// `folder` is denied for read and write: by the sandbox (Bash) and by a `Read` and an `Edit`
/// rule on everything under it (the tools, which run outside the sandbox).
fn assert_denied(what: &str, v: &Value, folder: &str) {
    for list in ["denyRead", "denyWrite"] {
        assert!(
            strings(v, &format!("/sandbox/filesystem/{list}")).contains(&folder.to_string()),
            "{what}: sandbox.filesystem.{list} does not name {folder}"
        );
    }
    let deny = strings(v, "/permissions/deny");
    let pattern = if folder.starts_with('/') {
        format!("/{folder}/**")
    } else {
        format!("{folder}/**")
    };
    for tool in ["Read", "Edit"] {
        let rule = format!("{tool}({pattern})");
        assert!(deny.contains(&rule), "{what}: no {rule}");
    }
}

fn shown(ps: &[PathBuf]) -> Vec<String> {
    ps.iter().map(|p| p.display().to_string()).collect()
}

fn wait_for(path: &Path) -> bool {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// `worker_denies().folders` lists the default place, and every role's settings file denies it.
fn default_place_is_denied_by_every_role() {
    assert!(
        shown(&makiba::base::worker_denies().folders).contains(&DEFAULT.to_string()),
        "worker_denies().folders does not list {DEFAULT}"
    );
    for role in ROLES {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("roles/{role}/settings.json"));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_denied(role, &v, DEFAULT);
    }
}

#[test]
fn the_base_configuration_folder_is_denied_by_every_role_and_by_the_task_settings() {
    // 1. At its default place: in the base's denies and in every role's settings file.
    // SAFETY: the only test of this binary, and no thread of it has started yet.
    unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    default_place_is_denied_by_every_role();

    // 2. Moved by `XDG_CONFIG_HOME` out of the home: the default place stays where it was, and
    // the moved one is in the base's denies resolved in this process (with the socket folder,
    // which no static file can name either) and in the per-task settings.
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let moved = root.join("config/makiba/base");
    let dir = root.join("state");
    let docs = root.join("docs");
    std::fs::create_dir_all(&dir).unwrap();
    let agent = serde_json::json!([root.join("no-such-agent"), root.join("record")]).to_string();
    // SAFETY: as above, still before any thread.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("person.key"));
        std::env::set_var("MAKIBA_DOCS_DIR", &docs);
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("MAKIBA_FAKE_AGENT", &agent);
    }
    default_place_is_denied_by_every_role();
    let moved_text = moved.display().to_string();
    assert!(
        shown(&makiba::base::worker_denies().resolved).contains(&moved_text),
        "worker_denies().resolved does not list {moved_text}"
    );
    {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
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
                [docs.join("t1").to_string_lossy()],
            )
            .unwrap();
    }
    let daemon_dir: PathBuf = dir.clone();
    std::thread::spawn(move || makibad::run(&daemon_dir, &|| Box::new(RealDenies(Stub::new()))));
    let file = dir.join("settings/t1.json");
    assert!(wait_for(&file), "no settings written within 20 s");
    // The file is written whole once; read it until it parses.
    let v: Value = {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(v) = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()) {
                break v;
            }
            assert!(Instant::now() < until, "{} is not JSON", file.display());
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    assert_denied("the per-task settings", &v, &moved_text);
}
