//! Issue 661 (follow-up of Issue 590): the approval key step of `makiba up` reads the terminals of
//! every session of the base on this machine with `list_session`, on a connection without an
//! organization, so `up` makes the key when no agent may read it. It still does not make it when
//! a live agent of another organization runs in any session, or when a session cannot be listed.
//!
//! The cores are stand-ins on Unix sockets in the base's socket folder under a temporary
//! `XDG_RUNTIME_DIR`: each answers `hello` with the pinned version and `terminal.list` with a fixed
//! list or a fixed error. `claude` and the base's client are stand-in scripts. Only the key step's
//! lines and the key file are asserted: the steps of `up` after it (a `makibad` subscribing to a
//! stand-in core) are not under test.

mod common;

use common::makibad;
use makiba::base::version::PINNED;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const THE_WAY: &str = "run makiba down --secretary, then makiba up";
const NO_ORG: &str = "this connection has no organization";
const SIGNER: &str = "makiba-owner namespaces=\"makiba-acceptance-approval\" ";
const OTHER_ORG: &str = "/another/org";

fn in_range() -> String {
    let (a, b, c) = makiba::agents::version::LOWEST;
    format!("{a}.{b}.{c} (Claude Code)")
}

fn script(path: &Path, body: &str) {
    common::exe::write(path, format!("#!/bin/sh\n{body}\n"));
}

/// A terminal as the core's `terminal.list` answers it.
fn terminal(name: &str, labels: Value, state: &str) -> Value {
    let exit_code = if state == "finished" {
        json!(0)
    } else {
        Value::Null
    };
    json!({"address": name, "name": name, "labels": labels, "state": state,
        "status": null, "turn": 0, "exit_code": exit_code})
}

/// What a stand-in core answers to `terminal.list`.
enum Listing {
    Terminals(Vec<Value>),
    Error,
}

struct Setup {
    root: tempfile::TempDir,
    /// The number of `terminal.list` calls each stand-in core answered.
    lists: Arc<Mutex<usize>>,
}

impl Setup {
    fn new(sessions: Vec<(&str, Listing)>) -> Setup {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        script(&bin.join("claude"), &format!("echo '{}'", in_range()));
        script(&bin.join("hc"), "exit 0");
        // The base's socket folder under `XDG_RUNTIME_DIR` (`base::session_socket`).
        let sockets = root.path().join("run/herdr-core");
        fs::create_dir_all(&sockets).unwrap();
        let lists = Arc::new(Mutex::new(0));
        for (session, listing) in sessions {
            let listener = UnixListener::bind(sockets.join(format!("{session}.sock"))).unwrap();
            let listing = Arc::new(listing);
            let lists = lists.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    let listing = listing.clone();
                    let lists = lists.clone();
                    std::thread::spawn(move || {
                        let mut writer = stream.try_clone().unwrap();
                        for line in BufReader::new(stream).lines() {
                            let Ok(line) = line else { return };
                            let Ok(r) = serde_json::from_str::<Value>(&line) else {
                                return;
                            };
                            let reply = match (r["method"].as_str(), &*listing) {
                                (Some("hello"), _) => json!({"jsonrpc": "2.0", "id": r["id"],
                                    "result": {"version": PINNED}}),
                                (Some("terminal.list"), Listing::Terminals(items)) => {
                                    *lists.lock().unwrap() += 1;
                                    json!({"jsonrpc": "2.0", "id": r["id"],
                                        "result": {"terminals": items}})
                                }
                                (Some("terminal.list"), Listing::Error) => {
                                    *lists.lock().unwrap() += 1;
                                    json!({"jsonrpc": "2.0", "id": r["id"],
                                        "error": {"code": -32000, "message": "the list broke"}})
                                }
                                _ => json!({"jsonrpc": "2.0", "id": r["id"],
                                    "error": {"code": -32601, "message": "not in this stand-in"}}),
                            };
                            if writeln!(writer, "{reply}").is_err() {
                                return;
                            }
                        }
                    });
                }
            });
        }
        Setup { root, lists }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    fn key(&self) -> PathBuf {
        self.path("config/makiba/approval.key")
    }

    /// `makiba up --no-secretary` from a login shell of this root, on the real base.
    fn up(&self) -> Output {
        let mut c = makibad::up(self.root.path());
        c.arg("--no-secretary");
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
        c.output().unwrap()
    }
}

impl Drop for Setup {
    /// A `makibad` that an `up` started is ended here.
    fn drop(&mut self) {
        let pid: Option<i32> = fs::read_to_string(self.path("state/makiba/makibad.lock"))
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

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_session_of_terminals_none_of_which_may_read_it_gets_the_key() {
    let s = Setup::new(vec![(
        "makiba",
        Listing::Terminals(vec![
            terminal("a shell", Value::Null, "running"),
            terminal(
                "w-gone",
                json!({"makiba.role": "worker", "makiba.org": OTHER_ORG}),
                "finished",
            ),
        ]),
    )]);
    let out = s.up();
    let said = said(&out);
    let created = format!(
        "approval key: {} (created); its line for .github/acceptance-approvers: {SIGNER}",
        s.key().display()
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.starts_with(&created)),
        "no line {created:?}: {said}"
    );
    let mode = fs::metadata(s.key()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{said}");
    assert!(!said.contains(NO_ORG), "{said}");
    assert!(
        *s.lists.lock().unwrap() >= 1,
        "the session was not listed: {said}"
    );
}

#[test]
fn a_live_agent_of_another_organization_in_any_session_blocks_the_key() {
    let s = Setup::new(vec![
        (
            "makiba",
            Listing::Terminals(vec![terminal("a shell", Value::Null, "running")]),
        ),
        (
            "other",
            Listing::Terminals(vec![terminal(
                "w1",
                json!({"makiba.role": "worker", "makiba.org": OTHER_ORG}),
                "running",
            )]),
        ),
    ]);
    let out = s.up();
    let said = said(&out);
    assert!(
        said.contains(&format!(
            "approval key: not made: agents on the base may read it: other: w1 of organization \
             {OTHER_ORG}; {THE_WAY}"
        )),
        "{said}"
    );
    assert!(!s.key().exists(), "the key was made: {said}");
    assert!(!said.contains(NO_ORG), "{said}");
}

#[test]
fn a_session_that_cannot_be_listed_blocks_the_key() {
    let s = Setup::new(vec![("makiba", Listing::Error)]);
    let out = s.up();
    let said = said(&out);
    assert!(
        said.contains("approval key: not made: cannot list the terminals of session makiba: "),
        "{said}"
    );
    assert!(!s.key().exists(), "the key was made: {said}");
    assert!(!said.contains(NO_ORG), "{said}");
}
