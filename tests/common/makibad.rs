//! Starting `makibad` in a test (Issue 312). Every test that starts `makibad`, in its own process
//! or as the `makiba` binary, does it through this module, so that `MAKIBA_KNOWLEDGE_DIR` names a
//! temporary folder of the test's own and never falls back to the person's `~/makiba-knowledge`
//! (whose `raw/` notes would launch the curator and could be written to).
//!
//! - In process: `run`, `run_with` and `run_until` stand for `makiba::daemon::*`. The first call
//!   in a test process sets the variable, once, to a folder of that process; a test that needs a
//!   folder of its own (with notes) holds `use_knowledge` while its daemon runs, under the lock
//!   that serializes its environment. A start with the variable unset, or set by anything but this
//!   module, panics with `MISSING` before the daemon starts.
//! - The binary: `daemon(root)` and `up(root)` give a `Makibad`, a command that already sets the
//!   variable to `<root>/knowledge`. Every way it runs (`spawn`, `output`, `status`) first checks
//!   that the command itself sets the variable (an inherited value may be the person's own) and
//!   panics with `MISSING` otherwise, so an `env_clear` or `env_remove` is caught.
//!
//! The check lives here, in the tests; `makibad` resolves the folder as before.

use makiba::base::Base;
use makiba::daemon::delivery::Delivery;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// The variable `makibad` reads the knowledge folder from.
pub const KNOWLEDGE_ENV: &str = makiba::daemon::curator::KNOWLEDGE_ENV;

/// The failure of a start without the variable.
pub const MISSING: &str = "a test that starts makibad sets MAKIBA_KNOWLEDGE_DIR to a temporary \
folder of its own: start makibad through tests/common/makibad.rs (daemon, up, run, run_with, \
run_until; use_knowledge for a folder of the test's own)";

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// This process's knowledge folder, made and set once.
static PROCESS: OnceLock<PathBuf> = OnceLock::new();
/// The folder a `use_knowledge` holds, if any.
static ACTIVE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The knowledge folder of this test process: `<target tmp>/makiba-knowledge-<pid>`, empty at
/// first, and set as `MAKIBA_KNOWLEDGE_DIR` the first time it is asked for.
pub fn process_knowledge() -> &'static Path {
    PROCESS.get_or_init(|| {
        let base = option_env!("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("makiba-knowledge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make the test process's knowledge folder");
        // SAFETY: once per process, before any daemon of this module runs in it.
        unsafe { std::env::set_var(KNOWLEDGE_ENV, &dir) };
        dir
    })
}

/// Holds `MAKIBA_KNOWLEDGE_DIR` on a folder of the test's own until dropped, then puts back the
/// process's folder. One at a time: hold the lock that serializes the test's environment.
#[must_use = "the folder is the knowledge folder only while this is held"]
pub struct Knowledge {
    path: PathBuf,
}

impl Knowledge {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Make `path` and hold `MAKIBA_KNOWLEDGE_DIR` on it while the returned value lives.
pub fn use_knowledge(path: &Path) -> Knowledge {
    let process = process_knowledge();
    let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(
        active.is_none(),
        "use_knowledge: another folder is held; hold the test's environment lock"
    );
    std::fs::create_dir_all(path).expect("make the test's knowledge folder");
    assert_ne!(path, process);
    // SAFETY: the caller holds its environment lock, so no other test of the process sets it.
    unsafe { std::env::set_var(KNOWLEDGE_ENV, path) };
    *active = Some(path.to_path_buf());
    Knowledge {
        path: path.to_path_buf(),
    }
}

impl Drop for Knowledge {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        if active.as_deref() == Some(self.path.as_path()) {
            *active = None;
            // SAFETY: as in `use_knowledge`.
            unsafe { std::env::set_var(KNOWLEDGE_ENV, process_knowledge()) };
        }
    }
}

/// Panic with `MISSING` unless `MAKIBA_KNOWLEDGE_DIR` is the folder this module set: the held
/// one, or else the process's.
pub fn check_env() {
    let process = process_knowledge();
    let want = ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| process.to_path_buf());
    match std::env::var_os(KNOWLEDGE_ENV) {
        Some(v) if !v.is_empty() && Path::new(&v) == want => {}
        _ => panic!("{MISSING}"),
    }
}

/// `makiba::daemon::run` with the knowledge folder checked first.
pub fn run(dir: &Path, connect: &dyn Fn() -> Box<dyn Base>) -> Result<(), String> {
    check_env();
    makiba::daemon::run(dir, connect)
}

/// `makiba::daemon::run_with` with the knowledge folder checked first.
pub fn run_with(
    dir: &Path,
    connect: &dyn Fn() -> Box<dyn Base>,
    delivery: Delivery,
) -> Result<(), String> {
    check_env();
    makiba::daemon::run_with(dir, connect, delivery)
}

/// `makiba::daemon::run_until` with the knowledge folder checked first.
pub fn run_until(
    dir: &Path,
    connect: &dyn Fn() -> Box<dyn Base>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    check_env();
    makiba::daemon::run_until(dir, connect, stop)
}

/// `<root>/knowledge`, made: the knowledge folder `daemon(root)` and `up(root)` give.
pub fn knowledge(root: &Path) -> PathBuf {
    let k = root.join("knowledge");
    std::fs::create_dir_all(&k).expect("make the test's knowledge folder");
    k
}

/// `makiba daemon` with `MAKIBA_KNOWLEDGE_DIR=<root>/knowledge`.
pub fn daemon(root: &Path) -> Makibad {
    Makibad::new(root, "daemon")
}

/// `makiba up` (which starts `makiba daemon` with its environment) with
/// `MAKIBA_KNOWLEDGE_DIR=<root>/knowledge`.
pub fn up(root: &Path) -> Makibad {
    Makibad::new(root, "up")
}

/// A command that starts `makibad`. It runs only through `spawn`, `output` or `status`, each of
/// which checks the knowledge variable first.
pub struct Makibad(Command);

impl Makibad {
    fn new(root: &Path, verb: &str) -> Makibad {
        let mut c = Command::new(MAKIBA);
        c.arg(verb).env(KNOWLEDGE_ENV, knowledge(root));
        Makibad(c)
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, a: S) -> &mut Self {
        self.0.arg(a);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    pub fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, k: K, v: V) -> &mut Self {
        self.0.env(k, v);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.0.envs(vars);
        self
    }

    pub fn env_remove<K: AsRef<OsStr>>(&mut self, k: K) -> &mut Self {
        self.0.env_remove(k);
        self
    }

    /// Clears everything, the knowledge variable too: set it again before the command runs.
    pub fn env_clear(&mut self) -> &mut Self {
        self.0.env_clear();
        self
    }

    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        self.0.current_dir(dir);
        self
    }

    pub fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.0.stdin(cfg);
        self
    }

    pub fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.0.stdout(cfg);
        self
    }

    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.0.stderr(cfg);
        self
    }

    /// For a test's own helper that sets up a `Command` (environment, folders); not for running it.
    pub fn with(&mut self, f: impl FnOnce(&mut Command)) -> &mut Self {
        f(&mut self.0);
        self
    }

    /// Panic with `MISSING` unless the command itself sets the knowledge variable.
    pub fn check(&self) {
        let set = self
            .0
            .get_envs()
            .any(|(k, v)| k == KNOWLEDGE_ENV && v.is_some_and(|v| !v.is_empty()));
        if !set {
            panic!("{MISSING}");
        }
    }

    pub fn spawn(&mut self) -> std::io::Result<Child> {
        self.check();
        self.0.spawn()
    }

    pub fn output(&mut self) -> std::io::Result<Output> {
        self.check();
        self.0.output()
    }

    pub fn status(&mut self) -> std::io::Result<ExitStatus> {
        self.check();
        self.0.status()
    }
}
