//! Issue 134: a run whose agent is `claude_code`, prepared and launched by `makibad` in this
//! process on the stand-in base (which runs nothing and records every launch spec), with a
//! stand-in `claude` on the path. What the base is asked to start is the argv of Issue 114 with
//! the worker's rules file, and settings that hold both the worker role's denies and the per-task
//! denies. No real `claude` is started.

use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

/// The value after `flag`.
fn value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let i = argv.iter().position(|a| a == flag)?;
    argv.get(i + 1).map(String::as_str)
}

fn repo_file(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

#[test]
fn a_claude_code_run_is_launched_with_the_role_rules_and_both_sets_of_denies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let docs = root.join("docs");
    let share = root.join("share");
    let person_key = root.join("elsewhere/person.key");
    std::fs::create_dir_all(&dir).unwrap();
    // The stand-in `claude`: found on the path, never run (the stand-in base runs nothing).
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\necho 'stand-in claude' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let denies = BaseDenies {
        commands: vec!["hc".into()],
        resolved: vec![root.join("run/herdr-core")],
        folders: vec![PathBuf::from("~/.config/herdr-core")],
    };
    // SAFETY: the only test of this binary, and no thread of it has started yet.
    unsafe {
        std::env::set_var("PATH", &path);
        std::env::set_var("MAKIBA_SHARE_DIR", &share);
        std::env::set_var("MAKIBA_PERSON_KEY", &person_key);
        std::env::set_var("MAKIBA_DOCS_DIR", &docs);
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::remove_var("MAKIBA_FAKE_AGENT");
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
                         'claude_code', 'the brief', ?1)",
                [workplace.to_string_lossy()],
            )
            .unwrap();
    }

    let launched = Arc::new(Mutex::new(Vec::new()));
    let (for_daemon, daemon_dir) = (Arc::clone(&launched), dir.clone());
    std::thread::spawn(move || {
        daemon::run(&daemon_dir, &|| {
            Box::new(Recording {
                stub: Stub::new(),
                denies: denies.clone(),
                launched: Arc::clone(&for_daemon),
            })
        })
    });
    let spec = wait(|| launched.lock().unwrap().first().cloned())
        .expect("the claude_code run was not launched within 20 s");
    let a = &spec.argv;

    // The argv of Issue 114, with the program found on the path.
    assert_eq!(Path::new(&a[0]), claude, "{a:?}");
    assert_eq!(
        value(a, "--setting-sources"),
        Some("project,local"),
        "{a:?}"
    );
    assert_eq!(value(a, "--permission-mode"), Some("auto"), "{a:?}");
    assert_eq!(
        value(a, "--dangerously-load-development-channels"),
        Some("server:makiba-channel"),
        "{a:?}"
    );
    let cfg: Value = serde_json::from_str(value(a, "--mcp-config").expect("--mcp-config")).unwrap();
    assert!(cfg["mcpServers"]["makiba"]["url"].is_string(), "{cfg}");
    assert!(cfg["mcpServers"]["makiba-channel"].is_object(), "{cfg}");
    assert!(
        !a.iter().any(|x| x.starts_with("--remote-control")),
        "{a:?}"
    );
    assert_eq!(a.last().map(String::as_str), Some("the brief"), "{a:?}");
    assert_eq!(spec.cwd, workplace);

    // The worker's rules file: outside the workplace, holding the role's rules.
    let rules = PathBuf::from(value(a, "--append-system-prompt-file").expect("no rules file"));
    assert!(!rules.starts_with(&workplace), "{}", rules.display());
    assert_eq!(
        std::fs::read_to_string(&rules).unwrap(),
        repo_file("roles/worker/rules.md")
    );

    // One settings file holding the role's denies, hooks and sandbox paths and the task's own.
    let settings = PathBuf::from(value(a, "--settings").expect("no settings file"));
    assert_eq!(settings, dir.join("settings/t1.json"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let role: Value = serde_json::from_str(&repo_file("roles/worker/settings.json")).unwrap();
    let deny = strings(&v["permissions"]["deny"]);
    for rule in strings(&role["permissions"]["deny"]) {
        assert!(
            deny.contains(&rule),
            "the role's deny {rule} is missing: {v}"
        );
    }
    assert_eq!(v["hooks"], role["hooks"], "the role's hooks: {v}");
    let fs = &v["sandbox"]["filesystem"];
    for list in ["denyRead", "denyWrite"] {
        let got = strings(&fs[list]);
        for p in strings(&role["sandbox"]["filesystem"][list]) {
            assert!(got.contains(&p), "the role's {list} {p} is missing: {v}");
        }
    }
    for p in [
        root.join("run/herdr-core"),
        person_key.clone(),
        dir.join("state.db"),
    ] {
        let p = p.to_string_lossy().into_owned();
        for list in ["denyRead", "denyWrite"] {
            assert!(strings(&fs[list]).contains(&p), "task {list} {p}: {v}");
        }
        for tool in ["Read(", "Edit("] {
            assert!(
                deny.iter().any(|r| r.starts_with(tool) && r.contains(&p)),
                "no task {tool}...) deny names {p}: {v}"
            );
        }
    }
    assert!(strings(&fs["allowWrite"]).contains(&workplace.to_string_lossy().into_owned()));
    assert_eq!(v["sandbox"]["enabled"], Value::Bool(true));
    assert_eq!(v["sandbox"]["allowUnsandboxedCommands"], Value::Bool(false));
}
