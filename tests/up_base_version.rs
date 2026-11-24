//! Issue 418 (docs/design/base-upgrade.md section 4): `makiba up` that finds a core of another
//! version than the pinned one refuses at once, before `hc server start` and before the ledger is
//! opened, and says what to run. The core is a stand-in on a Unix socket that answers `hello` with
//! a chosen version; `hc` and `claude` are stand-in scripts; every folder is a temporary one.

mod common;

use common::makibad;
use makiba::base::version::PINNED;
use makiba::spec::ErrorKind;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

/// What `claude --version` prints for the lowest supported version.
fn in_range() -> String {
    let (a, b, c) = makiba::agents::version::LOWEST;
    format!("{a}.{b}.{c} (Claude Code)")
}

/// A stand-in program at `path`, made through `common::exe` (Issue 379).
fn script(path: &Path, body: &str) {
    common::exe::write(path, format!("#!/bin/sh\n{body}\n"));
}

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

struct Setup {
    root: tempfile::TempDir,
}

impl Setup {
    /// A root with stand-in `claude` and `hc`, and a stand-in core of session `makiba` answering
    /// every `hello` with `version`.
    fn new(version: &str) -> Setup {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        script(&bin.join("claude"), &format!("echo '{}'", in_range()));
        let rec = root.path().join("hc.rec");
        script(
            &bin.join("hc"),
            &format!("echo \"CALL $*\" >> {}", rec.display()),
        );
        let sockets = root.path().join("run/herdr-core");
        fs::create_dir_all(&sockets).unwrap();
        let listener = UnixListener::bind(sockets.join("makiba.sock")).unwrap();
        let version = version.to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let version = version.clone();
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let Ok(r) = serde_json::from_str::<Value>(&line) else {
                            return;
                        };
                        let reply = if r["method"] == "hello" {
                            json!({"jsonrpc": "2.0", "id": r["id"], "result": {"version": version}})
                        } else {
                            json!({"jsonrpc": "2.0", "id": r["id"],
                                   "error": {"code": -32601, "message": "not in this stand-in"}})
                        };
                        if writeln!(writer, "{reply}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Setup { root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    /// `makiba up` from a login shell of this root, and how long it took.
    fn up(&self) -> (Output, Duration) {
        let mut c = makibad::up(self.root.path());
        c.env_clear()
            .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(self.root.path()))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("HC_BIN", self.path("bin/hc"))
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0");
        let start = Instant::now();
        let out = c.output().unwrap();
        (out, start.elapsed())
    }

    fn lock(&self) -> PathBuf {
        self.path("state/makiba/makibad.lock")
    }
}

impl Drop for Setup {
    /// A `makibad` that an `up` without the refusal started is ended here.
    fn drop(&mut self) {
        let pid: Option<i32> = fs::read_to_string(self.lock())
            .ok()
            .and_then(|s| s.trim().parse().ok());
        if let Some(pid) = pid {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until && unsafe { libc::kill(pid, 0) } == 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// `up` against a core answering `version`: refused at once, nothing run, the ledger untouched.
/// Returns what `up` said.
fn refused(version: &str) -> String {
    let s = Setup::new(version);
    let (out, took) = s.up();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(ErrorKind::Unavailable.exit_code()),
        "{said}"
    );
    assert!(took < Duration::from_secs(1), "took {took:?}: {said}");
    assert!(!s.path("hc.rec").exists(), "hc was run: {said}");
    assert!(!s.lock().exists(), "makibad was started: {said}");
    assert!(
        !s.path("state/makiba/state.db").exists(),
        "the ledger was opened: {said}"
    );
    assert!(
        said.contains(version),
        "the found version is not named: {said}"
    );
    said
}

#[test]
fn an_older_core_names_this_binary_and_base_upgrade() {
    let said = refused(&older());
    let exe = fs::canonicalize(env!("CARGO_BIN_EXE_makiba")).unwrap();
    assert!(said.contains(&exe.display().to_string()), "{said}");
    assert!(said.contains("base upgrade"), "{said}");
    assert!(said.contains(PINNED), "{said}");
}

#[test]
fn a_newer_core_says_this_makiba_is_older_than_the_base() {
    let said = refused(&newer());
    assert!(said.contains("older than the base"), "{said}");
    assert!(!said.contains("base upgrade"), "{said}");
}

#[test]
fn a_pre_release_core_cannot_be_compared() {
    let said = refused(&format!("{PINNED}-rc.1"));
    assert!(said.contains("cannot compare"), "{said}");
    assert!(!said.contains("base upgrade"), "{said}");
}

#[test]
fn a_malformed_core_version_cannot_be_compared() {
    let said = refused("0.1");
    assert!(said.contains("cannot compare"), "{said}");
    assert!(!said.contains("base upgrade"), "{said}");
}
