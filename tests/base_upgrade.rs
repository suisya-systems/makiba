//! Issue 422 (docs/design/base-upgrade.md sections 3, 5, 6; build Issue 4 of its section 8):
//! `makiba base upgrade` takes the herdr-core release this makiba pins with one [y/N] question,
//! asked right before the stop of the core.
//!
//! Everything is a stand-in in a temporary root: `claude` (and, in one case, `git`) are scripts;
//! the releases are `file://` folders whose archives hold a `herdr-core` script (`--version`
//! prints its version, `server start` appends `<its path> <version>` to `<root>/start.rec`); the
//! installed base is `<root>/inst/bin/herdr-core` with `hc` a link to it; the core of a session is
//! a thread on `<root>/run/herdr-core/<session>.sock` that answers `hello`, `terminal.list`,
//! `terminal.kill`, `terminal.rm`, `server.stop` and `events.subscribe`, records every method, and
//! when it is down starts again with the version a new line of `start.rec` names. The verb runs
//! with a pseudo-terminal as its standard input; the test reads the question and types the answer.

mod common;

use common::makibad;
use makiba::base::version::PINNED;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::ErrorKind;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind as IoKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

// ---- versions ------------------------------------------------------------------------------

fn pinned() -> (u64, u64, u64) {
    let p: Vec<u64> = PINNED.split('.').map(|x| x.parse().unwrap()).collect();
    (p[0], p[1], p[2])
}

fn older() -> String {
    match pinned() {
        (a, b, c) if c > 0 => format!("{a}.{b}.{}", c - 1),
        (a, b, _) if b > 0 => format!("{a}.{}.0", b - 1),
        (a, _, _) => format!("{}.0.0", a - 1),
    }
}

fn newer() -> String {
    let (a, b, c) = pinned();
    format!("{a}.{b}.{}", c + 1)
}

fn in_range() -> String {
    let (a, b, c) = makiba::agents::version::LOWEST;
    format!("{a}.{b}.{c} (Claude Code)")
}

fn code(kind: ErrorKind) -> Option<i32> {
    Some(kind.exit_code())
}

/// Poll until `f` holds, for up to `secs` seconds.
fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

// ---- the stand-in core ---------------------------------------------------------------------

#[derive(Default)]
struct CoreState {
    /// The version it answers with while it listens; `None` while it is down.
    version: Option<String>,
    /// Bumped at every start and stop: an acceptor of an older life ends.
    life: u64,
    terminals: Vec<Value>,
    /// Every method called, and `BIND <version>` / `STOPPED` at each start and stop.
    log: Vec<String>,
    subscribers: Vec<UnixStream>,
}

#[derive(Clone)]
struct Core {
    socket: PathBuf,
    state: Arc<Mutex<CoreState>>,
}

impl Core {
    /// The core of `session` under `<root>/run`, listening at once with `version` if given, and
    /// started again with the version of each new line of `start.rec` while it is down.
    fn start(root: &Path, session: &str, version: Option<&str>) -> Core {
        let dir = root.join("run/herdr-core");
        fs::create_dir_all(&dir).unwrap();
        let core = Core {
            socket: dir.join(format!("{session}.sock")),
            state: Arc::new(Mutex::new(CoreState::default())),
        };
        if let Some(v) = version {
            core.bind(v);
        }
        let rec = root.join("start.rec");
        let seen = lines(&rec).len();
        let watcher = core.clone();
        std::thread::spawn(move || {
            let mut seen = seen;
            loop {
                std::thread::sleep(Duration::from_millis(20));
                let all = lines(&rec);
                for line in all.iter().skip(seen) {
                    let down = watcher.state.lock().unwrap().version.is_none();
                    if down && let Some(v) = line.split_whitespace().nth(1) {
                        watcher.bind(v);
                    }
                }
                seen = all.len();
            }
        });
        core
    }

    fn bind(&self, version: &str) {
        let _ = fs::remove_file(&self.socket);
        let listener = UnixListener::bind(&self.socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let life = {
            let mut s = self.state.lock().unwrap();
            s.version = Some(version.to_string());
            s.life += 1;
            s.log.push(format!("BIND {version}"));
            s.life
        };
        let core = self.clone();
        std::thread::spawn(move || {
            loop {
                if core.state.lock().unwrap().life != life {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let core = core.clone();
                        std::thread::spawn(move || core.serve(stream));
                    }
                    Err(e) if e.kind() == IoKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
    }

    fn serve(&self, stream: UnixStream) {
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            let Ok(r) = serde_json::from_str::<Value>(&line) else {
                return;
            };
            line.clear();
            let method = r["method"].as_str().unwrap_or("").to_string();
            let mut stop = false;
            let result = {
                let mut s = self.state.lock().unwrap();
                let Some(version) = s.version.clone() else {
                    return;
                };
                s.log.push(method.clone());
                match method.as_str() {
                    "hello" => Some(json!({"version": version})),
                    "terminal.list" => {
                        let want = r["params"]["labels"].as_object().cloned();
                        let items: Vec<Value> = s
                            .terminals
                            .iter()
                            .filter(|t| {
                                want.as_ref().is_none_or(|w| {
                                    w.iter().all(|(k, v)| t["labels"].get(k) == Some(v))
                                })
                            })
                            .cloned()
                            .collect();
                        Some(json!({"terminals": items}))
                    }
                    "terminal.kill" => Some(json!({})),
                    "terminal.rm" => {
                        let id = r["params"]["terminal"].clone();
                        s.terminals.retain(|t| t["id"] != id);
                        Some(json!({}))
                    }
                    "server.stop" => {
                        stop = true;
                        Some(json!({}))
                    }
                    "events.subscribe" => {
                        let reply = json!({"jsonrpc": "2.0", "id": r["id"],
                            "result": {"seq": 0, "terminals": s.terminals}});
                        let _ = writeln!(writer, "{reply}");
                        s.subscribers.push(writer.try_clone().unwrap());
                        None
                    }
                    _ => None,
                }
            };
            let reply = match (&result, method.as_str()) {
                (Some(v), _) => json!({"jsonrpc": "2.0", "id": r["id"], "result": v}),
                (None, "events.subscribe") => continue,
                (None, _) => json!({"jsonrpc": "2.0", "id": r["id"],
                    "error": {"code": -32601, "message": "not in this stand-in"}}),
            };
            let _ = writeln!(writer, "{reply}");
            if stop {
                self.stop();
                return;
            }
        }
    }

    fn stop(&self) {
        let mut s = self.state.lock().unwrap();
        s.version = None;
        s.life += 1;
        s.log.push("STOPPED".into());
        for c in s.subscribers.drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
        let _ = fs::remove_file(&self.socket);
    }

    fn add(&self, id: &str, name: &str, labels: Value) {
        self.state
            .lock()
            .unwrap()
            .terminals
            .push(json!({"id": id, "name": name, "labels": labels, "status": "idle"}));
    }

    fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    fn version(&self) -> Option<String> {
        self.state.lock().unwrap().version.clone()
    }
}

fn lines(p: &Path) -> Vec<String> {
    fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

// ---- the pseudo-terminal -------------------------------------------------------------------

/// (master, slave) of a new pseudo-terminal, neither inherited by a program another test starts.
fn pty() -> (File, File) {
    let (mut m, mut s) = (0, 0);
    // SAFETY: openpty(3) with no name, termios or size; it fills both descriptors.
    let r = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(r, 0, "openpty");
    for fd in [m, s] {
        // SAFETY: fcntl(2) on a descriptor this process owns.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both descriptors were just opened and are owned by nothing else.
    unsafe { (File::from_raw_fd(m), File::from_raw_fd(s)) }
}

/// A running `makiba base upgrade`: its output so far, and the terminal to answer it on.
struct Verb {
    child: Child,
    master: Option<File>,
    out: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl Verb {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn said(&self) -> String {
        self.out.lock().unwrap().clone()
    }

    /// Wait until the question is shown; false if the verb ended first.
    fn wait_question(&mut self) -> bool {
        let until = Instant::now() + Duration::from_secs(120);
        while Instant::now() < until {
            if self.said().contains("[y/N]") {
                return true;
            }
            if self.child.try_wait().unwrap().is_some() {
                // Let the readers take the rest of the output.
                std::thread::sleep(Duration::from_millis(100));
                return self.said().contains("[y/N]");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no question after 120 s: {}", self.said());
    }

    fn answer(&mut self, a: &str) {
        let m = self.master.as_mut().expect("a terminal");
        m.write_all(format!("{a}\n").as_bytes()).unwrap();
        m.flush().unwrap();
    }

    /// Wait for the end; its exit code and everything it said.
    fn finish(mut self) -> (Option<i32>, String) {
        let until = Instant::now() + Duration::from_secs(180);
        let status = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s;
            }
            if Instant::now() > until {
                let _ = self.child.kill();
                panic!("makiba base upgrade did not end: {}", self.said());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        for r in self.readers.drain(..) {
            let _ = r.join();
        }
        (status.code(), self.said())
    }
}

// ---- the setup -----------------------------------------------------------------------------

/// The script a release of `version` holds as its `herdr-core`; its `server start` records
/// `started` (the version a server it starts would answer with) in `rec`, `<root>/start.rec`,
/// and returns once the session's socket listens, as a real start does (up to 10 s).
fn core_script(rec: &Path, version: &str, started: &str) -> String {
    let sockets = rec.with_file_name("run").join("herdr-core");
    format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         \x20 --version) echo \"herdr-core {version}\" ;;\n\
         \x20 server)\n\
         \x20   if [ \"$2\" = start ]; then\n\
         \x20     echo \"$(readlink -f \"$0\") {started}\" >> {rec}\n\
         \x20     n=0\n\
         \x20     while [ ! -S {sockets}/\"${{HC_SESSION:-makiba}}\".sock ] && [ $n -lt 200 ]; do sleep 0.05; n=$((n+1)); done\n\
         \x20   fi ;;\n\
         esac\n",
        rec = rec.display(),
        sockets = sockets.display()
    )
}

fn sha256(p: &Path) -> String {
    let out = Command::new("sha256sum").arg(p).output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

#[derive(Clone, Copy, Default)]
struct Options<'a> {
    /// The core's version, or `None` for a stopped core.
    core: Option<&'a str>,
    /// The installed file's version.
    file: &'a str,
    /// The manifest of PINNED carries a wrong SHA-256.
    wrong_hash: bool,
    /// What PINNED's `server start` starts, when not PINNED.
    pinned_starts: Option<&'a str>,
}

struct Setup {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    core: Core,
    /// Variables set (`Some`) or removed (`None`) on every command, after the defaults.
    extra: Mutex<BTreeMap<String, Option<OsString>>>,
}

impl Setup {
    fn new(o: Options) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        common::exe::write(
            &bin.join("claude"),
            format!("#!/bin/sh\necho '{}'\n", in_range()),
        );
        let rec = root.join("start.rec");
        for v in [PINNED.to_string(), older()] {
            let started = if v == PINNED {
                o.pinned_starts.unwrap_or(PINNED)
            } else {
                v.as_str()
            };
            release(
                &root,
                &v,
                &core_script(&rec, &v, started),
                o.wrong_hash && v == PINNED,
            );
        }
        let s = Setup {
            core: Core::start(&root, "makiba", o.core),
            _tmp: tmp,
            root,
            extra: Mutex::new(BTreeMap::new()),
        };
        s.install(&s.path("inst/bin"), o.file);
        s
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    /// `<dir>/herdr-core` printing `version`, and `<dir>/hc` a link to it: the base's layout.
    fn install(&self, dir: &Path, version: &str) {
        fs::create_dir_all(dir).unwrap();
        common::exe::write(
            &dir.join("herdr-core"),
            core_script(&self.path("start.rec"), version, version),
        );
        let _ = fs::remove_file(dir.join("hc"));
        std::os::unix::fs::symlink("herdr-core", dir.join("hc")).unwrap();
    }

    /// The bytes of the release `version`'s `herdr-core`.
    fn release_bytes(&self, version: &str) -> Vec<u8> {
        fs::read(self.path(&format!("src-{version}/herdr-core"))).unwrap()
    }

    fn set(&self, k: &str, v: impl Into<OsString>) {
        self.extra.lock().unwrap().insert(k.into(), Some(v.into()));
    }

    fn unset(&self, k: &str) {
        self.extra.lock().unwrap().insert(k.into(), None);
    }

    fn vars(&self) -> Vec<(String, Option<OsString>)> {
        let mut v: Vec<(String, Option<OsString>)> = vec![
            (
                makibad::KNOWLEDGE_ENV.into(),
                Some(makibad::knowledge(&self.root).into()),
            ),
            (
                "PATH".into(),
                Some(
                    format!(
                        "{}:{}:/usr/bin:/bin",
                        self.path("bin").display(),
                        self.path("inst/bin").display()
                    )
                    .into(),
                ),
            ),
            ("HOME".into(), Some(self.path("home").into())),
            ("XDG_CONFIG_HOME".into(), Some(self.path("config").into())),
            ("XDG_STATE_HOME".into(), Some(self.path("state").into())),
            ("XDG_RUNTIME_DIR".into(), Some(self.path("run").into())),
            ("TMPDIR".into(), Some(std::env::temp_dir().into())),
            (
                "HERDR_RELEASES_URL".into(),
                Some(format!("file://{}/releases", self.root.display()).into()),
            ),
            ("MAKIBA_DOOR".into(), Some("none".into())),
            ("MAKIBA_PORT".into(), Some("0".into())),
        ];
        v.extend(self.extra.lock().unwrap().clone());
        v
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(MAKIBA);
        c.args(args).env_clear();
        for (k, v) in self.vars() {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        c
    }

    /// `makiba base upgrade --no-secretary` on a pseudo-terminal (`terminal`), or with
    /// `/dev/null` as its standard input.
    fn verb_with(&self, terminal: bool) -> Verb {
        let mut c = self.command(&["base", "upgrade", "--no-secretary"]);
        let master = if terminal {
            let (m, s) = pty();
            c.stdin(Stdio::from(s));
            Some(m)
        } else {
            c.stdin(Stdio::null());
            None
        };
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let out = Arc::new(Mutex::new(String::new()));
        let mut readers = Vec::new();
        let streams: Vec<Box<dyn Read + Send>> = vec![
            Box::new(child.stdout.take().unwrap()),
            Box::new(child.stderr.take().unwrap()),
        ];
        for mut s in streams {
            let out = Arc::clone(&out);
            readers.push(std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        return;
                    }
                    out.lock()
                        .unwrap()
                        .push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }));
        }
        Verb {
            child,
            master,
            out,
            readers,
        }
    }

    fn verb(&self) -> Verb {
        self.verb_with(true)
    }

    /// The verb, answered with `a` when it asks; its exit code and what it said.
    fn verb_answered(&self, a: &str) -> (Option<i32>, String) {
        let mut v = self.verb();
        assert!(v.wait_question(), "no question: {}", v.said());
        v.answer(a);
        v.finish()
    }

    /// The verb, expected to end without a question.
    fn verb_refused(&self) -> (Option<i32>, String) {
        let v = self.verb();
        let (c, said) = v.finish();
        assert!(!said.contains("[y/N]"), "it asked: {said}");
        (c, said)
    }

    fn up(&self) -> (Option<i32>, String) {
        let mut c = makibad::up(&self.root);
        c.arg("--no-secretary").env_clear();
        for (k, v) in self.vars() {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        let out = c.output().unwrap();
        (
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        let out = self.command(args).stdin(Stdio::null()).output().unwrap();
        (
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn state(&self) -> PathBuf {
        self.path("state/makiba")
    }

    fn org(&self) -> String {
        fs::create_dir_all(self.state()).unwrap();
        fs::canonicalize(self.state())
            .unwrap()
            .display()
            .to_string()
    }

    fn makibad_pid(&self) -> Option<i32> {
        fs::read_to_string(self.state().join("makibad.lock"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// What "nothing changed" is compared with.
    fn mark(&self) -> Mark {
        let r = self.path("inst/bin/herdr-core");
        let m = fs::metadata(&r).unwrap();
        Mark {
            bytes: fs::read(&r).unwrap(),
            ino: m.ino(),
            mode: m.permissions().mode(),
            log: self.core.log().len(),
            starts: lines(&self.path("start.rec")).len(),
        }
    }

    /// Nothing changed since `mark`.
    fn unchanged(&self, mark: &Mark, said: &str) {
        let r = self.path("inst/bin/herdr-core");
        let m = fs::metadata(&r).unwrap();
        assert_eq!(fs::read(&r).unwrap(), mark.bytes, "{said}");
        assert_eq!(m.ino(), mark.ino, "{said}");
        assert_eq!(m.permissions().mode(), mark.mode, "{said}");
        assert_eq!(
            fs::read_link(self.path("inst/bin/hc")).unwrap(),
            PathBuf::from("herdr-core"),
            "{said}"
        );
        let names = |d: &Path| -> Vec<String> {
            fs::read_dir(d)
                .map(|e| {
                    e.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        assert!(
            !names(&self.path("inst/bin"))
                .iter()
                .any(|n| n.contains(".makiba-new-")),
            "a new file was left: {said}"
        );
        let base = names(&self.state().join("base"));
        assert!(
            !base
                .iter()
                .any(|n| n.starts_with("staging-") || n.starts_with("herdr-core-")),
            "{base:?}: {said}"
        );
        let calls = &self.core.log()[mark.log..];
        for m in ["terminal.kill", "terminal.rm", "server.stop"] {
            assert!(!calls.iter().any(|c| c == m), "{m} was called: {said}");
        }
        assert_eq!(
            lines(&self.path("start.rec")).len(),
            mark.starts,
            "the base was started: {said}"
        );
    }
}

impl Drop for Setup {
    /// A `makibad` a test started is ended here.
    fn drop(&mut self) {
        if let Some(pid) = self.makibad_pid() {
            // SAFETY: kill(2) of the pid makibad wrote into its own lock file.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            wait_for(10, || unsafe { libc::kill(pid, 0) } != 0);
        }
        // A folder a test made read-only is made writable again for the removal.
        // not an executable: a directory's mode
        let _ = fs::set_permissions(self.path("inst/bin"), fs::Permissions::from_mode(0o755));
    }
}

struct Mark {
    bytes: Vec<u8>,
    ino: u64,
    mode: u32,
    log: usize,
    starts: usize,
}

/// A release of `version` at `<root>/releases/core/<version>`, its archive holding `script` as
/// `herdr-core`; its manifest's SHA-256 is wrong when `wrong` is.
fn release(root: &Path, version: &str, script: &str, wrong: bool) {
    let src = root.join(format!("src-{version}"));
    let dir = root.join("releases/core").join(version);
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(src.join("herdr-core"), script).unwrap();
    let archive = dir.join("herdr-core.tar.gz");
    let status = Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(&src)
        .arg("herdr-core")
        .status()
        .unwrap();
    assert!(status.success());
    let sha = if wrong {
        "0".repeat(64)
    } else {
        sha256(&archive)
    };
    let targets: serde_json::Map<String, Value> = [
        "linux-x86_64",
        "linux-aarch64",
        "macos-aarch64",
        "macos-x86_64",
    ]
    .iter()
    .map(|t| {
        (
            t.to_string(),
            json!({"url": format!("file://{}", archive.display()), "sha256": sha}),
        )
    })
    .collect();
    fs::write(
        dir.join("manifest.json"),
        json!({"targets": targets}).to_string(),
    )
    .unwrap();
}

fn old_core_old_file() -> Options<'static> {
    Options {
        core: Some(Box::leak(older().into_boxed_str())),
        file: Box::leak(older().into_boxed_str()),
        ..Options::default()
    }
}

// ---- 1. S0 ---------------------------------------------------------------------------------

#[test]
fn each_s0_refusal_changes_nothing() {
    type Case = (&'static str, ErrorKind, fn(&Setup) -> Option<String>);
    let cases: Vec<Case> = vec![
        ("inside an agent", ErrorKind::Denied, |s| {
            s.set("CLAUDE_X", "1");
            None
        }),
        ("HC_BIN names no file", ErrorKind::Unavailable, |s| {
            s.set("HC_BIN", s.path("missing/hc"));
            None
        }),
        (
            "the file is not herdr-core or hc",
            ErrorKind::Unavailable,
            |s| {
                let odd = s.path("odd/bin");
                fs::create_dir_all(&odd).unwrap();
                common::exe::write(
                    &odd.join("herdr-core-0.1.21"),
                    core_script(&s.path("start.rec"), &older(), &older()),
                );
                std::os::unix::fs::symlink("herdr-core-0.1.21", odd.join("hc")).unwrap();
                s.set("HC_BIN", odd.join("hc"));
                None
            },
        ),
        ("the folder is read-only", ErrorKind::Unavailable, |s| {
            // not an executable: a directory's mode
            fs::set_permissions(s.path("inst/bin"), fs::Permissions::from_mode(0o555)).unwrap();
            None
        }),
        (
            "HC_BIN set and another hc on PATH",
            ErrorKind::Unavailable,
            |s| {
                let other = s.path("other/bin");
                fs::create_dir_all(&other).unwrap();
                common::exe::write(
                    &other.join("hc"),
                    core_script(&s.path("start.rec"), &older(), &older()),
                );
                s.set("HC_BIN", s.path("inst/bin/hc"));
                s.set(
                    "PATH",
                    format!(
                        "{}:{}:{}:/usr/bin:/bin",
                        s.path("bin").display(),
                        other.display(),
                        s.path("inst/bin").display()
                    ),
                );
                Some(other.join("hc").display().to_string())
            },
        ),
        (
            "another herdr-core beside HC_BIN",
            ErrorKind::Unavailable,
            |s| {
                let side = s.path("side/bin");
                fs::create_dir_all(&side).unwrap();
                std::os::unix::fs::symlink(s.path("inst/bin/herdr-core"), side.join("hc")).unwrap();
                common::exe::write(
                    &side.join("herdr-core"),
                    core_script(&s.path("start.rec"), &older(), &older()),
                );
                s.set("HC_BIN", side.join("hc"));
                Some(side.join("herdr-core").display().to_string())
            },
        ),
        (
            "another herdr-core first on PATH",
            ErrorKind::Unavailable,
            |s| {
                let other = s.path("other2");
                fs::create_dir_all(&other).unwrap();
                common::exe::write(
                    &other.join("herdr-core"),
                    core_script(&s.path("start.rec"), &older(), &older()),
                );
                s.set(
                    "PATH",
                    format!(
                        "{}:{}:{}:/usr/bin:/bin",
                        s.path("bin").display(),
                        other.display(),
                        s.path("inst/bin").display()
                    ),
                );
                Some(other.join("herdr-core").display().to_string())
            },
        ),
        ("the stand-in", ErrorKind::Unavailable, |s| {
            s.set("MAKIBA_BASE", "stub");
            None
        }),
    ];
    for (name, kind, prepare) in cases {
        let s = Setup::new(old_core_old_file());
        let mark = s.mark();
        let named = prepare(&s);
        let (c, said) = s.verb_refused();
        assert_eq!(c, code(kind), "{name}: {said}");
        if let Some(n) = named {
            assert!(said.contains(&n), "{name}: {n} not named: {said}");
        }
        // not an executable: a directory's mode
        let _ = fs::set_permissions(s.path("inst/bin"), fs::Permissions::from_mode(0o755));
        s.unchanged(&mark, &format!("{name}: {said}"));
    }

    // The standard input is not a terminal.
    let s = Setup::new(old_core_old_file());
    let mark = s.mark();
    let (c, said) = s.verb_with(false).finish();
    assert_eq!(c, code(ErrorKind::Usage), "{said}");
    assert!(!said.contains("[y/N]"), "{said}");
    s.unchanged(&mark, &said);

    // Both the core and the file are the pinned release.
    let s = Setup::new(Options {
        core: Some(PINNED),
        file: PINNED,
        ..Options::default()
    });
    let mark = s.mark();
    let (c, said) = s.verb_refused();
    assert_eq!(c, Some(0), "{said}");
    assert!(said.contains("nothing to upgrade"), "{said}");
    s.unchanged(&mark, &said);
}

// ---- 2, 3. versions ------------------------------------------------------------------------

#[test]
fn a_newer_core_or_file_is_refused() {
    let n = newer();
    let o = older();
    for (core, file) in [(n.as_str(), o.as_str()), (PINNED, n.as_str())] {
        let s = Setup::new(Options {
            core: Some(core),
            file,
            ..Options::default()
        });
        let mark = s.mark();
        let (c, said) = s.verb_refused();
        assert_eq!(c, code(ErrorKind::Unavailable), "{said}");
        assert!(said.contains(&n) && said.contains("by its path"), "{said}");
        s.unchanged(&mark, &said);
    }
}

#[test]
fn a_pre_release_or_malformed_version_is_refused() {
    let rc = format!("{PINNED}-rc.1");
    let o = older();
    for (core, file, shown) in [
        (rc.as_str(), o.as_str(), rc.as_str()),
        (PINNED, "0.1", "0.1"),
    ] {
        let s = Setup::new(Options {
            core: Some(core),
            file,
            ..Options::default()
        });
        let mark = s.mark();
        let (c, said) = s.verb_refused();
        assert_eq!(c, code(ErrorKind::Unavailable), "{said}");
        assert!(
            said.contains(shown) && said.contains("cannot compare"),
            "{said}"
        );
        s.unchanged(&mark, &said);
    }
}

// ---- 4. Claude Code and git ----------------------------------------------------------------

#[test]
fn an_old_claude_or_git_refuses_with_makibad_and_terminals_running() {
    let o = older();
    let s = Setup::new(Options {
        core: Some(PINNED),
        file: &o,
        ..Options::default()
    });
    s.core.add("t1", "mine", json!({}));
    let (c, said) = s.up();
    assert_eq!(c, Some(0), "{said}");
    let pid = s.makibad_pid().expect("makibad runs");
    for step in ["claude", "git"] {
        if step == "claude" {
            common::exe::write(
                &s.path("bin/claude"),
                "#!/bin/sh\necho '1.0.0 (Claude Code)'\n",
            );
        } else {
            common::exe::write(
                &s.path("bin/claude"),
                format!("#!/bin/sh\necho '{}'\n", in_range()),
            );
            common::exe::write(&s.path("bin/git"), "#!/bin/sh\necho 'git version 2.30.0'\n");
        }
        let mark = s.mark();
        let (c, said) = s.verb_refused();
        assert_eq!(c, code(ErrorKind::Unavailable), "{step}: {said}");
        assert_eq!(s.makibad_pid(), Some(pid), "{step}: {said}");
        // SAFETY: kill(2) with signal 0 only asks whether the process lives.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "{step}: makibad ended: {said}"
        );
        s.unchanged(&mark, &format!("{step}: {said}"));
        assert_eq!(s.core.state.lock().unwrap().terminals.len(), 1);
    }
    let _ = fs::remove_file(s.path("bin/git"));
}

// ---- 5. one run at a time ------------------------------------------------------------------

#[test]
fn a_second_run_and_up_are_refused_while_the_first_waits() {
    let s = Setup::new(old_core_old_file());
    s.install(&s.path("inst2/bin"), &older());
    let other = Core::start(&s.root, "other", Some(&older()));
    let mut a = s.verb();
    assert!(a.wait_question(), "{}", a.said());
    let new_file = s.path(&format!("inst/bin/.herdr-core.makiba-new-{}", a.pid()));
    let staging = s.state().join(format!("base/staging-{}", a.pid()));
    let before = fs::metadata(&new_file).unwrap().modified().unwrap();
    assert_eq!(fs::read(&new_file).unwrap(), s.release_bytes(PINNED));
    let starts = lines(&s.path("start.rec")).len();

    // B: the same file.
    let (c, said) = s.verb_refused();
    assert_eq!(c, code(ErrorKind::Conflict), "B: {said}");
    assert!(said.contains("session makiba"), "B: {said}");
    // C: another HC_BIN, the same session.
    s.set("HC_BIN", s.path("inst2/bin/hc"));
    let (c, said) = s.verb_refused();
    assert_eq!(c, code(ErrorKind::Conflict), "C: {said}");
    // D: the same state folder, another session and another file.
    s.set("MAKIBA_BASE_SESSION", "other");
    let (c, said) = s.verb_refused();
    assert_eq!(c, code(ErrorKind::Conflict), "D: {said}");
    assert!(said.contains(&s.state().display().to_string()), "D: {said}");
    assert!(
        !other.log().iter().any(|m| m == "server.stop"),
        "D stopped its core"
    );
    s.unset("MAKIBA_BASE_SESSION");
    s.unset("HC_BIN");
    // makiba up of the same session.
    let (c, said) = s.up();
    assert_eq!(c, code(ErrorKind::Conflict), "up: {said}");
    assert_eq!(
        lines(&s.path("start.rec")).len(),
        starts,
        "up ran hc: {said}"
    );

    assert!(staging.is_dir(), "A's staging was removed");
    assert_eq!(fs::read(&new_file).unwrap(), s.release_bytes(PINNED));
    assert_eq!(fs::metadata(&new_file).unwrap().modified().unwrap(), before);
    a.answer("n");
    let (c, said) = a.finish();
    assert_eq!(c, Some(0), "{said}");
}

// ---- 6. the hash ---------------------------------------------------------------------------

#[test]
fn a_wrong_hash_at_s1_stops_nothing() {
    let s = Setup::new(Options {
        wrong_hash: true,
        ..old_core_old_file()
    });
    let mark = s.mark();
    let (c, said) = s.verb_refused();
    assert_eq!(c, code(ErrorKind::Unavailable), "{said}");
    s.unchanged(&mark, &said);
}

// ---- 7. another organization ---------------------------------------------------------------

#[test]
fn another_organizations_agent_refuses_at_s2() {
    for role in ["worker", "secretary"] {
        let s = Setup::new(old_core_old_file());
        let other_dir = s.path("other-state");
        fs::create_dir_all(&other_dir).unwrap();
        let other_org = fs::canonicalize(&other_dir).unwrap().display().to_string();
        s.core.add(
            "t9",
            "theirs",
            json!({"makiba.org": other_org, "makiba.role": role}),
        );
        s.core.add("t1", "mine", json!({}));
        let mark = s.mark();
        let (c, said) = s.verb_refused();
        assert_eq!(c, code(ErrorKind::Conflict), "{role}: {said}");
        assert!(
            said.contains(&other_org) && said.contains("down --secretary"),
            "{role}: {said}"
        );
        s.unchanged(&mark, &format!("{role}: {said}"));

        // That organization goes down with its own state folder.
        s.set("MAKIBA_STATE_DIR", &other_dir);
        let calls = s.core.log().len();
        let (c, said) = s.run(&["down", "--secretary"]);
        assert_eq!(c, Some(0), "{role}: {said}");
        s.unset("MAKIBA_STATE_DIR");
        let after = &s.core.log()[calls..];
        assert!(
            after.iter().any(|m| m == "terminal.kill") && after.iter().any(|m| m == "terminal.rm"),
            "{role}: {after:?}"
        );

        let mut v = s.verb();
        assert!(v.wait_question(), "{role}: {}", v.said());
        assert!(!v.said().contains("theirs"), "{role}: {}", v.said());
        v.answer("n");
        let (c, said) = v.finish();
        assert_eq!(c, Some(0), "{role}: {said}");
    }
}

// ---- 8. no ---------------------------------------------------------------------------------

#[test]
fn no_at_s2_stops_nothing() {
    let s = Setup::new(old_core_old_file());
    s.core.add("t1", "mine", json!({}));
    let mark = s.mark();
    let (c, said) = s.verb_answered("n");
    assert_eq!(c, Some(0), "{said}");
    assert!(said.contains("nothing changed"), "{said}");
    s.unchanged(&mark, &said);
}

// ---- 9. yes --------------------------------------------------------------------------------

/// Four runs: `t-use` in use and `t-rev` in review, each with a verified launch and a worker key;
/// `t-q` queued with a launch under way; `t-done` completed. Returns each run's seeded launch id.
fn seed(s: &Setup) -> BTreeMap<String, i64> {
    let w = StateWriter::open(&s.state().join("state.db")).unwrap();
    let mut launches = BTreeMap::new();
    for (task, status, stage) in [
        ("t-use", "in_use", "verified"),
        ("t-rev", "review", "verified"),
        ("t-q", "queued", "launched"),
        ("t-done", "completed", "verified"),
    ] {
        let run = w
            .upsert_run(&RunUpsert {
                task_id: task,
                project_slug: "p",
                status: Some(status),
                ..Default::default()
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, 'spawn-' || ?2, 'document', 'claude_code', 'brief', ?3, ?4)",
                rusqlite::params![run, task, s.path(task).display().to_string(), stage],
            )
            .unwrap();
        launches.insert(task.to_string(), w.conn().last_insert_rowid());
        w.conn()
            .execute(
                "INSERT INTO agents (agent_id, role, run_id, key_sha256)
                 VALUES ('w-' || ?1, 'worker', ?2, 'key-' || ?1)",
                rusqlite::params![task, run],
            )
            .unwrap();
    }
    launches
}

fn ledger(s: &Setup) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        s.state().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

/// The newest `dispatcher_started`'s `base_version`, read through `makiba events`.
fn started_version(s: &Setup) -> Option<String> {
    let (c, out) = s.run(&["events", "--after", "0", "--limit", "1000", "--json"]);
    assert_eq!(c, Some(0), "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    v["events"]
        .as_array()?
        .iter()
        .rev()
        .find(|e| e["kind"] == "dispatcher_started")
        .and_then(|e| e["payload"]["base_version"].as_str().map(String::from))
}

/// After a completed upgrade: `makiba status` says subscribed, and the start carries PINNED.
fn subscribed_to_pinned(s: &Setup) {
    let (c, out) = s.run(&["status"]);
    assert_eq!(c, Some(0), "{out}");
    assert!(out.contains("subscribed: yes"), "{out}");
    assert_eq!(started_version(s).as_deref(), Some(PINNED));
}

#[test]
fn yes_upgrades_an_old_core_and_file() {
    let s = Setup::new(old_core_old_file());
    let org = s.org();
    s.core.add(
        "t1",
        "w1",
        json!({"makiba.org": org, "makiba.role": "worker"}),
    );
    s.core.add(
        "t2",
        "sec",
        json!({"makiba.org": org, "makiba.role": "secretary"}),
    );
    s.core.add("t3", "mine", json!({}));
    let seeded = seed(&s);
    let old_bytes = fs::read(s.path("inst/bin/herdr-core")).unwrap();
    let r = s.path("inst/bin/herdr-core");

    let mut v = s.verb();
    assert!(v.wait_question(), "{}", v.said());
    let q = v.said();
    for needle in [
        older().as_str(),
        PINNED,
        &r.display().to_string(),
        "not makiba's: mine",
        "worker 1",
        "secretary 1",
        "t-use",
        "t-rev",
        "t-q",
    ] {
        assert!(q.contains(needle), "{needle} not in the question: {q}");
    }
    assert!(!q.contains("t-done"), "{q}");
    v.answer("y");
    let (c, said) = v.finish();
    assert_eq!(c, Some(0), "{said}");

    assert!(s.core.log().iter().any(|m| m == "server.stop"), "{said}");
    assert_eq!(fs::read(&r).unwrap(), s.release_bytes(PINNED), "{said}");
    assert_eq!(
        fs::read_link(s.path("inst/bin/hc")).unwrap(),
        PathBuf::from("herdr-core")
    );
    let kept = s.state().join(format!("base/herdr-core-{}", older()));
    assert_eq!(fs::read(&kept).unwrap(), old_bytes, "{said}");
    subscribed_to_pinned(&s);
    for needle in [
        older().as_str(),
        PINNED,
        &kept.display().to_string(),
        "down --secretary --stop-base",
    ] {
        assert!(said.contains(needle), "{needle} not said: {said}");
    }

    let db = ledger(&s);
    let (stage, reason): (String, Option<String>) = db
        .query_row(
            "SELECT stage, failed_reason FROM run_launch WHERE id = ?1",
            [seeded["t-q"]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (stage.as_str(), reason.as_deref()),
        ("failed", Some("makiba down"))
    );
    for task in ["t-use", "t-rev", "t-q"] {
        let revoked: Option<String> = db
            .query_row(
                "SELECT revoked_at FROM agents WHERE agent_id = 'w-' || ?1",
                [task],
                |r| r.get(0),
            )
            .unwrap();
        assert!(revoked.is_some(), "{task}'s key is not revoked");
        let newest: i64 = db
            .query_row(
                "SELECT max(l.id) FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE r.task_id = ?1",
                [task],
                |r| r.get(0),
            )
            .unwrap();
        assert!(newest > seeded[task], "{task} has no new launch");
    }
    let done: (String, i64) = db
        .query_row(
            "SELECT r.status, max(l.id) FROM runs r JOIN run_launch l ON l.run_id = r.id
             WHERE r.task_id = 't-done'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(done, ("completed".to_string(), seeded["t-done"]));
}

// ---- 10. HC_BIN ----------------------------------------------------------------------------

#[test]
fn hc_bin_naming_the_file_is_accepted() {
    let s = Setup::new(old_core_old_file());
    let elsewhere = s.path("elsewhere/bin");
    s.install(&elsewhere, &older());
    let inst_before = fs::read(s.path("inst/bin/herdr-core")).unwrap();
    s.set("HC_BIN", elsewhere.join("hc"));
    s.set("PATH", format!("{}:/usr/bin:/bin", s.path("bin").display()));
    let (c, said) = s.verb_answered("y");
    assert_eq!(c, Some(0), "{said}");
    assert_eq!(
        fs::read(elsewhere.join("herdr-core")).unwrap(),
        s.release_bytes(PINNED),
        "{said}"
    );
    assert_eq!(
        fs::read(s.path("inst/bin/herdr-core")).unwrap(),
        inst_before
    );
}

// ---- 11. a stopped core --------------------------------------------------------------------

#[test]
fn a_stopped_core_with_an_old_file_is_upgraded() {
    let o = older();
    let s = Setup::new(Options {
        core: None,
        file: &o,
        ..Options::default()
    });
    let r = s.path("inst/bin/herdr-core");
    let (c, said) = s.verb_answered("y");
    assert_eq!(c, Some(0), "{said}");
    let log = s.core.log();
    assert_eq!(
        log.first().map(String::as_str),
        Some(format!("BIND {PINNED}").as_str()),
        "{log:?}"
    );
    let first = lines(&s.path("start.rec"));
    assert_eq!(
        first.first().map(String::as_str),
        Some(format!("{} {PINNED}", r.display()).as_str())
    );
    assert_eq!(fs::read(&r).unwrap(), s.release_bytes(PINNED));
    subscribed_to_pinned(&s);
}

// ---- 12, 13. one of the two old ------------------------------------------------------------

#[test]
fn an_old_core_with_a_pinned_file_is_restarted() {
    let o = older();
    let s = Setup::new(Options {
        core: Some(&o),
        file: PINNED,
        ..Options::default()
    });
    let r = s.path("inst/bin/herdr-core");
    let before = fs::metadata(&r).unwrap();
    let bytes = fs::read(&r).unwrap();
    let starts = lines(&s.path("start.rec")).len();
    let (c, said) = s.verb_answered("y");
    assert_eq!(c, Some(0), "{said}");
    assert!(s.core.log().iter().any(|m| m == "server.stop"), "{said}");
    let after = fs::metadata(&r).unwrap();
    assert_eq!(fs::read(&r).unwrap(), bytes);
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    let new_starts = lines(&s.path("start.rec"));
    assert!(
        new_starts[starts..].contains(&format!("{} {PINNED}", r.display())),
        "{new_starts:?}"
    );
    let kept = s.state().join(format!("base/herdr-core-{o}"));
    assert_eq!(fs::read(kept).unwrap(), s.release_bytes(&o), "{said}");
    subscribed_to_pinned(&s);
}

#[test]
fn a_pinned_core_with_an_old_file_replaces_the_file_only() {
    let o = older();
    let s = Setup::new(Options {
        core: Some(PINNED),
        file: &o,
        ..Options::default()
    });
    s.core.add("t1", "mine", json!({}));
    let r = s.path("inst/bin/herdr-core");
    let old_bytes = fs::read(&r).unwrap();
    let (c, said) = s.verb_answered("y");
    assert_eq!(c, Some(0), "{said}");
    for m in ["server.stop", "terminal.kill", "terminal.rm"] {
        assert!(!s.core.log().iter().any(|c| c == m), "{m}: {said}");
    }
    assert_eq!(fs::read(&r).unwrap(), s.release_bytes(PINNED));
    let kept = s.state().join(format!("base/herdr-core-{o}"));
    assert_eq!(fs::read(kept).unwrap(), old_bytes);
}

// ---- 14. the rollback ----------------------------------------------------------------------

#[test]
fn the_rollback_steps_bring_the_old_core_back() {
    let o = older();
    for file in [o.as_str(), PINNED] {
        let s = Setup::new(Options {
            core: Some(&o),
            file,
            ..Options::default()
        });
        let r = s.path("inst/bin/herdr-core");
        let (c, said) = s.verb_answered("y");
        assert_eq!(c, Some(0), "{file}: {said}");
        let kept = s.state().join(format!("base/herdr-core-{o}"));
        assert!(said.contains(&kept.display().to_string()), "{said}");

        // The steps the verb printed.
        let (c, out) = s.run(&["down", "--secretary", "--stop-base"]);
        assert_eq!(c, Some(0), "{file}: {out}");
        assert!(
            wait_for(10, || s.core.version().is_none()),
            "{file}: the core still runs"
        );
        common::exe::copy(&kept, &r);
        let v = Command::new(&r).arg("--version").output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&v.stdout).trim(),
            format!("herdr-core {o}")
        );
        // The old makiba's `up` starts the core with that file.
        let started = Command::new(&r).args(["server", "start"]).status().unwrap();
        assert!(started.success());
        assert!(
            wait_for(10, || s.core.version().as_deref() == Some(o.as_str())),
            "{file}: the old core did not start"
        );
        let (c, out) = s.up();
        assert_eq!(c, code(ErrorKind::Unavailable), "{file}: {out}");
        assert!(out.contains("base upgrade"), "{file}: {out}");
    }
}

// ---- 15. a wrong server --------------------------------------------------------------------

#[test]
fn a_wrong_server_after_the_start_is_named() {
    let o = older();
    let s = Setup::new(Options {
        pinned_starts: Some(Box::leak(o.clone().into_boxed_str())),
        ..old_core_old_file()
    });
    let r = s.path("inst/bin/herdr-core");
    let (c, said) = s.verb_answered("y");
    assert_eq!(c, code(ErrorKind::Unavailable), "{said}");
    assert!(said.contains("makibad"), "{said}");
    assert!(said.contains(&format!("answers {o}")), "{said}");
    assert!(said.contains(&r.display().to_string()), "{said}");
}
