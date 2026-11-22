//! M4-2 (Issue 32): the shape of what `makibad` writes for a run. The daemon runs in this process
//! on a base of the test's own (the stand-in, with denies of its own and a record of every launch
//! spec it is asked for), so nothing is started. The person key is moved by `MAKIBA_PERSON_KEY`,
//! so its deny must name the place resolved when the settings are written, not the default one.
//! Shape only: the host rerun of probes P1 and P11 shows what a sandboxed Bash can reach.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The stand-in, with the denies given and every launch spec kept.
struct Recording {
    stub: Stub,
    denies: BaseDenies,
    launched: Arc<Mutex<Vec<LaunchSpec>>>,
}

impl Base for Recording {
    fn name(&self) -> &'static str {
        self.stub.name()
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        self.stub.hello()
    }
    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        self.launched.lock().unwrap().push(spec.clone());
        self.stub.launch(spec)
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        self.stub.list()
    }
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.stub.get(t)
    }
    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.stub.read(t, lines)
    }
    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.stub.keys(t, keys)
    }
    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        self.stub.send(t, text, only_if_idle)
    }
    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        self.stub.end(t)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.stub.events()
    }
    fn start(&self) -> Result<(), BaseError> {
        self.stub.start()
    }
    fn worker_denies(&self) -> BaseDenies {
        self.denies.clone()
    }
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

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every run of exactly 64 hex digits in `text`: what a key looks like (spec section 4).
fn keys_in(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|w| w.len() == 64)
        .map(String::from)
        .collect()
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

/// `path` is denied for read and write: by the sandbox (Bash) and by a `Read` and an `Edit`
/// permission rule (the tools, which run outside the sandbox).
fn assert_denied(v: &Value, path: &str) {
    let fs = &v["sandbox"]["filesystem"];
    for list in ["denyRead", "denyWrite"] {
        assert!(
            strings(&fs[list]).iter().any(|p| p == path),
            "sandbox.filesystem.{list} does not name {path}: {v}"
        );
    }
    let rules = strings(&v["permissions"]["deny"]);
    for tool in ["Read(", "Edit("] {
        assert!(
            rules
                .iter()
                .any(|r| r.starts_with(tool) && r.contains(path)),
            "no {tool}...) deny rule names {path}: {v}"
        );
    }
}

#[test]
fn the_settings_and_the_launch_written_for_a_run_have_their_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let docs = root.join("docs");
    let person_key = root.join("elsewhere/person.key");
    std::fs::create_dir_all(&dir).unwrap();
    let denies = BaseDenies {
        commands: vec!["hc".into()],
        resolved: vec![root.join("run/herdr-core")],
        folders: vec![
            PathBuf::from("~/.config/herdr-core"),
            PathBuf::from("~/.local/state/herdr-core"),
        ],
    };
    // The test agent is launchable (the stand-in runs nothing), so the launch spec is made.
    let agent = serde_json::json!([root.join("no-such-agent"), root.join("record")]).to_string();
    // SAFETY: the only test of this binary, and no thread of it has started yet.
    unsafe {
        std::env::set_var("MAKIBA_PERSON_KEY", &person_key);
        std::env::set_var("MAKIBA_DOCS_DIR", &docs);
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("MAKIBA_FAKE_AGENT", &agent);
    }
    let workplace = docs.join("t1");
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
                [workplace.to_string_lossy()],
            )
            .unwrap();
    }

    let launched = Arc::new(Mutex::new(Vec::new()));
    let (for_daemon, daemon_dir) = (Arc::clone(&launched), dir.clone());
    std::thread::spawn(move || {
        makibad::run(&daemon_dir, &|| {
            Box::new(Recording {
                stub: Stub::new(),
                denies: denies.clone(),
                launched: Arc::clone(&for_daemon),
            })
        })
    });
    let spec = wait(|| launched.lock().unwrap().first().cloned())
        .expect("the run was not launched within 20 s");

    let file = dir.join("settings/t1.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();

    // 1. The base's denies, the person key file's and `state.db`'s; no Unix socket allowed.
    assert_denied(&v, &root.join("run/herdr-core").to_string_lossy());
    assert_denied(&v, "~/.config/herdr-core");
    assert_denied(&v, "~/.local/state/herdr-core");
    assert_denied(&v, &person_key.to_string_lossy());
    assert_denied(&v, &dir.join("state.db").to_string_lossy());
    let network = &v["sandbox"]["network"];
    assert!(network["allowUnixSockets"].is_null(), "{v}");
    assert_ne!(network["allowAllUnixSockets"], Value::Bool(true), "{v}");

    // 2. No path inside the worktree is named as writable configuration.
    assert!(!file.starts_with(&workplace));
    let inside = format!("{}/", workplace.display());
    let allowed = strings(&v["sandbox"]["filesystem"]["allowWrite"])
        .into_iter()
        .chain(strings(&v["permissions"]["allow"]));
    for entry in allowed {
        assert!(
            !entry.contains(&inside),
            "{entry} is inside the worktree: {v}"
        );
    }

    // 3. The launch arguments carry the token; the environment does not.
    let db = Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let hash: String = db
        .query_row(
            "SELECT key_sha256 FROM agents WHERE agent_id = 'w-t1' AND role = 'worker'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        keys_in(&spec.argv.join(" "))
            .iter()
            .any(|k| sha256_hex(k) == hash),
        "the token is not in the launch arguments: {:?}",
        spec.argv
    );
    let env: String = spec.env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    assert!(keys_in(&env).is_empty(), "a key in the environment: {env}");
}
