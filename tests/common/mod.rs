//! Shared test helpers (M0-3). `base()` returns the stand-in by default. With
//! `MAKIBA_TEST_REAL_BASE=1` it starts the real base in a place of its own and stops only that,
//! when the returned value is dropped.
#![allow(dead_code)]

pub mod local;

use makiba::base::stub::Stub;
use makiba::base::version::checked_hello;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, Detail, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use makiba::base::{BlockKind, State, Status};
use std::collections::BTreeMap;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

/// The line the fixture's terminal prints.
pub const LINE: &str = "makiba-fixture-line";

pub fn real_wanted() -> bool {
    std::env::var("MAKIBA_TEST_REAL_BASE").as_deref() == Ok("1")
}

/// Where a real base lives for one test run. Nothing here is a person's own place.
#[derive(Debug, Clone)]
pub struct Place {
    pub session: String,
    pub root: PathBuf,
    pub config_home: PathBuf,
    pub state_home: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Place {
    pub fn folders(&self) -> Vec<&Path> {
        vec![
            &self.root,
            &self.config_home,
            &self.state_home,
            &self.runtime_dir,
        ]
    }
}

/// The base a test uses, with its place (`None` for the stand-in).
pub struct Fixture {
    base: Box<dyn Base>,
    pub place: Option<Place>,
}

impl Deref for Fixture {
    type Target = dyn Base;
    fn deref(&self) -> &(dyn Base + 'static) {
        &*self.base
    }
}

pub fn base() -> Fixture {
    if real_wanted() {
        let real = Real::start();
        let place = Some(real.place.clone());
        Fixture {
            base: Box::new(real),
            place,
        }
    } else {
        Fixture {
            base: Box::new(Stub::scripted(&[LINE])),
            place: None,
        }
    }
}

/// A base that runs the argv it is given (the stand-in does not): the real base when wanted,
/// otherwise `local::Local`. For tests that need a program to behave, such as the test agent.
pub fn running_base() -> Fixture {
    if real_wanted() {
        return base();
    }
    Fixture {
        base: Box::new(local::Local::new()),
        place: None,
    }
}

/// Read the `status` object of `hc get --json`, wherever it sits.
fn find_status(v: &serde_json::Value) -> Option<Status> {
    let status = find_status_value(v)?;
    let state = match status.get("state")?.as_str()? {
        "idle" => State::Idle,
        "working" => State::Working,
        "blocked" => State::Blocked,
        "done" => State::Done,
        "error" => State::Error,
        _ => return None,
    };
    let kind = match status.get("kind").and_then(|k| k.as_str()) {
        Some("permission") => Some(BlockKind::Permission),
        Some("question") => Some(BlockKind::Question),
        Some("auth") => Some(BlockKind::Auth),
        _ => None,
    };
    let msg = status.get("msg").and_then(|m| m.as_str()).map(String::from);
    Some(Status { state, kind, msg })
}

fn find_status_value(v: &serde_json::Value) -> Option<&serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m
            .get("status")
            .filter(|s| s.get("state").is_some())
            .or_else(|| m.values().find_map(find_status_value)),
        _ => None,
    }
}

/// The folders a person's own base would use, as the base reads them when no place is given.
pub fn persons_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let pick =
        |var: &str, fallback: PathBuf| std::env::var_os(var).map(PathBuf::from).unwrap_or(fallback);
    vec![
        pick("XDG_CONFIG_HOME", home.join(".config")).join("herdr-core"),
        pick("XDG_STATE_HOME", home.join(".local/state")).join("herdr-core"),
        pick("XDG_RUNTIME_DIR", PathBuf::from("/tmp")).join("herdr-core"),
    ]
}

/// A cheap fingerprint of folders: every path, its size and its modification time.
pub fn snapshot(folders: &[PathBuf]) -> Vec<String> {
    fn walk(p: &Path, out: &mut Vec<String>) {
        let Ok(meta) = fs::symlink_metadata(p) else {
            out.push(format!("{} absent", p.display()));
            return;
        };
        out.push(format!(
            "{} {} {:?}",
            p.display(),
            meta.len(),
            meta.modified().ok()
        ));
        if meta.is_dir() {
            let mut kids: Vec<_> = fs::read_dir(p)
                .map(|d| d.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            kids.sort();
            for k in kids {
                walk(&k, out);
            }
        }
    }
    let mut out = Vec::new();
    for f in folders {
        walk(f, &mut out);
    }
    out
}

pub fn unsupported(what: &str) -> BaseError {
    BaseError::Other(Detail {
        kind: "unsupported".into(),
        message: format!("{what} is not part of the test fixture's real base (M1 builds it)"),
        hint: None,
    })
}

/// The real base for tests: `hc` run in a place of its own. A test helper only; the real
/// implementation is `src/base/herdr.rs`, built in M1. Flags follow scripts/probes/lib.sh.
pub struct Real {
    hc: PathBuf,
    pub place: Place,
    terminals: Mutex<Vec<String>>,
}

impl Real {
    pub fn start() -> Real {
        let hc = std::env::var_os("HC_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("hc"));
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("mk-{pid}"));
        let place = Place {
            session: format!("makiba-test-{pid}"),
            config_home: root.join("config"),
            state_home: root.join("state"),
            runtime_dir: root.join("run"),
            root,
        };
        for d in [&place.config_home, &place.state_home, &place.runtime_dir] {
            fs::create_dir_all(d.join("herdr-core")).expect("make the place's folders");
        }
        fs::write(
            place.config_home.join("herdr-core/config.toml"),
            "notes = false\n",
        )
        .expect("write the place's config");
        let real = Real {
            hc,
            place,
            terminals: Mutex::new(Vec::new()),
        };
        // Refuse a base that is not the pinned one before anything is started.
        real.hello().expect("the real base is the pinned release");
        real
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.hc);
        for v in [
            "HC_SOCKET",
            "HC_SCOPE",
            "HC_TERMINAL_ID",
            "HC_TERMINAL_SOCKET",
        ] {
            c.env_remove(v);
        }
        c.env("HC_SESSION", &self.place.session)
            .env("XDG_CONFIG_HOME", &self.place.config_home)
            .env("XDG_STATE_HOME", &self.place.state_home)
            .env("XDG_RUNTIME_DIR", &self.place.runtime_dir);
        c
    }

    fn run(&self, args: &[&str]) -> Result<Output, BaseError> {
        self.command().args(args).output().map_err(|e| {
            BaseError::Unavailable(Detail {
                kind: "spawn".into(),
                message: format!("cannot run {}: {e}", self.hc.display()),
                hint: Some("set HC_BIN".into()),
            })
        })
    }

    fn fail(kind: &str, out: &Output) -> BaseError {
        BaseError::Other(Detail {
            kind: kind.into(),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            hint: None,
        })
    }
}

impl Drop for Real {
    /// Stops only this place's base: the session is ours, so `server stop` reaches nothing else.
    fn drop(&mut self) {
        assert!(self.place.session.starts_with("makiba-test-"));
        for t in self.terminals.lock().unwrap().iter() {
            let _ = self.run(&["kill", t]);
            let _ = self.run(&["rm", t]);
        }
        let _ = self.run(&["server", "stop"]);
        let _ = fs::remove_dir_all(&self.place.root);
    }
}

impl Base for Real {
    fn name(&self) -> &'static str {
        "herdr-core"
    }

    fn hello(&self) -> Result<BaseInfo, BaseError> {
        checked_hello(|| {
            let out = self.run(&["--version"])?;
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let version = text
                .split_whitespace()
                .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .unwrap_or("")
                .to_string();
            Ok(BaseInfo {
                name: "herdr-core".into(),
                version,
            })
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        let cwd = spec.cwd.to_string_lossy().to_string();
        let mut args: Vec<String> = ["run", "--bg", "--no-parent", "--name"]
            .map(String::from)
            .into();
        args.extend([spec.name.clone(), "--cwd".into(), cwd, "--".into()]);
        if !spec.unset.is_empty() || !spec.env.is_empty() {
            args.push("env".into());
            args.extend(
                spec.unset
                    .iter()
                    .flat_map(|v| ["-u".to_string(), v.clone()]),
            );
            args.extend(spec.env.iter().map(|(k, v)| format!("{k}={v}")));
        }
        args.extend(spec.argv.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run(&refs)?;
        if !out.status.success() {
            return Err(Self::fail("launch", &out));
        }
        self.terminals.lock().unwrap().push(spec.name.clone());
        Ok(TerminalRef(spec.name.clone()))
    }

    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        Err(unsupported("list"))
    }

    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        let out = self.run(&["get", "--json", &t.0])?;
        if !out.status.success() {
            return Err(BaseError::NotFound(Detail {
                kind: "terminal".into(),
                message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
                hint: None,
            }));
        }
        Ok(Terminal {
            id: t.clone(),
            name: t.0.clone(),
            tags: BTreeMap::new(),
            status: serde_json::from_slice(&out.stdout)
                .ok()
                .and_then(|v| find_status(&v)),
            turn: None,
            alive: true,
            exit_code: None,
        })
    }

    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        let n = lines.to_string();
        let out = self.run(&["read", "--lines", &n, &t.0])?;
        if !out.status.success() {
            return Err(Self::fail("read", &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(String::from)
            .collect())
    }

    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        let mut args = vec!["keys", t.0.as_str()];
        args.extend(keys);
        let out = self.run(&args)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(Self::fail("keys", &out))
        }
    }

    fn send(&self, t: &TerminalRef, text: &str, _only_if_idle: bool) -> Result<(), BaseError> {
        let out = self.run(&["send", &t.0, text])?;
        if out.status.success() {
            Ok(())
        } else {
            Err(Self::fail("send", &out))
        }
    }

    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        let _ = self.run(&["kill", &t.0])?;
        let out = self.run(&["rm", &t.0])?;
        if !out.status.success() {
            return Err(Self::fail("end", &out));
        }
        self.terminals.lock().unwrap().retain(|n| n != &t.0);
        Ok(())
    }

    fn events(&self) -> Result<EventStream, BaseError> {
        Err(unsupported("events"))
    }

    fn start(&self) -> Result<(), BaseError> {
        Ok(()) // the first `hc run` starts the place's core
    }

    fn worker_denies(&self) -> BaseDenies {
        BaseDenies::default()
    }
}

/// A code run's workplace as `makibad` prepares it (Issue 241): a repository of the run's own,
/// its git folder `<state>/git/<task>`, borrowing the objects of the project clone `repo`, on a
/// new `branch` from `main`.
pub fn run_workplace(state: &Path, repo: &Path, workplace: &Path, task: &str, branch: &str) {
    let gitdir = makiba::daemon::run_repo::dir(&state.join("settings"), task);
    let hooks = state.join("git-hooks");
    makiba::daemon::run_repo::create(repo, &gitdir, workplace, branch, "main", &hooks).unwrap();
}
