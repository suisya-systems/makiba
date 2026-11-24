//! Issue 295: the secretary's GitHub CLI finds the person's stored login.
//!
//! The base's core runs with makiba's base folder as its `XDG_CONFIG_HOME`, and every terminal it
//! starts inherits that (and whatever `GH_CONFIG_DIR` the core was started with). The daemon runs in
//! this process on a stand-in base that keeps every launch spec, launching the secretary as
//! `makiba up` asks it to. The secretary's spec is applied over such a terminal environment as the
//! base applies it (its `env`, then its `unset` through `env -u`), and the GitHub CLI's lookup of
//! its configuration folder must land where the person's own shell would: never the base's folder.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The stand-in, keeping every launch spec.
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

/// The GitHub CLI's configuration folder: `GH_CONFIG_DIR` when not empty, else
/// `$XDG_CONFIG_HOME/gh` when not empty, else `$HOME/.config/gh`.
fn gh_dir(env: &BTreeMap<String, String>) -> PathBuf {
    let get = |k: &str| env.get(k).filter(|v| !v.is_empty());
    if let Some(d) = get("GH_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(x) = get("XDG_CONFIG_HOME") {
        return Path::new(x).join("gh");
    }
    Path::new(get("HOME").expect("HOME")).join(".config/gh")
}

/// The secretary's spec as makibad, with `xdg` and `gh` as its own `XDG_CONFIG_HOME` and
/// `GH_CONFIG_DIR`, asks the base to start it; `None` is unset.
fn secretary_spec(root: &Path, xdg: Option<&Path>, gh: Option<&Path>) -> LaunchSpec {
    let dir = root.join("state");
    let bin = root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    common::exe::write(&claude, "#!/bin/sh\necho 'stand-in claude' >&2\nexit 1\n");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let _held_knowledge = makibad::use_knowledge(&root.join("knowledge"));
    // SAFETY: the only test of this binary, and no daemon of it runs while the environment changes.
    unsafe {
        std::env::set_var("PATH", &path);
        std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("elsewhere/person.key"));
        std::env::set_var("MAKIBA_GOALS_DIR", root.join("goals"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::remove_var("MAKIBA_FAKE_AGENT");
        std::env::remove_var("MAKIBA_LAUNCH_AGENT");
        std::env::remove_var("MAKIBA_SECRETARY_REMOTE_CONTROL");
        std::env::set_var("MAKIBA_SECRETARY", "1");
        match xdg {
            Some(x) => std::env::set_var("XDG_CONFIG_HOME", x),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match gh {
            Some(g) => std::env::set_var("GH_CONFIG_DIR", g),
            None => std::env::remove_var("GH_CONFIG_DIR"),
        }
    }
    let base = Shared(Arc::new(Recording::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (dir, base, stop) = (dir.clone(), base.clone(), Arc::clone(&stop));
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), stop))
    };
    let until = Instant::now() + Duration::from_secs(20);
    let spec = loop {
        let found = base
            .0
            .launched
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.name == "secretary")
            .cloned();
        if let Some(s) = found {
            break Some(s);
        }
        if Instant::now() > until {
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    stop.store(true, Ordering::SeqCst);
    let _ = daemon.join();
    spec.expect("no secretary launch within 20 s")
}

/// A secretary launch does not lead the GitHub CLI to the base's folder: it finds the person's own
/// configuration folder, as the person's shell (and makibad) has it, whatever the core inherited.
#[test]
fn the_secretarys_github_cli_finds_the_persons_folder_not_the_bases() {
    let home = std::env::var("HOME").expect("HOME");
    let home_gh = Path::new(&home).join(".config/gh");
    // (makibad's XDG_CONFIG_HOME, makibad's GH_CONFIG_DIR, a GH_CONFIG_DIR the core was started
    // with, where the CLI must look); paths relative to the case's root.
    type Case = (
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    );
    let cases: [Case; 5] = [
        (Some("person"), None, None, Some("person/gh")),
        (Some(""), None, None, None),
        (None, None, None, None),
        (
            Some("person"),
            Some("own-gh"),
            Some("stale-gh"),
            Some("own-gh"),
        ),
        (Some("person"), None, Some("stale-gh"), Some("person/gh")),
    ];
    for (xdg, gh, core_gh, want) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let at = |p: &str| {
            if p.is_empty() {
                PathBuf::new()
            } else {
                root.join(p)
            }
        };
        let person = xdg.map(at);
        // The base's folder as `makiba up` names it from the person's configuration.
        let base_folder = person
            .clone()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| home_gh.parent().unwrap().to_path_buf())
            .join("makiba/base");
        let spec = secretary_spec(&root, person.as_deref(), gh.map(at).as_deref());

        // A terminal of the core: the core's environment, then the spec's env, then its unset.
        let mut env = BTreeMap::from([
            ("HOME".to_string(), home.clone()),
            (
                "XDG_CONFIG_HOME".to_string(),
                base_folder.display().to_string(),
            ),
        ]);
        if let Some(g) = core_gh {
            env.insert("GH_CONFIG_DIR".into(), at(g).display().to_string());
        }
        env.extend(spec.env.clone());
        for v in &spec.unset {
            env.remove(v);
        }
        let got = gh_dir(&env);
        let case = format!("XDG {xdg:?}, GH_CONFIG_DIR {gh:?}, core's {core_gh:?}");
        assert!(
            !got.starts_with(&base_folder),
            "{case}: the CLI looks in the base's folder {}",
            got.display()
        );
        let want = want.map(at).unwrap_or_else(|| home_gh.clone());
        assert_eq!(got, want, "{case}");
    }
}
