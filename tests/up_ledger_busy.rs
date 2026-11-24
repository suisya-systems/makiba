//! Issue 288: `makiba up` meets a ledger (`state.db`) whose write lock another process holds.
//! A lock held longer than the 5 s busy timeout of a connection, but shorter than up's own bound,
//! is waited for; one held past the bound ends `up` with "the ledger is busy", not an internal
//! error.
//!
//! The stand-ins are those of `tests/up.rs`; the stand-in `claude` also leaves a mark, so the test
//! knows `up` has started. The lock is taken before `up` starts, so `up` always meets it.

mod common;

use common::makibad;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

/// Longer than the 5 s busy timeout of every connection (src/db/mod.rs).
const HELD: Duration = Duration::from_secs(8);
/// Longer than any bound `up` may take; the test's own watchdog.
const WATCHDOG: Duration = Duration::from_secs(120);

fn in_range() -> String {
    let (a, b, c) = makiba::agents::version::LOWEST;
    format!("{a}.{b}.{c} (Claude Code)")
}

struct Setup {
    root: tempfile::TempDir,
}

impl Setup {
    fn new() -> Setup {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let mark = root.path().join("claude.mark");
        script(
            &bin.join("claude"),
            &format!("touch {}\necho '{}'", mark.display(), in_range()),
        );
        let rec = root.path().join("hc.rec");
        script(
            &bin.join("hc"),
            &format!(
                "echo \"CALL $*\" >> {rec}\nenv | sort >> {rec}",
                rec = rec.display()
            ),
        );
        Setup { root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    fn spawn_up(&self) -> Child {
        let _ = fs::remove_file(self.path("claude.mark"));
        makibad::up(self.root.path())
            .env_clear()
            .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(self.root.path()))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("HC_BIN", self.path("bin/hc"))
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Wait until the stand-in `claude` has run: `up` has started and comes to the ledger next.
    fn wait_started(&self, child: &mut Child) {
        let until = Instant::now() + WATCHDOG;
        while !self.path("claude.mark").exists() {
            assert!(Instant::now() < until, "up did not start");
            if child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn ledger(&self) -> PathBuf {
        self.path("state/makiba/state.db")
    }

    fn lock(&self) -> PathBuf {
        self.path("state/makiba/makibad.lock")
    }

    fn daemon_pid(&self) -> Option<i32> {
        fs::read_to_string(self.lock()).ok()?.trim().parse().ok()
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until && unsafe { libc::kill(pid, 0) } == 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn script(path: &Path, body: &str) {
    common::exe::write(path, format!("#!/bin/sh\n{body}\n"));
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Take the ledger's write lock on a connection of the test's own, with a write in it.
fn hold(path: &Path) -> Connection {
    let c = Connection::open(path).unwrap();
    c.busy_timeout(Duration::from_secs(30)).unwrap();
    c.execute_batch(
        "BEGIN IMMEDIATE;
         UPDATE org_sessions SET last_writer_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = 1;",
    )
    .unwrap();
    c
}

/// Wait for `child` to end, at most `WATCHDOG`; a child still running is killed and fails the test.
fn finish(mut child: Child) -> Output {
    let until = Instant::now() + WATCHDOG;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > until {
            let _ = child.kill();
            panic!("up still ran after {WATCHDOG:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn a_second_up_waits_for_a_ledger_held_past_the_busy_timeout() {
    let s = Setup::new();
    let first = finish(s.spawn_up());
    assert!(first.status.success(), "{}", text(&first));
    let pid = s.daemon_pid().expect("makibad is not running");

    let held = hold(&s.ledger());
    let mut child = s.spawn_up();
    s.wait_started(&mut child);
    std::thread::sleep(HELD);
    held.execute_batch("COMMIT").unwrap();
    drop(held);

    let again = finish(child);
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("already running"), "{}", text(&again));
    assert_eq!(s.daemon_pid(), Some(pid), "a second makibad was started");
}

#[test]
fn up_on_a_ledger_held_past_its_bound_says_the_ledger_is_busy() {
    let s = Setup::new();
    fs::create_dir_all(s.ledger().parent().unwrap()).unwrap();
    {
        let mut c = makiba::db::open(&s.ledger()).unwrap();
        makiba::db::migrate(&mut c).unwrap();
    }
    let held = hold(&s.ledger());
    let mut child = s.spawn_up();
    s.wait_started(&mut child);
    let started = Instant::now();
    let out = finish(child);
    let waited = started.elapsed();
    held.execute_batch("ROLLBACK").unwrap();

    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("makiba: unavailable:"), "{said}");
    assert!(said.contains("ledger") && said.contains("busy"), "{said}");
    assert!(!said.contains("internal"), "{said}");
    assert!(
        waited >= HELD,
        "up gave up after {waited:?}, before it waited past the busy timeout"
    );
    assert!(
        !s.lock().exists() || s.daemon_pid().is_none(),
        "makibad was started"
    );
    assert!(!s.path("hc.rec").exists(), "the base was started");
}
