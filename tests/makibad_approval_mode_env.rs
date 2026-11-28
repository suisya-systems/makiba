//! Issue 637: a `makibad` a test starts as the `makiba` binary (through tests/common/makibad.rs)
//! does not inherit the merge approval mode of the test's own process, which on a machine that
//! runs `makibad` is the person's (`MAKIBA_MERGE_APPROVAL_MODE=acceptance`); a mode the test sets
//! on the command still reaches it.
//!
//! Each case runs in a child of this test binary whose own environment carries the decoy
//! `MAKIBA_MERGE_APPROVAL_MODE=acceptance`. The child starts `makibad` on the stand-in base, with
//! no door, and reads the mode `makibad` records in its `dispatcher_started` (src/daemon/mod.rs).

mod common;

use common::makibad;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MODE_ENV: &str = makiba::daemon::gate::MODE_ENV;
/// Set in the child that runs a case's body.
const CHILD: &str = "MAKIBAD_MODE_CHILD";
const DECOY: &str = "acceptance";
const WAIT: Duration = Duration::from_secs(60);

/// Run the calling test's body in a child of this binary whose environment holds the decoy mode:
/// `true` in the child (run the body), `false` in the parent once the child passed.
fn in_decoy(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        assert_eq!(
            std::env::var(MODE_ENV).as_deref(),
            Ok(DECOY),
            "the child's own environment holds the decoy"
        );
        return true;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD, "1")
        .env(MODE_ENV, DECOY)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "{test} with the decoy mode failed:\n{text}"
    );
    assert!(
        text.contains("1 passed"),
        "{test} did not run in the child:\n{text}"
    );
    false
}

/// A `makibad` killed when dropped, so a failed case leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The newest `dispatcher_started` payload in `state.db`, if any yet.
fn started(state: &Path) -> Option<Value> {
    let c = Connection::open_with_flags(state.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()?;
    c.busy_timeout(Duration::from_secs(5)).ok()?;
    let p: String = c
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'dispatcher_started'
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()?;
    serde_json::from_str(&p).ok()
}

/// Start `makibad` through tests/common/makibad.rs in a folder of its own, with the mode set on
/// the command to `given` or not set at all; the mode it records at start.
fn started_mode(given: Option<&str>) -> Value {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let state = root.join("state");
    for d in ["state", "home", "config", "xdg-state", "data", "run", "tmp"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let log = root.join("makibad.log");
    let mut c = makibad::daemon(root);
    c.env("MAKIBA_STATE_DIR", &state)
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE", "stub")
        .env("MAKIBA_PERSON_KEY", root.join("key/person.key"))
        .env("MAKIBA_GATE_KEY", root.join("key/gate.key"))
        .env("MAKIBA_WORKERS_DIR", root.join("workers"))
        .env("MAKIBA_REPOS_DIR", root.join("repos"))
        .env("MAKIBA_DOCS_DIR", root.join("docs"))
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("xdg-state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("TMPDIR", root.join("tmp"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(m) = given {
        c.env(MODE_ENV, m);
    }
    let _d = Daemon(
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + WAIT;
    loop {
        if let Some(p) = started(&state) {
            return p["merge_approval_mode"].clone();
        }
        assert!(
            Instant::now() < until,
            "makibad wrote no dispatcher_started within {WAIT:?}:\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The decoy in the test's own process does not reach the `makibad` it starts: `makibad` runs
/// under its default, `human`.
#[test]
fn a_decoy_mode_does_not_reach_the_makibad_a_test_starts() {
    if !in_decoy("a_decoy_mode_does_not_reach_the_makibad_a_test_starts") {
        return;
    }
    assert_eq!(
        started_mode(None),
        json!("human"),
        "makibad inherited the test process's mode"
    );
}

/// A mode the test sets on the command reaches `makibad`, whatever the test's own process holds.
#[test]
fn a_mode_the_test_sets_reaches_makibad() {
    if !in_decoy("a_mode_the_test_sets_reaches_makibad") {
        return;
    }
    assert_eq!(started_mode(Some("acceptance")), json!("acceptance"));
    assert_eq!(started_mode(Some("human")), json!("human"));
}
